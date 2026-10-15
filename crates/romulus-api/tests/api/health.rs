use axum::http::StatusCode;
use sqlx::PgPool;

use crate::harness::TestApp;

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn health_reports_the_database_up(db: PgPool) {
    let response = TestApp::new(db).get("/api/health").await;
    assert_eq!(response.status, StatusCode::OK);
    let body = response.json();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["database"], "ok");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn health_is_unavailable_without_a_database(db: PgPool) {
    let app = TestApp::new(db.clone());
    db.close().await;
    let response = app.get("/api/health").await;
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.code(), "unavailable");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_response_carries_a_request_id(db: PgPool) {
    let response = TestApp::new(db).get("/api/health").await;
    assert!(response.headers.contains_key("x-request-id"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn openapi_document_lists_health(db: PgPool) {
    let response = TestApp::new(db).get("/api/openapi.json").await;
    assert_eq!(response.status, StatusCode::OK);
    let spec = response.json();
    assert!(spec["paths"]["/api/health"]["get"].is_object(), "{spec}");
    assert!(spec["components"]["schemas"]["ErrorBody"].is_object());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn unknown_endpoints_answer_json_404(db: PgPool) {
    let response = TestApp::new(db).get("/api/nope").await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.code(), "not_found");
}
