//! The guard runs before routing, so these go to an unknown path: 404 means
//! it let the request through, 403 `csrf` means it stopped it. Real endpoints
//! check it too (`auth.rs`, `invitations.rs`).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use romulus_api::csrf;
use serde_json::json;
use sqlx::PgPool;

use crate::harness::{ORIGIN, TestApp};

fn post(origin: Option<&str>, marked: bool) -> Request<Body> {
    let mut request = Request::post("/api/anything");
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    if marked {
        request = request.header(csrf::HEADER, "1");
    }
    request.body(Body::empty()).expect("request")
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn same_origin_mutation_with_header_passes(db: PgPool) {
    let response = TestApp::new(db)
        .post_json("/api/anything", &json!({}))
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn mutation_without_header_is_refused(db: PgPool) {
    let response = TestApp::new(db).send(post(Some(ORIGIN), false)).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.code(), "csrf");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn mutation_from_another_origin_is_refused(db: PgPool) {
    let app = TestApp::new(db);
    for origin in [
        Some("http://evil.test"),
        Some("http://localhost:41001"),
        None,
    ] {
        let response = app.send(post(origin, true)).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN, "origin {origin:?}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn bearer_requests_are_exempt(db: PgPool) {
    let request = Request::put("/api/anything")
        .header(header::AUTHORIZATION, "Bearer rom_test")
        .body(Body::empty())
        .expect("request");
    let response = TestApp::new(db).send(request).await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn safe_methods_are_not_checked(db: PgPool) {
    let request = Request::get("/api/health")
        .header(header::ORIGIN, "http://evil.test")
        .body(Body::empty())
        .expect("request");
    let response = TestApp::new(db).send(request).await;
    assert_eq!(response.status, StatusCode::OK);
}
