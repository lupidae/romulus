//! Drafts over REST: one-click creation, list, rename, delete, publish.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, quick, serve, stored_sql, version},
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

    // Its content is the version's SQL and presentation.
    let id: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(stored_sql(&db, id).await, V1);
    let content = app.state.collab.content(id).await.unwrap();
    assert_eq!(content.presentation.layout.len(), 2);
    assert_eq!(content.presentation.groups["g1"].name, "Core");
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
