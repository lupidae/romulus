//! Drives the real router in-process, and inserts the rows most tests need.

use axum::{
    Router,
    body::{Body, Bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use romulus_api::{AppState, Config, app, csrf, domain::Role};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The `Origin` the web app sends, matching `Config::for_tests().public_url`.
pub const ORIGIN: &str = "http://localhost:41000";

pub struct TestApp {
    router: Router,
}

impl TestApp {
    pub fn new(db: PgPool) -> Self {
        Self {
            router: app(AppState::new(db, Config::for_tests())),
        }
    }

    pub async fn send(&self, request: Request<Body>) -> TestResponse {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        TestResponse {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.send(Request::get(path).body(Body::empty()).expect("request"))
            .await
    }

    /// A JSON POST as the web app sends it: CSRF header and same `Origin`.
    pub async fn post_json(&self, path: &str, body: &Value) -> TestResponse {
        let request = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, ORIGIN)
            .header(csrf::HEADER, "1")
            .body(Body::from(body.to_string()))
            .expect("request");
        self.send(request).await
    }
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl TestResponse {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!("not JSON ({err}): {}", String::from_utf8_lossy(&self.body))
        })
    }

    /// The stable `code` of an error body.
    pub fn code(&self) -> String {
        self.json()["code"]
            .as_str()
            .expect("an error body")
            .to_owned()
    }
}

pub async fn user(db: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name)
         VALUES ($1, $2, true, $3) RETURNING id",
        format!("sub-{name}"),
        format!("{}@romulus.test", name.to_lowercase()),
        name,
    )
    .fetch_one(db)
    .await
    .expect("insert user")
}

pub async fn org(db: &PgPool, slug: &str) -> Uuid {
    sqlx::query_scalar!(
        "INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id",
        slug,
    )
    .fetch_one(db)
    .await
    .expect("insert organization")
}

pub async fn member(db: &PgPool, org: Uuid, user: Uuid, role: Role) {
    sqlx::query!(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)",
        org,
        user,
        role as Role,
    )
    .execute(db)
    .await
    .expect("insert membership");
}

pub async fn database(db: &PgPool, org: Uuid, slug: &str) -> Uuid {
    sqlx::query_scalar!(
        "INSERT INTO databases (org_id, name, slug) VALUES ($1, $2, $2) RETURNING id",
        org,
        slug,
    )
    .fetch_one(db)
    .await
    .expect("insert database")
}
