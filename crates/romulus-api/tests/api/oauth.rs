//! OAuth for the MCP endpoint (`oauth`): discovery, client registration,
//! the authorization request and its consent, the token endpoint (PKCE,
//! codes once, refresh rotation and replays), revocation, connected apps,
//! and what an app's token may do at `/api/mcp`.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use romulus_api::{
    AppState, Config,
    domain::{AccessTokenScope, Role},
    mail::Mailer,
    oauth, token,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::harness::{ORIGIN, Session, TestApp, TestResponse, member, org, signed_in};

/// Claude Code's kind of redirect: a loopback one, on whatever port.
const LOOPBACK: &str = "http://localhost/callback";
/// claude.ai's.
const CLAUDE: &str = "https://claude.ai/api/mcp/auth_callback";
const RESOURCE: &str = "http://localhost:41000/api/mcp";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn query(pairs: &[(&str, &str)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        query.append_pair(name, value);
    }
    query.finish()
}

/// A query string's value, from a URL.
fn param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url)
        .or_else(|_| url::Url::parse(&format!("http://romulus.invalid{url}")))
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// A registration, as an MCP client sends it: JSON, no cookie, no CSRF
/// header.
async fn register(app: &TestApp, body: &Value) -> TestResponse {
    app.send(
        Request::post("/api/oauth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn client(app: &TestApp, name: &str, redirect: &str) -> String {
    let registered = register(
        app,
        &json!({
            "client_name": name,
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "none",
        }),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::CREATED,
        "{}",
        registered.json()
    );
    registered.json()["client_id"].as_str().unwrap().to_owned()
}

/// A form POST to a token-ish endpoint, as clients send it.
async fn form(app: &TestApp, path: &str, pairs: &[(&str, &str)]) -> TestResponse {
    app.send(
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(query(pairs)))
            .unwrap(),
    )
    .await
}

/// The authorization request a client sends the browser to.
fn authorize_url(
    client_id: &str,
    redirect: &str,
    verifier: &str,
    extra: &[(&str, &str)],
) -> String {
    let challenge = challenge(verifier);
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("state", "the-state"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("scope", "romulus.read romulus.write"),
        ("resource", RESOURCE),
    ];
    for (name, value) in extra {
        pairs.retain(|(key, _)| key != name);
        if !value.is_empty() {
            pairs.push((name, value));
        }
    }
    format!("/api/oauth/authorize?{}", query(&pairs))
}

/// A request the browser started: its id, and the cookie that binds it.
struct Started {
    id: Uuid,
    binding: String,
}

fn started(response: &TestResponse) -> Started {
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    let location = response.location();
    assert!(
        location.starts_with("/app/oauth/authorize?request="),
        "{location}"
    );
    let id: Uuid = param(location, "request").unwrap().parse().unwrap();
    let name = format!(
        "{}_oauth_{}",
        Config::for_tests().session_cookie,
        id.simple()
    );
    let value = response.cookie_value(&name).expect("the binding cookie");
    Started {
        id,
        binding: format!("{name}={value}"),
    }
}

/// A browser request: the session and the binding cookie, and what the web
/// app adds to anything but a GET.
async fn browser(
    app: &TestApp,
    method: Method,
    path: &str,
    cookies: &[&str],
    body: Option<&Value>,
) -> TestResponse {
    let mut request = Request::builder().method(method.clone()).uri(path);
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, cookies.join("; "));
    }
    if method != Method::GET {
        request = request
            .header(header::ORIGIN, ORIGIN)
            .header(romulus_api::csrf::HEADER, "1")
            .header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
    app.send(request.body(body).unwrap()).await
}

/// The person's decision on a request: where the browser goes.
async fn decide(app: &TestApp, who: &Session, request: &Started, decision: &Value) -> TestResponse {
    browser(
        app,
        Method::POST,
        &format!("/api/oauth/requests/{}", request.id),
        &[&who.cookie(), &request.binding],
        Some(decision),
    )
    .await
}

/// `who` approves `client_id` (asking for `extra`), up to the code.
async fn code_for(
    app: &TestApp,
    who: &Session,
    client_id: &str,
    verifier: &str,
    extra: &[(&str, &str)],
    decision: &Value,
) -> String {
    let request = started(
        &app.get(&authorize_url(client_id, LOOPBACK, verifier, extra))
            .await,
    );
    let decided = decide(app, who, &request, decision).await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.json());
    let url = decided.json()["redirect_url"].as_str().unwrap().to_owned();
    param(&url, "code").expect("a code")
}

fn exchange<'a>(client_id: &'a str, code: &'a str, verifier: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", LOOPBACK),
        ("resource", RESOURCE),
    ]
}

struct Tokens {
    client_id: String,
    access: String,
    refresh: String,
}

/// A whole connection of a client named `name` by `who`, with `decision`.
async fn connect(app: &TestApp, who: &Session, name: &str, decision: &Value) -> Tokens {
    let client_id = client(app, name, LOOPBACK).await;
    let verifier = token::generate();
    let code = code_for(app, who, &client_id, &verifier, &[], decision).await;
    let tokens = form(
        app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(tokens.status, StatusCode::OK, "{}", tokens.json());
    let body = tokens.json();
    Tokens {
        client_id,
        access: body["access_token"].as_str().unwrap().to_owned(),
        refresh: body["refresh_token"].as_str().unwrap().to_owned(),
    }
}

async fn refresh(app: &TestApp, tokens: &Tokens, refresh: &str) -> TestResponse {
    form(
        app,
        "/api/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &tokens.client_id),
            ("refresh_token", refresh),
        ],
    )
    .await
}

/// One MCP request with `token`.
async fn mcp(app: &TestApp, token: &str, method: &str, params: Value) -> TestResponse {
    app.send(
        Request::post("/api/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn ping(app: &TestApp, token: &str) -> StatusCode {
    mcp(app, token, "ping", json!({})).await.status
}

fn allow() -> Value {
    json!({ "allow": true })
}

/// An OAuth access token of `who`, as a connection would have made it (for
/// other modules' tests).
pub(super) async fn connected(db: &PgPool, who: &Session, scope: AccessTokenScope) -> String {
    let client = sqlx::query_scalar!(
        "INSERT INTO oauth_clients (name, redirect_uris) VALUES ('Test app', $1) RETURNING id",
        &[LOOPBACK.to_owned()],
    )
    .fetch_one(db)
    .await
    .unwrap();
    let grant = sqlx::query_scalar!(
        "INSERT INTO oauth_grants (client_id, user_id, scope, resource, expires_at, idle_expires_at)
         VALUES ($1, $2, $3, $4, now() + interval '90 days', now() + interval '30 days')
         RETURNING id",
        client,
        who.user,
        scope as AccessTokenScope,
        RESOURCE,
    )
    .fetch_one(db)
    .await
    .unwrap();
    let secret = format!("{}{}", oauth::ACCESS_PREFIX, token::generate());
    sqlx::query!(
        "INSERT INTO oauth_access_tokens (grant_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
        grant,
        token::hash(&secret),
    )
    .execute(db)
    .await
    .unwrap();
    secret
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_metadata_documents_say_where_to_sign_in_and_how(db: PgPool) {
    let app = TestApp::new(db);
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/api/mcp",
    ] {
        let document = app.get(path).await;
        assert_eq!(document.status, StatusCode::OK, "{path}");
        assert_eq!(document.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        let body = document.json();
        assert_eq!(body["resource"], RESOURCE);
        assert_eq!(
            body["authorization_servers"],
            json!(["http://localhost:41000"])
        );
        assert_eq!(
            body["scopes_supported"],
            json!(["romulus.read", "romulus.write"])
        );
        assert_eq!(body["bearer_methods_supported"], json!(["header"]));
    }

    let server = app.get("/.well-known/oauth-authorization-server").await;
    assert_eq!(server.status, StatusCode::OK);
    let body = server.json();
    assert_eq!(body["issuer"], "http://localhost:41000");
    for (field, path) in [
        ("authorization_endpoint", "authorize"),
        ("token_endpoint", "token"),
        ("registration_endpoint", "register"),
        ("revocation_endpoint", "revoke"),
    ] {
        assert_eq!(
            body[field],
            format!("http://localhost:41000/api/oauth/{path}")
        );
    }
    assert_eq!(body["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        body["token_endpoint_auth_methods_supported"],
        json!(["none"])
    );
    assert_eq!(body["response_types_supported"], json!(["code"]));
    assert_eq!(
        body["grant_types_supported"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(body["authorization_response_iss_parameter_supported"], true);
    // Dynamic registration, not client ID metadata documents.
    assert!(body.get("client_id_metadata_document_supported").is_none());

    // A client in a browser may call the open endpoints (no credentials).
    let preflight = app
        .send(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/oauth/token")
                .header(header::ORIGIN, "http://localhost:6274")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(preflight.status, StatusCode::NO_CONTENT);
    assert_eq!(preflight.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(
        !preflight
            .headers
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_mcp_endpoint_challenges_with_the_resource_metadata(db: PgPool) {
    let app = TestApp::new(db);
    let anonymous = app
        .send(
            Request::post("/api/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let challenge = anonymous.headers[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap();
    assert!(
        challenge.contains(
            "resource_metadata=\"http://localhost:41000/.well-known/oauth-protected-resource/api/mcp\""
        ),
        "{challenge}"
    );
    assert!(
        challenge.contains("scope=\"romulus.read romulus.write\""),
        "{challenge}"
    );
    // No token, no error (RFC 6750, 3.1): this is how connecting starts.
    assert!(!challenge.contains("error="), "{challenge}");

    let bad = format!("{}{}", oauth::ACCESS_PREFIX, token::generate());
    let refused = mcp(&app, &bad, "ping", json!({})).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let challenge = refused.headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.contains("error=\"invalid_token\""), "{challenge}");
    assert!(challenge.contains("resource_metadata="), "{challenge}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn clients_register_https_or_loopback_redirects_and_are_public(db: PgPool) {
    let app = TestApp::new(db.clone());
    let registered = register(
        &app,
        &json!({
            "client_name": "Claude",
            "redirect_uris": [CLAUDE, "http://127.0.0.1:33418/", LOOPBACK],
            "token_endpoint_auth_method": "client_secret_post",
            "logo_uri": "https://evil.test/pixel.png",
        }),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::CREATED,
        "{}",
        registered.json()
    );
    assert_eq!(registered.headers[header::CACHE_CONTROL], "no-store");
    let body = registered.json();
    let id: Uuid = body["client_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(body["client_name"], "Claude");
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert!(body.get("client_secret").is_none());
    assert_eq!(
        body["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert!(body["client_id_issued_at"].is_i64());
    let stored = sqlx::query_scalar!("SELECT redirect_uris FROM oauth_clients WHERE id = $1", id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 3);

    for uri in [
        "http://claude.ai/callback",
        "http://192.168.1.10/callback",
        "cursor://anysphere.cursor-mcp/oauth/callback",
        "javascript:alert(1)",
        "https://claude.ai/cb#fragment",
    ] {
        let refused = register(&app, &json!({ "redirect_uris": [uri] })).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(refused.json()["error"], "invalid_redirect_uri", "{uri}");
        assert!(refused.json()["error_description"].is_string());
    }
    let none = register(&app, &json!({ "client_name": "x" })).await;
    assert_eq!(none.json()["error"], "invalid_redirect_uri");
    let grant = register(
        &app,
        &json!({ "redirect_uris": [CLAUDE], "grant_types": ["client_credentials"] }),
    )
    .await;
    assert_eq!(grant.json()["error"], "invalid_client_metadata");
    let garbage = app
        .send(
            Request::post("/api/oauth/register")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST);
    assert_eq!(garbage.json()["error"], "invalid_client_metadata");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn registrations_are_limited_per_client_address(db: PgPool) {
    let app = TestApp::new(db);
    let body = json!({ "client_name": "Loop", "redirect_uris": [LOOPBACK] });
    let mut created = 0;
    let limited = loop {
        let answered = register(&app, &body).await;
        if answered.status != StatusCode::CREATED {
            break answered;
        }
        created += 1;
        assert!(created <= 40, "registrations without end");
    };
    assert!(created >= 30, "a burst first: {created}");
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers.contains_key(header::RETRY_AFTER));
    assert_eq!(limited.json()["error"], "rate_limited");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_person_signs_in_approves_and_their_agent_works_as_them(db: PgPool) {
    let (mailer, outbox) = Mailer::memory();
    let app =
        TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_mailer(mailer));
    let alice = signed_in(&db, "Alice").await;
    let acme = org(&db, "acme").await;
    member(&db, acme, alice.user, Role::Admin).await;
    let client_id = client(&app, "Claude Code (romulus)", LOOPBACK).await;
    let verifier = token::generate();

    // The agent opens the browser: signed out or not, the request is kept
    // and the browser goes to the consent page, bound to it by a cookie.
    let start = app
        .get(&authorize_url(
            &client_id,
            "http://localhost:3118/callback",
            &verifier,
            &[],
        ))
        .await;
    let request = started(&start);
    let set = start
        .set_cookie(request.binding.split('=').next().unwrap())
        .unwrap();
    assert!(
        set.contains("HttpOnly") && set.contains("Path=/api/oauth"),
        "{set}"
    );

    // The consent page needs the person, and this browser.
    let path = format!("/api/oauth/requests/{}", request.id);
    let signed_out = browser(&app, Method::GET, &path, &[&request.binding], None).await;
    assert_eq!(signed_out.status, StatusCode::UNAUTHORIZED);
    let elsewhere = browser(&app, Method::GET, &path, &[&alice.cookie()], None).await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);
    let shown = browser(
        &app,
        Method::GET,
        &path,
        &[&alice.cookie(), &request.binding],
        None,
    )
    .await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.json());
    let body = shown.json();
    assert_eq!(body["client"]["name"], "Claude Code (romulus)");
    assert_eq!(body["client"]["redirect_host"], "localhost");
    assert_eq!(body["client"]["loopback"], true);
    assert_eq!(body["scope"], "write");

    // Someone else's browser, with their own session, cannot answer it.
    let bob = signed_in(&db, "Bob").await;
    let hijack = browser(&app, Method::POST, &path, &[&bob.cookie()], Some(&allow())).await;
    assert_eq!(hijack.status, StatusCode::NOT_FOUND);

    let decided = decide(&app, &alice, &request, &allow()).await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.json());
    let url = decided.json()["redirect_url"].as_str().unwrap().to_owned();
    assert!(
        url.starts_with("http://localhost:3118/callback?code="),
        "{url}"
    );
    assert_eq!(param(&url, "state").as_deref(), Some("the-state"));
    assert_eq!(
        param(&url, "iss").as_deref(),
        Some("http://localhost:41000")
    );
    // Answered once.
    assert_eq!(
        decide(&app, &alice, &request, &allow()).await.status,
        StatusCode::NOT_FOUND
    );

    let code = param(&url, "code").unwrap();
    let mut pairs = exchange(&client_id, &code, &verifier);
    pairs.retain(|(name, _)| *name != "redirect_uri");
    pairs.push(("redirect_uri", "http://localhost:3118/callback"));
    let tokens = form(&app, "/api/oauth/token", &pairs).await;
    assert_eq!(tokens.status, StatusCode::OK, "{}", tokens.json());
    assert_eq!(tokens.headers[header::CACHE_CONTROL], "no-store");
    let body = tokens.json();
    let access = body["access_token"].as_str().unwrap();
    assert!(access.starts_with("rmls_oat_"), "{access}");
    assert!(
        body["refresh_token"]
            .as_str()
            .unwrap()
            .starts_with("rmls_ort_")
    );
    assert_eq!(body["token_type"], "Bearer");
    assert!((3590..=3600).contains(&body["expires_in"].as_i64().unwrap()));
    assert_eq!(body["scope"], "romulus.read romulus.write");

    // The agent is Alice, through the app.
    let initialize = mcp(
        &app,
        access,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "2.1.4" },
        }),
    )
    .await;
    assert_eq!(initialize.status, StatusCode::OK);
    let whoami = mcp(
        &app,
        access,
        "tools/call",
        json!({ "name": "whoami", "arguments": {} }),
    )
    .await
    .json();
    let text = whoami["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("through the connected app \"Claude Code (romulus)\" (read-write)"),
        "{text}"
    );
    assert_eq!(
        whoami["result"]["structuredContent"]["token"]["kind"],
        "app"
    );

    // A write is in the Activity with the app's name.
    let created = mcp(
        &app,
        access,
        "tools/call",
        json!({ "name": "create_database", "arguments": { "org": "acme", "name": "Billing" } }),
    )
    .await
    .json();
    assert_eq!(created["result"]["isError"], false, "{created}");
    let audited = sqlx::query_scalar!(
        r#"SELECT details->>'app_name' AS "app?" FROM audit_events
           WHERE org_id = $1 AND action = 'agent_action'"#,
        acme
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(audited, [Some("Claude Code (romulus)".to_owned())]);

    // In the person's connected apps, with the client that used it.
    let apps = app.as_user(&alice).get("/api/me/connected-apps").await;
    assert_eq!(apps.status, StatusCode::OK);
    let apps = apps.json();
    assert_eq!(apps.as_array().unwrap().len(), 1);
    assert_eq!(apps[0]["client_id"], client_id.as_str());
    assert_eq!(apps[0]["name"], "Claude Code (romulus)");
    assert_eq!(apps[0]["scope"], "write");
    assert_eq!(apps[0]["redirect_host"], "localhost");
    assert!(apps[0]["last_used_at"].is_string());
    assert_eq!(apps[0]["last_client"], "claude-code 2.1.4");
    assert_eq!(
        app.as_user(&bob).get("/api/me/connected-apps").await.json(),
        json!([])
    );

    // And told by email.
    let mut notice = None;
    for _ in 0..100 {
        if let Some(sent) = outbox.to("alice@romulus.test").last() {
            notice = Some(sent.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let notice = notice.expect("an email about the new app");
    assert_eq!(notice.kind, "app_connected");
    assert!(
        notice
            .text
            .contains("\"Claude Code (romulus)\" was just connected")
    );
    assert!(notice.text.contains("/app/settings/apps"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn denying_sends_access_denied_back_to_the_app(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let client_id = client(&app, "Cursor", LOOPBACK).await;
    let request = started(
        &app.get(&authorize_url(
            &client_id,
            LOOPBACK,
            &token::generate(),
            &[],
        ))
        .await,
    );
    let denied = decide(&app, &alice, &request, &json!({ "allow": false })).await;
    assert_eq!(denied.status, StatusCode::OK);
    let url = denied.json()["redirect_url"].as_str().unwrap().to_owned();
    assert_eq!(param(&url, "error").as_deref(), Some("access_denied"));
    assert_eq!(param(&url, "state").as_deref(), Some("the-state"));
    assert!(param(&url, "iss").is_some());
    assert!(param(&url, "code").is_none());
    assert_eq!(
        sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM oauth_codes"#)
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );

    // Allowing more than was asked is refused.
    let reader = started(
        &app.get(&authorize_url(
            &client_id,
            LOOPBACK,
            &token::generate(),
            &[("scope", "romulus.read")],
        ))
        .await,
    );
    let wider = decide(
        &app,
        &alice,
        &reader,
        &json!({ "allow": true, "scope": "write" }),
    )
    .await;
    assert_eq!(wider.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn state_is_optional_and_echoed_only_when_sent(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let client_id = client(&app, "No state", LOOPBACK).await;
    let verifier = token::generate();

    // Without `state`, PKCE alone protects the flow: approved, and the way
    // back carries the code and `iss`, no `state`.
    let request = started(
        &app.get(&authorize_url(
            &client_id,
            LOOPBACK,
            &verifier,
            &[("state", "")],
        ))
        .await,
    );
    let decided = decide(&app, &alice, &request, &allow()).await;
    assert_eq!(decided.status, StatusCode::OK, "{}", decided.json());
    let url = decided.json()["redirect_url"].as_str().unwrap().to_owned();
    assert!(!url.contains("state="), "{url}");
    assert!(param(&url, "iss").is_some());
    let code = param(&url, "code").expect("a code");
    let tokens = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(tokens.status, StatusCode::OK, "{}", tokens.json());

    // Denied or refused, the same: no `state` invented.
    let request = started(
        &app.get(&authorize_url(
            &client_id,
            LOOPBACK,
            &verifier,
            &[("state", "")],
        ))
        .await,
    );
    let denied = decide(&app, &alice, &request, &json!({ "allow": false })).await;
    let url = denied.json()["redirect_url"].as_str().unwrap().to_owned();
    assert_eq!(param(&url, "error").as_deref(), Some("access_denied"));
    assert!(!url.contains("state="), "{url}");
    let refused = app
        .get(&authorize_url(
            &client_id,
            LOOPBACK,
            &verifier,
            &[("state", ""), ("code_challenge_method", "plain")],
        ))
        .await;
    assert_eq!(
        param(refused.location(), "error").as_deref(),
        Some("invalid_request")
    );
    assert!(!refused.location().contains("state="));

    // Sent, it comes back exactly as it was, characters and all.
    let odd = "a b+c/d=é&x";
    let request = started(
        &app.get(&authorize_url(
            &client_id,
            LOOPBACK,
            &verifier,
            &[("state", odd)],
        ))
        .await,
    );
    let decided = decide(&app, &alice, &request, &allow()).await;
    let url = decided.json()["redirect_url"].as_str().unwrap().to_owned();
    assert_eq!(param(&url, "state").as_deref(), Some(odd));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_authorization_request_is_refused_where_it_cannot_be_trusted(db: PgPool) {
    let app = TestApp::new(db.clone());
    let client_id = client(&app, "Loop", LOOPBACK).await;
    let verifier = token::generate();

    // Unknown client, unregistered redirect: never sent anywhere but home.
    for (url, code) in [
        (
            authorize_url(&Uuid::new_v4().to_string(), LOOPBACK, &verifier, &[]),
            "unknown_client",
        ),
        (
            authorize_url("not-a-uuid", LOOPBACK, &verifier, &[]),
            "unknown_client",
        ),
        (
            authorize_url(&client_id, "https://evil.test/callback", &verifier, &[]),
            "invalid_redirect_uri",
        ),
        (
            authorize_url(&client_id, "http://localhost:9/other", &verifier, &[]),
            "invalid_redirect_uri",
        ),
        (
            format!(
                "{}&client_id={client_id}",
                authorize_url(&client_id, LOOPBACK, &verifier, &[])
            ),
            "invalid_request",
        ),
    ] {
        let refused = app.get(&url).await;
        assert_eq!(refused.status, StatusCode::SEE_OTHER, "{url}");
        assert_eq!(
            refused.location(),
            format!("/app/oauth/authorize?error={code}"),
            "{url}"
        );
    }

    // The rest goes back to the client, with the reason.
    let long_state = "s".repeat(1025);
    for (extra, error) in [
        (vec![("state", long_state.as_str())], "invalid_request"),
        (vec![("code_challenge_method", "plain")], "invalid_request"),
        (vec![("code_challenge_method", "")], "invalid_request"),
        (vec![("code_challenge", "")], "invalid_request"),
        (vec![("code_challenge", "short")], "invalid_request"),
        (
            vec![("response_type", "token")],
            "unsupported_response_type",
        ),
        (vec![("scope", "romulus.read admin")], "invalid_scope"),
        (
            vec![("resource", "https://other.test/mcp")],
            "invalid_target",
        ),
        (
            vec![("resource", "http://localhost:41000/")],
            "invalid_target",
        ),
    ] {
        let refused = app
            .get(&authorize_url(&client_id, LOOPBACK, &verifier, &extra))
            .await;
        assert_eq!(refused.status, StatusCode::SEE_OTHER, "{extra:?}");
        let location = refused.location();
        assert!(location.starts_with(LOOPBACK), "{location}");
        assert_eq!(
            param(location, "error").as_deref(),
            Some(error),
            "{extra:?}"
        );
        assert!(param(location, "iss").is_some());
    }

    // A loopback redirect is the registered one on any port; no resource
    // and no scope are fine (the MCP endpoint, everything).
    let any_port = app
        .get(&authorize_url(
            &client_id,
            "http://localhost:51234/callback",
            &verifier,
            &[("resource", ""), ("scope", "")],
        ))
        .await;
    started(&any_port);
    assert_eq!(
        sqlx::query_scalar!(r#"SELECT scope AS "scope: AccessTokenScope" FROM oauth_requests"#)
            .fetch_one(&db)
            .await
            .unwrap(),
        AccessTokenScope::Write
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_code_needs_its_pkce_verifier_and_works_once_within_a_minute(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let client_id = client(&app, "Loop", LOOPBACK).await;
    let verifier = token::generate();

    // No verifier: refused, and the code is still good.
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    let mut without = exchange(&client_id, &code, &verifier);
    without.retain(|(name, _)| *name != "code_verifier");
    let missing = form(&app, "/api/oauth/token", &without).await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.json()["error"], "invalid_request");
    // The wrong one, or the challenge itself (`plain`): refused, and the
    // code is spent.
    let wrong = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &challenge(&verifier)),
    )
    .await;
    assert_eq!(wrong.json()["error"], "invalid_grant");
    let then = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(then.json()["error"], "invalid_grant");

    // Exchanged once; a second time revokes what the first gave.
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    let first = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    let access = first.json()["access_token"].as_str().unwrap().to_owned();
    assert_eq!(ping(&app, &access).await, StatusCode::OK);
    let again = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(again.json()["error"], "invalid_grant");
    assert_eq!(ping(&app, &access).await, StatusCode::UNAUTHORIZED);

    // Another client, another redirect URI, another resource: refused.
    let other = client(&app, "Other", LOOPBACK).await;
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    let stolen = form(
        &app,
        "/api/oauth/token",
        &exchange(&other, &code, &verifier),
    )
    .await;
    assert_eq!(stolen.json()["error"], "invalid_grant");
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    let mut moved = exchange(&client_id, &code, &verifier);
    moved.retain(|(name, _)| *name != "redirect_uri");
    moved.push(("redirect_uri", "http://localhost:9/callback"));
    assert_eq!(
        form(&app, "/api/oauth/token", &moved).await.json()["error"],
        "invalid_grant"
    );
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    let mut elsewhere = exchange(&client_id, &code, &verifier);
    elsewhere.retain(|(name, _)| *name != "resource");
    elsewhere.push(("resource", "https://other.test/mcp"));
    assert_eq!(
        form(&app, "/api/oauth/token", &elsewhere).await.json()["error"],
        "invalid_target"
    );

    // A minute later: expired.
    let code = code_for(&app, &alice, &client_id, &verifier, &[], &allow()).await;
    sqlx::query!(
        "UPDATE oauth_codes SET created_at = now() - interval '61 seconds' WHERE code_hash = $1",
        token::hash(&code)
    )
    .execute(&db)
    .await
    .unwrap();
    let late = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(late.json()["error"], "invalid_grant");
    assert!(
        late.json()["error_description"]
            .as_str()
            .unwrap()
            .contains("expired")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_token_endpoint_answers_in_oauth_s_terms(db: PgPool) {
    let app = TestApp::new(db.clone());
    let client_id = client(&app, "Loop", LOOPBACK).await;

    let json_body = app
        .send(
            Request::post("/api/oauth/token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "grant_type": "refresh_token" }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(json_body.status, StatusCode::BAD_REQUEST);
    assert_eq!(json_body.json()["error"], "invalid_request");

    let unknown = form(
        &app,
        "/api/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &Uuid::new_v4().to_string()),
            ("refresh_token", "x"),
        ],
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.json()["error"], "invalid_client");

    let grant = form(
        &app,
        "/api/oauth/token",
        &[
            ("grant_type", "client_credentials"),
            ("client_id", &client_id),
        ],
    )
    .await;
    assert_eq!(grant.json()["error"], "unsupported_grant_type");
    let twice = form(
        &app,
        "/api/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
        ],
    )
    .await;
    assert_eq!(twice.json()["error"], "invalid_request");

    // A client failing again and again is refused before anything is read.
    let mut refused = 0;
    let cut_off = loop {
        let answered = form(
            &app,
            "/api/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", "guess"),
                ("code_verifier", &token::generate()),
            ],
        )
        .await;
        if answered.status != StatusCode::BAD_REQUEST {
            break answered;
        }
        refused += 1;
        assert!(refused <= 40, "guesses without end");
    };
    assert!(refused >= 18, "a burst first: {refused}");
    assert_eq!(cut_off.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(cut_off.headers.contains_key(header::RETRY_AFTER));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn refresh_tokens_rotate_and_a_replay_ends_the_connection(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let tokens = connect(&app, &alice, "Claude", &allow()).await;

    let rotated = refresh(&app, &tokens, &tokens.refresh).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.json());
    let next = rotated.json();
    let (access, refresh_token) = (
        next["access_token"].as_str().unwrap().to_owned(),
        next["refresh_token"].as_str().unwrap().to_owned(),
    );
    assert_ne!(refresh_token, tokens.refresh);
    assert_eq!(next["scope"], "romulus.read romulus.write");
    assert_eq!(ping(&app, &access).await, StatusCode::OK);
    // Widening on refresh is refused; the same scope is fine.
    let same = form(
        &app,
        "/api/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &tokens.client_id),
            ("refresh_token", &refresh_token),
            ("scope", "romulus.write"),
            ("resource", RESOURCE),
        ],
    )
    .await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.json());
    let latest = same.json()["refresh_token"].as_str().unwrap().to_owned();

    // The old refresh token again: a replay. Every token of the connection
    // stops working.
    let replay = refresh(&app, &tokens, &tokens.refresh).await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST);
    assert_eq!(replay.json()["error"], "invalid_grant");
    assert_eq!(ping(&app, &access).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        refresh(&app, &tokens, &latest).await.json()["error"],
        "invalid_grant"
    );
    assert_eq!(
        app.as_user(&alice)
            .get("/api/me/connected-apps")
            .await
            .json(),
        json!([])
    );

    // A read-only connection cannot widen itself.
    let reader = connect(
        &app,
        &alice,
        "Reader",
        &json!({ "allow": true, "scope": "read" }),
    )
    .await;
    let wider = form(
        &app,
        "/api/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &reader.client_id),
            ("refresh_token", &reader.refresh),
            ("scope", "romulus.read romulus.write"),
        ],
    )
    .await;
    assert_eq!(wider.json()["error"], "invalid_scope");

    // Idle for 30 days (its refresh token expired): over.
    let idle = connect(&app, &alice, "Idle", &allow()).await;
    sqlx::query!(
        "UPDATE oauth_refresh_tokens SET expires_at = now() - interval '1 second',
                created_at = now() - interval '31 days'
         WHERE token_hash = $1",
        token::hash(&idle.refresh)
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(
        refresh(&app, &idle, &idle.refresh).await.json()["error"],
        "invalid_grant"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn access_tokens_expire_and_belong_to_the_mcp_endpoint(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let tokens = connect(&app, &alice, "Claude", &allow()).await;
    assert_eq!(ping(&app, &tokens.access).await, StatusCode::OK);

    // Minted for another resource (the public URL changed since): refused.
    sqlx::query!("UPDATE oauth_grants SET resource = 'https://old.example/api/mcp'")
        .execute(&db)
        .await
        .unwrap();
    let elsewhere = mcp(&app, &tokens.access, "ping", json!({})).await;
    assert_eq!(elsewhere.status, StatusCode::UNAUTHORIZED);
    assert!(
        elsewhere.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another resource")
    );
    sqlx::query!("UPDATE oauth_grants SET resource = $1", RESOURCE)
        .execute(&db)
        .await
        .unwrap();

    // An hour on: expired, and the client is told to refresh.
    sqlx::query!(
        "UPDATE oauth_access_tokens SET expires_at = now() - interval '1 second',
                created_at = now() - interval '2 hours'"
    )
    .execute(&db)
    .await
    .unwrap();
    let expired = mcp(&app, &tokens.access, "ping", json!({})).await;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    let challenge = expired.headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.contains("error=\"invalid_token\""), "{challenge}");
    assert!(challenge.contains("expired"), "{challenge}");

    // An app's token opens nothing but the MCP endpoint.
    let me = app.get_with_token("/api/me", &tokens.access).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_read_only_app_reads_and_is_asked_to_step_up_for_writes(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    // Asked for everything, allowed to read.
    let client_id = client(&app, "Claude", LOOPBACK).await;
    let verifier = token::generate();
    let code = code_for(
        &app,
        &alice,
        &client_id,
        &verifier,
        &[],
        &json!({ "allow": true, "scope": "read" }),
    )
    .await;
    let tokens = form(
        &app,
        "/api/oauth/token",
        &exchange(&client_id, &code, &verifier),
    )
    .await;
    assert_eq!(tokens.json()["scope"], "romulus.read");
    let access = tokens.json()["access_token"].as_str().unwrap().to_owned();

    let listed = mcp(&app, &access, "tools/list", json!({})).await.json();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"get_schema"), "{names:?}");
    assert!(!names.contains(&"create_draft"), "{names:?}");

    let write = mcp(
        &app,
        &access,
        "tools/call",
        json!({ "name": "create_database", "arguments": { "name": "Billing" } }),
    )
    .await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);
    let challenge = write.headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(
        challenge.contains("error=\"insufficient_scope\""),
        "{challenge}"
    );
    assert!(
        challenge.contains("scope=\"romulus.read romulus.write\""),
        "{challenge}"
    );
    assert!(challenge.contains("resource_metadata="), "{challenge}");
    assert_eq!(write.json()["id"], 1);

    // Reading tools answer as usual.
    let whoami = mcp(
        &app,
        &access,
        "tools/call",
        json!({ "name": "whoami", "arguments": {} }),
    )
    .await;
    assert_eq!(whoami.status, StatusCode::OK);
    assert!(
        whoami.json()["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("(read-only)")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn revoking_a_token_ends_it_or_its_whole_connection(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let tokens = connect(&app, &alice, "Claude", &allow()).await;

    // An access token: itself.
    let revoked = form(
        &app,
        "/api/oauth/revoke",
        &[
            ("token", &tokens.access),
            ("token_type_hint", "access_token"),
            ("client_id", &tokens.client_id),
        ],
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert_eq!(ping(&app, &tokens.access).await, StatusCode::UNAUTHORIZED);
    let fresh = refresh(&app, &tokens, &tokens.refresh).await;
    assert_eq!(fresh.status, StatusCode::OK);
    let access = fresh.json()["access_token"].as_str().unwrap().to_owned();
    let refresh_token = fresh.json()["refresh_token"].as_str().unwrap().to_owned();

    // Another client's word revokes nothing.
    let other = client(&app, "Other", LOOPBACK).await;
    form(
        &app,
        "/api/oauth/revoke",
        &[("token", &refresh_token), ("client_id", &other)],
    )
    .await;
    assert_eq!(ping(&app, &access).await, StatusCode::OK);

    // A refresh token: the whole connection.
    let ended = form(&app, "/api/oauth/revoke", &[("token", &refresh_token)]).await;
    assert_eq!(ended.status, StatusCode::OK);
    assert_eq!(ping(&app, &access).await, StatusCode::UNAUTHORIZED);

    // Unknown tokens are fine; no token is not.
    let unknown = form(&app, "/api/oauth/revoke", &[("token", "whatever")]).await;
    assert_eq!(unknown.status, StatusCode::OK);
    let none = form(&app, "/api/oauth/revoke", &[]).await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    assert_eq!(none.json()["error"], "invalid_request");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn disconnecting_an_app_cuts_off_every_token_it_holds(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let bob = signed_in(&db, "Bob").await;
    let claude = connect(&app, &alice, "Claude", &allow()).await;
    let cursor = connect(
        &app,
        &alice,
        "Cursor",
        &json!({ "allow": true, "scope": "read" }),
    )
    .await;
    let apps = app
        .as_user(&alice)
        .get("/api/me/connected-apps")
        .await
        .json();
    assert_eq!(apps.as_array().unwrap().len(), 2);
    let cursor_row = apps
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "Cursor")
        .unwrap();
    assert_eq!(cursor_row["scope"], "read");

    // Not Bob's to disconnect.
    let path = format!("/api/me/connected-apps/{}", claude.client_id);
    assert_eq!(
        app.as_user(&bob).delete(&path).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(ping(&app, &claude.access).await, StatusCode::OK);

    let gone = app.as_user(&alice).delete(&path).await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    assert_eq!(ping(&app, &claude.access).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        refresh(&app, &claude, &claude.refresh).await.json()["error"],
        "invalid_grant"
    );
    // The other app is untouched.
    assert_eq!(ping(&app, &cursor.access).await, StatusCode::OK);
    let left = app
        .as_user(&alice)
        .get("/api/me/connected-apps")
        .await
        .json();
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "Cursor");
    // Twice is not found.
    assert_eq!(
        app.as_user(&alice).delete(&path).await.status,
        StatusCode::NOT_FOUND
    );
    // Signed out, nothing.
    assert_eq!(
        app.get("/api/me/connected-apps").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_sweeper_forgets_what_can_no_longer_be_used(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let kept = connect(&app, &alice, "Kept", &allow()).await;
    let unused = client(&app, "Never used", LOOPBACK).await;
    let ended = connect(&app, &alice, "Ended", &allow()).await;
    app.get(&authorize_url(&unused, LOOPBACK, &token::generate(), &[]))
        .await;

    sqlx::query!(
        "UPDATE oauth_clients SET created_at = now() - interval '31 days', last_used_at = NULL
         WHERE id = $1",
        unused.parse::<Uuid>().unwrap()
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!("UPDATE oauth_requests SET created_at = now() - interval '21 minutes'")
        .execute(&db)
        .await
        .unwrap();
    // A connection revoked 31 days ago, its client unused since.
    let ended_id: Uuid = ended.client_id.parse().unwrap();
    sqlx::query!(
        "UPDATE oauth_grants SET revoked_at = now() - interval '31 days' WHERE client_id = $1",
        ended_id
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE oauth_clients SET last_used_at = now() - interval '31 days' WHERE id = $1",
        ended_id
    )
    .execute(&db)
    .await
    .unwrap();

    let swept = oauth::apps::sweep(&db).await.unwrap();
    assert_eq!(swept.requests, 1, "{swept:?}");
    assert_eq!(swept.grants, 1, "{swept:?}");
    // The unused one now; the ended one, its grant gone, too.
    assert_eq!(swept.clients, 2, "{swept:?}");
    let left = sqlx::query_scalar!("SELECT name FROM oauth_clients ORDER BY created_at")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(left, [Some("Kept".to_owned())]);
    assert_eq!(ping(&app, &kept.access).await, StatusCode::OK);
    // An unknown client is told so: it registers again.
    let refused = refresh(&app, &ended, &ended.refresh).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.json()["error"], "invalid_client");
}
