//! A draft's source over REST (`drafts::text`): read with its compile,
//! written as a person's edit (live, a rewrite for undo, an editor for
//! approvals), and its rename decisions.

use axum::http::StatusCode;
use romulus_api::collab::doc::RewriteNote;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::Transact;

use super::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, quick, serve, version},
};

const V1: &str = "CREATE TABLE public.users (id bigint);\n";

async fn setup(db: &PgPool) -> (TestApp, Team, Uuid, String) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({})).await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let path = team.path(&format!("/databases/shop/drafts/{id}"));
    (app, team, id, path)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn members_read_the_text_with_its_compile(db: PgPool) {
    let (app, team, _, path) = setup(&db).await;
    let read = app
        .as_user(&team.reviewer)
        .get(&format!("{path}/text?wait=true"))
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.json());
    let body = read.json();
    assert_eq!(body["mode"], "schema");
    assert_eq!(body["text"], V1);
    assert_eq!(body["base_version"], 1);
    assert_eq!(body["compile"]["state"], "ok");
    assert_eq!(body["compile"]["current"], true);
    assert_eq!(body["sha256"].as_str().unwrap().len(), 64);
    let outsider = app
        .as_user(&team.outsider)
        .get(&format!("{path}/text"))
        .await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_developer_writes_it_live_and_becomes_an_editor(db: PgPool) {
    let (app, team, id, path) = setup(&db).await;
    let addr = serve(&app).await;
    let mut watcher =
        Client::join(addr, &format!("/api/orgs/acme/drafts/{id}/ws"), &team.admin).await;
    watcher.until(|c| c.sql() == V1).await;
    let sha = app
        .as_user(&team.developer)
        .get(&format!("{path}/text"))
        .await
        .json()["sha256"]
        .as_str()
        .unwrap()
        .to_owned();

    let written = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/text"),
            &json!({
                "edits": [{ "old": "", "new": "CREATE TABLE public.orders (id bigint);\n" }],
                "sha256": sha,
                "wait": true,
            }),
        )
        .await;
    assert_eq!(written.status, StatusCode::OK, "{}", written.json());
    let body = written.json();
    assert!(
        body["text"]
            .as_str()
            .unwrap()
            .ends_with("public.orders (id bigint);\n")
    );
    assert_eq!(body["compile"]["state"], "ok");
    watcher.until(|c| c.sql().contains("public.orders")).await;
    // A rewrite for undo histories, by an agent.
    let note = watcher.roots.rewrites(&watcher.doc.transact());
    assert_eq!(
        note,
        Some(RewriteNote {
            count: 1,
            reason: "agent".into()
        })
    );
    let editors: Vec<Uuid> =
        sqlx::query_scalar!("SELECT user_id FROM draft_editors WHERE draft_id = $1", id)
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(editors, [team.developer.user]);

    // The text read before is not the text now: refused, nothing overwritten.
    let stale = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/text"),
            &json!({ "text": "-- nothing", "sha256": sha }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "draft_changed");
    let mode = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/text"),
            &json!({ "text": "x", "mode": "migration" }),
        )
        .await;
    assert_eq!(mode.code(), "draft_mode_changed");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn writing_is_for_developers_and_checked(db: PgPool) {
    let (app, team, _, path) = setup(&db).await;
    let reviewer = app
        .as_user(&team.reviewer)
        .put(&format!("{path}/text"), &json!({ "text": "x" }))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
    for (body, code) in [
        (json!({}), "validation"),
        (
            json!({ "text": "x", "edits": [{ "new": "y" }] }),
            "validation",
        ),
        (json!({ "edits": [] }), "validation"),
        (
            json!({ "edits": [{ "old": "nowhere", "new": "y" }] }),
            "edit_not_found",
        ),
        (
            json!({ "edits": [{ "old": "bigint", "new": "int" }, { "old": "s", "new": "x" }] }),
            "edit_ambiguous",
        ),
    ] {
        let refused = app
            .as_user(&team.developer)
            .put(&format!("{path}/text"), &body)
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(refused.code(), code, "{body}");
    }
    let huge = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/text"),
            &json!({ "text": "x".repeat((1 << 20) + 1) }),
        )
        .await;
    assert_eq!(huge.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn rename_decisions_are_written_for_the_person(db: PgPool) {
    let (app, team, id, path) = setup(&db).await;
    let set = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/renames"),
            &json!({ "renames": { "table/public//users/people": false, "column/public/users/a/b": true } }),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    assert_eq!(
        set.json()["renames"],
        json!({ "table/public//users/people": false, "column/public/users/a/b": true })
    );
    let cleared: Value = app
        .as_user(&team.developer)
        .put(
            &format!("{path}/renames"),
            &json!({ "renames": { "column/public/users/a/b": null } }),
        )
        .await
        .json();
    assert_eq!(
        cleared["renames"],
        json!({ "table/public//users/people": false })
    );
    let editors = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM draft_editors WHERE draft_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(editors, 1, "a decision is an edit");
    let reviewer = app
        .as_user(&team.reviewer)
        .put(&format!("{path}/renames"), &json!({ "renames": {} }))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
}
