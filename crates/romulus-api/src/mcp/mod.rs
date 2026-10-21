//! The MCP endpoint (Model Context Protocol, docs/mcp.md): a person's AI
//! agent (Claude Code, Claude Desktop, Cursor…) works in romulus through it,
//! as them.
//!
//! `POST /api/mcp` takes one JSON-RPC message (the streamable HTTP
//! transport) and answers one JSON object: never a stream, since every tool
//! answers within the request (long ones wait a bounded time, then hand back
//! a handle to poll). `GET` and `DELETE` answer 405: there is no
//! server-initiated stream and no session. Both eras of the protocol are
//! served, statelessly ([`protocol`]).
//!
//! **Who.** A personal access token only (`auth::access_tokens::Agent`,
//! `Authorization: Bearer rmls_pat_…`), never the session cookie: nothing
//! here is ambient, which is why the CSRF guard lets this path through. An
//! `Origin` header, when sent, must be the public URL's (DNS rebinding: a
//! page elsewhere cannot drive it from someone's browser). Requests are
//! limited per token (`rate::Limits::access_tokens`), then each tool's calls
//! as the web app limits its person.
//!
//! **What.** [`tools`] are a thin adapter over the REST API: each calls the
//! handlers the web app calls, as the token's person ([`api::Api`]), so
//! roles, validation, limits and the audit log are the web app's; a
//! read-only token is offered the reading tools only, and its lent identity
//! refuses any write. [`prompts`] are the common flows; [`resources`] each
//! version's SQL and the CI guide.
//!
//! **OAuth.** Not yet: MCP's authorization flow (OAuth 2.1, protected
//! resource metadata) would let a client connect without pasting a token.
//! The seam is [`unauthorized`] (the `WWW-Authenticate` challenge a client
//! reads first) and `Agent` (what a bearer token resolves to): an
//! authorization server issuing access tokens for a person would resolve to
//! the same `Agent`.

pub mod api;
pub mod prompts;
pub mod protocol;
pub mod resources;
pub mod tools;

use std::net::SocketAddr;

use axum::{
    Extension,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, State},
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use utoipa_axum::{router::OpenApiRouter, routes};

use self::{
    api::Api,
    protocol::{Era, Incoming, RpcError, code},
    tools::{Ctx, ToolError},
};
use crate::{
    ApiError, AppState,
    auth::access_tokens::{self, Agent},
    error::ErrorBody,
};

/// The endpoint's path, as the CSRF guard and the deadline know it.
pub const PATH: &str = "/api/mcp";

/// A message's size at most: a whole schema in a tool's arguments, escaped.
const BODY_LIMIT: usize = 4 * 1024 * 1024;

/// What romulus tells a client about using it, once (`initialize`,
/// `server/discover`).
const INSTRUCTIONS: &str = "romulus keeps an organization's Postgres schemas: versions (immutable, \
numbered), drafts (the next version in the making, edited live by people and agents), the \
migration between any two, tests of a migration on generated rows, and playgrounds to query. \
Start with whoami, then list_databases. To change a schema: create_draft, update_draft (exact \
`edits`), get_draft (problems and the migration with its hazards), test_migration, then \
request_review; publish_draft only when your person says so. Tools marked as changing shared \
state are seen by the whole organization. Never paste secrets into a draft.";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(message))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// One MCP message: a JSON-RPC request (answered), or a notification or a
/// client's response (accepted, 202).
#[utoipa::path(
    post,
    path = "/mcp",
    operation_id = "mcp_message",
    tag = "mcp",
    request_body(content = Object, description = "One JSON-RPC 2.0 message (MCP, streamable HTTP)"),
    responses(
        (status = 200, description = "The JSON-RPC response (`result`, or `error`)", body = Object),
        (status = 202, description = "A notification or a response, accepted"),
        (status = 400, description = "A JSON-RPC error: malformed, a batch, headers that do not match the body, an unsupported protocol version", body = Object),
        (status = 401, description = "No live personal access token (`WWW-Authenticate: Bearer`)", body = Object),
        (status = 403, description = "An `Origin` other than romulus's", body = Object),
        (status = 404, description = "An unknown method (protocol 2026-07-28)", body = Object),
        (status = 413, body = ErrorBody),
        (status = 429, description = "Too many requests with this token (`Retry-After`)", body = Object),
    )
)]
pub(crate) async fn message(
    State(state): State<AppState>,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut parts: Parts,
    body: Body,
) -> Response {
    let headers = parts.headers.clone();
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.as_bytes() != state.config.public_origin().as_bytes()
    {
        tracing::info!(?origin, "mcp: refused a foreign origin");
        return rpc_error(
            None,
            &RpcError::new(
                StatusCode::FORBIDDEN,
                code::REFUSED,
                "This origin may not call romulus's MCP endpoint.",
            ),
        );
    }
    let agent = match Agent::from_request_parts(&mut parts, &state).await {
        Ok(agent) => agent,
        Err(err) => return unauthorized(&err),
    };
    if let Err(ApiError::RateLimited { retry_after }) =
        state.limits.access_tokens.check(agent.token.id)
    {
        let mut response = rpc_error(
            None,
            &RpcError::new(
                StatusCode::TOO_MANY_REQUESTS,
                code::REFUSED,
                format!(
                    "Too many requests with this token: wait {} s.",
                    retry_after.as_secs().max(1)
                ),
            ),
        );
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(retry_after.as_secs().max(1)),
        );
        return response;
    }
    let bytes = match crate::extract::read_body(&headers, body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => return err.into_response(),
    };
    let (id, method, params) = match read_message(&bytes) {
        Ok(Some(request)) => request,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(response) => return *response,
    };
    let era = if method == "initialize" {
        Era::Legacy(None)
    } else {
        match protocol::era(&method, Some(&params), &headers) {
            Ok(era) => era,
            Err(err) => return rpc_error(Some(&id), &err),
        }
    };
    if let Some(client) = protocol::client_info(&method, Some(&params)) {
        access_tokens::note_client(&state.db, agent.token.id, &client).await;
    }
    let ctx = Ctx {
        public_url: state.config.public_url.clone(),
        api: Api::new(state, agent, client.map(|Extension(info)| info)),
    };
    match handle(&ctx, &era, &method, &params).await {
        Ok(result) => {
            let mut body = protocol::finish(&era, &method, result);
            body["id"] = id;
            json_response(StatusCode::OK, &body)
        }
        Err(err) => {
            // A modern client learns an unknown method by its status too.
            let mut err = err;
            if err.code == code::METHOD_NOT_FOUND && matches!(era, Era::Modern(_)) {
                err.status = StatusCode::NOT_FOUND;
            }
            rpc_error(Some(&id), &err)
        }
    }
}

/// The request a message is (`id`, `method`, `params`), `None` for what is
/// only accepted (a notification, a client's response), or the error
/// response for what is not a JSON-RPC message.
fn read_message(bytes: &[u8]) -> Result<Option<(Value, String, Value)>, Box<Response>> {
    let refuse = |id: Option<&Value>, code: i64, message: &str| {
        Box::new(rpc_error(
            id,
            &RpcError::new(StatusCode::BAD_REQUEST, code, message),
        ))
    };
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|err| refuse(None, code::PARSE_ERROR, &format!("Parse error: {err}")))?;
    if value.is_array() {
        return Err(refuse(
            None,
            code::INVALID_REQUEST,
            "Batches are not supported: send one message per request (MCP 2025-06-18 and \
             later).",
        ));
    }
    let incoming = serde_json::from_value::<Incoming>(value).map_err(|_| {
        refuse(
            None,
            code::INVALID_REQUEST,
            "Invalid request: not a JSON-RPC 2.0 message.",
        )
    })?;
    if incoming.jsonrpc.as_deref() != Some("2.0") {
        return Err(refuse(
            incoming.id.as_ref(),
            code::INVALID_REQUEST,
            "Invalid request: `jsonrpc` must be \"2.0\".",
        ));
    }
    let Some(method) = incoming.method else {
        // A response to a request romulus never sends: nothing to do.
        return if incoming.result.is_some() || incoming.error.is_some() {
            Ok(None)
        } else {
            Err(refuse(
                incoming.id.as_ref(),
                code::INVALID_REQUEST,
                "Invalid request: no `method`.",
            ))
        };
    };
    // A notification (`notifications/initialized`, a cancellation…):
    // accepted; every request already stands alone.
    let Some(id) = incoming.id else {
        return Ok(None);
    };
    if !(id.is_string() || id.is_number()) {
        return Err(refuse(
            None,
            code::INVALID_REQUEST,
            "Invalid request: `id` must be a string or a number.",
        ));
    }
    Ok(Some((id, method, incoming.params.unwrap_or(Value::Null))))
}

/// A request's result, or its JSON-RPC error.
async fn handle(ctx: &Ctx, era: &Era, method: &str, params: &Value) -> Result<Value, RpcError> {
    let token = &ctx.api.agent().token;
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            Ok(json!({
                "protocolVersion": protocol::negotiate(asked),
                "capabilities": protocol::capabilities(),
                "serverInfo": protocol::server_info(),
                "instructions": INSTRUCTIONS,
            }))
        }
        "server/discover" => Ok(json!({
            "supportedVersions": protocol::supported(),
            "capabilities": protocol::capabilities(),
            "instructions": INSTRUCTIONS,
        })),
        // Accepted for clients that send it, though nothing is logged to
        // them.
        "ping" | "logging/setLevel" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools::all()
                .into_iter()
                .filter(|tool| token.can_write() || !tool.writes)
                .map(tools::Tool::listing)
                .collect::<Vec<_>>(),
        })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(tool) = tools::find(name) else {
                return Err(invalid_params(format!("Unknown tool: {name}")));
            };
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            if !(arguments.is_object() || arguments.is_null()) {
                return Err(invalid_params("`arguments` must be an object."));
            }
            Ok(tool_result(tools::call(ctx, tool, arguments).await))
        }
        "prompts/list" => Ok(prompts::list()),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            prompts::get(name, &arguments).map_err(invalid_params)
        }
        "resources/list" => resources::list(ctx).await.map_err(|err| tool_failure(&err)),
        "resources/templates/list" => Ok(resources::templates()),
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            match resources::read(ctx, uri).await {
                Ok(Some(contents)) => Ok(contents),
                Ok(None) => Err(not_a_resource(era, uri)),
                Err(ToolError::Api(failure)) if failure.status == StatusCode::NOT_FOUND => {
                    Err(not_a_resource(era, uri))
                }
                Err(err) => Err(tool_failure(&err)),
            }
        }
        _ => Err(RpcError::new(
            StatusCode::OK,
            code::METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )),
    }
}

/// A tool's answer: its text, or its error for the model to act on
/// (`isError`).
fn tool_result(result: Result<tools::Output, ToolError>) -> Value {
    match result {
        Ok(output) => json!({
            "content": [{ "type": "text", "text": output.text }],
            "isError": false,
        }),
        Err(err) => json!({
            "content": [{ "type": "text", "text": err.text() }],
            "isError": true,
        }),
    }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(StatusCode::OK, code::INVALID_PARAMS, message)
}

/// A resource that does not exist: -32602 since 2026-07-28, -32002 before.
fn not_a_resource(era: &Era, uri: &str) -> RpcError {
    let code = match era {
        Era::Modern(_) => code::INVALID_PARAMS,
        Era::Legacy(_) => -32002,
    };
    RpcError::new(StatusCode::OK, code, format!("Resource not found: {uri}"))
        .with_data(json!({ "uri": uri }))
}

fn tool_failure(err: &ToolError) -> RpcError {
    RpcError::new(StatusCode::OK, code::INTERNAL_ERROR, err.text())
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn rpc_error(id: Option<&Value>, err: &RpcError) -> Response {
    json_response(err.status, &err.body(id))
}

/// 401 with the challenge MCP clients read first. An OAuth authorization
/// server would add `resource_metadata` here (the protected resource
/// metadata's URL), which is what sends a client through its flow.
fn unauthorized(err: &ApiError) -> Response {
    let message = match err {
        ApiError::InvalidToken(message) => message.clone(),
        other => other.to_string(),
    };
    let mut response = rpc_error(
        None,
        &RpcError::new(
            StatusCode::UNAUTHORIZED,
            code::UNAUTHORIZED,
            message.clone(),
        ),
    );
    // A header value: visible ASCII only (`…` becomes `...`), no quotes.
    let description: String = message
        .replace('…', "...")
        .chars()
        .map(|c| match c {
            '"' | '\\' => '\'',
            c if c == ' ' || c.is_ascii_graphic() => c,
            _ => '?',
        })
        .collect();
    let challenge = format!(
        "Bearer realm=\"romulus\", error=\"invalid_token\", error_description=\"{description}\""
    );
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}
