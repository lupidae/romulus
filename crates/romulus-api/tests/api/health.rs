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

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_wrong_method_answers_in_the_api_format(db: PgPool) {
    let response = TestApp::new(db)
        .post_json("/api/health", &serde_json::json!({}))
        .await;
    assert_eq!(response.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.code(), "method_not_allowed");
}

/// The web app's generated types key operations by id: two `list` handlers
/// in different modules would collide there. Name them (`operation_id`).
#[test]
fn openapi_operation_ids_are_unique() {
    let spec = serde_json::to_value(romulus_api::openapi()).unwrap();
    let mut seen = std::collections::HashSet::new();
    for (path, methods) in spec["paths"].as_object().unwrap() {
        for (method, operation) in methods.as_object().unwrap() {
            if let Some(id) = operation["operationId"].as_str() {
                assert!(
                    seen.insert(id.to_owned()),
                    "{method} {path}: operationId {id} is taken"
                );
            }
        }
    }
}
