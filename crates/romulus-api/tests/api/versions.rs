//! Versions from the browser: SQL uploads, the compile dry run, listing,
//! reading and rendering; numbering; and `store`'s guarantees.

use std::time::Duration;

use axum::http::StatusCode;
use romulus_api::{
    AppState, Config,
    compile::SqlCompiler,
    domain::{SourceKind, TokenScope, VersionOrigin},
    versions::{
        layout::VersionLayout,
        store::{NewVersion, StoreError, Stored, store},
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::harness::{Team, TestApp, api_token, database, model, model_json};

const BILLING: &str = "CREATE TABLE invoices (id bigint);\nCREATE TABLE plans (id bigint);\n";

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_upload_stores_the_model_and_the_sql_verbatim(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db);
    let dev = app.as_user(&team.developer);

    let response = dev
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": BILLING, "message": "  First cut  " }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let created = response.json();
    assert_eq!(created["number"], 1);
    assert_eq!(created["origin"], "upload");
    assert_eq!(created["source_kind"], "sql");
    assert_eq!(created["message"], "First cut");
    assert_eq!(
        created["author"],
        json!({ "kind": "user", "id": team.developer.user, "name": "Developer-acme" })
    );

    let version = dev
        .get(&team.path("/databases/billing/versions/1"))
        .await
        .json();
    assert_eq!(version["source"], BILLING);
    // The model is named after the database, not the sandbox it compiled in.
    assert_eq!(version["model"]["database"], "billing");
    let tables: Vec<&str> = version["model"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(tables, ["invoices", "plans"]);
    assert_eq!(version["layout"], json!({ "layout": {}, "groups": {} }));

    // The SQL tab shows exactly what was uploaded; the rest is rendered.
    let texts = dev
        .get(&team.path("/databases/billing/versions/1/texts"))
        .await
        .json();
    assert_eq!(texts["sql"], BILLING);
    assert!(texts["json"].as_str().unwrap().contains("\"invoices\""));
    assert!(texts["dbml"].as_str().unwrap().contains("invoices"));
    assert!(texts["mermaid"].as_str().unwrap().contains("erDiagram"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn sql_that_does_not_compile_stores_nothing_and_says_where(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db.clone());
    let sql = "CREATE TABLE a (id bigint);\nCREATE oops b ();";

    let response = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": sql }),
        )
        .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = response.json();
    assert_eq!(body["code"], "compile_failed");
    let diagnostic = &body["details"]["diagnostics"][0];
    assert_eq!(diagnostic["code"], "syntax_error");
    assert_eq!(
        diagnostic["span"]["from"],
        json!({ "line": 2, "column": 8 })
    );
    assert_eq!(diagnostic["span"]["utf16_start"], 35);

    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn uploads_are_validated_before_compiling(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db);
    let dev = app.as_user(&team.developer);
    let path = team.path("/databases/billing/versions");

    let blank = dev.post(&path, &json!({ "sql": "  \n " })).await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(blank.json()["details"]["sql"].is_string());
    let long = dev
        .post(
            &path,
            &json!({ "sql": BILLING, "message": "x".repeat(1001) }),
        )
        .await;
    assert!(long.json()["details"]["message"].is_string());
    let missing = dev
        .post(
            &team.path("/databases/nope/versions"),
            &json!({ "sql": BILLING }),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_a_compiler_an_upload_is_a_503_and_stores_nothing(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::new(db.clone());
    let response = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": BILLING }),
        )
        .await;
    assert_eq!(
        (response.status, response.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM versions"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_role_matrix_holds(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db);
    let upload = json!({ "sql": BILLING });
    let path = team.path("/databases/billing/versions");
    let check = team.path("/databases/billing/compile");

    let dev = app.as_user(&team.developer);
    assert_eq!(dev.post(&path, &upload).await.status, StatusCode::CREATED);
    assert_eq!(dev.post(&check, &upload).await.status, StatusCode::OK);
    let admin = app.as_user(&team.admin);
    assert_eq!(admin.post(&path, &upload).await.status, StatusCode::CREATED);

    // Reviewers read every version, and upload nothing.
    let reviewer = app.as_user(&team.reviewer);
    for read in [path.clone(), format!("{path}/1"), format!("{path}/2/texts")] {
        assert_eq!(reviewer.get(&read).await.status, StatusCode::OK, "{read}");
    }
    assert_eq!(
        reviewer.post(&path, &upload).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reviewer.post(&check, &upload).await.status,
        StatusCode::FORBIDDEN
    );

    let outsider = app.as_user(&team.outsider);
    for response in [
        outsider.get(&path).await,
        outsider.get(&format!("{path}/1")).await,
        outsider.get(&format!("{path}/1/texts")).await,
        outsider.post(&path, &upload).await,
        outsider.post(&check, &upload).await,
    ] {
        assert_eq!(response.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(app.get(&path).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_dry_run_says_what_would_change(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db);
    let dev = app.as_user(&team.developer);
    let check = team.path("/databases/billing/compile");

    // Against nothing: what the SQL creates.
    let first = dev.post(&check, &json!({ "sql": BILLING })).await.json();
    assert_eq!(first["diagnostics"], json!([]));
    assert_eq!(first["changes"]["against"], Value::Null);
    assert_eq!(first["changes"]["summary"], "2 tables added");
    assert_eq!(first["changes"]["unchanged"], false);

    dev.post(
        &team.path("/databases/billing/versions"),
        &json!({ "sql": BILLING }),
    )
    .await;
    let same = dev.post(&check, &json!({ "sql": BILLING })).await.json();
    assert_eq!(same["changes"]["against"], 1);
    assert_eq!(same["changes"]["unchanged"], true);
    let more = dev
        .post(
            &check,
            &json!({ "sql": format!("{BILLING}CREATE TABLE coupons (id bigint);") }),
        )
        .await
        .json();
    assert_eq!(more["changes"]["summary"], "1 table added");

    let broken = dev.post(&check, &json!({ "sql": "oops" })).await.json();
    assert_eq!(broken["changes"], Value::Null);
    assert_eq!(broken["diagnostics"][0]["code"], "syntax_error");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn versions_list_newest_first_a_page_at_a_time(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::new(db);
    for n in 1..=5 {
        let tables: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
        let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
        app.push(id, &token, Some("application/json"), model_json(&tables))
            .await;
    }
    let reviewer = app.as_user(&team.reviewer);
    let numbers = |page: &Value| -> Vec<i64> {
        page["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["number"].as_i64().unwrap())
            .collect()
    };

    let first = reviewer
        .get(&team.path("/databases/billing/versions?limit=2"))
        .await
        .json();
    assert_eq!(numbers(&first), [5, 4]);
    assert_eq!(first["next_before"], 4);
    let second = reviewer
        .get(&team.path("/databases/billing/versions?limit=2&before=4"))
        .await
        .json();
    assert_eq!(numbers(&second), [3, 2]);
    let last = reviewer
        .get(&team.path("/databases/billing/versions?limit=2&before=2"))
        .await
        .json();
    assert_eq!(numbers(&last), [1]);
    assert_eq!(last["next_before"], Value::Null);
    let all = reviewer
        .get(&team.path("/databases/billing/versions"))
        .await
        .json();
    assert_eq!(numbers(&all), [5, 4, 3, 2, 1]);
    assert_eq!(all["versions"][0]["author"]["kind"], "token");

    assert_eq!(
        reviewer
            .get(&team.path("/databases/billing/versions/9"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reviewer
            .get(&team.path("/databases/billing/versions/latest"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn concurrent_uploads_and_pushes_get_every_number_once(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = std::sync::Arc::new(TestApp::compiling(db.clone()));
    let path = team.path("/databases/billing/versions");

    let mut tasks = Vec::new();
    for i in 0..24 {
        let (app, token, path, session) = (
            app.clone(),
            token.clone(),
            path.clone(),
            team.developer.clone(),
        );
        tasks.push(tokio::spawn(async move {
            // Every push is different, so none is "unchanged".
            if i % 2 == 0 {
                app.push(
                    id,
                    &token,
                    Some("application/json"),
                    model_json(&[&format!("pushed_{i}")]),
                )
                .await
                .status
            } else {
                app.as_user(&session)
                    .post(
                        &path,
                        &json!({ "sql": format!("CREATE TABLE up_{i} (id bigint);") }),
                    )
                    .await
                    .status
            }
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::CREATED);
    }
    let numbers: Vec<i32> = sqlx::query_scalar!(
        "SELECT number FROM versions WHERE database_id = $1 ORDER BY number",
        id
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(numbers, (1..=24).collect::<Vec<_>>());
}

fn new_version(database_id: Uuid, tables: &[&str]) -> NewVersion {
    NewVersion {
        database_id,
        model: model(tables).canonical(),
        source: tables.join(","),
        source_kind: SourceKind::Sql,
        origin: VersionOrigin::Draft,
        message: None,
        created_by: None,
        created_by_token: None,
        layout: None,
        expected_base: None,
        publishes_draft: None,
        approvals: None,
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_inherits_the_layout_pruned_to_its_tables(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let layouts = async |db: &PgPool| -> Vec<Value> {
        sqlx::query_scalar!(
            "SELECT layout FROM versions WHERE database_id = $1 ORDER BY number",
            id
        )
        .fetch_all(db)
        .await
        .unwrap()
    };

    // Without one: the empty layout, never null.
    store(&db, new_version(id, &["users"]), false)
        .await
        .unwrap();
    assert_eq!(layouts(&db).await, [json!({ "layout": {}, "groups": {} })]);

    // A draft's layout is stored as given, without tables the model lacks.
    let mut arranged = new_version(id, &["users", "orders"]);
    arranged.layout = Some(VersionLayout::from_value(&json!({
        "layout": {
            "public.users": { "x": 10, "y": 20 },
            "public.orders": { "x": 300, "y": 20 },
            "public.ghost": { "x": 0, "y": 0 }
        },
        "groups": { "g1": { "name": "Shop", "color": "amber",
                            "members": ["public.orders", "public.ghost"] } }
    })));
    store(&db, arranged, false).await.unwrap();
    let v2 = json!({
        "layout": { "public.orders": { "x": 300.0, "y": 20.0 }, "public.users": { "x": 10.0, "y": 20.0 } },
        "groups": { "g1": { "name": "Shop", "color": "amber", "members": ["public.orders"] } }
    });
    assert_eq!(layouts(&db).await[1], v2);

    // The next one inherits it, minus the table it dropped.
    store(&db, new_version(id, &["users"]), false)
        .await
        .unwrap();
    assert_eq!(
        layouts(&db).await[2],
        json!({
            "layout": { "public.users": { "x": 10.0, "y": 20.0 } },
            "groups": { "g1": { "name": "Shop", "color": "amber", "members": [] } }
        })
    );

    // The API serves it typed.
    let app = TestApp::new(db);
    let version = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing/versions/2"))
        .await
        .json();
    assert_eq!(version["layout"], v2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_stale_base_stores_nothing_and_names_the_latest(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;

    // No version yet: a draft based on nothing publishes v1.
    let mut first = new_version(id, &["users"]);
    first.expected_base = Some(None);
    let Ok(Stored::Created(v1)) = store(&db, first, false).await else {
        panic!("v1 is created");
    };
    // Based on nothing again: v1 came out meanwhile.
    let mut late = new_version(id, &["orders"]);
    late.expected_base = Some(None);
    let Err(StoreError::StaleBase {
        latest: Some(latest),
    }) = store(&db, late, false).await
    else {
        panic!("refused");
    };
    assert_eq!(latest.number, 1);

    let mut on_v1 = new_version(id, &["users", "orders"]);
    on_v1.expected_base = Some(Some(v1.id));
    let Ok(Stored::Created(v2)) = store(&db, on_v1, false).await else {
        panic!("v2 is created");
    };
    assert_eq!(v2.number, 2);
    let mut stale = new_version(id, &["users", "coupons"]);
    stale.expected_base = Some(Some(v1.id));
    let err = store(&db, stale, false).await.unwrap_err();
    let api: romulus_api::ApiError = err.into();
    assert_eq!(
        (api.status(), api.code()),
        (StatusCode::CONFLICT, "stale_base")
    );
    assert!(api.to_string().contains("v2"), "{api}");

    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

/// Uploads through the real sandbox compiler (needs `ROMULUS_SANDBOX_URL`
/// and the sandbox up: `just sandbox`; skipped, loudly, without it). One
/// test, one pool prefix of its own: one process per prefix.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_sandbox_compiles_uploads_and_sql_pushes(db: PgPool) {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    let Ok(url) = std::env::var("ROMULUS_SANDBOX_URL") else {
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "SKIPPED: ROMULUS_SANDBOX_URL is not set, the sandbox upload test did not run"
            );
        }
        return;
    };
    let mut config = Config::for_tests();
    config.sandbox_url = Some(url);
    config.sandbox_prefix = "romulus_api_test".into();
    let compiler = SqlCompiler::sandbox(&config);
    for _ in 0..120 {
        if compiler.status().await == romulus_api::compile::CompilerStatus::Ok {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::with_state(AppState::new(db, config).with_compiler(compiler.clone()));
    let dev = app.as_user(&team.developer);
    let path = team.path("/databases/billing/versions");

    // An error Postgres finds, located in the text.
    let broken = "CREATE TABLE users (id bigint PRIMARY KEY);\n\
                  CREATE TABLE orders (id bigint, user_id bigint REFERENCES customers (id));\n";
    let response = dev.post(&path, &json!({ "sql": broken })).await;
    assert_eq!(response.code(), "compile_failed", "{}", response.json());
    let diagnostic = &response.json()["details"]["diagnostics"][0];
    assert_eq!(diagnostic["span"]["from"]["line"], 2, "{diagnostic}");

    let fixed = broken.replace("customers", "users");
    let response = dev.post(&path, &json!({ "sql": fixed })).await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let version = dev.get(&format!("{path}/1")).await.json();
    assert_eq!(version["source"], fixed);
    assert_eq!(version["model"]["database"], "billing");
    assert_eq!(version["model"]["entities"].as_array().unwrap().len(), 2);

    // A SQL push of the same schema, formatted differently, changes nothing.
    let reformatted = fixed.replace("bigint", "BIGINT");
    let push = app
        .push(id, &token, Some("application/sql"), reformatted)
        .await;
    assert_eq!(push.status, StatusCode::OK, "{}", push.json());
    assert_eq!(push.json()["status"], "unchanged");
    let push = app
        .push(
            id,
            &token,
            Some("text/plain"),
            format!("{fixed}CREATE TABLE coupons (code text PRIMARY KEY);"),
        )
        .await;
    assert_eq!(push.status, StatusCode::CREATED, "{}", push.json());
    assert_eq!(push.json()["version"]["source_kind"], "sql");

    compiler.close().await;
}
