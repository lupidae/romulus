//! Tidy: a draft's SQL rewritten in canonical form, for the people in it
//! live, as a minimal edit; refused when the text does not compile or the
//! rewrite would be another schema.

use axum::http::StatusCode;
use romulus_api::{
    AppState, Config,
    collab::{
        Hub,
        compile::{Compiled, SqlCompiler},
    },
    domain::Schema,
    drafts::tidy::canonical_sql,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{Assoc, IndexedSequence, Text, Transact};

use super::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, model_of, quick, serve, stored_sql, version},
};

const V1: &str = "CREATE TABLE users (id bigint);\n\n-- edits from the graph\nCREATE TABLE orders (id bigint);\n";

struct Draft {
    app: TestApp,
    team: Team,
    id: Uuid,
    ws: String,
    tidy: String,
}

async fn draft_in(db: &PgPool, app: TestApp, sql: &str) -> Draft {
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, sql, json!({})).await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    Draft {
        ws: format!("/api/orgs/acme/drafts/{id}/ws"),
        tidy: team.path(&format!("/databases/shop/drafts/{id}/tidy")),
        app,
        team,
        id,
    }
}

async fn draft(db: &PgPool) -> Draft {
    draft_in(db, live_app(db.clone(), quick()), V1).await
}

fn compiled(client: &Client) -> bool {
    client
        .last_event("compile")
        .is_some_and(|e| e["state"] == "ok" || e["state"] == "failed")
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tidy_rewrites_the_sql_canonically_for_everyone(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    alice.until(compiled).await;

    let response = d.app.as_user(&d.team.developer).post_empty(&d.tidy).await;
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
    let body = response.json();
    assert_eq!(body["changed"], true);

    // Line comments (the marker) go; the schema stays.
    let expected = canonical_sql(&model_of(V1));
    assert!(!expected.contains("--"), "{expected}");
    assert_eq!(model_of(&expected), model_of(V1));
    bob.until(|c| c.sql() == expected).await;
    alice.until(|c| c.sql() == expected).await;
    // A compile of the tidy text follows.
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["sha256"] == body["sha256"])
        })
        .await;

    // Tidy again: nothing to do.
    let again = d
        .app
        .as_user(&d.team.developer)
        .post_empty(&d.tidy)
        .await
        .json();
    assert_eq!(again["changed"], false);
    assert_eq!(again["sha256"], body["sha256"]);

    d.app.state.collab.shutdown().await;
    assert_eq!(stored_sql(&db, d.id).await, expected);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tidy_is_a_minimal_edit_that_keeps_carets(db: PgPool) {
    let sql = "CREATE TABLE users (id bigint);\n";
    let d = draft_in(&db, live_app(db.clone(), quick()), sql).await;
    let addr = serve(&d.app).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    // Bob's caret sits on `users`.
    let at = u32::try_from(sql.find("users").unwrap()).unwrap();
    let caret = bob
        .roots
        .sql
        .sticky_index(&bob.doc.transact_mut(), at, Assoc::After)
        .unwrap();

    let response = d.app.as_user(&d.team.developer).post_empty(&d.tidy).await;
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
    let expected = canonical_sql(&model_of(sql));
    bob.until(|c| c.sql() == expected).await;
    let offset = {
        let txn = bob.doc.transact();
        caret.get_offset(&txn).unwrap().index as usize
    };
    assert!(
        bob.sql()[offset..].starts_with("users"),
        "{} at {offset}",
        bob.sql()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tidy_refuses_sql_that_does_not_compile(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    alice.until(compiled).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "ERROR\n"))
        .await;
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;

    let response = d.app.as_user(&d.team.developer).post_empty(&d.tidy).await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.code(), "compile_failed");
    let diagnostics = &response.json()["details"]["diagnostics"];
    assert_eq!(diagnostics[0]["severity"], "error", "{diagnostics}");
    assert_eq!(alice.sql(), format!("{V1}ERROR\n"));
}

/// A compiler that also models `CREATE FOREIGN TABLE remote`, which remus
/// does not write back.
fn foreign_compiler() -> SqlCompiler {
    SqlCompiler::from_fn(|sql| {
        let mut model = serde_json::to_value(model_of(sql)).unwrap();
        if sql.contains("CREATE FOREIGN TABLE remote") {
            model["entities"].as_array_mut().unwrap().push(json!({
                "schema": "public", "name": "remote", "kind": "f", "comment": null,
                "partition_key": null, "view_definition": null,
            }));
        }
        Compiled {
            model: Some(<Schema as serde::Deserialize>::deserialize(model).unwrap()),
            diagnostics: Vec::new(),
        }
    })
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tidy_never_changes_the_schema(db: PgPool) {
    let hub = Hub::with_settings(db.clone(), foreign_compiler(), quick());
    let app = TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_collab(hub));
    let sql = "CREATE TABLE users (id bigint);\nCREATE FOREIGN TABLE remote (id int) SERVER s;\n";
    let d = draft_in(&db, app, sql).await;

    let response = d.app.as_user(&d.team.developer).post_empty(&d.tidy).await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.code(), "tidy_unsafe");
    assert_eq!(d.app.state.collab.content(d.id).await.unwrap().sql, sql);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_developers_and_admins_tidy_open_drafts(db: PgPool) {
    let d = draft(&db).await;
    let as_ = |s| d.app.as_user(s);
    assert_eq!(
        as_(&d.team.reviewer).post_empty(&d.tidy).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        as_(&d.team.outsider).post_empty(&d.tidy).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        d.app.post_json(&d.tidy, &Value::Null).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        as_(&d.team.admin).post_empty(&d.tidy).await.status,
        StatusCode::OK
    );
    let other = d
        .team
        .path(&format!("/databases/shop/drafts/{}/tidy", Uuid::nil()));
    assert_eq!(
        as_(&d.team.developer).post_empty(&other).await.status,
        StatusCode::NOT_FOUND
    );

    // Published, it is closed for good.
    let preview = as_(&d.team.developer)
        .get(
            &d.team
                .path(&format!("/databases/shop/drafts/{}/publish", d.id)),
        )
        .await
        .json();
    let published = as_(&d.team.developer)
        .post(
            &d.team
                .path(&format!("/databases/shop/drafts/{}/publish", d.id)),
            &json!({ "sha256": preview["sha256"] }),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{:?}",
        published.json()
    );
    let response = as_(&d.team.developer).post_empty(&d.tidy).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "draft_published");
}
