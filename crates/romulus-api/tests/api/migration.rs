//! Migrations: between two versions, and from a draft's base to the draft,
//! as statements and as each tool's files; who may read them.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, quick, serve, version},
};

/// A model whose tables have these columns (name, type, not null).
fn model(tables: &[(&str, &[(&str, &str, bool)])]) -> Value {
    let entities: Vec<Value> = tables
        .iter()
        .map(|(name, columns)| {
            let columns: Vec<Value> = columns
                .iter()
                .map(|(column, ty, not_null)| {
                    json!({
                        "name": column, "type": ty, "type_schema": "pg_catalog",
                        "type_name": ty, "type_kind": "b", "not_null": not_null,
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

async fn insert_version(db: &PgPool, database: Uuid, number: i32, model: &Value, message: &str) {
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout, message)
         VALUES ($1, $2, $3, $4, 'remus_json', 'push', '{}', $5)",
        database,
        number,
        model,
        model.to_string(),
        message,
    )
    .execute(db)
    .await
    .expect("insert version");
}

/// `shop` at v1 (users) and v2 (users.email required, invoices added).
async fn shop(db: &PgPool) -> (TestApp, Team) {
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    let v1 = model(&[("users", &[("id", "bigint", true), ("email", "text", false)])]);
    let v2 = model(&[
        ("users", &[("id", "bigint", true), ("email", "text", true)]),
        ("invoices", &[("id", "bigint", true)]),
    ]);
    insert_version(db, shop, 1, &v1, "Users").await;
    insert_version(db, shop, 2, &v2, "Add invoices, require emails").await;
    (TestApp::new(db.clone()), team)
}

fn statements(body: &Value) -> Vec<&str> {
    body["statements"]
        .as_array()
        .expect("statements")
        .iter()
        .map(|s| s["sql"].as_str().expect("sql"))
        .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_between_versions_has_its_statements_and_files(db: PgPool) {
    let (app, team) = shop(&db).await;
    let response = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/migration?from=1&to=2&format=sqlx"))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
    let body = response.json();
    assert_eq!(body["from"], 1);
    assert_eq!(body["to"], 2);
    assert_eq!(body["format"], "sqlx");
    assert_eq!(
        statements(&body),
        [
            "CREATE TABLE public.invoices (\n    id bigint NOT NULL\n);",
            "ALTER TABLE public.users ALTER COLUMN email SET NOT NULL;",
        ]
    );
    assert_eq!(body["destructive"], 1);
    assert_eq!(body["statements"][1]["destructive"], true);
    let files = body["files"].as_array().expect("files");
    assert_eq!(files.len(), 1);
    let path = files[0]["path"].as_str().expect("path");
    let (timestamp, name) = path.split_once('_').expect("a timestamp first");
    assert!(
        timestamp.len() == 14 && timestamp.bytes().all(|b| b.is_ascii_digit()),
        "{path}"
    );
    assert_eq!(
        name, "add_invoices_require_emails.sql",
        "named after v2's message"
    );
    let content = files[0]["content"].as_str().expect("content");
    assert!(content.starts_with("-- Migration of acme/shop from v1 to v2"));
    assert!(
        !content.contains("BEGIN;"),
        "sqlx runs it in its own transaction"
    );
    assert_eq!(files[0]["destructive"].as_array().map(Vec::len), Some(1));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rollback_migration_drops_what_the_newer_version_added(db: PgPool) {
    let (app, team) = shop(&db).await;
    let body = app
        .as_user(&team.developer)
        .get(&team.path("/databases/shop/migration?from=2&to=1"))
        .await
        .json();
    assert_eq!(body["format"], "plain");
    assert_eq!(
        statements(&body),
        [
            "ALTER TABLE public.users ALTER COLUMN email DROP NOT NULL;",
            "DROP TABLE public.invoices;",
        ]
    );
    assert_eq!(body["statements"][1]["section"], "removals");
    assert!(
        body["files"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("_v2_to_v1.sql")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_tool_gets_its_layout(db: PgPool) {
    let (app, team) = shop(&db).await;
    let user = app.as_user(&team.admin);
    for (format, files, first) in [
        ("plain", 1, ".sql"),
        ("sqlx", 1, ".sql"),
        ("flyway", 1, "V2__add_invoices_require_emails.sql"),
        ("prisma", 1, "_add_invoices_require_emails/migration.sql"),
        ("dbmate", 1, ".sql"),
        ("golang_migrate", 2, ".up.sql"),
    ] {
        let response = user
            .get(&team.path(&format!(
                "/databases/shop/migration?from=1&to=2&format={format}"
            )))
            .await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "{format}: {}",
            response.json()
        );
        let body = response.json();
        let list = body["files"].as_array().expect("files");
        assert_eq!(list.len(), files, "{format}");
        let path = list[0]["path"].as_str().expect("path");
        assert!(path.ends_with(first), "{format}: {path}");
    }
    let dbmate = user
        .get(&team.path("/databases/shop/migration?from=1&to=2&format=dbmate"))
        .await
        .json();
    let content = dbmate["files"][0]["content"].as_str().unwrap();
    assert!(content.contains("-- migrate:down"));
    assert!(
        content.contains("DROP TABLE public.invoices;"),
        "the down migration"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_file_name_and_options_can_be_chosen(db: PgPool) {
    let (app, team) = shop(&db).await;
    let body = app
        .as_user(&team.developer)
        .get(&team.path(
            "/databases/shop/migration?from=1&to=2&format=flyway&name=Billing%20%26%20emails&concurrently=true",
        ))
        .await
        .json();
    assert_eq!(body["files"][0]["path"], "V2__billing_emails.sql");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn members_read_migrations_and_nobody_else(db: PgPool) {
    let (app, team) = shop(&db).await;
    let path = team.path("/databases/shop/migration?from=1&to=2");
    for session in [&team.admin, &team.developer, &team.reviewer] {
        assert_eq!(app.as_user(session).get(&path).await.status, StatusCode::OK);
    }
    assert_eq!(
        app.as_user(&team.outsider).get(&path).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.get(&path).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_needs_two_versions_that_exist_and_a_known_tool(db: PgPool) {
    let (app, team) = shop(&db).await;
    let user = app.as_user(&team.developer);
    let zero = user
        .get(&team.path("/databases/shop/migration?from=0&to=2"))
        .await;
    assert_eq!(zero.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(zero.json()["details"]["from"].is_string());
    let missing = user
        .get(&team.path("/databases/shop/migration?from=1&to=9"))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let tool = user
        .get(&team.path("/databases/shop/migration?from=1&to=2&format=rails"))
        .await;
    assert_eq!(tool.status, StatusCode::BAD_REQUEST);
    let same = user
        .get(&team.path("/databases/shop/migration?from=2&to=2"))
        .await
        .json();
    assert_eq!(same["statements"], json!([]));
    assert!(
        same["files"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Nothing to migrate")
    );
}

// ---- Drafts --------------------------------------------------------------------

const V1: &str = "CREATE TABLE users (id bigint PRIMARY KEY);\n";

async fn draft(db: &PgPool) -> (TestApp, Team, String) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({})).await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id = created["id"].as_str().expect("id").to_owned();
    (app, team, id)
}

async fn type_in(app: &TestApp, id: &str, team: &Team, text: &'static str) {
    let addr = serve(app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{id}/ws"),
        &team.developer,
    )
    .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.insert(txn, 0, text);
        })
        .await;
    alice
        .until(|c| {
            c.events.iter().any(|e| {
                e["type"] == "compile"
                    && (e["state"] == "failed" || e["model"].to_string().contains("invoices"))
            })
        })
        .await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_migrates_from_its_base_to_what_it_holds(db: PgPool) {
    let (app, team, id) = draft(&db).await;
    type_in(&app, &id, &team, "CREATE TABLE invoices (id bigint);\n").await;
    let path = team.path(&format!(
        "/databases/shop/drafts/{id}/migration?format=golang_migrate"
    ));
    let response = app.as_user(&team.reviewer).get(&path).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
    let body = response.json();
    assert_eq!(body["from"], 1);
    assert_eq!(body["to"], 2);
    assert_eq!(statements(&body), ["CREATE TABLE public.invoices ();"]);
    let files = body["files"].as_array().unwrap();
    assert!(
        files[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("_draft_from_v1.up.sql")
    );
    assert_eq!(files[1]["role"], "down");
    assert!(
        files[0]["content"]
            .as_str()
            .unwrap()
            .contains("to draft \"Draft from v1\"")
    );
    assert_eq!(
        app.as_user(&team.outsider).get(&path).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_with_errors_has_no_migration(db: PgPool) {
    let (app, team, id) = draft(&db).await;
    type_in(&app, &id, &team, "ERROR here;\n").await;
    let response = app
        .as_user(&team.developer)
        .get(&team.path(&format!("/databases/shop/drafts/{id}/migration")))
        .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.code(), "compile_failed");
    assert!(
        response.json()["details"]["diagnostics"]
            .as_array()
            .is_some_and(|d| !d.is_empty())
    );
}
