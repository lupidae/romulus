//! Personal access tokens (`auth::access_tokens`): created by the person
//! signed in, shown once, stored hashed, revoked, expired, and accepted at
//! the MCP endpoint only.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use romulus_api::token;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::harness::{Session, TestApp, TestResponse, signed_in};

const TOKENS: &str = "/api/me/access-tokens";

/// `initialize` at the MCP endpoint with `token`.
pub async fn initialize(app: &TestApp, token: &str) -> TestResponse {
    app.send(
        Request::post("/api/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": { "name": "test-client", "version": "1.0" },
                    },
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await
}

async fn create(app: &TestApp, who: &Session, body: &Value) -> TestResponse {
    app.as_user(who).post(TOKENS, body).await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_is_shown_once_stored_hashed_and_listed_without_its_secret(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let created = create(
        &app,
        &alice,
        &json!({ "name": "Claude Code", "scope": "write", "expires_in_days": 90 }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let body = created.json();
    let secret = body["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("rmls_pat_"), "{secret}");
    assert_eq!(secret.len(), "rmls_pat_".len() + 43);
    assert_eq!(body["token"]["scope"], "write");
    assert!(body["token"]["expires_at"].is_string());

    // Only the hash is stored.
    let stored = sqlx::query_scalar!("SELECT token_hash FROM personal_access_tokens")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, token::hash(&secret));

    let listed = app.as_user(&alice).get(TOKENS).await.json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(!listed.to_string().contains(&secret));
    assert!(listed[0]["last_used_at"].is_null());

    // Used: when, and by which client.
    assert_eq!(initialize(&app, &secret).await.status, StatusCode::OK);
    let listed = app.as_user(&alice).get(TOKENS).await.json();
    assert!(listed[0]["last_used_at"].is_string());
    assert_eq!(listed[0]["last_client"], "test-client 1.0");

    // Someone else's list is theirs.
    let bob = signed_in(&db, "Bob").await;
    assert_eq!(app.as_user(&bob).get(TOKENS).await.json(), json!([]));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_revoked_token_stops_working_at_once(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let body = create(&app, &alice, &json!({ "name": "laptop", "scope": "read" }))
        .await
        .json();
    let secret = body["secret"].as_str().unwrap().to_owned();
    let id = body["token"]["id"].as_str().unwrap().to_owned();
    assert!(body["token"]["expires_at"].is_null(), "no expiry asked");
    assert_eq!(initialize(&app, &secret).await.status, StatusCode::OK);

    // Not someone else's to revoke.
    let bob = signed_in(&db, "Bob").await;
    let theirs = app.as_user(&bob).delete(&format!("{TOKENS}/{id}")).await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(initialize(&app, &secret).await.status, StatusCode::OK);

    let revoked = app.as_user(&alice).delete(&format!("{TOKENS}/{id}")).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let refused = initialize(&app, &secret).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let challenge = refused.headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.starts_with("Bearer "), "{challenge}");
    assert!(
        refused.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("revoked")
    );
    assert_eq!(app.as_user(&alice).get(TOKENS).await.json(), json!([]));
    // Twice is not found.
    let again = app.as_user(&alice).delete(&format!("{TOKENS}/{id}")).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_expired_token_is_refused_and_says_so(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let body = create(
        &app,
        &alice,
        &json!({ "name": "old", "scope": "write", "expires_in_days": 30 }),
    )
    .await
    .json();
    let secret = body["secret"].as_str().unwrap().to_owned();
    sqlx::query!(
        "UPDATE personal_access_tokens
         SET created_at = now() - interval '31 days', expires_at = now() - interval '1 day'"
    )
    .execute(&db)
    .await
    .unwrap();
    let refused = initialize(&app, &secret).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(
        refused.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("expired")
    );
    assert_eq!(
        app.as_user(&alice).get(TOKENS).await.json()[0]["expired"],
        true
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn names_and_expiries_are_checked(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    for body in [
        json!({ "name": " ", "scope": "write" }),
        json!({ "name": "x".repeat(101), "scope": "write" }),
        json!({ "name": "ok", "scope": "write", "expires_in_days": 7 }),
    ] {
        let refused = create(&app, &alice, &body).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let bad_scope = create(&app, &alice, &json!({ "name": "ok", "scope": "admin" })).await;
    assert_eq!(bad_scope.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Signed out: nothing.
    let anonymous = app.call(axum::http::Method::GET, TOKENS, None, None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_person_has_at_most_fifty_live_tokens(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let user: Uuid = app.as_user(&alice).get("/api/me").await.json()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    for n in 0..50 {
        sqlx::query!(
            "INSERT INTO personal_access_tokens (user_id, name, token_hash, scope)
             VALUES ($1, $2, $3, 'read')",
            user,
            format!("t{n}"),
            token::hash(&token::generate()),
        )
        .execute(&db)
        .await
        .unwrap();
    }
    let refused = create(
        &app,
        &alice,
        &json!({ "name": "one more", "scope": "read" }),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "too_many_tokens");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_personal_token_opens_the_mcp_endpoint_and_nothing_else(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let secret = create(&app, &alice, &json!({ "name": "agent", "scope": "write" }))
        .await
        .json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    // The REST API never takes it as a person: bearer requests are tokens'.
    for path in ["/api/me", "/api/orgs", TOKENS] {
        let refused = app.get_with_token(path, &secret).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // Nor as a database's API token on the machine API.
    let database = Uuid::nil();
    let push = app
        .push(database, &secret, Some("application/json"), "{}")
        .await;
    assert_eq!(push.status, StatusCode::UNAUTHORIZED);
    assert_eq!(push.code(), "invalid_token");
}
