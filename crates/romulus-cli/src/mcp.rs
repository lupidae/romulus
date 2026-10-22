//! romulus's MCP endpoint (`/api/mcp`), the door a personal access token
//! opens: one JSON-RPC message per request, statelessly (MCP's streamable
//! HTTP, never a stream). Each tool answers text for agents; this client
//! asks for the API's own JSON instead (`_meta` `romulus/data`), and reads
//! refusals the same way (`code`, `message`, `details`).
//!
//! Requests that did not reach romulus, or that it could not answer just
//! then (a 502 from a proxy, a 429), are tried again a few times before
//! giving up; a refusal is final.

use std::time::Duration;

use reqwest::{StatusCode, header};
use serde_json::{Value, json};
use url::Url;

/// The protocol version spoken (stateless: no `initialize` needed, sent
/// once anyway so the token's page shows which client used it).
const PROTOCOL: &str = "2025-11-25";
/// What makes each tool answer its data.
const DATA_META: &str = "romulus/data";
/// One request at most: romulus answers a tool call within a minute.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);
const TRIES: u32 = 4;

/// Why a call did not give an answer.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// No token, or one romulus refused.
    #[error("{0}")]
    Token(String),
    /// romulus could not be reached, or did not answer as romulus does.
    #[error("{0}")]
    Connection(String),
    /// The tool refused or failed: the API's error.
    #[error("{message} ({code})")]
    Tool {
        code: String,
        message: String,
        details: Option<Value>,
    },
}

impl CallError {
    pub fn code(&self) -> &str {
        match self {
            Self::Token(_) => "token",
            Self::Connection(_) => "connection",
            Self::Tool { code, .. } => code,
        }
    }
}

/// A client of one romulus, with one token.
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
    token: String,
    next_id: std::sync::atomic::AtomicU64,
}

impl Client {
    /// `base` is romulus's address (`https://romulus.example.com`).
    pub fn new(base: &str, token: &str) -> Result<Self, CallError> {
        let mut base = Url::parse(base.trim())
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
            .ok_or_else(|| {
                CallError::Connection(format!(
                    "`{base}` is not romulus's address: give it as https://romulus.example.com"
                ))
            })?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let endpoint = base
            .join("api/mcp")
            .map_err(|e| CallError::Connection(e.to_string()))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(CallError::Token(
                "ROMULUS_TOKEN is not set: create a personal access token with write access in \
                 romulus (account menu, Access tokens) and give it to this job as a secret"
                    .into(),
            ));
        }
        if token.starts_with("rom_") {
            return Err(CallError::Token(
                "ROMULUS_TOKEN is a database API token (rom_…), which pushes schemas: romulus \
                 test needs a personal access token (rmls_pat_…), from the account menu, Access \
                 tokens"
                    .into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(format!("romulus-cli/{}", crate::VERSION))
            .build()
            .map_err(|e| CallError::Connection(e.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            token: token.to_owned(),
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Says who calls (shown on the token's page); checks the token.
    pub async fn initialize(&self) -> Result<(), CallError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "romulus-cli", "version": crate::VERSION },
            }),
            true,
        )
        .await
        .map(drop)
    }

    /// Calls a tool: its data, or its error. `writes`: it changes
    /// something, so it is tried again only when romulus surely did not act
    /// (no connection, a rate limit), never after an answer that may have
    /// come too late.
    pub async fn call(
        &self,
        tool: &str,
        arguments: Value,
        writes: bool,
    ) -> Result<Value, CallError> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments, "_meta": { DATA_META: true } }),
                !writes,
            )
            .await?;
        let data = result.get("structuredContent").cloned();
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if result["isError"] == true {
            let error = data.as_ref().map(|d| &d["error"]);
            return Err(match error {
                Some(error) if error["code"].is_string() => {
                    let code = error["code"].as_str().unwrap_or("").to_owned();
                    let message = error["message"].as_str().unwrap_or(&text).to_owned();
                    if code == "read_only_token" {
                        CallError::Token(format!(
                            "{message}: romulus test writes (a draft, a test), so it needs a \
                             token with write access"
                        ))
                    } else {
                        CallError::Tool {
                            code,
                            message,
                            details: error.get("details").cloned().filter(|d| !d.is_null()),
                        }
                    }
                }
                _ => CallError::Tool {
                    code: "refused".into(),
                    message: text,
                    details: None,
                },
            });
        }
        data.ok_or_else(|| {
            CallError::Connection(format!(
                "romulus answered {tool} without its data: it may be older than this CLI (update \
                 romulus, or use the CLI of its release)"
            ))
        })
    }

    /// One JSON-RPC request: its `result`. `again`: it may be sent again
    /// after a gateway error or a lost connection.
    async fn request(&self, method: &str, params: Value, again: bool) -> Result<Value, CallError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut tries = 0;
        loop {
            tries += 1;
            let sent = self
                .http
                .post(self.endpoint.clone())
                .bearer_auth(&self.token)
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header("MCP-Protocol-Version", PROTOCOL)
                .json(&body)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(err) if tries < TRIES && (again || err.is_connect()) => {
                    drop(err);
                    wait(tries, None).await;
                    continue;
                }
                Err(err) => {
                    return Err(CallError::Connection(format!(
                        "cannot reach romulus at {}: {}",
                        self.endpoint,
                        error_chain(&err)
                    )));
                }
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs);
            let retryable = status == StatusCode::TOO_MANY_REQUESTS
                || (again && matches!(status.as_u16(), 500 | 502 | 503 | 504));
            if retryable && tries < TRIES {
                wait(tries, retry_after).await;
                continue;
            }
            let text = response.text().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&text).ok();
            let rpc_message = parsed
                .as_ref()
                .and_then(|v| v["error"]["message"].as_str())
                .map(str::to_owned);
            if status == StatusCode::UNAUTHORIZED {
                return Err(CallError::Token(format!(
                    "romulus refused ROMULUS_TOKEN ({}): it was revoked, has expired or is \
                     mistyped. Create a new personal access token (account menu, Access tokens)",
                    rpc_message.unwrap_or_else(|| "401".into())
                )));
            }
            let Some(parsed) = parsed.filter(|v| v.get("jsonrpc").is_some()) else {
                return Err(CallError::Connection(format!(
                    "{} did not answer as romulus does (HTTP {status}): check the address \
                     (ROMULUS_URL, --url)",
                    self.endpoint
                )));
            };
            if let Some(error) = parsed.get("error") {
                return Err(CallError::Connection(format!(
                    "romulus refused the request (HTTP {status}): {}",
                    error["message"].as_str().unwrap_or("no message")
                )));
            }
            return Ok(parsed["result"].clone());
        }
    }
}

/// Before try `tries + 1`: what romulus asked, else 1 s, 2 s, 4 s.
async fn wait(tries: u32, asked: Option<Duration>) {
    let backoff = Duration::from_secs(1 << (tries - 1).min(4));
    tokio::time::sleep(asked.unwrap_or(backoff).min(Duration::from_secs(30))).await;
}

/// An error and its causes, on one line.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}
