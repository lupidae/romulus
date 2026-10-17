//! Drafts over REST: one-click creation, list, rename, delete, publish.

use axum::http::StatusCode;
use romulus_api::{
    domain::{DraftStatus, SourceKind, VersionOrigin},
    versions::store::{NewVersion, StoreError, store},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, model_of, quick, serve, stored_sql, version},
};

const V1: &str = "CREATE TABLE users (id bigint PRIMARY KEY);\nCREATE TABLE orders (id bigint);\n";

fn v1_layout() -> Value {
    json!({
        "layout": {
            "public.users": {"x": 0.0, "y": 0.0},
            "public.orders": {"x": 400.0, "y": 0.0},
        },
        "groups": {
            "g1": {"name": "Core", "color": "blue", "members": ["public.users", "public.orders"]},
        },
    })
}

/// A team whose database `shop` has version 1.
async fn shop(db: &PgPool) -> (TestApp, Team, Uuid) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, v1_layout()).await;
    (app, team, shop)
}

async fn new_draft(app: &TestApp, team: &Team) -> Value {
    let response = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    response.json()
}

fn draft_path(team: &Team, draft: &Value, rest: &str) -> String {
    team.path(&format!(
        "/databases/shop/drafts/{}{rest}",
        draft["id"].as_str().expect("id")
    ))
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_starts_from_the_latest_version_in_one_click(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    assert_eq!(draft["name"], "Draft from v1");
    assert_eq!(draft["status"], "open");
    assert_eq!(draft["base_version"], 1);
    assert_eq!(draft["latest_version"], 1);
    assert_eq!(draft["stale"], false);
    assert_eq!(draft["created_by"]["name"], "Developer-acme");

    // Its content is the version's SQL, layout and groups.
    let id: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(stored_sql(&db, id).await, V1);
    let content = app.state.collab.content(id).await.unwrap();
    assert_eq!(content.layout.layout.len(), 2);
    assert_eq!(content.layout.groups["g1"].name, "Core");
    // And the version's model is its first compile: the graph shows at once.
    assert!(content.model.is_some());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_without_versions_gets_an_empty_draft(db: PgPool) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "shop").await;
    let draft = new_draft(&app, &team).await;
    assert_eq!(draft["name"], "New draft");
    assert_eq!(draft["base_version"], Value::Null);
    assert_eq!(draft["stale"], false);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_developers_and_admins_create_drafts(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let path = team.path("/databases/shop/drafts");
    let reviewer = app.as_user(&team.reviewer).post_empty(&path).await;
    assert_eq!(
        (reviewer.status, reviewer.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let outsider = app.as_user(&team.outsider).post_empty(&path).await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    let admin = app.as_user(&team.admin).post_empty(&path).await;
    assert_eq!(admin.status, StatusCode::CREATED);
    let missing = app
        .as_user(&team.admin)
        .post_empty(&team.path("/databases/nope/drafts"))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_member_lists_open_drafts_with_who_is_in_them(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let _alice = Client::join(addr, &ws, &team.developer).await;
    let _bob = Client::join(addr, &ws, &team.reviewer).await;

    let list = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/drafts"))
        .await;
    assert_eq!(list.status, StatusCode::OK);
    let drafts = list.json();
    assert_eq!(drafts.as_array().unwrap().len(), 1);
    assert_eq!(
        drafts[0]["present"],
        json!([
            {"user_id": team.developer.user, "name": "Developer-acme", "editing": true},
            {"user_id": team.reviewer.user, "name": "Reviewer-acme", "editing": false},
        ])
    );
    let outsider = app
        .as_user(&team.outsider)
        .get(&team.path("/databases/shop/drafts"))
        .await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn renaming_validates_and_tells_everyone_in_the_draft(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let mut watcher = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap()),
        &team.reviewer,
    )
    .await;
    let path = draft_path(&team, &draft, "");

    let empty = app
        .as_user(&team.developer)
        .patch(&path, &json!({"name": "  "}))
        .await;
    assert_eq!(
        (empty.status, empty.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "validation")
    );
    let reviewer = app
        .as_user(&team.reviewer)
        .patch(&path, &json!({"name": "mine"}))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);

    let renamed = app
        .as_user(&team.developer)
        .patch(&path, &json!({"name": " Add billing "}))
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json()["name"], "Add billing");
    watcher.until(|c| c.last_event("renamed").is_some()).await;
    assert_eq!(
        watcher.last_event("renamed").unwrap()["name"],
        "Add billing"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_hides_the_draft_and_lets_its_editors_go(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let mut editor = Client::join(addr, &ws, &team.developer).await;
    let path = draft_path(&team, &draft, "");

    let reviewer = app.as_user(&team.reviewer).delete(&path).await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
    let deleted = app.as_user(&team.admin).delete(&path).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);

    editor.until(|c| c.closed.is_some()).await;
    assert_eq!(editor.closed, Some(4404));
    assert_eq!(editor.last_event("status").unwrap()["status"], "deleted");
    assert_eq!(
        app.as_user(&team.admin).get(&path).await.status,
        StatusCode::NOT_FOUND
    );
    let list = app
        .as_user(&team.admin)
        .get(&team.path("/databases/shop/drafts"))
        .await;
    assert_eq!(list.json(), json!([]));
    assert_eq!(
        app.as_user(&team.admin).delete(&path).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_creates_the_next_version_and_closes_the_draft(db: PgPool) {
    let (app, team, shop) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let mut alice = Client::join(addr, &ws, &team.developer).await;
    let mut bob = Client::join(addr, &ws, &team.reviewer).await;

    // Alice adds a table and drops orders (not a rename: invoices comes from
    // another statement). Orders' position stays in the doc (undo would bring
    // it back), not in the version.
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            let sql = roots.sql(txn);
            roots
                .sql
                .remove_range(txn, 0, u32::try_from(sql.encode_utf16().count()).unwrap());
            roots.sql.insert(
                txn,
                0,
                "CREATE TABLE invoices (id bigint);\nCREATE TABLE users (id bigint PRIMARY KEY);\n",
            );
        })
        .await;
    alice
        .until(|c| {
            c.events.iter().any(|e| {
                e["type"] == "compile"
                    && e["model"]["entities"]
                        .as_array()
                        .is_some_and(|e| e.iter().any(|t| t["name"] == "invoices"))
            })
        })
        .await;

    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json());
    let preview = preview.json();
    assert_eq!(preview["number"], 2);
    assert_eq!(preview["stale"], false);
    assert_eq!(
        preview["diff"]["summary"]["text"],
        "1 table added, 1 removed"
    );
    assert_eq!(
        preview["base_model"]["entities"].as_array().unwrap().len(),
        2
    );
    let reviewer = app
        .as_user(&team.reviewer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);

    let published = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": preview["sha256"], "message": "Invoices"}),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
    assert_eq!(published.json()["number"], 2);

    let stored = sqlx::query!(
        r#"SELECT source, origin::text AS "origin!", message, layout, model, created_by
           FROM versions WHERE database_id = $1 AND number = 2"#,
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(stored.origin, "draft");
    assert_eq!(stored.message.as_deref(), Some("Invoices"));
    assert!(stored.source.contains("invoices"));
    assert_eq!(stored.created_by, Some(team.developer.user));
    assert_eq!(stored.model["database"], "shop");
    assert_eq!(
        stored.layout,
        json!({
            "layout": {"public.users": {"x": 0.0, "y": 0.0}},
            "groups": {"g1": {"name": "Core", "color": "blue", "members": ["public.users"]}},
        })
    );

    // Everyone in the draft hears it, then the draft is published.
    for client in [&mut alice, &mut bob] {
        client
            .until(|c| {
                c.last_event("status")
                    .is_some_and(|e| e["status"] == "published")
            })
            .await;
        assert_eq!(client.last_event("status").unwrap()["version"], 2);
    }
    let after = app
        .as_user(&team.reviewer)
        .get(&draft_path(&team, &draft, ""))
        .await;
    assert_eq!(after.json()["status"], "published");
    assert_eq!(after.json()["published_version"], 2);
    // Nothing changes it any more; nobody can open it live again.
    let again = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(again.code(), "draft_published");
    assert!(
        Client::connect(addr, &ws, Some(&team.developer), super::harness::ORIGIN)
            .await
            .is_err_and(|refused| refused.0 == 409)
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_refuses_a_stale_base(db: PgPool) {
    let (app, team, shop) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await
        .json();
    // Someone published v2 meanwhile.
    version(&db, shop, 2, V1, json!({})).await;

    let listed = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, ""))
        .await
        .json();
    assert_eq!(listed["stale"], true);
    assert_eq!(listed["latest_version"], 2);
    let refused = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "stale_base")
    );
    assert!(refused.json()["message"].as_str().unwrap().contains("v2"));
    // Still open, still editable.
    let id: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    assert!(app.state.collab.freeze(id, "x").await.unwrap().is_ok());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_what_was_not_reviewed_is_refused_and_unfreezes(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let mut alice = Client::join(addr, &ws, &team.developer).await;

    let refused = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": "0".repeat(64)}),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "draft_changed")
    );
    // Everyone saw it freeze, then open again; editing works.
    alice
        .until(|c| {
            let statuses: Vec<_> = c
                .events
                .iter()
                .filter(|e| e["type"] == "status")
                .map(|e| e["status"].clone())
                .collect();
            statuses == [json!("publishing"), json!("open")]
        })
        .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, "-- still editable\n");
        })
        .await;
    let id: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let mut content = app.state.collab.content(id).await.unwrap();
    for _ in 0..50 {
        if content.sql.contains("still editable") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        content = app.state.collab.content(id).await.unwrap();
    }
    assert!(content.sql.contains("still editable"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_sql_with_errors_is_refused(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let mut alice = Client::join(addr, &ws, &team.developer).await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, "ERROR here;\n");
        })
        .await;
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;

    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await
        .json();
    assert_eq!(preview["diff"], Value::Null);
    assert_eq!(preview["diagnostics"][0]["code"], "sql");
    let refused = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.json()["details"]["diagnostics"][0]["code"], "sql");
}

fn draft_id(draft: &Value) -> Uuid {
    draft["id"].as_str().expect("id").parse().expect("uuid")
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_is_stored_only_if_its_draft_is_still_open(db: PgPool) {
    let (app, team, shop) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    // Deleted between the freeze and the store.
    sqlx::query!(
        "UPDATE drafts SET status = 'deleted' WHERE id = $1",
        draft_id(&draft)
    )
    .execute(&db)
    .await
    .unwrap();

    let refused = store(
        &db,
        NewVersion {
            database_id: shop,
            model: model_of("CREATE TABLE invoices (id bigint);"),
            source: "CREATE TABLE invoices (id bigint);".into(),
            source_kind: SourceKind::Sql,
            origin: VersionOrigin::Draft,
            message: None,
            created_by: Some(team.developer.user),
            created_by_token: None,
            layout: None,
            expected_base: None,
            publishes_draft: Some(draft_id(&draft)),
            approvals: None,
            migration_sql: None,
            compiled_on: None,
            discussion: Vec::new(),
        },
        false,
    )
    .await;
    assert!(
        matches!(refused, Err(StoreError::DraftNotOpen)),
        "{refused:?}"
    );
    let versions = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(versions, 1, "nothing stored");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_being_published_cannot_be_deleted(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let path = draft_path(&team, &draft, "");
    app.state
        .collab
        .freeze(draft_id(&draft), "Carol")
        .await
        .unwrap()
        .unwrap();

    let busy = app.as_user(&team.admin).delete(&path).await;
    assert_eq!(
        (busy.status, busy.code().as_str()),
        (StatusCode::CONFLICT, "draft_busy")
    );
    assert_eq!(
        app.as_user(&team.admin).get(&path).await.status,
        StatusCode::OK
    );
    // Publishing failed: now it can go.
    app.state.collab.unfreeze(draft_id(&draft));
    assert_eq!(
        app.as_user(&team.admin).delete(&path).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_a_database_deletes_its_open_drafts(db: PgPool) {
    let (app, team, _) = shop(&db).await;
    let draft = new_draft(&app, &team).await;
    let deleted = app
        .as_user(&team.admin)
        .delete(&team.path("/databases/shop"))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: DraftStatus" FROM drafts WHERE id = $1"#,
        draft_id(&draft)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(status, DraftStatus::Deleted);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_from_a_pushed_model_compiles_its_own_sql(db: PgPool) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let model = serde_json::to_value(model_of(V1)).unwrap();
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, 1, $2, $3, 'remus_json', 'push', '{}')",
        shop,
        model,
        model.to_string(),
    )
    .execute(&db)
    .await
    .unwrap();

    let draft = new_draft(&app, &team).await;
    // The pushed model is not what the rendered SQL compiles to, byte for
    // byte: nothing is cached, the draft compiles on opening.
    let cached = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM draft_compiles WHERE draft_id = $1"#,
        draft_id(&draft)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(cached, 0);
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft_id(&draft));
    let mut alice = Client::join(addr, &ws, &team.developer).await;
    assert!(alice.sql().contains("CREATE TABLE"), "{}", alice.sql());
    alice
        .until(|c| c.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;
}

// ---- Update draft ---------------------------------------------------------------

/// A model whose tables have these columns (name, type).
fn model_json(tables: &[(&str, &[(&str, &str)])]) -> Value {
    let entities: Vec<Value> = tables
        .iter()
        .map(|(name, columns)| {
            let columns: Vec<Value> = columns
                .iter()
                .map(|(column, ty)| {
                    json!({
                        "name": column, "type": ty, "type_schema": "pg_catalog",
                        "type_name": ty, "type_kind": "b", "not_null": false,
                        "default": null, "identity": null, "generated": null, "comment": null,
                    })
                })
                .collect();
            json!({
                "schema": "public", "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null, "columns": columns,
            })
        })
        .collect();
    json!({
        "remus_version": "0.2", "database": "shop", "server_version": "17",
        "entities": entities,
    })
}

/// Version `number` with its SQL and the model it compiles to; `pushed`: as
/// remus JSON (the model is its source).
async fn version_of(
    db: &PgPool,
    database: Uuid,
    number: i32,
    sql: &str,
    model: &Value,
    pushed: bool,
) {
    let (source, kind, origin) = if pushed {
        (model.to_string(), "remus_json", "push")
    } else {
        (sql.to_owned(), "sql", "upload")
    };
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, $2, $3, $4, $5::text::version_source_kind, $6::text::version_origin, '{}')",
        database,
        number,
        model,
        source,
        kind,
        origin,
    )
    .execute(db)
    .await
    .expect("insert version");
}

const U1: &str =
    "CREATE TABLE users (id bigint, email text);\n\nCREATE TABLE orders (id bigint);\n";
const USERS: (&str, &[(&str, &str)]) = ("users", &[("id", "bigint"), ("email", "text")]);
const ORDERS: (&str, &[(&str, &str)]) = ("orders", &[("id", "bigint")]);

/// A team whose database `shop` has v1 = [`U1`], a draft from it, and Alice
/// (a developer) in it.
async fn stale_setup(db: &PgPool) -> (TestApp, Team, Uuid, Value, Client) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version_of(db, shop, 1, U1, &model_json(&[USERS, ORDERS]), false).await;
    let draft = new_draft(&app, &team).await;
    let addr = serve(&app).await;
    let ws = format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap());
    let alice = Client::join(addr, &ws, &team.developer).await;
    (app, team, shop, draft, alice)
}

/// Inserts `text` right after the first occurrence of `after`.
fn insert_after(
    after: &'static str,
    text: &'static str,
) -> impl FnOnce(&romulus_api::collab::doc::Roots, &mut yrs::TransactionMut) {
    move |roots, txn| {
        use yrs::Text;
        let sql = roots.sql(txn);
        let at = sql.find(after).expect("anchor") + after.len();
        roots.sql.insert(txn, u32::try_from(at).unwrap(), text);
    }
}

async fn room_sql_with(app: &TestApp, id: Uuid, needle: &str) -> String {
    for _ in 0..100 {
        let sql = app.state.collab.content(id).await.unwrap().sql;
        if sql.contains(needle) {
            return sql;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the room never got {needle:?}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn updating_merges_the_latest_version_and_moves_the_base(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, "CREATE TABLE coupons (code text);\n");
        })
        .await;
    room_sql_with(&app, id, "coupons").await;

    // Someone published v2: users got a column.
    let v2 = "CREATE TABLE users (id bigint, email text, plan text);\n\nCREATE TABLE orders (id bigint);\n";
    version_of(
        &db,
        shop,
        2,
        v2,
        &model_json(&[
            (
                "users",
                &[("id", "bigint"), ("email", "text"), ("plan", "text")],
            ),
            ORDERS,
        ]),
        false,
    )
    .await;
    let stale = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, ""))
        .await
        .json();
    assert_eq!(
        (stale["stale"].clone(), stale["latest_version"].clone()),
        (json!(true), json!(2))
    );

    let updated = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    assert_eq!(
        updated.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": true})
    );

    // Everyone has the merged text: theirs' users, ours' orders and coupons.
    let merged = "CREATE TABLE users (id bigint, email text, plan text);\n\n\
                  CREATE TABLE orders (id bigint, total numeric);\n\
                  CREATE TABLE coupons (code text);\n";
    alice.until(|c| c.sql() == merged).await;
    alice
        .until(|c| {
            c.last_event("updated").is_some()
                && c.last_event("status")
                    .is_some_and(|e| e["status"] == "open")
        })
        .await;
    let statuses: Vec<_> = alice
        .events
        .iter()
        .filter(|e| e["type"] == "status")
        .map(|e| e["status"].clone())
        .collect();
    assert_eq!(statuses, [json!("updating"), json!("open")]);
    assert_eq!(
        alice.last_event("updated").unwrap(),
        &json!({"type": "updated", "version": 2, "by": "Developer-acme", "conflicts": 0})
    );
    let now = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, ""))
        .await
        .json();
    assert_eq!(
        (now["stale"].clone(), now["base_version"].clone()),
        (json!(false), json!(2))
    );
    let base_sql = sqlx::query_scalar!("SELECT base_sql FROM drafts WHERE id = $1", id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(base_sql.as_deref(), Some(v2));

    // Publishing now creates v3.
    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await
        .json();
    assert_eq!(preview["number"], 3);
    assert_eq!(preview["stale"], false);

    // Nothing newer: updating again changes nothing.
    let again = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(
        again.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": false})
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn conflicts_block_publishing_until_resolved(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    room_sql_with(&app, id, "total").await;
    let v2 = "CREATE TABLE users (id bigint, email text);\n\nCREATE TABLE orders (id bigint, placed date);\n";
    version_of(
        &db,
        shop,
        2,
        v2,
        &model_json(&[USERS, ("orders", &[("id", "bigint"), ("placed", "date")])]),
        false,
    )
    .await;

    let updated = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(updated.json()["conflicts"], 1, "{}", updated.json());
    let block = "-- <<<<<<< yours\nCREATE TABLE orders (id bigint, total numeric);\n\
                 -- ======= v2\nCREATE TABLE orders (id bigint, placed date);\n-- >>>>>>> v2";
    alice.until(|c| c.sql().contains(block)).await;

    // The compile refuses it, and so does publishing.
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["diagnostics"][0]["code"] == "conflict")
        })
        .await;
    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await
        .json();
    assert_eq!(preview["diagnostics"][0]["code"], "conflict");
    let refused = app
        .as_user(&team.developer)
        .post(
            &draft_path(&team, &draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "compile_failed")
    );

    // Keep theirs: the block becomes theirs' statement.
    alice
        .edit(move |roots, txn| {
            use yrs::Text;
            let sql = roots.sql(txn);
            let at = sql.find(block).unwrap();
            let at = u32::try_from(at).unwrap();
            roots
                .sql
                .remove_range(txn, at, u32::try_from(block.len()).unwrap());
            roots
                .sql
                .insert(txn, at, "CREATE TABLE orders (id bigint, placed date);");
        })
        .await;
    alice
        .until(|c| c.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;
    let preview = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, "/publish"))
        .await
        .json();
    assert_eq!(preview["diagnostics"], json!([]));
    assert_eq!(preview["number"], 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_pushed_version_merges_without_spurious_conflicts(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    room_sql_with(&app, id, "total").await;
    // v2 pushed as remus JSON: its SQL is render_sql's, nothing like ours.
    version_of(
        &db,
        shop,
        2,
        "",
        &model_json(&[
            (
                "users",
                &[("id", "bigint"), ("email", "text"), ("plan", "text")],
            ),
            ORDERS,
        ]),
        true,
    )
    .await;

    let updated = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(updated.json()["conflicts"], 0, "{}", updated.json());
    // users comes in as render_sql writes it; orders is ours, as typed.
    alice.until(|c| c.sql().contains("plan text")).await;
    let sql = alice.sql();
    assert!(sql.starts_with("CREATE TABLE public.users ("), "{sql}");
    assert!(
        sql.contains("\n\nCREATE TABLE orders (id bigint, total numeric);\n"),
        "{sql}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_newer_version_while_conflicts_remain_updates_them(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    room_sql_with(&app, id, "total").await;
    for (n, ty) in [(2, "date"), (3, "timestamptz")] {
        let columns = [("id", "bigint"), ("placed", ty)];
        let sql = format!(
            "CREATE TABLE users (id bigint, email text);\n\nCREATE TABLE orders (id bigint, placed {ty});\n"
        );
        version_of(
            &db,
            shop,
            n,
            &sql,
            &model_json(&[USERS, ("orders", columns.as_slice())]),
            false,
        )
        .await;
        if n == 2 {
            let first = app
                .as_user(&team.developer)
                .post_empty(&draft_path(&team, &draft, "/update"))
                .await;
            assert_eq!(first.json()["conflicts"], 1);
            alice.until(|c| c.sql().contains("-- >>>>>>> v2")).await;
        }
    }
    let second = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(
        second.json(),
        json!({"base_version": 3, "conflicts": 1, "changed": true})
    );
    alice.until(|c| c.sql().contains("-- >>>>>>> v3")).await;
    let sql = alice.sql();
    assert!(
        sql.contains("placed timestamptz") && !sql.contains("placed date"),
        "{sql}"
    );
    assert!(sql.contains("total numeric"), "{sql}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn who_may_update_and_when(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    let path = draft_path(&team, &draft, "/update");
    version_of(
        &db,
        shop,
        2,
        "CREATE TABLE users (id bigint, email text, plan text);\n\nCREATE TABLE orders (id bigint);\n",
        &model_json(&[("users", &[("id", "bigint"), ("email", "text"), ("plan", "text")]), ORDERS]),
        false,
    )
    .await;
    assert_eq!(
        app.as_user(&team.reviewer).post_empty(&path).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.as_user(&team.outsider).post_empty(&path).await.status,
        StatusCode::NOT_FOUND
    );

    // Being published: busy.
    app.state.collab.freeze(id, "Carol").await.unwrap().unwrap();
    let busy = app.as_user(&team.developer).post_empty(&path).await;
    assert_eq!(
        (busy.status, busy.code().as_str()),
        (StatusCode::CONFLICT, "draft_busy")
    );
    app.state.collab.unfreeze(id);

    // Text that does not parse is not merged; the draft stays editable.
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, "CREATE TABLE (\n");
        })
        .await;
    room_sql_with(&app, id, "CREATE TABLE (").await;
    let refused = app.as_user(&team.developer).post_empty(&path).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "draft_has_errors")
    );
    assert_eq!(
        refused.json()["details"]["diagnostics"][0]["code"],
        "syntax_error"
    );
    assert!(app.state.collab.freeze(id, "x").await.unwrap().is_ok());
    app.state.collab.unfreeze(id);
    let still = app
        .as_user(&team.developer)
        .get(&draft_path(&team, &draft, ""))
        .await
        .json();
    assert_eq!(still["base_version"], 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn uploads_are_never_refused_for_what_looks_like_a_conflict(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db);
    // A banner, and a whole block's markers, in an uploaded file's comments.
    let sql = "-- ==========================================\n-- Users\n\
               -- ==========================================\n\
               -- <<<<<<< yours\nCREATE TABLE users (id bigint);\n-- ======= v2\n-- >>>>>>> v2\n";
    let response = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": sql }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_update_that_stopped_after_its_rewrite_runs_again_cleanly(db: PgPool) {
    let (app, team, shop, draft, mut alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    room_sql_with(&app, id, "total").await;
    let v2 = "CREATE TABLE users (id bigint, email text, plan text);\n\nCREATE TABLE orders (id bigint);\n";
    version_of(
        &db,
        shop,
        2,
        v2,
        &model_json(&[
            (
                "users",
                &[("id", "bigint"), ("email", "text"), ("plan", "text")],
            ),
            ORDERS,
        ]),
        false,
    )
    .await;
    let path = draft_path(&team, &draft, "/update");
    assert_eq!(
        app.as_user(&team.developer).post_empty(&path).await.json()["conflicts"],
        0
    );
    let merged = room_sql_with(&app, id, "plan text").await;

    // As if the base had not moved after the rewrite (the database failing).
    sqlx::query!(
        "UPDATE drafts SET base_version_id = (SELECT id FROM versions WHERE database_id = $2 AND number = 1),
                           base_sql = NULL
         WHERE id = $1",
        id,
        shop,
    )
    .execute(&db)
    .await
    .unwrap();
    let again = app.as_user(&team.developer).post_empty(&path).await;
    assert_eq!(
        again.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": false}),
        "nothing merged twice"
    );
    assert_eq!(app.state.collab.content(id).await.unwrap().sql, merged);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_the_update_rewrites_a_draft_it_froze(db: PgPool) {
    use romulus_api::collab::{compile::sha256_hex, patch::changes, room::RewriteError};

    let (app, _team, _shop, draft, _alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    let hub = &app.state.collab;
    let (content, ticket) = hub.freeze_to_update(id, "Carol").await.unwrap().unwrap();
    let sha = sha256_hex(&content.sql);
    let edit = changes(&content.sql, &format!("{}-- tidied\n", content.sql));

    // Anyone else (Tidy) is refused meanwhile.
    let refused = hub
        .rewrite(id, sha.clone(), edit.clone(), "tidy")
        .await
        .unwrap();
    assert!(
        matches!(refused, Err(RewriteError::NotOpen(_))),
        "{refused:?}"
    );
    // The update itself is not.
    assert!(
        hub.rewrite_update(id, ticket, 2, sha.clone(), edit.clone())
            .await
            .unwrap()
            .unwrap()
    );
    // Once it is over, its ticket is worth nothing.
    hub.unfreeze(id);
    let text = hub.content(id).await.unwrap().sql;
    let late = hub
        .rewrite_update(id, ticket, 2, sha256_hex(&text), changes(&text, ""))
        .await
        .unwrap();
    assert!(matches!(late, Err(RewriteError::NotOpen(_))), "{late:?}");
}

/// v2 (users gets plan) and the update to it, for the retry tests.
async fn updated_to_v2(db: &PgPool) -> (TestApp, Team, Uuid, Value, Client) {
    let (app, team, shop, draft, mut alice) = stale_setup(db).await;
    alice
        .edit(insert_after("orders (id bigint", ", total numeric"))
        .await;
    room_sql_with(&app, draft_id(&draft), "total").await;
    version_of(
        db,
        shop,
        2,
        "CREATE TABLE users (id bigint, email text, plan text);\n\nCREATE TABLE orders (id bigint);\n",
        &model_json(&[
            ("users", &[("id", "bigint"), ("email", "text"), ("plan", "text")]),
            ORDERS,
        ]),
        false,
    )
    .await;
    (app, team, shop, draft, alice)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_update_that_failed_after_its_rewrite_is_finished_whatever_was_typed_since(db: PgPool) {
    let (app, team, shop, draft, mut alice) = updated_to_v2(&db).await;
    let id = draft_id(&draft);
    let path = draft_path(&team, &draft, "/update");
    let first = app.as_user(&team.developer).post_empty(&path).await;
    assert_eq!(first.json()["conflicts"], 0, "{}", first.json());
    room_sql_with(&app, id, "plan text").await;

    // As if the base had not moved after the rewrite: the new one pending.
    sqlx::query!(
        "UPDATE drafts SET pending_base_version_id = base_version_id, pending_base_sql = base_sql,
                           base_version_id = (SELECT id FROM versions WHERE database_id = $2 AND number = 1),
                           base_sql = NULL
         WHERE id = $1",
        id,
        shop,
    )
    .execute(&db)
    .await
    .unwrap();
    // Then someone edits the statement v2 changed.
    alice.until(|c| c.sql().contains("plan text")).await;
    alice.edit(insert_after("plan text", ", nick text")).await;
    let edited = room_sql_with(&app, id, "nick text").await;

    let again = app.as_user(&team.developer).post_empty(&path).await;
    assert_eq!(
        again.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": false}),
        "finished, not merged again"
    );
    assert_eq!(app.state.collab.content(id).await.unwrap().sql, edited);
    let row = sqlx::query!(
        r#"SELECT pending_base_version_id IS NULL AS "cleared!" FROM drafts WHERE id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(row.cleared);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_pending_base_whose_rewrite_never_landed_is_merged_for_real(db: PgPool) {
    let (app, team, shop, draft, _alice) = updated_to_v2(&db).await;
    let id = draft_id(&draft);
    // Pending, but the doc never noted the merge: the text has none of v2.
    sqlx::query!(
        "UPDATE drafts SET pending_base_version_id =
                               (SELECT id FROM versions WHERE database_id = $2 AND number = 2),
                           pending_base_sql = 'CREATE TABLE stale (id int);'
         WHERE id = $1",
        id,
        shop,
    )
    .execute(&db)
    .await
    .unwrap();
    let updated = app
        .as_user(&team.developer)
        .post_empty(&draft_path(&team, &draft, "/update"))
        .await;
    assert_eq!(
        updated.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": true})
    );
    let sql = room_sql_with(&app, id, "plan text").await;
    assert!(
        sql.contains("total numeric") && !sql.contains("stale"),
        "{sql}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_the_update_merges_the_layout_of_a_draft_it_froze(db: PgPool) {
    use romulus_api::{collab::room::RewriteError, versions::layout::TablePosition};

    let (app, _team, _shop, draft, _alice) = stale_setup(&db).await;
    let id = draft_id(&draft);
    let hub = &app.state.collab;
    let at = |x| vec![("public.users".to_owned(), Some(TablePosition { x, y: 0.0 }))];
    let (_, ticket) = hub.freeze_to_update(id, "Carol").await.unwrap().unwrap();
    hub.merge_layout(id, ticket, at(1.0), Vec::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        hub.content(id).await.unwrap().layout.layout["public.users"].x,
        1.0
    );
    // Once the update is over, its ticket is worth nothing.
    hub.unfreeze(id);
    let late = hub
        .merge_layout(id, ticket, at(2.0), Vec::new())
        .await
        .unwrap();
    assert!(matches!(late, Err(RewriteError::NotOpen(_))), "{late:?}");
    assert_eq!(
        hub.content(id).await.unwrap().layout.layout["public.users"].x,
        1.0
    );
}
