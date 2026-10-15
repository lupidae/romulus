//! Drives the real router in-process, and inserts the rows most tests need.

use axum::{
    Router,
    body::{Body, Bytes},
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use romulus_api::{
    AppState, Config, app,
    compile::{Code, Compiled, Diagnostic, LineIndex, SqlCompiler},
    csrf,
    domain::{Role, Schema, TokenScope},
    token,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The `Origin` the web app sends, matching `Config::for_tests().public_url`.
pub const ORIGIN: &str = "http://localhost:41000";

pub struct TestApp {
    router: Router,
    pub state: AppState,
}

impl TestApp {
    pub fn new(db: PgPool) -> Self {
        Self::with_config(db, Config::for_tests())
    }

    pub fn with_config(db: PgPool, config: Config) -> Self {
        Self::with_state(AppState::new(db, config))
    }

    /// With [`fake_compiler`], so SQL uploads and pushes work without a sandbox.
    pub fn compiling(db: PgPool) -> Self {
        Self::with_state(AppState::new(db, Config::for_tests()).with_compiler(fake_compiler()))
    }

    pub fn with_state(state: AppState) -> Self {
        Self {
            router: app(state.clone()),
            state,
        }
    }

    /// A machine API push: `Authorization: Bearer`, no cookie, no CSRF header.
    pub async fn push(
        &self,
        database: Uuid,
        token: &str,
        content_type: Option<&str>,
        body: impl Into<Body>,
    ) -> TestResponse {
        let mut request = Request::post(format!("/api/databases/{database}/versions"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        self.send(request.body(body.into()).expect("request")).await
    }

    /// A machine API read with a token.
    pub async fn get_with_token(&self, path: &str, token: &str) -> TestResponse {
        self.send(
            Request::get(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
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
        self.call(Method::POST, path, None, Some(body)).await
    }

    /// A request as the web app sends it, as `session` (or signed out): the
    /// CSRF header and our `Origin` on anything but a GET.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        session: Option<&Session>,
        body: Option<&Value>,
    ) -> TestResponse {
        let mut request = Request::builder().method(method.clone()).uri(path);
        if method != Method::GET {
            request = request
                .header(header::ORIGIN, ORIGIN)
                .header(csrf::HEADER, "1");
        }
        if let Some(session) = session {
            request = request.header(header::COOKIE, session.cookie());
        }
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        self.send(request.body(body).expect("request")).await
    }

    /// `alice.get("/api/me")` reads better than `call(GET, …, Some(&alice))`.
    pub fn as_user<'a>(&'a self, session: &'a Session) -> As<'a> {
        As { app: self, session }
    }
}

pub struct As<'a> {
    app: &'a TestApp,
    session: &'a Session,
}

impl As<'_> {
    pub async fn get(&self, path: &str) -> TestResponse {
        self.app
            .call(Method::GET, path, Some(self.session), None)
            .await
    }

    pub async fn post(&self, path: &str, body: &Value) -> TestResponse {
        self.app
            .call(Method::POST, path, Some(self.session), Some(body))
            .await
    }

    pub async fn post_empty(&self, path: &str) -> TestResponse {
        self.app
            .call(Method::POST, path, Some(self.session), None)
            .await
    }

    pub async fn patch(&self, path: &str, body: &Value) -> TestResponse {
        self.app
            .call(Method::PATCH, path, Some(self.session), Some(body))
            .await
    }

    pub async fn delete(&self, path: &str) -> TestResponse {
        self.app
            .call(Method::DELETE, path, Some(self.session), None)
            .await
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

    pub fn location(&self) -> &str {
        self.headers[header::LOCATION]
            .to_str()
            .expect("an ASCII Location")
    }

    /// The `Set-Cookie` for `name`, whole (value and attributes).
    pub fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
            .map(str::to_owned)
    }

    /// Just the value of the cookie `name` this response sets.
    pub fn cookie_value(&self, name: &str) -> Option<String> {
        let set = self.set_cookie(name)?;
        let pair = set.split(';').next()?;
        Some(pair.split_once('=')?.1.to_owned())
    }
}

/// A signed-in browser: the session cookie's value.
#[derive(Debug, Clone)]
pub struct Session {
    pub user: Uuid,
    pub token: String,
}

impl Session {
    pub fn cookie(&self) -> String {
        format!("{}={}", Config::for_tests().session_cookie, self.token)
    }
}

/// A session for `user`, as the sign-in would create it.
pub async fn session(db: &PgPool, user: Uuid) -> Session {
    let token = token::generate();
    sqlx::query!(
        "INSERT INTO sessions (id_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '7 days')",
        token::hash(&token),
        user,
    )
    .execute(db)
    .await
    .expect("insert session");
    Session { user, token }
}

/// A user and a session for them.
pub async fn signed_in(db: &PgPool, name: &str) -> Session {
    let id = user(db, name).await;
    session(db, id).await
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

/// An organization with one signed-in member per role, and someone signed
/// in who is not a member.
pub struct Team {
    pub org: Uuid,
    pub slug: String,
    pub admin: Session,
    pub developer: Session,
    pub reviewer: Session,
    pub outsider: Session,
}

impl Team {
    pub async fn new(db: &PgPool, slug: &str) -> Self {
        let org = org(db, slug).await;
        let mut sessions = Vec::new();
        for (who, role) in [
            ("Admin", Some(Role::Admin)),
            ("Developer", Some(Role::Developer)),
            ("Reviewer", Some(Role::Reviewer)),
            ("Outsider", None),
        ] {
            let id = user(db, &format!("{who}-{slug}")).await;
            if let Some(role) = role {
                member(db, org, id, role).await;
            }
            sessions.push(session(db, id).await);
        }
        let [admin, developer, reviewer, outsider] =
            <[Session; 4]>::try_from(sessions).expect("four sessions");
        Self {
            org,
            slug: slug.to_owned(),
            admin,
            developer,
            reviewer,
            outsider,
        }
    }

    /// `/api/orgs/<slug><rest>`.
    pub fn path(&self, rest: &str) -> String {
        format!("/api/orgs/{}{rest}", self.slug)
    }
}

/// An API token for `database`, as the API would create it; returns the secret.
pub async fn api_token(db: &PgPool, database: Uuid, scope: TokenScope) -> String {
    let secret = format!("rom_{}", token::generate());
    sqlx::query!(
        "INSERT INTO api_tokens (database_id, name, token_hash, scope) VALUES ($1, $2, $3, $4)",
        database,
        format!("{scope:?} token"),
        token::hash(&secret),
        scope as TokenScope,
    )
    .execute(db)
    .await
    .expect("insert API token");
    secret
}

/// A remus model with one table per name, each with a `bigint` id.
pub fn model(tables: &[&str]) -> Schema {
    let entities: Vec<Value> = tables
        .iter()
        .map(|name| {
            serde_json::json!({
                "schema": "public", "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null,
                "columns": [{
                    "name": "id", "type": "bigint", "type_schema": "pg_catalog",
                    "type_name": "int8", "type_kind": "b", "not_null": true,
                    "default": null, "identity": null, "generated": null, "comment": null
                }]
            })
        })
        .collect();
    Schema::from_json(
        &serde_json::json!({
            "remus_version": "0.1", "database": "app", "server_version": "17",
            "entities": entities
        })
        .to_string(),
    )
    .expect("a valid model")
}

/// [`model`] as a push body.
pub fn model_json(tables: &[&str]) -> String {
    serde_json::to_string(&model(tables)).expect("serialisable")
}

/// Stands in for the sandbox: `CREATE TABLE <name>` makes a table (see
/// [`model`]); the word `oops` is a syntax error located where it is.
pub fn fake_compiler() -> SqlCompiler {
    SqlCompiler::from_fn(|sql| {
        if let Some(at) = sql.find("oops") {
            return Compiled {
                model: None,
                diagnostics: vec![
                    Diagnostic::error(Code::SyntaxError, "syntax error at or near \"oops\"")
                        .with_span(LineIndex::new(sql).span(sql, at..at + 4)),
                ],
            };
        }
        let tables: Vec<&str> = sql
            .match_indices("CREATE TABLE ")
            .filter_map(|(at, _)| sql[at + 13..].split([' ', '(', ';']).next())
            .collect();
        let mut model = model(&tables);
        model.database = "sandbox".into();
        Compiled {
            model: Some(model),
            diagnostics: Vec::new(),
        }
    })
}
