//! Removing a version: hidden everywhere, numbers never given twice,
//! restorable for 30 days, then purged; drafts based on it update onto the
//! latest version left; the audit log keeps who, when and why.

use std::fmt::Write as _;

use axum::http::StatusCode;
use romulus_api::{domain::TokenScope, versions::removal};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    harness::{Team, TestApp, api_token, database, model_json},
    live::{live_app, quick, version},
};

/// `shop` with v1..=n, each one more table (`t1`…`tn`).
async fn shop(db: &PgPool, n: i32) -> (Team, Uuid) {
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    for k in 1..=n {
        let mut sql = String::new();
        for t in 1..=k {
            let _ = writeln!(sql, "CREATE TABLE t{t} (id bigint);");
        }
        version(db, shop, k, &sql, json!({})).await;
    }
    (team, shop)
}

async fn remove(
    app: &TestApp,
    team: &Team,
    number: i32,
    reason: &str,
) -> super::harness::TestResponse {
    app.as_user(&team.admin)
        .post(
            &team.path(&format!("/databases/shop/versions/{number}/remove")),
            &json!({ "reason": reason }),
        )
        .await
}

async fn numbers(app: &TestApp, team: &Team) -> Vec<i64> {
    app.as_user(&team.reviewer)
        .get(&team.path("/databases/shop/versions"))
        .await
        .json()["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["number"].as_i64().unwrap())
        .collect()
}

async fn shop_json(app: &TestApp, team: &Team) -> Value {
    app.as_user(&team.reviewer)
        .get(&team.path("/databases/shop"))
        .await
        .json()
}

/// Removed `days` ago, as if an admin had then (a removal is only ever
/// set, never edited, so the test sets it this way).
async fn removed_days_ago(db: &PgPool, shop: Uuid, number: i32, days: i32) {
    sqlx::query!(
        "UPDATE versions SET removed_at = now() - make_interval(days => $3), removal_reason = 'old'
         WHERE database_id = $1 AND number = $2",
        shop,
        number,
        days,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_removed_version_is_hidden_everywhere(db: PgPool) {
    let (team, _) = shop(&db, 3).await;
    let app = TestApp::new(db.clone());

    let removed = remove(&app, &team, 2, "  Pushed from the wrong database  ").await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.json());
    let removed = removed.json();
    assert_eq!(removed["version"]["number"], 2);
    assert_eq!(removed["reason"], "Pushed from the wrong database");
    assert_eq!(removed["removed_by"]["name"], "Admin-acme");

    assert_eq!(numbers(&app, &team).await, [3, 1]);
    let page = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/versions/2"))
        .await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    assert_eq!(page.json()["message"], "v2 was removed.");
    for path in [
        "/databases/shop/versions/2/texts",
        "/databases/shop/diff?from=1&to=2",
        "/databases/shop/migration?from=2&to=3",
    ] {
        let response = app.as_user(&team.reviewer).get(&team.path(path)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
    }
    // The database says how many are left and which numbers have none.
    let database = shop_json(&app, &team).await;
    assert_eq!(database["latest"]["number"], 3);
    assert_eq!(database["versions"], 2);
    assert_eq!(database["missing"], json!([2]));
    assert_eq!(database["next_number"], 4);
    // Only developers see what was removed.
    let listed = app
        .as_user(&team.developer)
        .get(&team.path("/databases/shop/versions/removed"))
        .await;
    assert_eq!(listed.json().as_array().unwrap().len(), 1);
    assert_eq!(
        app.as_user(&team.reviewer)
            .get(&team.path("/databases/shop/versions/removed"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_admins_remove_and_restore_and_say_why(db: PgPool) {
    let (team, _) = shop(&db, 2).await;
    let app = TestApp::new(db.clone());
    let path = team.path("/databases/shop/versions/2/remove");
    for (who, status) in [
        (&team.developer, StatusCode::FORBIDDEN),
        (&team.reviewer, StatusCode::FORBIDDEN),
        (&team.outsider, StatusCode::NOT_FOUND),
    ] {
        let response = app.as_user(who).post(&path, &json!({"reason": "x"})).await;
        assert_eq!(response.status, status);
    }
    let blank = remove(&app, &team, 2, "   ").await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(blank.json()["details"]["reason"].is_string());
    let long = remove(&app, &team, 2, &"x".repeat(1001)).await;
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        remove(&app, &team, 9, "x").await.status,
        StatusCode::NOT_FOUND
    );

    assert_eq!(remove(&app, &team, 2, "wrong").await.status, StatusCode::OK);
    assert_eq!(
        remove(&app, &team, 2, "again").await.code(),
        "version_removed"
    );
    let restore = team.path("/databases/shop/versions/2/restore");
    let refused = app.as_user(&team.developer).post_empty(&restore).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let restored = app.as_user(&team.admin).post_empty(&restore).await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.json());
    assert_eq!(restored.json()["number"], 2);
    let again = app.as_user(&team.admin).post_empty(&restore).await;
    assert_eq!(again.code(), "version_not_removed");
    assert_eq!(numbers(&app, &team).await, [2, 1]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_audit_log_keeps_who_when_and_why(db: PgPool) {
    let (team, _) = shop(&db, 2).await;
    let app = TestApp::new(db.clone());
    remove(&app, &team, 2, "Leaked a secret in a comment").await;
    app.as_user(&team.admin)
        .post_empty(&team.path("/databases/shop/versions/2/restore"))
        .await;

    let activity = app
        .as_user(&team.developer)
        .get(&team.path("/databases/shop/activity"))
        .await;
    assert_eq!(activity.status, StatusCode::OK);
    let events = activity.json()["events"].clone();
    let actions: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].clone(),
                e["version_number"].clone(),
                e["actor"]["name"].clone(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            (json!("version_restored"), json!(2), json!("Admin-acme")),
            (json!("version_removed"), json!(2), json!("Admin-acme")),
        ]
    );
    assert_eq!(events[1]["reason"], "Leaked a secret in a comment");
    let reviewer = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/activity"))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);

    // Append only: neither changed nor deleted while the database lives.
    let err = sqlx::query!("UPDATE audit_events SET reason = 'nothing'")
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(
        romulus_api::db::constraint(&err),
        Some("audit_events_are_append_only")
    );
    assert!(
        sqlx::query!("DELETE FROM audit_events")
            .execute(&db)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn removing_the_latest_falls_back_to_the_one_before(db: PgPool) {
    let (team, shop) = shop(&db, 2).await;
    let app = TestApp::compiling(db.clone());
    remove(&app, &team, 2, "bad").await;
    assert_eq!(shop_json(&app, &team).await["latest"]["number"], 1);

    // The next upload is v3: v2's number stays given.
    let uploaded = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/shop/versions"),
            &json!({"sql": "CREATE TABLE t9 (id bigint);"}),
        )
        .await;
    assert_eq!(uploaded.json()["number"], 3, "{}", uploaded.json());

    // Removing every version leaves a database with none, still numbering on.
    remove(&app, &team, 3, "bad too").await;
    remove(&app, &team, 1, "all of it").await;
    let database = shop_json(&app, &team).await;
    assert_eq!(database["latest"], Value::Null);
    assert_eq!(database["versions"], 0);
    assert_eq!(database["next_number"], 4);
    let latest = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/versions?limit=5"))
        .await;
    assert_eq!(latest.json()["versions"], json!([]));
    let last = sqlx::query_scalar!(
        "SELECT last_version_number FROM databases WHERE id = $1",
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(last, 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_push_is_compared_with_the_latest_version_left(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let token = api_token(&db, shop, TokenScope::Push).await;
    let app = TestApp::new(db.clone());
    let push = |tables: &'static [&'static str]| {
        let (app, token) = (&app, &token);
        async move {
            app.push(shop, token, Some("application/json"), model_json(tables))
                .await
        }
    };
    assert_eq!(push(&["users"]).await.json()["version"]["number"], 1);
    assert_eq!(
        push(&["users", "orders"]).await.json()["version"]["number"],
        2
    );
    remove(&app, &team, 2, "orders were never real").await;

    // Unchanged: v1's model, the latest left.
    let same = push(&["users"]).await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.json());
    assert_eq!(same.json()["status"], "unchanged");
    assert_eq!(same.json()["version"]["number"], 1);
    // The removed version's model is new again: v3.
    let again = push(&["users", "orders"]).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.json());
    assert_eq!(again.json()["version"]["number"], 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_can_be_restored_for_thirty_days(db: PgPool) {
    let (team, shop) = shop(&db, 2).await;
    let app = TestApp::new(db.clone());
    removed_days_ago(&db, shop, 2, removal::RESTORE_DAYS + 1).await;
    let late = app
        .as_user(&team.admin)
        .post_empty(&team.path("/databases/shop/versions/2/restore"))
        .await;
    assert_eq!(late.code(), "restore_expired");

    removed_days_ago(&db, shop, 1, removal::RESTORE_DAYS - 1).await;
    let in_time = app
        .as_user(&team.admin)
        .post_empty(&team.path("/databases/shop/versions/1/restore"))
        .await;
    assert_eq!(in_time.status, StatusCode::OK);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_purge_deletes_old_removals_and_never_gives_their_number_again(db: PgPool) {
    let (team, shop) = shop(&db, 3).await;
    let app = live_app(db.clone(), quick());
    // A draft published as v3, and one still open on v2.
    let v3 = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = 3",
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let v2 = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = 2",
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let published = sqlx::query_scalar!(
        "INSERT INTO drafts (database_id, name, base_version_id, status, published_version_id)
         VALUES ($1, 'Became v3', $2, 'published', $3) RETURNING id",
        shop,
        v2,
        v3
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let open = sqlx::query_scalar!(
        "INSERT INTO drafts (database_id, name, base_version_id) VALUES ($1, 'On v2', $2) RETURNING id",
        shop,
        v2
    )
    .fetch_one(&db)
    .await
    .unwrap();
    removed_days_ago(&db, shop, 3, 31).await;
    removed_days_ago(&db, shop, 2, 31).await;
    // Too recent to purge, and before 30 days the database refuses anyway.
    removed_days_ago(&db, shop, 1, 3).await;
    let v1 = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = 1",
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(
        sqlx::query!("DELETE FROM versions WHERE id = $1", v1)
            .execute(&db)
            .await
            .is_err()
    );

    // v3 goes with the draft published as it; v2 waits for the open draft.
    assert_eq!(removal::purge(&db).await.unwrap(), 1);
    let left: Vec<i32> = sqlx::query_scalar!(
        "SELECT number FROM versions WHERE database_id = $1 ORDER BY number",
        shop
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(left, [1, 2]);
    let drafts: Vec<Uuid> = sqlx::query_scalar!("SELECT id FROM drafts")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(drafts, [open]);
    assert_ne!(open, published);

    sqlx::query!("UPDATE drafts SET status = 'deleted' WHERE id = $1", open)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(removal::purge(&db).await.unwrap(), 1);
    // The deleted draft forgets its base.
    let base = sqlx::query_scalar!("SELECT base_version_id FROM drafts WHERE id = $1", open)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(base, None);

    let purged: Vec<(String, Option<i32>, Option<Uuid>)> = sqlx::query!(
        r#"SELECT action::text AS "action!", version_number, actor_id FROM audit_events ORDER BY id"#
    )
    .fetch_all(&db)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.action, r.version_number, r.actor_id))
    .collect();
    assert_eq!(
        purged,
        [
            ("version_purged".to_owned(), Some(3), None),
            ("version_purged".to_owned(), Some(2), None)
        ]
    );
    // Numbers stay given: the next version is v4.
    assert_eq!(shop_json(&app, &team).await["next_number"], 4);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_based_on_a_removed_version_updates_onto_the_one_before(db: PgPool) {
    let (team, _) = shop(&db, 2).await;
    let app = live_app(db.clone(), quick());
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let path = |rest: &str| {
        team.path(&format!(
            "/databases/shop/drafts/{}{rest}",
            draft["id"].as_str().unwrap()
        ))
    };
    assert_eq!(draft["base_version"], 2);
    remove(&app, &team, 2, "t2 should not exist").await;

    let stale = app.as_user(&team.developer).get(&path("")).await.json();
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["base_removed"], true);
    assert_eq!(stale["latest_version"], 1);
    assert_eq!(stale["next_version"], 3);
    let preview = app
        .as_user(&team.developer)
        .get(&path("/publish"))
        .await
        .json();
    let publish = app
        .as_user(&team.developer)
        .post(&path("/publish"), &json!({"sha256": preview["sha256"]}))
        .await;
    assert_eq!(publish.code(), "stale_base");

    // Updating merges v1 in: t2, which only v2 had, goes.
    let updated = app
        .as_user(&team.developer)
        .post_empty(&path("/update"))
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    assert_eq!(updated.json()["base_version"], 1);
    let sql = app
        .state
        .collab
        .content(draft["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .sql;
    assert!(!sql.contains("t2"), "{sql}");
    let now = app.as_user(&team.developer).get(&path("")).await.json();
    assert_eq!(
        (now["stale"].clone(), now["base_removed"].clone()),
        (json!(false), json!(false))
    );

    // A new draft starts from v1, the latest left.
    let fresh = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    assert_eq!(fresh["base_version"], 1);
    assert_eq!(fresh["name"], "Draft from v1");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_whose_only_version_was_removed_keeps_its_text(db: PgPool) {
    let (team, _) = shop(&db, 1).await;
    let app = live_app(db.clone(), quick());
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let path = |rest: &str| team.path(&format!("/databases/shop/drafts/{id}{rest}"));
    remove(&app, &team, 1, "wrong database").await;
    assert_eq!(
        app.as_user(&team.developer).get(&path("")).await.json()["stale"],
        true
    );

    let updated = app
        .as_user(&team.developer)
        .post_empty(&path("/update"))
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    assert_eq!(updated.json()["base_version"], Value::Null);
    let now = app.as_user(&team.developer).get(&path("")).await.json();
    assert_eq!(now["stale"], false);
    assert!(
        app.state
            .collab
            .content(id)
            .await
            .unwrap()
            .sql
            .contains("t1")
    );
    // Published, it is v2: v1's number stays given.
    let preview = app
        .as_user(&team.developer)
        .get(&path("/publish"))
        .await
        .json();
    assert_eq!(preview["number"], 2);
    assert_eq!(preview["stale"], false);
}

const HISTORY: &[(&str, &str)] = &[
    (
        "20240101000000_users.sql",
        "CREATE TABLE users (id bigint);\n",
    ),
    (
        "20240102000000_posts.sql",
        "CREATE TABLE posts (id bigint);\n",
    ),
];

async fn import(
    app: &TestApp,
    team: &Team,
    files: &[(&str, &str)],
) -> super::harness::TestResponse {
    let mut text = String::new();
    for (path, sql) in files {
        let _ = write!(
            text,
            "--b\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{path}\"\r\nContent-Type: application/sql\r\n\r\n{sql}\r\n"
        );
    }
    text.push_str("--b--\r\n");
    app.send(
        axum::http::Request::post(team.path("/databases/shop/imports"))
            .header(axum::http::header::ORIGIN, super::harness::ORIGIN)
            .header(romulus_api::csrf::HEADER, "1")
            .header(axum::http::header::COOKIE, team.developer.cookie())
            .header(
                axum::http::header::CONTENT_TYPE,
                "multipart/form-data; boundary=b",
            )
            .body(axum::body::Body::from(text))
            .unwrap(),
    )
    .await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn imports_do_not_see_removed_versions(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "shop").await;
    let app = TestApp::compiling(db.clone());
    let first = import(&app, &team, HISTORY).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json());

    // The second migration was a mistake: removed, it is as if never
    // imported, so importing again brings it back, numbered after.
    remove(&app, &team, 2, "imported by mistake").await;
    let again = import(&app, &team, HISTORY).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.json());
    assert_eq!(again.json()["created"], 1);
    assert_eq!(again.json()["migrations"][1]["version"], 3);
    assert_eq!(numbers(&app, &team).await, [3, 1]);

    // Restoring v2 now would import that migration twice.
    let restore = app
        .as_user(&team.admin)
        .post_empty(&team.path("/databases/shop/versions/2/restore"))
        .await;
    assert_eq!(restore.code(), "migration_imported_again");

    // A database whose only other version is removed imports without
    // `append`.
    let team2 = Team::new(&db, "beta").await;
    let blog = database(&db, team2.org, "shop").await;
    version(&db, blog, 1, "CREATE TABLE x (id bigint);", json!({})).await;
    remove(&app, &team2, 1, "not ours").await;
    let fresh = import(&app, &team2, HISTORY).await;
    assert_eq!(fresh.status, StatusCode::CREATED, "{}", fresh.json());
    assert_eq!(fresh.json()["migrations"][0]["version"], 2);
}
