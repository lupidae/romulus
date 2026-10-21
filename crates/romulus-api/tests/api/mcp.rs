//! The MCP endpoint (`mcp`): the protocol in both eras, who gets in (a
//! personal access token, never the cookie; romulus's own `Origin`), and
//! every tool for each role, through the same handlers as the web app.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use chrono::Utc;
use romulus_api::{billing::hours, domain::AccessTokenScope, token};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    billing,
    harness::{ORIGIN, Session, Team, TestApp, TestResponse, database, member, org, user},
    live::{Client, serve, version},
    migration_tests, playgrounds,
};

/// A personal access token of `who`, as the API would create it.
pub(super) async fn pat(db: &PgPool, who: &Session, scope: AccessTokenScope) -> String {
    let secret = format!("rmls_pat_{}", token::generate());
    sqlx::query!(
        "INSERT INTO personal_access_tokens (user_id, name, token_hash, scope)
         VALUES ($1, $2, $3, $4)",
        who.user,
        format!("{scope:?} agent"),
        token::hash(&secret),
        scope as AccessTokenScope,
    )
    .execute(db)
    .await
    .expect("insert access token");
    secret
}

/// An MCP client with a token.
struct Mcp<'a> {
    app: &'a TestApp,
    token: String,
}

impl<'a> Mcp<'a> {
    fn new(app: &'a TestApp, token: String) -> Self {
        Self { app, token }
    }

    async fn post(&self, body: &Value, headers: &[(&str, &str)]) -> TestResponse {
        let mut request = Request::post("/api/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        self.app
            .send(request.body(Body::from(body.to_string())).expect("request"))
            .await
    }

    /// A legacy-era request: the whole JSON-RPC response, which must be 200.
    async fn rpc(&self, method: &str, params: Value) -> Value {
        let response = self
            .post(
                &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }),
                &[],
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        let body = response.json();
        assert_eq!(body["id"], 7);
        body
    }

    /// A tool's text, and whether it is an error.
    async fn tool(&self, name: &str, arguments: Value) -> (String, bool) {
        let body = self
            .rpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        let result = &body["result"];
        assert!(result.is_object(), "{body}");
        (
            result["content"][0]["text"]
                .as_str()
                .expect("text content")
                .to_owned(),
            result["isError"] == true,
        )
    }

    async fn ok(&self, name: &str, arguments: Value) -> String {
        let (text, error) = self.tool(name, arguments).await;
        assert!(!error, "{name} failed: {text}");
        text
    }

    async fn refused(&self, name: &str, arguments: Value) -> String {
        let (text, error) = self.tool(name, arguments).await;
        assert!(error, "{name} should have been refused: {text}");
        text
    }
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "modern-client", "version": "9" },
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

/// The id in `text` after `prefix` (`Created draft <id>`).
fn id_after(text: &str, prefix: &str) -> Uuid {
    let at = text
        .find(prefix)
        .unwrap_or_else(|| panic!("{prefix:?} in {text}"))
        + prefix.len();
    text[at..at + 36].parse().expect("an id")
}

async fn audited(db: &PgPool) -> Vec<Value> {
    sqlx::query_scalar!(
        r#"SELECT details AS "details!" FROM audit_events WHERE action = 'agent_action' ORDER BY id"#
    )
    .fetch_all(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_handshake_is_stateless_and_both_eras_are_answered(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );

    let init = mcp
        .post(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": { "name": "claude-code", "version": "2.1" } },
            }),
            &[],
        )
        .await;
    assert_eq!(init.status, StatusCode::OK);
    assert!(init.headers.get("mcp-session-id").is_none(), "no session");
    let result = &init.json()["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "romulus");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["instructions"].as_str().unwrap().contains("whoami"));
    // An older version than romulus speaks: its newest legacy one.
    let old = mcp
        .rpc("initialize", json!({ "protocolVersion": "2024-11-05" }))
        .await;
    assert_eq!(old["result"]["protocolVersion"], "2025-11-25");

    // Notifications are accepted, with no body.
    let notified = mcp
        .post(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            &[],
        )
        .await;
    assert_eq!(notified.status, StatusCode::ACCEPTED);
    assert!(notified.body.is_empty());
    assert_eq!(mcp.rpc("ping", json!({})).await["result"], json!({}));

    // Legacy lists, without the modern fields.
    let tools = mcp.rpc("tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "whoami",
        "list_databases",
        "get_database",
        "get_schema",
        "list_versions",
        "diff_versions",
        "search_schema",
        "create_draft",
        "update_draft",
        "get_draft",
        "generate_migration",
        "confirm_renames",
        "test_migration",
        "get_migration_test",
        "request_review",
        "publish_draft",
        "create_playground",
        "playground_query",
        "list_playgrounds",
        "create_database",
        "push_schema",
        "ci_setup",
    ] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    // CI secrets are made by people, in the web app.
    assert!(!names.contains(&"create_push_token"), "{names:?}");
    assert!(tools["result"].get("resultType").is_none());
    let create = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "create_draft")
        .unwrap();
    assert_eq!(create["annotations"]["readOnlyHint"], false);
    assert!(
        create["description"]
            .as_str()
            .unwrap()
            .starts_with("Changes shared state")
    );

    // Modern: per-request metadata, headers mirroring the body.
    let discover = mcp
        .post(
            &json!({ "jsonrpc": "2.0", "id": "d", "method": "server/discover",
                     "params": { "_meta": modern_meta() } }),
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "server/discover"),
            ],
        )
        .await;
    assert_eq!(discover.status, StatusCode::OK, "{}", discover.json());
    let result = &discover.json()["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"][0], "2026-07-28");
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "romulus"
    );
    let call = mcp
        .post(
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": { "name": "whoami", "arguments": {}, "_meta": modern_meta() } }),
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "whoami"),
            ],
        )
        .await;
    assert_eq!(call.status, StatusCode::OK);
    assert_eq!(call.json()["result"]["resultType"], "complete");
    assert_eq!(call.json()["result"]["isError"], false);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn modern_headers_must_match_and_versions_be_known(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let mcp = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list",
                       "params": { "_meta": modern_meta() } });

    let missing = mcp
        .post(&list, &[("mcp-protocol-version", "2026-07-28")])
        .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.json()["error"]["code"], -32020);

    let mismatched = mcp
        .post(
            &list,
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "prompts/list"),
            ],
        )
        .await;
    assert_eq!(mismatched.json()["error"]["code"], -32020);

    let mut future = list.clone();
    future["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    let unsupported = mcp
        .post(
            &future,
            &[
                ("mcp-protocol-version", "2099-01-01"),
                ("mcp-method", "tools/list"),
            ],
        )
        .await;
    assert_eq!(unsupported.status, StatusCode::BAD_REQUEST);
    let error = &unsupported.json()["error"];
    assert_eq!(error["code"], -32022);
    assert!(
        error["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2025-06-18"))
    );

    // An unknown method: 404 for a modern client, a plain error for others.
    let unknown = json!({ "jsonrpc": "2.0", "id": 2, "method": "nope/nothing",
                          "params": { "_meta": modern_meta() } });
    let modern = mcp
        .post(
            &unknown,
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "nope/nothing"),
            ],
        )
        .await;
    assert_eq!(modern.status, StatusCode::NOT_FOUND);
    assert_eq!(modern.json()["error"]["code"], -32601);
    let legacy = mcp.rpc("nope/nothing", json!({})).await;
    assert_eq!(legacy["error"]["code"], -32601);

    // A legacy request naming a version romulus does not speak.
    let refused = mcp
        .post(
            &json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
            &[("mcp-protocol-version", "1999-01-01")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn malformed_messages_batches_and_other_methods_are_refused(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let token = pat(&db, &alice, AccessTokenScope::Read).await;
    let mcp = Mcp::new(&app, token.clone());

    let send_raw = |body: &'static str| {
        Request::post("/api/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("request")
    };
    let garbage = app.send(send_raw("{ not json")).await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST);
    assert_eq!(garbage.json()["error"]["code"], -32700);

    let batch = mcp
        .post(
            &json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]),
            &[],
        )
        .await;
    assert_eq!(batch.status, StatusCode::BAD_REQUEST);
    assert_eq!(batch.json()["error"]["code"], -32600);

    let not_jsonrpc = mcp.post(&json!({ "id": 1, "method": "ping" }), &[]).await;
    assert_eq!(not_jsonrpc.json()["error"]["code"], -32600);

    let unknown_tool = mcp
        .rpc(
            "tools/call",
            json!({ "name": "drop_everything", "arguments": {} }),
        )
        .await;
    assert_eq!(unknown_tool["error"]["code"], -32602);

    // A client's response to nothing is accepted and ignored.
    let response = mcp
        .post(&json!({ "jsonrpc": "2.0", "id": 9, "result": {} }), &[])
        .await;
    assert_eq!(response.status, StatusCode::ACCEPTED);

    // No stream, no session to end.
    for method in [Method::GET, Method::DELETE] {
        let answered = app
            .send(
                Request::builder()
                    .method(method.clone())
                    .uri("/api/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(answered.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_a_personal_token_gets_in_from_romulus_s_origin_or_none(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let token = pat(&db, &alice, AccessTokenScope::Write).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string();
    let request = |auth: Option<String>, cookie: Option<String>, origin: Option<&str>| {
        let mut request =
            Request::post("/api/mcp").header(header::CONTENT_TYPE, "application/json");
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, auth);
        }
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        request.body(Body::from(ping.clone())).unwrap()
    };

    // No token: 401 with the challenge MCP clients read.
    let anonymous = app.send(request(None, None, None)).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(
        anonymous.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Bearer realm=\"romulus\"")
    );
    // The session cookie is never enough (and no CSRF refusal either).
    let cookie = app.send(request(None, Some(alice.cookie()), None)).await;
    assert_eq!(cookie.status, StatusCode::UNAUTHORIZED);
    // A database's API token is not a person's.
    let api = app
        .send(request(
            Some(format!("Bearer rom_{}", token::generate())),
            None,
            None,
        ))
        .await;
    assert_eq!(api.status, StatusCode::UNAUTHORIZED);

    let bearer = Some(format!("Bearer {token}"));
    assert_eq!(
        app.send(request(bearer.clone(), None, None)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.send(request(bearer.clone(), None, Some(ORIGIN)))
            .await
            .status,
        StatusCode::OK
    );
    // Another site's page (DNS rebinding, a malicious tab): refused.
    let foreign = app
        .send(request(bearer, None, Some("http://evil.test")))
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert!(foreign.json()["error"]["message"].is_string());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn requests_are_limited_per_token(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let busy = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
    let calm = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let mut limited = None;
    for _ in 0..80 {
        let answered = busy.post(&ping, &[]).await;
        if answered.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(answered);
            break;
        }
    }
    let limited = limited.expect("a burst of 80 is limited");
    assert!(limited.headers.contains_key(header::RETRY_AFTER));
    assert_eq!(calm.post(&ping, &[]).await.status, StatusCode::OK);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_person_s_tokens_share_a_budget_and_failing_clients_are_cut_off(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    // Each token has its own budget, and the person one across them all.
    for _ in 0..2 {
        let token = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
        for _ in 0..60 {
            assert_eq!(token.post(&ping, &[]).await.status, StatusCode::OK);
        }
    }
    // Its own budget is untouched; the person's (refilling meanwhile) runs
    // out first.
    let third = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
    let mut limited = None;
    for _ in 0..60 {
        let answered = third.post(&ping, &[]).await;
        if answered.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(answered);
            break;
        }
    }
    let limited = limited.expect("the person's budget ends before the token's");
    assert!(
        limited.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("your access tokens"),
        "{}",
        limited.json()
    );
    assert!(limited.headers.contains_key(header::RETRY_AFTER));

    // A client sending tokens that are not valid is refused after 20, before
    // anything is looked up: a good token from it waits too.
    let bad = Mcp::new(&app, format!("rmls_pat_{}", token::generate()));
    let mut refused = 0;
    let cut_off = loop {
        let answered = bad.post(&ping, &[]).await;
        if answered.status != StatusCode::UNAUTHORIZED {
            break answered;
        }
        refused += 1;
        assert!(refused <= 40, "a client failing without end is cut off");
    };
    assert!(refused >= 20, "20 tries first, then {refused}");
    assert_eq!(cut_off.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(cut_off.headers.contains_key(header::RETRY_AFTER));
    let bob = super::harness::signed_in(&db, "Bob").await;
    let good = Mcp::new(&app, pat(&db, &bob, AccessTokenScope::Read).await);
    assert_eq!(
        good.post(&ping, &[]).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_that_cannot_be_checked_is_a_server_error_not_a_401(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = super::harness::signed_in(&db, "Alice").await;
    let mcp = Mcp::new(&app, pat(&db, &alice, AccessTokenScope::Read).await);
    // The database goes away: the token is not wrong, so no 401 tells the
    // client to drop it.
    db.close().await;
    let answered = mcp
        .post(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }), &[])
        .await;
    assert_eq!(answered.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!answered.headers.contains_key(header::WWW_AUTHENTICATE));
    assert!(
        answered.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("try again"),
        "{}",
        answered.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn whoami_names_the_person_their_token_and_roles(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let other = org(&db, "globex").await;
    member(
        &db,
        other,
        team.developer.user,
        romulus_api::domain::Role::Reviewer,
    )
    .await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Read).await,
    );
    let text = mcp.ok("whoami", json!({})).await;
    assert!(text.contains("Developer-acme"), "{text}");
    assert!(text.contains("read-only"), "{text}");
    assert!(text.contains("- acme: acme, developer"), "{text}");
    assert!(text.contains("- globex: globex, reviewer"), "{text}");
    // The same facts, structured, for a program checking its connection.
    let answer = mcp
        .rpc("tools/call", json!({ "name": "whoami", "arguments": {} }))
        .await;
    let facts = &answer["result"]["structuredContent"];
    assert_eq!(facts["user"]["name"], "Developer-acme", "{answer}");
    assert_eq!(facts["token"]["access"], "read-only");
    assert_eq!(facts["organizations"][1]["role"], "reviewer", "{facts}");
    // Several organizations: tools ask which.
    let ambiguous = mcp.refused("list_databases", json!({})).await;
    assert!(
        ambiguous.contains("`acme`") && ambiguous.contains("`globex`"),
        "{ambiguous}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reviewers_read_and_cannot_write_and_outsiders_see_nothing(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let reviewer = Mcp::new(
        &app,
        pat(&db, &team.reviewer, AccessTokenScope::Write).await,
    );
    let listed = reviewer.ok("list_databases", json!({})).await;
    assert!(
        listed.contains("- shop: shop, Postgres 17, latest v2"),
        "{listed}"
    );
    let schema = reviewer
        .ok("get_schema", json!({ "database": "shop", "version": 1 }))
        .await;
    assert!(schema.contains("CREATE TABLE public.customers"), "{schema}");
    let refused = reviewer
        .refused("create_draft", json!({ "database": "shop" }))
        .await;
    assert!(refused.contains("(forbidden)"), "{refused}");
    assert!(
        refused.contains("Reviewers have read-only access"),
        "{refused}"
    );
    let refused = reviewer
        .refused("create_database", json!({ "name": "Billing" }))
        .await;
    assert!(refused.contains("(forbidden)"), "{refused}");

    // Someone of another organization: nothing of acme, as if it did not
    // exist.
    let outsider = Mcp::new(
        &app,
        pat(&db, &team.outsider, AccessTokenScope::Write).await,
    );
    let refused = outsider
        .refused("list_databases", json!({ "org": "acme" }))
        .await;
    assert!(refused.contains("(not_found)"), "{refused}");
    let refused = outsider
        .refused("get_schema", json!({ "org": "acme", "database": "shop" }))
        .await;
    assert!(refused.contains("(not_found)"), "{refused}");
    let found = outsider
        .tool("search_schema", json!({ "query": "customers" }))
        .await;
    assert!(found.0.contains("Nothing matches"), "{found:?}");
    // A developer's draft id says nothing to them either.
    let developer = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let created = developer
        .ok("create_draft", json!({ "database": "shop" }))
        .await;
    let draft = id_after(&created, "Created draft ");
    let refused = outsider
        .refused("get_draft", json!({ "draft": draft.to_string() }))
        .await;
    assert!(refused.contains("No draft"), "{refused}");
    let refused = reviewer
        .refused(
            "update_draft",
            json!({ "draft": draft.to_string(), "sql": "CREATE TABLE public.x (id bigint);" }),
        )
        .await;
    assert!(refused.contains("(forbidden)"), "{refused}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_read_only_token_never_writes(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Read).await,
    );
    let tools = mcp.rpc("tools/list", json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap().clone();
    assert!(tools.len() > 10, "the reading tools are offered");
    assert!(
        tools
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true),
        "only reading tools are offered"
    );
    for (name, arguments) in [
        ("create_draft", json!({ "database": "shop" })),
        ("create_database", json!({ "name": "Billing" })),
        (
            "push_schema",
            json!({ "database": "shop", "sql": "CREATE TABLE public.y (id bigint);" }),
        ),
    ] {
        let refused = mcp.refused(name, arguments).await;
        assert!(refused.contains("(read_only_token)"), "{name}: {refused}");
    }
    let drafts = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM drafts"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(drafts, 0);
    // Reading works as for the person.
    assert!(
        mcp.ok(
            "diff_versions",
            json!({ "database": "shop", "from": 1, "to": 2 })
        )
        .await
        .contains("orders")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn onboarding_creates_a_database_and_only_its_first_version_from_sql(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let created = mcp
        .ok(
            "create_database",
            json!({ "name": "Billing Service", "pg_major": 16 }),
        )
        .await;
    assert!(created.contains("acme/billing-service"), "{created}");
    assert!(created.contains("Postgres 16"), "{created}");

    let refused = mcp
        .refused(
            "push_schema",
            json!({ "database": "billing-service", "sql": "CREATE TABLE public.invoices (id bigint);\nERROR here" }),
        )
        .await;
    assert!(
        refused.contains("does not compile") && refused.contains("line 2:1"),
        "{refused}"
    );
    // A dump as pg_dump prints it: what a version does not hold is left
    // out, and said.
    let dump = "\\restrict k1\nSET statement_timeout = 0;\n\
                CREATE TABLE public.invoices (id bigint);\n\
                ALTER TABLE public.invoices OWNER TO app;\n\
                CREATE FUNCTION public.f() RETURNS int LANGUAGE sql AS 'select 1';\n\
                \\unrestrict k1\n";
    let pushed = mcp
        .ok(
            "push_schema",
            json!({ "database": "billing-service", "sql": dump, "message": "Initial" }),
        )
        .await;
    assert!(pushed.contains("Created v1"), "{pushed}");
    assert!(
        pushed.contains("Left out, as a schema version does not hold them (5)"),
        "{pushed}"
    );
    assert!(
        pushed.contains("- line 4: ALTER TABLE public.invoices OWNER TO app"),
        "{pushed}"
    );
    let source: String = sqlx::query_scalar!(
        "SELECT v.source FROM versions v JOIN databases d ON d.id = v.database_id
         WHERE d.slug = 'billing-service'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(source.trim(), "CREATE TABLE public.invoices (id bigint);");
    let summary = mcp
        .ok(
            "get_schema",
            json!({ "database": "billing-service", "format": "summary" }),
        )
        .await;
    assert!(summary.contains("table public.invoices("), "{summary}");
    let versions = mcp
        .ok("list_versions", json!({ "database": "billing-service" }))
        .await;
    assert!(
        versions.contains("- v1:") && versions.contains("Initial"),
        "{versions}"
    );

    // Only the first version comes from SQL: later ones are drafts, so
    // reviews and tests apply.
    let again = mcp
        .refused(
            "push_schema",
            json!({ "database": "billing-service", "sql": "CREATE TABLE public.refunds (id bigint);" }),
        )
        .await;
    assert!(
        again.contains("already has a version")
            && again.contains("create a draft and publish it, so reviews and tests apply"),
        "{again}"
    );
    let numbers: Vec<i32> = sqlx::query_scalar!(
        "SELECT v.number FROM versions v JOIN databases d ON d.id = v.database_id
         WHERE d.slug = 'billing-service'"
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(numbers, [1], "no second version");

    // CI's token: its person makes it, on the page the guide links.
    let id: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'billing-service'")
        .fetch_one(&db)
        .await
        .unwrap();
    let guide = mcp
        .ok("ci_setup", json!({ "database": "billing-service" }))
        .await;
    assert!(
        guide.contains(&format!("/api/databases/{id}/versions"))
            && guide.contains("secrets.ROMULUS_TOKEN")
            && guide.contains("/app/acme/billing-service/settings#tokens"),
        "{guide}"
    );

    // Every write is in the Activity, with the token.
    let events = audited(&db).await;
    let tools: Vec<&str> = events.iter().map(|e| e["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["create_database", "push_schema"]);
    let token_id: Uuid = sqlx::query_scalar!("SELECT id FROM personal_access_tokens")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(events.iter().all(|e| e["token_id"] == json!(token_id)));
    let shown = app
        .as_user(&team.developer)
        .get(&team.path("/databases/billing-service/activity"))
        .await
        .json();
    assert_eq!(shown["events"][0]["action"], "agent_action");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn plan_limits_are_tool_errors_saying_what_lifts_them(db: PgPool) {
    // Hosted billing on, acme on the Free plan: one database.
    let billed = billing::billed(&db).await;
    let mcp = Mcp::new(
        &billed.app,
        pat(&db, &billed.team.developer, AccessTokenScope::Write).await,
    );
    mcp.ok("create_database", json!({ "org": "acme", "name": "First" }))
        .await;
    let refused = mcp
        .refused(
            "create_database",
            json!({ "org": "acme", "name": "Second" }),
        )
        .await;
    assert!(refused.contains("(plan_limit)"), "{refused}");
    assert!(refused.contains("Starter keeps 3 databases"), "{refused}");
    assert!(refused.contains("Billing page"), "{refused}");
    let listed = mcp.ok("list_databases", json!({ "org": "acme" })).await;
    assert!(
        listed.contains("First") && !listed.contains("Second"),
        "{listed}"
    );

    // The month's playground hours used up: a query would start the
    // playground, so it is refused, as in the console.
    let shop = database(&db, billed.team.org, "shop").await;
    version(&db, shop, 1, playgrounds::V1, json!({})).await;
    let built = mcp
        .ok(
            "create_playground",
            json!({ "org": "acme", "database": "shop" }),
        )
        .await;
    let playground = id_after(&built, "Playground ");
    let query = json!({ "playground": playground.to_string(), "sql": "SELECT 1" });
    mcp.ok("playground_query", query.clone()).await;
    let now = Utc::now();
    let minute = now - chrono::Duration::seconds(60);
    hours::look(&billed.app.state, minute, now).await.unwrap();
    sqlx::query!(
        "UPDATE playground_usage SET seconds = 7190 WHERE org_id = $1",
        billed.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    hours::look(&billed.app.state, minute, now).await.unwrap();
    let refused = mcp.refused("playground_query", query).await;
    assert!(refused.contains("(plan_limit)"), "{refused}");
    assert!(refused.contains("hours of playgrounds"), "{refused}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_changes_live_for_people_in_it_and_publishes_by_the_rules(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let addr = serve(&app).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );

    let created = mcp
        .ok(
            "create_draft",
            json!({
                "database": "shop",
                "name": "Add refunds",
                "edits": [{ "old": "", "new": "CREATE TABLE public.refunds (id bigint PRIMARY KEY);\n" }],
            }),
        )
        .await;
    assert!(created.contains("\"Add refunds\""), "{created}");
    assert!(created.contains("Its SQL compiles."), "{created}");
    let draft = id_after(&created, "Created draft ");

    // Someone has it open: the agent's edit reaches them live.
    let mut alice = Client::join(
        addr,
        &team.path(&format!("/drafts/{draft}/ws")),
        &team.admin,
    )
    .await;
    alice.until(|c| c.sql().contains("refunds")).await;
    let read = mcp
        .ok("get_draft", json!({ "draft": draft.to_string() }))
        .await;
    let sha = read
        .split("(sha256 ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("the text's sha256")
        .to_owned();
    let reviewed = read
        .split("reviewed_sha256: ")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .expect("reviewed_sha256")
        .to_owned();
    assert!(read.contains("CREATE TABLE public.refunds"), "{read}");
    assert!(read.contains("Migration v2 → v3"), "{read}");

    let updated = mcp
        .ok(
            "update_draft",
            json!({
                "draft": draft.to_string(),
                "expected_sha256": sha,
                "edits": [{ "old": "public.refunds (id bigint PRIMARY KEY)", "new": "public.refunds (id bigint PRIMARY KEY, amount bigint)" }],
            }),
        )
        .await;
    assert!(updated.contains("compiles"), "{updated}");
    alice.until(|c| c.sql().contains("amount bigint")).await;
    // The same sha256 again is stale now: nothing is overwritten.
    let stale = mcp
        .refused(
            "update_draft",
            json!({ "draft": draft.to_string(), "expected_sha256": sha, "sql": "-- gone" }),
        )
        .await;
    assert!(stale.contains("(draft_changed)"), "{stale}");
    let wrong = mcp
        .refused(
            "update_draft",
            json!({ "draft": draft.to_string(), "edits": [{ "old": "no such text", "new": "x" }] }),
        )
        .await;
    assert!(wrong.contains("(edit_not_found)"), "{wrong}");
    // The agent's person is an editor now: their approval would not count.
    let editors = super::draft_text::stored_editors(&db, draft).await;
    assert!(editors.contains(&team.developer.user), "{editors:?}");

    // The organization requires an approval: publishing says what is missing.
    sqlx::query!("UPDATE organizations SET approvals_required = 1")
        .execute(&db)
        .await
        .unwrap();
    let read = mcp
        .ok(
            "get_draft",
            json!({ "draft": draft.to_string(), "text": false, "migration": false }),
        )
        .await;
    let reviewed_now = read
        .split("reviewed_sha256: ")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .unwrap()
        .to_owned();
    assert_ne!(reviewed_now, reviewed, "the edit is another text to review");
    let held = mcp
        .refused(
            "publish_draft",
            json!({ "draft": draft.to_string(), "reviewed_sha256": reviewed_now }),
        )
        .await;
    assert!(held.contains("requires 1 approval"), "{held}");
    let review = mcp
        .ok("request_review", json!({ "draft": draft.to_string() }))
        .await;
    assert!(review.contains("Review requested"), "{review}");
    sqlx::query!("UPDATE organizations SET approvals_required = 0")
        .execute(&db)
        .await
        .unwrap();
    let outdated = mcp
        .refused(
            "publish_draft",
            json!({ "draft": draft.to_string(), "reviewed_sha256": reviewed }),
        )
        .await;
    assert!(
        outdated.contains("changed since you reviewed it"),
        "{outdated}"
    );
    let published = mcp
        .ok(
            "publish_draft",
            json!({ "draft": draft.to_string(), "reviewed_sha256": reviewed_now, "message": "Refunds" }),
        )
        .await;
    assert!(
        published.contains("Published v3 of acme/shop"),
        "{published}"
    );
    let events = audited(&db).await;
    let tools: Vec<&str> = events.iter().map(|e| e["tool"].as_str().unwrap()).collect();
    assert_eq!(
        tools,
        [
            "create_draft",
            "update_draft",
            "request_review",
            "publish_draft"
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_made_but_not_written_is_audited_and_named_for_the_agent(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let failed = mcp
        .refused(
            "create_draft",
            json!({
                "database": "shop",
                "name": "Add refunds",
                "edits": [{ "old": "no such text", "new": "x" }],
            }),
        )
        .await;
    // The draft exists: the agent is told which, and not to make another.
    assert!(failed.contains("(edit_not_found)"), "{failed}");
    assert!(
        failed.contains("do not call create_draft again"),
        "{failed}"
    );
    let draft = id_after(&failed, "The draft ");
    let drafts: Vec<Uuid> = sqlx::query_scalar!("SELECT id FROM drafts")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(drafts, [draft]);
    // And it is in the Activity, though the tool failed after making it.
    let events = audited(&db).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["tool"], "create_draft");
    assert_eq!(events[0]["draft_id"], json!(draft));
    assert_eq!(events[0]["outcome"], "created draft \"Add refunds\"");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn migrations_between_versions_and_renames_of_a_draft(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let migration = mcp
        .ok(
            "generate_migration",
            json!({ "database": "shop", "from": 1, "to": 2, "format": "sqlx" }),
        )
        .await;
    assert!(migration.contains("Migration v1 → v2"), "{migration}");
    assert!(
        migration.contains("CREATE TABLE public.orders"),
        "{migration}"
    );
    assert!(migration.contains("--- file "), "{migration}");
    let back = mcp
        .ok(
            "generate_migration",
            json!({ "database": "shop", "from": 2, "to": 1 }),
        )
        .await;
    assert!(back.contains("[DELETES DATA]"), "{back}");
    let diff = mcp
        .ok(
            "diff_versions",
            json!({ "database": "shop", "from": 1, "to": 2 }),
        )
        .await;
    assert!(diff.contains("public.orders:"), "{diff}");
    assert!(diff.contains("-- after:"), "{diff}");

    let created = mcp.ok("create_draft", json!({ "database": "shop" })).await;
    let draft = id_after(&created, "Created draft ");
    let decided = mcp
        .ok(
            "confirm_renames",
            json!({ "draft": draft.to_string(), "renames": { "table/public//orders/purchases": false } }),
        )
        .await;
    assert!(
        decided.contains("table/public//orders/purchases: drop and add"),
        "{decided}"
    );
    let cleared = mcp
        .ok(
            "confirm_renames",
            json!({ "draft": draft.to_string(), "renames": { "table/public//orders/purchases": null } }),
        )
        .await;
    assert!(cleared.contains("romulus's own choices"), "{cleared}");
    let neither = mcp
        .refused(
            "generate_migration",
            json!({ "database": "shop", "from": 1 }),
        )
        .await;
    assert!(neither.starts_with("Invalid arguments"), "{neither}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_test_runs_and_answers_its_verdict(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let developer = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let tested = developer
        .ok(
            "test_migration",
            json!({ "database": "shop", "from": 1, "to": 2, "wait_seconds": 30 }),
        )
        .await;
    assert!(tested.contains("Migration test "), "{tested}");
    assert!(tested.contains("PASSED"), "{tested}");
    let test = id_after(&tested, "Migration test ");
    let reviewer = Mcp::new(
        &app,
        pat(&db, &team.reviewer, AccessTokenScope::Write).await,
    );
    let read = reviewer
        .ok("get_migration_test", json!({ "test": test.to_string() }))
        .await;
    assert!(read.contains("PASSED"), "{read}");
    let refused = reviewer
        .refused(
            "test_migration",
            json!({ "database": "shop", "from": 1, "to": 2 }),
        )
        .await;
    assert!(refused.contains("(forbidden)"), "{refused}");
    // Not waiting answers the handle at once, and how to follow it.
    let started = developer
        .ok(
            "test_migration",
            json!({ "database": "shop", "from": 1, "to": 2, "wait_seconds": 0 }),
        )
        .await;
    let handle = id_after(&started, "Migration test ");
    assert!(
        started.contains("PASSED")
            || started.contains(&format!("call get_migration_test with test={handle}")),
        "{started}"
    );
    // Each test started is in the Activity at once, with its id.
    let tests: Vec<Value> = audited(&db)
        .await
        .into_iter()
        .filter(|e| e["tool"] == "test_migration")
        .collect();
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[1]["test_id"], json!(handle));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_playground_is_built_and_queried_read_only_even_by_a_read_token(db: PgPool) {
    let (app, team, _host) = playgrounds::setup(&db).await;
    let developer = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let built = developer
        .ok("create_playground", json!({ "database": "shop" }))
        .await;
    assert!(built.contains(": ready."), "{built}");
    let playground = id_after(&built, "Playground ");
    let listed = developer
        .ok("list_playgrounds", json!({ "database": "shop" }))
        .await;
    assert!(listed.contains(&playground.to_string()), "{listed}");

    let reader = Mcp::new(&app, pat(&db, &team.reviewer, AccessTokenScope::Read).await);
    let rows = reader
        .ok(
            "playground_query",
            json!({ "playground": playground.to_string(), "sql": "SELECT 1" }),
        )
        .await;
    assert!(rows.contains("Statement 1"), "{rows}");
    assert!(rows.lines().any(|line| line == "1"), "{rows}");
    let kept = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM playground_runs WHERE playground_id = $1 AND kind = 'query'"#,
        playground,
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(kept, 1, "in the shared history");
    // A read token creates none.
    let refused = reader
        .refused("create_playground", json!({ "database": "shop" }))
        .await;
    assert!(refused.contains("(read_only_token)"), "{refused}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn search_finds_tables_and_columns_across_organizations(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(
        &db,
        shop,
        1,
        "CREATE TABLE public.customers (id bigint);\nCREATE TABLE public.customer_notes (id bigint);\n",
        json!({}),
    )
    .await;
    let other = org(&db, "globex").await;
    let them = user(&db, "Them").await;
    member(&db, other, them, romulus_api::domain::Role::Admin).await;
    let theirs = database(&db, other, "secret").await;
    version(
        &db,
        theirs,
        1,
        "CREATE TABLE public.customers (id bigint);\n",
        json!({}),
    )
    .await;

    let mcp = Mcp::new(&app, pat(&db, &team.reviewer, AccessTokenScope::Read).await);
    let found = mcp
        .ok("search_schema", json!({ "query": "customer" }))
        .await;
    assert!(
        found.contains("acme/shop v1: table public.customers"),
        "{found}"
    );
    assert!(
        found.contains("acme/shop v1: table public.customer_notes"),
        "{found}"
    );
    assert!(!found.contains("globex"), "{found}");
    // Exact names first.
    let exact = mcp
        .ok("search_schema", json!({ "query": "customers" }))
        .await;
    let first = exact.lines().nth(1).unwrap();
    assert!(first.ends_with("table public.customers"), "{exact}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn prompts_and_resources_are_offered(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(
        &db,
        shop,
        1,
        "CREATE TABLE public.customers (id bigint);\n",
        json!({}),
    )
    .await;
    let mcp = Mcp::new(&app, pat(&db, &team.reviewer, AccessTokenScope::Read).await);
    let prompts = mcp.rpc("prompts/list", json!({})).await;
    assert_eq!(prompts["result"]["prompts"].as_array().unwrap().len(), 3);
    let prompt = mcp
        .rpc(
            "prompts/get",
            json!({ "name": "plan_safe_migration", "arguments": { "database": "shop", "change": "add refunds" } }),
        )
        .await;
    assert!(
        prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("add refunds")
    );
    let missing = mcp
        .rpc(
            "prompts/get",
            json!({ "name": "review_draft", "arguments": {} }),
        )
        .await;
    assert_eq!(missing["error"]["code"], -32602);

    let resources = mcp.rpc("resources/list", json!({})).await;
    let uris: Vec<&str> = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, ["romulus://guide/ci", "romulus://acme/shop/v1"]);
    let read = mcp
        .rpc("resources/read", json!({ "uri": "romulus://acme/shop/v1" }))
        .await;
    assert!(
        read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("public.customers")
    );
    let none = mcp
        .rpc("resources/read", json!({ "uri": "romulus://acme/shop/v9" }))
        .await;
    assert!(none["error"]["code"].is_i64(), "{none}");
    let templates = mcp.rpc("resources/templates/list", json!({})).await;
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        "romulus://{org}/{database}/v{version}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_agent_of_someone_who_never_accepted_the_terms_is_told_to_sign_in(db: PgPool) {
    let mut config = romulus_api::Config::for_tests();
    config.legal = Some(billing::legal_config(false));
    let app = TestApp::with_config(db.clone(), config);
    // An account from before the Terms: a member, never asked.
    let team = Team::new(&db, "acme").await;
    let agent = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let refused = agent.refused("list_databases", json!({})).await;
    assert!(refused.contains("(terms_required)"), "{refused}");
    assert!(
        refused.contains("sign in to romulus once in their browser"),
        "{refused}"
    );
    // The agent cannot accept for them; once they did, it goes on.
    let accept = json!({
        "version": "1.0", "locale": "en", "professional_use": true,
        "copy_version": romulus_api::billing::copy::version(
            romulus_api::domain::TermsEdition::Professional
        ),
    });
    let accepted = app
        .as_user(&team.developer)
        .post("/api/me/terms", &accept)
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.json());
    agent.ok("list_databases", json!({})).await;
}
