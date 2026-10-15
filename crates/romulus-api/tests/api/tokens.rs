//! API tokens: created by developers, shown once, stored hashed, scoped to
//! one database, revocable.

use axum::http::StatusCode;
use romulus_api::{domain::TokenScope, token};
use serde_json::json;
use sqlx::PgPool;

use crate::harness::{Team, TestApp, api_token, database, model_json};

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_is_shown_once_and_only_its_hash_is_stored(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let app = TestApp::new(db.clone());
    let dev = app.as_user(&team.developer);

    let response = dev
        .post(
            &team.path("/databases/billing/tokens"),
            &json!({ "name": "GitHub Actions", "scope": "push" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let created = response.json();
    let secret = created["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("rom_") && secret.len() == 47, "{secret}");
    assert_eq!(created["token"]["name"], "GitHub Actions");
    assert_eq!(created["token"]["scope"], "push");
    assert_eq!(created["token"]["created_by"], "Developer-acme");

    // The row holds sha256(secret) and nothing else derived from it.
    let row = sqlx::query!(
        "SELECT token_hash, to_jsonb(api_tokens.*)::text AS \"row!\" FROM api_tokens WHERE database_id = $1",
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(row.token_hash, token::hash(&secret));
    assert!(!row.row.contains(&secret[4..]));

    // Listing never shows it again.
    let list = dev.get(&team.path("/databases/billing/tokens")).await;
    assert_eq!(list.status, StatusCode::OK);
    let list = list.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("secret").is_none());
    assert!(!list.to_string().contains(&secret[4..]));
    assert!(list[0]["last_used_at"].is_null());

    // It works, and its use is recorded.
    let push = app
        .push(
            id,
            &secret,
            Some("application/json"),
            model_json(&["users"]),
        )
        .await;
    assert_eq!(push.status, StatusCode::CREATED);
    let list = dev
        .get(&team.path("/databases/billing/tokens"))
        .await
        .json();
    assert!(list[0]["last_used_at"].is_string());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_developers_and_admins_manage_tokens(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::new(db.clone());
    let path = team.path("/databases/billing/tokens");
    let body = json!({ "name": "CI", "scope": "full" });
    let token_id: uuid::Uuid = sqlx::query_scalar!("SELECT id FROM api_tokens")
        .fetch_one(&db)
        .await
        .unwrap();
    let one = format!("{path}/{token_id}");

    let reviewer = app.as_user(&team.reviewer);
    assert_eq!(reviewer.get(&path).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reviewer.post(&path, &body).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(reviewer.delete(&one).await.status, StatusCode::FORBIDDEN);

    let outsider = app.as_user(&team.outsider);
    assert_eq!(outsider.get(&path).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        outsider.post(&path, &body).await.status,
        StatusCode::NOT_FOUND
    );

    let admin = app.as_user(&team.admin);
    assert_eq!(admin.get(&path).await.status, StatusCode::OK);
    assert_eq!(admin.post(&path, &body).await.status, StatusCode::CREATED);
    assert_eq!(admin.delete(&one).await.status, StatusCode::NO_CONTENT);

    let dev = app.as_user(&team.developer);
    assert_eq!(dev.get(&path).await.json().as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_revoked_token_stops_working_at_once(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let app = TestApp::new(db);
    let dev = app.as_user(&team.developer);
    let created = dev
        .post(
            &team.path("/databases/billing/tokens"),
            &json!({ "name": "CI", "scope": "push" }),
        )
        .await
        .json();
    let secret = created["secret"].as_str().unwrap();
    let path = team.path(&format!(
        "/databases/billing/tokens/{}",
        created["token"]["id"].as_str().unwrap()
    ));

    assert_eq!(dev.delete(&path).await.status, StatusCode::NO_CONTENT);
    let push = app
        .push(id, secret, Some("application/json"), model_json(&["users"]))
        .await;
    assert_eq!(
        (push.status, push.code().as_str()),
        (StatusCode::UNAUTHORIZED, "invalid_token")
    );
    // Gone from the list; revoking again is a 404.
    assert_eq!(
        dev.get(&team.path("/databases/billing/tokens"))
            .await
            .json(),
        json!([])
    );
    assert_eq!(dev.delete(&path).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_names_its_purpose(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::new(db);
    let dev = app.as_user(&team.developer);
    let path = team.path("/databases/billing/tokens");
    let blank = dev
        .post(&path, &json!({ "name": "  ", "scope": "push" }))
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(blank.json()["details"]["name"].is_string());
    let bad_scope = dev
        .post(&path, &json!({ "name": "CI", "scope": "admin" }))
        .await;
    assert_eq!(bad_scope.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_works_for_its_own_database_only(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let billing = database(&db, team.org, "billing").await;
    let other = database(&db, team.org, "other").await;
    let token = api_token(&db, billing, TokenScope::Full).await;
    let app = TestApp::new(db);

    let push = app
        .push(
            other,
            &token,
            Some("application/json"),
            model_json(&["users"]),
        )
        .await;
    assert_eq!(
        (push.status, push.code().as_str()),
        (StatusCode::FORBIDDEN, "wrong_database")
    );
    let read = app
        .get_with_token(&format!("/api/databases/{other}/versions/latest"), &token)
        .await;
    assert_eq!(read.code(), "wrong_database");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn push_tokens_push_and_full_tokens_also_read(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let push = api_token(&db, id, TokenScope::Push).await;
    let full = api_token(&db, id, TokenScope::Full).await;
    let app = TestApp::new(db);
    let latest = format!("/api/databases/{id}/versions/latest");

    // Nothing to read yet.
    assert_eq!(
        app.get_with_token(&latest, &full).await.status,
        StatusCode::NOT_FOUND
    );
    for token in [&push, &full] {
        let response = app
            .push(id, token, Some("application/json"), model_json(&["users"]))
            .await;
        assert!(response.status.is_success(), "{}", response.json());
    }

    let denied = app.get_with_token(&latest, &push).await;
    assert_eq!(
        (denied.status, denied.code().as_str()),
        (StatusCode::FORBIDDEN, "insufficient_scope")
    );
    let read = app.get_with_token(&latest, &full).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.json()["number"], 1);
    assert_eq!(read.json()["model"]["entities"][0]["name"], "users");
    let by_number = app
        .get_with_token(&format!("/api/databases/{id}/versions/1"), &full)
        .await;
    assert_eq!(by_number.json()["id"], read.json()["id"]);
    let garbage = app
        .get_with_token(&format!("/api/databases/{id}/versions/first"), &full)
        .await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_machine_api_takes_tokens_never_cookies(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let app = TestApp::new(db);
    let path = format!("/api/databases/{id}/versions");

    // No token: told what is missing, even as a signed-in browser would ask.
    let signed_in = app.as_user(&team.admin).post(&path, &json!({})).await;
    assert_eq!(
        (signed_in.status, signed_in.code().as_str()),
        (StatusCode::UNAUTHORIZED, "invalid_token")
    );
    let read = app
        .as_user(&team.admin)
        .get(&format!("{path}/latest"))
        .await;
    assert_eq!(read.code(), "invalid_token");
    let garbage = app
        .push(id, "not-a-token", Some("application/json"), "{}")
        .await;
    assert_eq!(garbage.code(), "invalid_token");
    let unknown = app
        .push(
            id,
            &format!("rom_{}", token::generate()),
            Some("application/json"),
            "{}",
        )
        .await;
    assert_eq!(unknown.code(), "invalid_token");
}
