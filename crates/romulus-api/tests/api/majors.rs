//! Several Postgres majors per database (docs/MVP.md, "Several Postgres
//! majors"): the major chosen at creation, compiles routed to it (uploads,
//! pushes, imports, drafts), moving to another one after a dry run, what
//! versions keep, pushes from another major flagged, migrations planned for
//! the database's major, and what the server says it compiles.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use romulus_api::{
    ApiError, AppState, Config,
    collab::Hub,
    compile::{Code, Compiled, Diagnostic, LineIndex, SqlCompiler},
    domain::{MigrationFormat, PgMajor, SourceKind, TokenScope, VersionOrigin},
    versions::store::{NewImport, NewVersion, StoreError, store, store_import},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::Text;

use crate::{
    harness::{Team, TestApp, api_token, database, fake_compile, model},
    live::{Client, quick, serve},
};

/// What Postgres says, before 17, of an exclusion constraint on a
/// partitioned table.
const NOT_BEFORE_17: &str = "exclusion constraints are not supported on partitioned tables";

/// Schema SQL only 17 and 18 compile.
const BOOKINGS: &str = "CREATE TABLE bookings (\n  room int,\n  during tstzrange,\n  EXCLUDE USING gist (room WITH =, during WITH &&)\n) PARTITION BY LIST (room);\n";

/// The harness's fake compiler, on whichever major it is asked: its model
/// says it was compiled there, and before 17 it refuses an exclusion
/// constraint on a partitioned table, located, as Postgres does.
fn majors_compiler() -> SqlCompiler {
    SqlCompiler::from_fn_per_major(|major, sql| {
        if major.get() < 17
            && let Some(at) = sql.find("EXCLUDE")
            && sql.contains("PARTITION BY")
        {
            return Compiled {
                model: None,
                diagnostics: vec![
                    Diagnostic::error(Code::Sql, NOT_BEFORE_17)
                        .with_span(LineIndex::new(sql).span(sql, at..at + 7)),
                ],
            };
        }
        let mut compiled = fake_compile(sql);
        if let Some(model) = &mut compiled.model {
            model.server_version = format!("{major}.4 (sandbox)");
        }
        compiled
    })
}

fn app(db: &PgPool) -> TestApp {
    TestApp::with_state(
        AppState::new(db.clone(), Config::for_tests()).with_compiler(majors_compiler()),
    )
}

async fn create(app: &TestApp, team: &Team, slug: &str, major: Option<i64>) -> Value {
    let mut body = json!({ "name": slug, "slug": slug });
    if let Some(major) = major {
        body["pg_major"] = json!(major);
    }
    let created = app
        .as_user(&team.developer)
        .post(&team.path("/databases"), &body)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    created.json()
}

async fn upload(app: &TestApp, team: &Team, slug: &str, sql: &str) -> crate::harness::TestResponse {
    app.as_user(&team.developer)
        .post(
            &team.path(&format!("/databases/{slug}/versions")),
            &json!({ "sql": sql }),
        )
        .await
}

async fn history(app: &TestApp, team: &Team, slug: &str) -> Vec<Value> {
    app.as_user(&team.reviewer)
        .get(&team.path(&format!("/databases/{slug}/versions")))
        .await
        .json()["versions"]
        .as_array()
        .expect("versions")
        .clone()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_targets_17_unless_another_major_is_chosen(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);

    assert_eq!(create(&app, &team, "default", None).await["pg_major"], 17);
    assert_eq!(
        create(&app, &team, "legacy", Some(15)).await["pg_major"],
        15
    );
    let read = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/legacy"))
        .await
        .json();
    assert_eq!(read["pg_major"], 15);
    let listed = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases"))
        .await
        .json();
    let majors: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["slug"].clone(), d["pg_major"].clone()))
        .collect();
    assert!(majors.contains(&(json!("legacy"), json!(15))), "{majors:?}");

    for unsupported in [json!(14), json!(19), json!("17")] {
        let refused = app
            .as_user(&team.developer)
            .post(
                &team.path("/databases"),
                &json!({ "name": "x", "slug": "x", "pg_major": unsupported }),
            )
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{unsupported}"
        );
        assert_eq!(refused.code(), "validation");
        assert!(
            refused.json().to_string().contains("pg_major"),
            "{}",
            refused.json()
        );
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn uploads_and_dry_runs_compile_on_the_database_s_major(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    create(&app, &team, "legacy", Some(15)).await;
    create(&app, &team, "current", None).await;

    // PostgreSQL 15 refuses what 17 has, located in the text.
    let refused = upload(&app, &team, "legacy", BOOKINGS).await;
    assert_eq!(refused.code(), "compile_failed", "{}", refused.json());
    let error = &refused.json()["details"]["diagnostics"][0];
    assert_eq!(error["message"], NOT_BEFORE_17);
    assert_eq!(error["span"]["from"]["line"], 4);
    let checked = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/legacy/compile"),
            &json!({ "sql": BOOKINGS }),
        )
        .await
        .json();
    assert_eq!(checked["diagnostics"][0]["message"], NOT_BEFORE_17);

    // 17 compiles it.
    let created = upload(&app, &team, "current", BOOKINGS).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());

    // What compiled where is kept with each version.
    let created = upload(&app, &team, "legacy", "CREATE TABLE users (id bigint);").await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    assert_eq!(
        created.json()["postgres"],
        json!({
            "server_version": "15.4 (sandbox)", "major": 15, "database_major": 15,
            "other_major": false,
        })
    );
    assert_eq!(
        history(&app, &team, "current").await[0]["postgres"]["major"],
        17
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_major_the_latest_version_does_not_compile_on_is_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    create(&app, &team, "rooms", None).await;
    assert_eq!(
        upload(&app, &team, "rooms", BOOKINGS).await.status,
        StatusCode::CREATED
    );
    let dev = app.as_user(&team.developer);

    // The dry run says why, where, and changes nothing.
    let check = dev
        .post(
            &team.path("/databases/rooms/major/check"),
            &json!({ "pg_major": 15 }),
        )
        .await;
    assert_eq!(check.status, StatusCode::OK, "{}", check.json());
    let check = check.json();
    assert_eq!(check["compiles"], false);
    assert_eq!(
        (check["against"].clone(), check["against_major"].clone()),
        (json!(1), json!(17))
    );
    assert_eq!(check["diagnostics"][0]["message"], NOT_BEFORE_17);
    assert_eq!(check["sql"], BOOKINGS);

    let refused = dev
        .put(
            &team.path("/databases/rooms/major"),
            &json!({ "pg_major": 15 }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "incompatible_major");
    assert_eq!(
        refused.json()["message"],
        "v1 does not compile on Postgres 15. Fix the error first, or stay on 17."
    );
    assert_eq!(
        refused.json()["details"]["diagnostics"][0]["message"],
        NOT_BEFORE_17
    );
    let unchanged = dev.get(&team.path("/databases/rooms")).await.json();
    assert_eq!(unchanged["pg_major"], 17);

    // 18 compiles it: the move goes through, and is in the activity.
    let check = dev
        .post(
            &team.path("/databases/rooms/major/check"),
            &json!({ "pg_major": 18 }),
        )
        .await
        .json();
    assert_eq!(check["compiles"], true);
    assert_eq!(check["changes"]["unchanged"], true);
    assert_eq!(check["sql"], Value::Null);
    let moved = dev
        .put(
            &team.path("/databases/rooms/major"),
            &json!({ "pg_major": 18 }),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.json());
    assert_eq!(moved.json()["pg_major"], 18);
    let activity = dev
        .get(&team.path("/databases/rooms/activity"))
        .await
        .json();
    let event = &activity["events"][0];
    assert_eq!(event["action"], "major_changed");
    assert_eq!(
        (
            event["details"]["from"].clone(),
            event["details"]["to"].clone()
        ),
        (json!(17), json!(18))
    );
    assert_eq!(event["actor"]["name"], "Developer-acme");
    assert_eq!(event["version_number"], 1);

    // The version keeps the major it was compiled on; the next one is 18's.
    let created = upload(&app, &team, "rooms", "CREATE TABLE guests (id bigint);").await;
    assert_eq!(created.json()["postgres"]["major"], 18);
    let versions = history(&app, &team, "rooms").await;
    assert_eq!(versions[1]["postgres"]["major"], 17);
    assert_eq!(versions[1]["postgres"]["database_major"], 17);

    // Moving to where it is already is nothing.
    let same = dev
        .put(
            &team.path("/databases/rooms/major"),
            &json!({ "pg_major": 18 }),
        )
        .await;
    assert_eq!(same.status, StatusCode::OK);
    let events = dev
        .get(&team.path("/databases/rooms/activity"))
        .await
        .json()["events"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(events, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_developers_move_a_database(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    create(&app, &team, "rooms", None).await;
    let body = json!({ "pg_major": 16 });
    for path in ["/databases/rooms/major/check"] {
        let reviewer = app
            .as_user(&team.reviewer)
            .post(&team.path(path), &body)
            .await;
        assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
        let outsider = app
            .as_user(&team.outsider)
            .post(&team.path(path), &body)
            .await;
        assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    }
    let path = team.path("/databases/rooms/major");
    assert_eq!(
        app.as_user(&team.reviewer).put(&path, &body).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.as_user(&team.outsider).put(&path, &body).await.status,
        StatusCode::NOT_FOUND
    );
    let unsupported = app
        .as_user(&team.developer)
        .put(&path, &json!({ "pg_major": 13 }))
        .await;
    assert_eq!(unsupported.code(), "validation");
    // Admins may.
    let admin = app.as_user(&team.admin).put(&path, &body).await;
    assert_eq!(admin.json()["pg_major"], 16);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_without_versions_moves_without_a_compiler(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    // No sandbox at all: nothing to compile.
    let app = TestApp::new(db.clone());
    create(&app, &team, "empty", None).await;
    let dev = app.as_user(&team.developer);
    let check = dev
        .post(
            &team.path("/databases/empty/major/check"),
            &json!({ "pg_major": 16 }),
        )
        .await
        .json();
    assert_eq!(
        (check["compiles"].clone(), check["against"].clone()),
        (json!(true), Value::Null)
    );
    let moved = dev
        .put(
            &team.path("/databases/empty/major"),
            &json!({ "pg_major": 16 }),
        )
        .await;
    assert_eq!(moved.json()["pg_major"], 16);
}

/// A remus push of `tables`, as read from a server on `server_version`.
fn pushed(tables: &[&str], server_version: &str) -> String {
    let mut model = model(tables);
    server_version.clone_into(&mut model.server_version);
    serde_json::to_string(&model).unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_push_from_another_major_is_flagged_and_stays_flagged(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let token = api_token(&db, shop, TokenScope::Push).await;
    let app = app(&db);

    let json = Some("application/json");
    let push = app
        .push(
            shop,
            &token,
            json,
            pushed(&["users"], "16.4 (Debian 16.4-1.pgdg120+2)"),
        )
        .await;
    assert_eq!(push.status, StatusCode::CREATED, "{}", push.json());
    let push = app
        .push(shop, &token, json, pushed(&["users", "orders"], "17.2"))
        .await;
    assert_eq!(push.status, StatusCode::CREATED);
    // SQL is compiled on the database's own major: never flagged.
    let sql = app
        .push(
            shop,
            &token,
            Some("application/sql"),
            "CREATE TABLE users (id bigint);",
        )
        .await;
    assert_eq!(sql.status, StatusCode::CREATED, "{}", sql.json());

    let flags = |versions: &[Value]| -> Vec<(i64, bool, Value)> {
        versions
            .iter()
            .map(|v| {
                (
                    v["number"].as_i64().unwrap(),
                    v["postgres"]["other_major"].as_bool().unwrap(),
                    v["postgres"]["major"].clone(),
                )
            })
            .collect()
    };
    assert_eq!(
        flags(&history(&app, &team, "shop").await),
        [
            (3, false, json!(17)),
            (2, false, json!(17)),
            (1, true, json!(16))
        ]
    );
    let first = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/versions/1"))
        .await
        .json();
    assert_eq!(
        first["postgres"]["server_version"],
        "16.4 (Debian 16.4-1.pgdg120+2)"
    );
    assert_eq!(first["postgres"]["database_major"], 17);

    // Moving the database to 16 keeps the history as it was pushed.
    let moved = app
        .as_user(&team.developer)
        .put(
            &team.path("/databases/shop/major"),
            &json!({ "pg_major": 16 }),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.json());
    let push = app
        .push(shop, &token, json, pushed(&["users", "carts"], "16.4"))
        .await;
    assert_eq!(push.status, StatusCode::CREATED);
    assert_eq!(
        flags(&history(&app, &team, "shop").await),
        [
            (4, false, json!(16)),
            (3, false, json!(17)),
            (2, false, json!(17)),
            (1, true, json!(16)),
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn imports_replay_on_the_database_s_major(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    let created = create(&app, &team, "legacy", Some(15)).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let token = api_token(&db, id, TokenScope::Push).await;
    let import = |sql: &'static str| {
        Request::post(format!("/api/databases/{id}/imports?filename=V1__init.sql"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/sql")
            .body(Body::from(sql))
            .unwrap()
    };

    let failed = app.send(import(BOOKINGS)).await;
    assert_eq!(failed.code(), "import_failed", "{}", failed.json());
    assert!(failed.json().to_string().contains(NOT_BEFORE_17));

    let imported = app.send(import("CREATE TABLE users (id bigint);")).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    assert_eq!(
        history(&app, &team, "legacy").await[0]["postgres"]["major"],
        15
    );
}

/// Version `number` of `database` as a push: one table `t` with a column
/// `g` generated from `a` by `expression`.
async fn generated_version(db: &PgPool, database: Uuid, number: i32, expression: &str) {
    let model = json!({
        "remus_version": "0.2", "database": "shop", "server_version": "17.4",
        "entities": [{
            "schema": "public", "name": "t", "kind": "r", "comment": null,
            "partition_key": null, "view_definition": null,
            "columns": [
                {"name": "a", "type": "integer", "type_schema": "pg_catalog", "type_name": "int4",
                 "type_kind": "b", "not_null": false, "default": null, "identity": null,
                 "generated": null, "comment": null},
                {"name": "g", "type": "integer", "type_schema": "pg_catalog", "type_name": "int4",
                 "type_kind": "b", "not_null": false, "default": expression, "identity": null,
                 "generated": "s", "comment": null},
            ],
        }],
    });
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, $2, $3, $4, 'remus_json', 'push', '{}')",
        database,
        number,
        model,
        model.to_string(),
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn migrations_are_planned_for_the_database_s_major(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    generated_version(&db, shop, 1, "(a * 2)").await;
    generated_version(&db, shop, 2, "(a * 3)").await;
    let app = app(&db);
    let migration = || async {
        let body = app
            .as_user(&team.reviewer)
            .get(&team.path("/databases/shop/migration?from=1&to=2"))
            .await
            .json();
        let header = body["files"][0]["content"]
            .as_str()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let major = body["pg_major"].clone();
        assert!(
            header.contains(&format!("for PostgreSQL {major},")),
            "{header}"
        );
        body["statements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["sql"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        migration().await,
        ["ALTER TABLE public.t ALTER COLUMN g SET EXPRESSION AS ((a * 3));"]
    );
    let moved = app
        .as_user(&team.developer)
        .put(
            &team.path("/databases/shop/major"),
            &json!({ "pg_major": 16 }),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.json());
    // 16 has no SET EXPRESSION: dropped and added again.
    assert_eq!(
        migration().await,
        [
            "ALTER TABLE public.t DROP COLUMN g;",
            "ALTER TABLE public.t ADD COLUMN g integer GENERATED ALWAYS AS ((a * 3)) STORED;",
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_server_says_which_majors_it_compiles(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    let majors = app
        .as_user(&team.reviewer)
        .get("/api/postgres-majors")
        .await
        .json();
    assert_eq!(
        majors,
        json!([
            {"major": 15, "default": false, "status": "ok", "server_version": "15.0"},
            {"major": 16, "default": false, "status": "ok", "server_version": "16.0"},
            {"major": 17, "default": true, "status": "ok", "server_version": "17.0"},
            {"major": 18, "default": false, "status": "ok", "server_version": "18.0"},
        ])
    );
    assert_eq!(
        app.get("/api/postgres-majors").await.status,
        StatusCode::UNAUTHORIZED
    );

    let health = app.get("/api/health").await.json();
    assert_eq!(health["sandbox"], "ok");
    assert_eq!(
        health["sandboxes"],
        json!({"15": "ok", "16": "ok", "17": "ok", "18": "ok"})
    );
    // Without sandboxes, none compiles.
    let off = TestApp::new(db.clone()).get("/api/health").await.json();
    assert_eq!(off["sandbox"], "off");
    assert_eq!(off["sandboxes"]["15"], "off");
    let none = TestApp::new(db)
        .as_user(&team.reviewer)
        .get("/api/postgres-majors")
        .await
        .json();
    assert_eq!(
        none[1],
        json!({"major": 16, "default": false, "status": "off", "server_version": null})
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_open_draft_compiles_on_the_new_major_once_the_database_moves(db: PgPool) {
    let state = AppState::new(db.clone(), Config::for_tests()).with_compiler(majors_compiler());
    let hub = Hub::with_settings(db.clone(), state.compiler.clone(), quick());
    let app = TestApp::with_state(state.with_collab(hub));
    let team = Team::new(&db, "acme").await;
    create(&app, &team, "rooms", None).await;
    assert_eq!(
        upload(&app, &team, "rooms", "CREATE TABLE users (id bigint);\n")
            .await
            .status,
        StatusCode::CREATED
    );
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/rooms/drafts"))
        .await
        .json();
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let addr = serve(&app).await;
    let mut alice = Client::join(addr, &ws, &team.developer).await;
    alice.edit(|roots, txn| roots.sql.push(txn, BOOKINGS)).await;
    alice
        .until(|c| {
            c.last_event("compile").is_some_and(|e| {
                e["state"] == "ok"
                    && e["model"]["entities"]
                        .as_array()
                        .is_some_and(|t| t.len() == 2)
            })
        })
        .await;

    // v1 compiles on 15: the database moves, the draft hears it and its
    // text compiles there (where it fails).
    let moved = app
        .as_user(&team.developer)
        .put(
            &team.path("/databases/rooms/major"),
            &json!({ "pg_major": 15 }),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.json());
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;
    let event = alice.last_event("major").expect("told of the move");
    assert_eq!(
        (event["pg_major"].clone(), event["by"].clone()),
        (json!(15), json!("Developer-acme"))
    );
    let compile = alice.last_event("compile").unwrap();
    assert_eq!(compile["diagnostics"][0]["message"], NOT_BEFORE_17);

    // What publishing, Tidy and the migration compile on now.
    let content = app
        .state
        .collab
        .content(draft["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap();
    assert_eq!(content.major, PgMajor::new(15).unwrap());
}

// ---- Review fixes ------------------------------------------------------------

/// A version compiled on one major, stored after the database moved to
/// another (a publish or a push racing a switch): refused under the lock,
/// nothing stored, 409 `major_changed`. Sequenced by storing with the major
/// the compile ran on while the database is already on the other.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_compiled_before_a_move_is_not_stored_after_it(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "shop").await;
    let stale = |compiled_on: Option<PgMajor>| NewVersion {
        database_id: id,
        model: model(&["users"]).canonical(),
        source: "CREATE TABLE users (id bigint);".into(),
        source_kind: SourceKind::Sql,
        origin: VersionOrigin::Draft,
        message: None,
        created_by: None,
        created_by_token: None,
        layout: None,
        expected_base: None,
        publishes_draft: None,
        approvals: None,
        test_override: None,
        migration_sql: None,
        compiled_on,
        discussion: Vec::new(),
        renames: None,
    };
    // Compiled on 17, the database meanwhile on 15.
    sqlx::query!("UPDATE databases SET pg_major = 15 WHERE id = $1", id)
        .execute(&db)
        .await
        .unwrap();
    let refused = store(&db, stale(PgMajor::new(17)), false)
        .await
        .unwrap_err();
    let StoreError::MajorChanged { compiled_on, now } = refused else {
        panic!("expected a major change, got {refused:?}");
    };
    assert_eq!((compiled_on.get(), now.get()), (17, 15));
    match ApiError::from(StoreError::MajorChanged { compiled_on, now }) {
        ApiError::Conflict { code, message } => {
            assert_eq!(code, "major_changed");
            assert!(message.contains("Try again"), "{message}");
        }
        other => panic!("expected a 409, got {other:?}"),
    }
    let stored: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(stored, 0);

    // An import replayed on 17 is refused the same way.
    let import = store_import(
        &db,
        NewImport {
            database_id: id,
            format: MigrationFormat::Sqlx,
            created_by: None,
            created_by_token: None,
            append: false,
            imported_before: 0,
            latest_id: None,
            last_number: 0,
            staging: Uuid::from_u128(7),
            versions: Vec::new(),
            replayed_on: PgMajor::DEFAULT,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(import, StoreError::MajorChanged { .. }),
        "{import:?}"
    );

    // Compiled on the database's major, or not compiled here (remus JSON):
    // stored.
    assert!(store(&db, stale(PgMajor::new(15)), false).await.is_ok());
    let mut json = stale(None);
    json.model = model(&["users", "orders"]).canonical();
    assert!(store(&db, json, false).await.is_ok());
}

/// The publish path reports it as such: the draft's compile ran on the old
/// major (the room's), the database is now on another.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_after_a_move_answers_major_changed(db: PgPool) {
    let state = AppState::new(db.clone(), Config::for_tests()).with_compiler(majors_compiler());
    let team = Team::new(&db, "acme").await;
    let app = TestApp::with_state(state);
    create(&app, &team, "rooms", None).await;
    let database_id: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'rooms'")
        .fetch_one(&db)
        .await
        .unwrap();
    let draft_id: Uuid = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/rooms/drafts"))
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query!(
        "UPDATE databases SET pg_major = 16 WHERE id = $1",
        database_id
    )
    .execute(&db)
    .await
    .unwrap();
    let refused = romulus_api::drafts::versions::create_version(
        &db,
        romulus_api::drafts::versions::DraftVersion {
            database_id,
            draft: draft_id,
            expected_base: None,
            model: model(&["rooms"]).canonical(),
            source: "CREATE TABLE rooms (id bigint);".into(),
            migration_sql: None,
            layout: romulus_api::versions::layout::VersionLayout::default(),
            message: None,
            created_by: Uuid::nil(),
            approvals: None,
            test_override: None,
            compiled_on: PgMajor::DEFAULT,
            discussion: Vec::new(),
            renames: None,
        },
    )
    .await
    .err()
    .expect("refused");
    assert!(
        matches!(
            refused,
            ApiError::Conflict {
                code: "major_changed",
                ..
            }
        ),
        "{refused:?}"
    );
}

/// A server compiles only the majors it has a sandbox for: a database is
/// not created on another one, nor moved there.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_major_this_server_does_not_compile_is_refused(db: PgPool) {
    let mut config = Config::for_tests();
    // 17 only (unreachable: whether it answers does not matter here).
    config.sandbox_urls.insert(
        PgMajor::DEFAULT,
        "postgres://nobody@127.0.0.1:1/postgres".into(),
    );
    let compiler = SqlCompiler::sandbox(&config);
    assert!(compiler.serves(PgMajor::DEFAULT));
    assert!(!compiler.serves(PgMajor::new(15).unwrap()));
    let app = TestApp::with_state(AppState::new(db.clone(), config).with_compiler(compiler));
    let team = Team::new(&db, "acme").await;
    let dev = app.as_user(&team.developer);

    let refused = dev
        .post(
            &team.path("/databases"),
            &json!({ "name": "Legacy", "slug": "legacy", "pg_major": 15 }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "major_unavailable");
    assert_eq!(
        refused.json()["message"],
        "This server does not compile Postgres 15: it has no sandbox for it. Use another version, or ask whoever runs romulus to add it."
    );
    create(&app, &team, "current", None).await;
    for (method, path) in [
        ("check", "/databases/current/major/check"),
        ("switch", "/databases/current/major"),
    ] {
        let body = json!({ "pg_major": 16 });
        let response = if method == "check" {
            dev.post(&team.path(path), &body).await
        } else {
            dev.put(&team.path(path), &body).await
        };
        assert_eq!(
            response.code(),
            "major_unavailable",
            "{method}: {}",
            response.json()
        );
    }
}

/// `PgMajor` is read from SQL like from JSON: another number is an error.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unsupported_major_never_reads_as_one(db: PgPool) {
    let fifteen = sqlx::query_scalar!(r#"SELECT 15::smallint AS "m!: PgMajor""#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(fifteen, PgMajor::new(15).unwrap());
    assert!(
        sqlx::query_scalar!(r#"SELECT 14::smallint AS "m!: PgMajor""#)
            .fetch_one(&db)
            .await
            .is_err()
    );
}

/// A draft is seeded with its version's compile only under a major romulus
/// compiles: a version claiming another (14) seeds under the database's.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_is_never_seeded_under_an_unsupported_major(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "shop").await;
    let mut old = model(&["users"]);
    "14.9".clone_into(&mut old.server_version);
    let old = serde_json::to_value(old).unwrap();
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, 1, $2, 'CREATE TABLE users (id bigint);', 'sql', 'upload', '{}')",
        id,
        old,
    )
    .execute(&db)
    .await
    .unwrap();
    let app = app(&db);
    let draft: Uuid = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let seeded = sqlx::query_scalar!(
        r#"SELECT pg_major AS "pg_major: PgMajor" FROM draft_compiles WHERE draft_id = $1"#,
        draft
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(seeded, PgMajor::DEFAULT);
}

/// SQL pushed with a token compiles on the database's major, like an upload.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_sql_push_compiles_on_the_database_s_major(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = app(&db);
    let created = create(&app, &team, "legacy", Some(15)).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let token = api_token(&db, id, TokenScope::Push).await;
    let sql = Some("application/sql");

    let refused = app.push(id, &token, sql, BOOKINGS).await;
    assert_eq!(refused.code(), "compile_failed", "{}", refused.json());
    assert_eq!(
        refused.json()["details"]["diagnostics"][0]["message"],
        NOT_BEFORE_17
    );

    let pushed = app
        .push(id, &token, sql, "CREATE TABLE users (id bigint);")
        .await;
    assert_eq!(pushed.status, StatusCode::CREATED, "{}", pushed.json());
    let versions = history(&app, &team, "legacy").await;
    assert_eq!(
        versions[0]["postgres"],
        json!({
            "server_version": "15.4 (sandbox)", "major": 15, "database_major": 15,
            "other_major": false,
        })
    );
}
