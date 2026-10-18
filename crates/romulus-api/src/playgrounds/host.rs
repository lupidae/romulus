//! The playground host, as the API reaches it: the playground service over
//! the private link (`ROMULUS_PLAY_URL`), every call signed with
//! `ROMULUS_PLAY_KEY` for exactly what it may do
//! (`romulus_playground::signing`). Without them, playgrounds are off: every
//! call answers [`HostError::Off`].
//!
//! [`Backend`] is the seam: [`Remote`] in production, a fake in tests
//! (`TestApp::with_playgrounds`).

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use romulus_playground::{
    protocol::{
        Access, Build, BuildEvent, ErrorBody, Health, Listing, Migrate, MigrationEvent, Status,
    },
    signing::{Claims, Op},
};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use url::Url;
use uuid::Uuid;

use crate::{ApiError, config::Secret};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Who a call is for: the playground's organization, the person (`None`:
/// romulus itself) and their name, their access.
#[derive(Debug, Clone)]
pub struct Caller {
    pub org: Uuid,
    pub user: Option<(Uuid, String)>,
    pub access: Access,
}

/// One console session on the host: text frames each way. Dropping the
/// sender closes it.
pub struct ConsoleLink {
    pub to_host: mpsc::Sender<String>,
    pub from_host: mpsc::Receiver<String>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum HostError {
    #[error("playgrounds are not configured on this server")]
    Off,
    #[error("the playground host is unavailable: {0}")]
    Unavailable(String),
    #[error("{message}")]
    Refused {
        status: u16,
        code: String,
        message: String,
    },
}

impl HostError {
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused { code, .. } => Some(code),
            _ => None,
        }
    }
}

impl From<HostError> for ApiError {
    fn from(err: HostError) -> Self {
        match err {
            HostError::Off => ApiError::Down {
                code: "host_unavailable",
                message: "Playgrounds are not available on this server.".into(),
            },
            HostError::Unavailable(reason) => {
                tracing::warn!(%reason, "playground host unavailable");
                ApiError::Down {
                    code: "host_unavailable",
                    message: "The playground host is not answering. Try again in a moment.".into(),
                }
            }
            HostError::Refused { code, message, .. } => match code.as_str() {
                "org_full" => ApiError::Conflict {
                    code: "playground_limit",
                    message,
                },
                "host_full" => ApiError::Down {
                    code: "host_unavailable",
                    message,
                },
                "busy" => ApiError::Conflict {
                    code: "playground_busy",
                    message: "The playground is busy (someone is resetting it or running a migration). Try again when it is done.".into(),
                },
                "not_found" => ApiError::not_found("The playground is gone from its host."),
                "not_ready" => ApiError::Conflict {
                    code: "playground_not_ready",
                    message: "The playground is not ready yet.".into(),
                },
                _ => {
                    tracing::warn!(%code, %message, "the playground host refused a call");
                    ApiError::Down {
                        code: "host_unavailable",
                        message: "The playground host could not do it. Try again in a moment."
                            .into(),
                    }
                }
            },
        }
    }
}

/// What the API asks of a playground host.
pub trait Backend: Send + Sync + 'static {
    fn build(
        &self,
        id: Uuid,
        caller: Caller,
        build: Build,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<BuildEvent>, HostError>>;
    fn migrate(
        &self,
        id: Uuid,
        caller: Caller,
        migrate: Migrate,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<MigrationEvent>, HostError>>;
    fn reset(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<Status, HostError>>;
    fn destroy(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<(), HostError>>;
    fn status(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<Status, HostError>>;
    fn list(&self) -> BoxFuture<'_, Result<Listing, HostError>>;
    fn console(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<ConsoleLink, HostError>>;
    fn health(&self) -> BoxFuture<'_, Result<Health, HostError>>;
}

/// The playground host of this API. Cheap to clone.
#[derive(Clone)]
pub struct Host(Option<Arc<dyn Backend>>);

impl Host {
    /// Off: every call answers [`HostError::Off`].
    pub fn off() -> Self {
        Self(None)
    }

    pub fn new(backend: impl Backend) -> Self {
        Self(Some(Arc::new(backend)))
    }

    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    pub fn backend(&self) -> Result<&dyn Backend, HostError> {
        self.0.as_deref().ok_or(HostError::Off)
    }
}

/// The playground service at `url`.
pub struct Remote {
    url: Url,
    key: Secret,
    http: reqwest::Client,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl Remote {
    pub fn new(url: Url, key: Secret) -> Self {
        Self {
            url,
            key,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("an HTTP client"),
        }
    }

    fn sign(&self, op: Op, id: Option<Uuid>, caller: &Caller, body: Option<&[u8]>) -> String {
        let mut claims = Claims::new(op, id, caller.access, now()).org(caller.org);
        if let Some((user, name)) = &caller.user {
            claims = claims.user(*user).name(name);
        }
        if let Some(body) = body {
            claims = claims.body(body);
        }
        claims.sign(self.key.expose().as_bytes())
    }

    fn url(&self, path: &str) -> Url {
        self.url.join(path).expect("a path under the service's URL")
    }

    async fn answer<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, HostError> {
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| HostError::Unavailable(e.to_string()))?;
        if status.is_success() {
            let bytes = if bytes.is_empty() {
                &b"null"[..]
            } else {
                &bytes[..]
            };
            return serde_json::from_slice(bytes)
                .map_err(|e| HostError::Unavailable(e.to_string()));
        }
        let body: ErrorBody = serde_json::from_slice(&bytes).unwrap_or_else(|_| ErrorBody {
            code: "unknown".into(),
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
        Err(HostError::Refused {
            status: status.as_u16(),
            code: body.code,
            message: body.message,
        })
    }

    async fn simple<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        op: Op,
        id: Option<Uuid>,
        caller: &Caller,
        timeout: Duration,
    ) -> Result<T, HostError> {
        let response = self
            .http
            .request(method, self.url(path))
            .header("authorization", self.sign(op, id, caller, None))
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| HostError::Unavailable(e.to_string()))?;
        Self::answer(response).await
    }

    /// A call answering NDJSON: its events, as they come, on a channel.
    async fn stream<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        op: Op,
        id: Uuid,
        caller: &Caller,
        body: Vec<u8>,
    ) -> Result<mpsc::Receiver<T>, HostError> {
        let response = self
            .http
            .post(self.url(path))
            .header(
                "authorization",
                self.sign(op, Some(id), caller, Some(&body)),
            )
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| HostError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::answer::<serde_json::Value>(response)
                .await
                .err()
                .unwrap_or_else(|| HostError::Unavailable("refused".into())));
        }
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut bytes = response.bytes_stream();
            let mut pending: Vec<u8> = Vec::new();
            while let Some(Ok(chunk)) = bytes.next().await {
                pending.extend_from_slice(&chunk);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    let line = &line[..line.len() - 1];
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    match serde_json::from_slice::<T>(line) {
                        Ok(event) => {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                        Err(err) => tracing::warn!(error = %err, "unreadable playground event"),
                    }
                }
            }
        });
        Ok(rx)
    }
}

impl Backend for Remote {
    fn build(
        &self,
        id: Uuid,
        caller: Caller,
        build: Build,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<BuildEvent>, HostError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(&build).map_err(|e| HostError::Unavailable(e.to_string()))?;
            self.stream(
                &format!("playgrounds/{id}/build"),
                Op::Build,
                id,
                &caller,
                body,
            )
            .await
        })
    }

    fn migrate(
        &self,
        id: Uuid,
        caller: Caller,
        migrate: Migrate,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<MigrationEvent>, HostError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(&migrate).map_err(|e| HostError::Unavailable(e.to_string()))?;
            self.stream(
                &format!("playgrounds/{id}/migration"),
                Op::Migrate,
                id,
                &caller,
                body,
            )
            .await
        })
    }

    fn reset(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        Box::pin(async move {
            self.simple(
                reqwest::Method::POST,
                &format!("playgrounds/{id}/reset"),
                Op::Reset,
                Some(id),
                &caller,
                Duration::from_mins(5),
            )
            .await
        })
    }

    fn destroy(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            self.simple::<serde_json::Value>(
                reqwest::Method::DELETE,
                &format!("playgrounds/{id}"),
                Op::Destroy,
                Some(id),
                &caller,
                Duration::from_mins(5),
            )
            .await
            .map(drop)
        })
    }

    fn status(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        Box::pin(async move {
            self.simple(
                reqwest::Method::GET,
                &format!("playgrounds/{id}"),
                Op::Status,
                Some(id),
                &caller,
                Duration::from_secs(5),
            )
            .await
        })
    }

    fn list(&self) -> BoxFuture<'_, Result<Listing, HostError>> {
        Box::pin(async move {
            let caller = Caller {
                org: Uuid::nil(),
                user: None,
                access: Access::Develop,
            };
            self.simple(
                reqwest::Method::GET,
                "playgrounds",
                Op::List,
                None,
                &caller,
                Duration::from_secs(30),
            )
            .await
        })
    }

    fn console(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<ConsoleLink, HostError>> {
        Box::pin(async move {
            let mut url = self.url(&format!("playgrounds/{id}/console"));
            let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
            url.set_scheme(scheme)
                .map_err(|()| HostError::Unavailable("a websocket URL".into()))?;
            let mut request = url
                .as_str()
                .into_client_request()
                .map_err(|e| HostError::Unavailable(e.to_string()))?;
            request.headers_mut().insert(
                "authorization",
                self.sign(Op::Console, Some(id), &caller, None)
                    .parse()
                    .map_err(|_| HostError::Unavailable("a header".into()))?,
            );
            let (socket, _) = tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async(request),
            )
            .await
            .map_err(|_| HostError::Unavailable("no answer".into()))?
            .map_err(|e| match e {
                tungstenite::Error::Http(response) => HostError::Refused {
                    status: response.status().as_u16(),
                    code: "refused".into(),
                    message: format!("the host refused the console ({})", response.status()),
                },
                other => HostError::Unavailable(other.to_string()),
            })?;
            let (mut sink, mut stream) = socket.split();
            let (to_host, mut outgoing) = mpsc::channel::<String>(16);
            let (incoming, from_host) = mpsc::channel::<String>(16);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        out = outgoing.recv() => match out {
                            Some(text) => {
                                if sink.send(tungstenite::Message::Text(text.into())).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        },
                        frame = stream.next() => match frame {
                            Some(Ok(tungstenite::Message::Text(text))) => {
                                if incoming.send(text.to_string()).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) | tungstenite::Message::Binary(_) | tungstenite::Message::Frame(_))) => {}
                            Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => break,
                        },
                    }
                }
                let _ = sink.close().await;
            });
            Ok(ConsoleLink { to_host, from_host })
        })
    }

    fn health(&self) -> BoxFuture<'_, Result<Health, HostError>> {
        Box::pin(async move {
            let response = self
                .http
                .get(self.url("health"))
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .map_err(|e| HostError::Unavailable(e.to_string()))?;
            Self::answer(response).await
        })
    }
}
