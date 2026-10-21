//! The JSON-RPC side of MCP: messages, protocol versions, and what each era
//! of the protocol asks of a request and a result.
//!
//! Two eras are served, statelessly, on one endpoint (the 2026-07-28
//! revision's "dual-era server"):
//!
//! - **Legacy** (2025-03-26 to 2025-11-25): the client opens with
//!   `initialize`; romulus answers the version it asked for when it speaks
//!   it, its newest legacy one otherwise. No session is minted (the
//!   transport makes them optional): every request stands alone, so a
//!   restart loses nothing. An `MCP-Protocol-Version` header, when sent,
//!   must be one of [`LEGACY`].
//! - **Modern** (2026-07-28): every request carries its version, client
//!   and capabilities in `params._meta`; `server/discover` describes the
//!   server. The headers mirroring the body (`MCP-Protocol-Version`,
//!   `Mcp-Method`, `Mcp-Name`) must match it (400 `HeaderMismatch`
//!   otherwise); results carry `resultType` and the server's identity, list
//!   results how long they may be cached.

use axum::http::{HeaderMap, StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Protocol versions served with per-request metadata, newest first.
pub const MODERN: &[&str] = &["2026-07-28"];
/// Protocol versions served after an `initialize`, newest first.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT: &str = "io.modelcontextprotocol/clientInfo";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

/// How long a client may keep a list (tools, prompts, resources): a
/// minute, for the caller alone (it depends on their token).
const LIST_TTL_MS: u64 = 60_000;

/// JSON-RPC and MCP error codes.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Implementation-defined (-32000 to -32019): no valid token.
    pub const UNAUTHORIZED: i64 = -32001;
    /// Implementation-defined: refused (the `Origin`, the rate).
    pub const REFUSED: i64 = -32003;
    /// The headers do not match the body (2026-07-28).
    pub const HEADER_MISMATCH: i64 = -32020;
    /// A protocol version this server does not speak (2026-07-28).
    pub const UNSUPPORTED_VERSION: i64 = -32022;
}

/// A JSON-RPC message from the client, as far as routing needs it.
#[derive(Debug, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// Absent: a notification. A number or a string otherwise.
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    /// A response the client sends (to a request this server never makes).
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
}

/// Which era a request is of, and the version it speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Era {
    /// `initialize` came first (or, statelessly, could have): the version
    /// the client's header names, if any.
    Legacy(Option<String>),
    /// Per-request metadata, with this version.
    Modern(String),
}

/// A JSON-RPC error, with the HTTP status the response goes with.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub status: StatusCode,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(status: StatusCode, code: i64, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The error response for request `id` (`null` when unknown).
    pub fn body(&self, id: Option<&Value>) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        json!({ "jsonrpc": "2.0", "id": id.cloned().unwrap_or(Value::Null), "error": error })
    }
}

/// The version a legacy `initialize` is answered with: the client's when
/// romulus speaks it, its newest legacy one otherwise (the client then
/// decides whether it can go on).
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|asked| LEGACY.iter().find(|v| **v == asked))
        .copied()
        .unwrap_or(LEGACY[0])
}

/// Every version, newest first: what a version error lists.
pub fn supported() -> Vec<&'static str> {
    MODERN.iter().chain(LEGACY).copied().collect()
}

/// The `_meta` of a request's params, if any.
fn meta(params: Option<&Value>) -> Option<&Map<String, Value>> {
    params?.get("_meta")?.as_object()
}

/// The client a request names itself as (`clientInfo`), as `name version`.
pub fn client_info(method: &str, params: Option<&Value>) -> Option<String> {
    let info = if method == "initialize" {
        params?.get("clientInfo")?
    } else {
        meta(params)?.get(META_CLIENT)?
    };
    let name = info.get("name")?.as_str()?;
    let version = info.get("version").and_then(Value::as_str).unwrap_or("");
    Some(format!("{name} {version}").trim().to_owned())
}

/// Which era `method` with `params` and `headers` is of, after the checks
/// that era asks of the headers.
pub fn era(method: &str, params: Option<&Value>, headers: &HeaderMap) -> Result<Era, RpcError> {
    let header = |name: &str| {
        headers
            .get(name)
            .map(|value| value.to_str().map(str::to_owned))
    };
    let declared = meta(params).and_then(|meta| meta.get(META_VERSION));
    let Some(declared) = declared else {
        // Legacy: a version header, when sent, must be one romulus speaks.
        return match header("mcp-protocol-version") {
            None => Ok(Era::Legacy(None)),
            Some(Ok(version)) if LEGACY.contains(&version.as_str()) => {
                Ok(Era::Legacy(Some(version)))
            }
            Some(Ok(version)) if MODERN.contains(&version.as_str()) => Err(mismatch(
                "MCP-Protocol-Version names a version that sends its version in \
                 `_meta`, and this request has none",
            )),
            Some(_) => Err(unsupported(
                header_text(headers, "mcp-protocol-version").as_deref(),
            )),
        };
    };
    let Some(version) = declared.as_str() else {
        return Err(RpcError::new(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            format!("`_meta[\"{META_VERSION}\"]` must be a string."),
        ));
    };
    match header("mcp-protocol-version") {
        Some(Ok(sent)) if sent == version => {}
        Some(Ok(sent)) => {
            return Err(mismatch(&format!(
                "MCP-Protocol-Version header value '{sent}' does not match body value '{version}'"
            )));
        }
        _ => return Err(mismatch("the MCP-Protocol-Version header is missing")),
    }
    if !MODERN.contains(&version) {
        return Err(unsupported(Some(version)));
    }
    match header("mcp-method") {
        Some(Ok(sent)) if sent == method => {}
        Some(Ok(sent)) => {
            return Err(mismatch(&format!(
                "Mcp-Method header value '{sent}' does not match body value '{method}'"
            )));
        }
        _ => return Err(mismatch("the Mcp-Method header is missing")),
    }
    let named = match method {
        "tools/call" | "prompts/get" => params.and_then(|p| p.get("name")),
        "resources/read" => params.and_then(|p| p.get("uri")),
        _ => None,
    };
    if let Some(named) = named.and_then(Value::as_str) {
        match header("mcp-name").map(|value| value.ok().and_then(|v| decode_header(&v))) {
            Some(Some(sent)) if sent == named => {}
            Some(Some(sent)) => {
                return Err(mismatch(&format!(
                    "Mcp-Name header value '{sent}' does not match body value '{named}'"
                )));
            }
            Some(None) => return Err(mismatch("the Mcp-Name header is malformed")),
            None => return Err(mismatch("the Mcp-Name header is missing")),
        }
    }
    Ok(Era::Modern(version.to_owned()))
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}

fn mismatch(what: &str) -> RpcError {
    RpcError::new(
        StatusCode::BAD_REQUEST,
        code::HEADER_MISMATCH,
        format!("Header mismatch: {what}."),
    )
}

fn unsupported(requested: Option<&str>) -> RpcError {
    RpcError::new(
        StatusCode::BAD_REQUEST,
        code::UNSUPPORTED_VERSION,
        format!(
            "Unsupported protocol version. romulus speaks {}.",
            supported().join(", ")
        ),
    )
    .with_data(json!({ "supported": supported(), "requested": requested }))
}

/// A header value as the client meant it: plain ASCII, or the
/// `=?base64?…?=` form of anything else.
pub fn decode_header(value: &str) -> Option<String> {
    match value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    {
        Some(encoded) => String::from_utf8(STANDARD.decode(encoded).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

/// What the server says of itself.
pub fn server_info() -> Value {
    json!({
        "name": "romulus",
        "title": "romulus",
        "version": env!("CARGO_PKG_VERSION"),
        "websiteUrl": "https://github.com/lupidae/romulus",
    })
}

/// What romulus offers: tools, prompts and resources, with lists that do not
/// change while a client is connected (it caches them).
pub fn capabilities() -> Value {
    json!({
        "tools": { "listChanged": false },
        "prompts": { "listChanged": false },
        "resources": { "listChanged": false, "subscribe": false },
    })
}

/// A result as the era wants it: modern results say they are complete and
/// who answered, and lists how long they may be kept.
pub fn finish(era: &Era, method: &str, mut result: Value) -> Value {
    if let (Era::Modern(_), Some(object)) = (era, result.as_object_mut()) {
        object.insert("resultType".into(), json!("complete"));
        let meta = object
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(META_SERVER.into(), server_info());
        }
        if matches!(
            method,
            "tools/list"
                | "prompts/list"
                | "resources/list"
                | "resources/templates/list"
                | "server/discover"
        ) {
            object.insert("ttlMs".into(), json!(LIST_TTL_MS));
            object.insert("cacheScope".into(), json!("private"));
        }
    }
    json!({ "jsonrpc": "2.0", "result": result })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn modern(extra: &Value) -> Value {
        let mut params = json!({ "_meta": { META_VERSION: "2026-07-28" } });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        params
    }

    #[test]
    fn initialize_gets_the_clients_version_or_the_newest_legacy_one() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate(Some("2024-11-05")), "2025-11-25");
        assert_eq!(negotiate(Some("2026-07-28")), "2025-11-25");
        assert_eq!(negotiate(None), "2025-11-25");
    }

    #[test]
    fn legacy_requests_need_no_headers_but_a_known_version() {
        assert_eq!(
            era("tools/list", None, &HeaderMap::new()).unwrap(),
            Era::Legacy(None)
        );
        assert_eq!(
            era(
                "tools/list",
                None,
                &headers(&[("mcp-protocol-version", "2025-06-18")])
            )
            .unwrap(),
            Era::Legacy(Some("2025-06-18".into()))
        );
        let err = era(
            "tools/list",
            None,
            &headers(&[("mcp-protocol-version", "1999-01-01")]),
        )
        .unwrap_err();
        assert_eq!(
            (err.status, err.code),
            (StatusCode::BAD_REQUEST, code::UNSUPPORTED_VERSION)
        );
    }

    #[test]
    fn modern_requests_mirror_their_body_in_headers() {
        let params = modern(&json!({ "name": "whoami" }));
        let good = headers(&[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "whoami"),
        ]);
        assert_eq!(
            era("tools/call", Some(&params), &good).unwrap(),
            Era::Modern("2026-07-28".into())
        );
        for bad in [
            headers(&[("mcp-method", "tools/call"), ("mcp-name", "whoami")]),
            headers(&[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-name", "whoami"),
            ]),
            headers(&[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/list"),
                ("mcp-name", "whoami"),
            ]),
            headers(&[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
            ]),
            headers(&[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "get_schema"),
            ]),
        ] {
            let err = era("tools/call", Some(&params), &bad).unwrap_err();
            assert_eq!(err.code, code::HEADER_MISMATCH, "{bad:?}");
        }
        // Base64-encoded names compare decoded.
        let encoded = headers(&[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "=?base64?d2hvYW1p?="),
        ]);
        assert!(era("tools/call", Some(&params), &encoded).is_ok());
    }

    #[test]
    fn an_unknown_modern_version_lists_the_supported_ones() {
        let params = json!({ "_meta": { META_VERSION: "2099-01-01" } });
        let err = era(
            "tools/list",
            Some(&params),
            &headers(&[
                ("mcp-protocol-version", "2099-01-01"),
                ("mcp-method", "tools/list"),
            ]),
        )
        .unwrap_err();
        assert_eq!(err.code, code::UNSUPPORTED_VERSION);
        assert_eq!(err.data.unwrap()["supported"][0], "2026-07-28");
    }

    #[test]
    fn modern_results_say_they_are_complete_and_who_answered() {
        let era = Era::Modern("2026-07-28".into());
        let list = finish(&era, "tools/list", json!({ "tools": [] }));
        assert_eq!(list["result"]["resultType"], "complete");
        assert_eq!(list["result"]["cacheScope"], "private");
        assert_eq!(list["result"]["_meta"][META_SERVER]["name"], "romulus");
        let legacy = finish(&Era::Legacy(None), "tools/list", json!({ "tools": [] }));
        assert!(legacy["result"].get("resultType").is_none());
    }

    #[test]
    fn clients_are_named_by_their_info() {
        let init = json!({ "clientInfo": { "name": "claude-code", "version": "2.1.4" } });
        assert_eq!(
            client_info("initialize", Some(&init)).as_deref(),
            Some("claude-code 2.1.4")
        );
        let call = json!({ "_meta": { META_CLIENT: { "name": "cursor" } } });
        assert_eq!(
            client_info("tools/list", Some(&call)).as_deref(),
            Some("cursor")
        );
    }
}
