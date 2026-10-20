//! The playground service's side of the runner's socket.

use std::{path::PathBuf, time::Duration};

use hyper::{Method, StatusCode};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::{
    protocol::{Create, ErrorBody, Fork, Health, Listing, Space, Started, State},
    unix::{UnixHttpError, request},
};

/// Operations that copy a cluster (snapshot, restore) may take a while on a
/// filesystem without reflinks.
const SLOW: Duration = Duration::from_mins(3);
const QUICK: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The runner is not answering (its socket).
    #[error("the runner is unavailable: {0}")]
    Unavailable(#[from] UnixHttpError),
    /// The runner refused or failed.
    #[error("{message} ({code})")]
    Refused {
        status: u16,
        code: String,
        message: String,
    },
    #[error("unreadable answer from the runner: {0}")]
    Answer(#[from] serde_json::Error),
}

impl RunnerError {
    /// The runner's error code, when it answered one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// A client of the runner at `socket`.
#[derive(Debug, Clone)]
pub struct Runner {
    socket: PathBuf,
}

impl Runner {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub async fn create(&self, id: Uuid, create: &Create) -> Result<(), RunnerError> {
        let body = serde_json::to_vec(create)?;
        self.call::<serde_json::Value>(Method::POST, &path(id, ""), Some(body), QUICK)
            .await
            .map(drop)
    }

    pub async fn start(&self, id: Uuid) -> Result<Started, RunnerError> {
        self.call(Method::POST, &path(id, "/start"), None, QUICK)
            .await
    }

    pub async fn stop(&self, id: Uuid) -> Result<(), RunnerError> {
        self.call::<serde_json::Value>(Method::POST, &path(id, "/stop"), None, QUICK)
            .await
            .map(drop)
    }

    pub async fn snapshot(&self, id: Uuid) -> Result<(), RunnerError> {
        self.call::<serde_json::Value>(Method::POST, &path(id, "/snapshot"), None, SLOW)
            .await
            .map(drop)
    }

    pub async fn restore(&self, id: Uuid) -> Result<(), RunnerError> {
        self.call::<serde_json::Value>(Method::POST, &path(id, "/restore"), None, SLOW)
            .await
            .map(drop)
    }

    /// Creates `id` as a copy of another playground's cluster (left stopped).
    pub async fn fork(&self, id: Uuid, fork: &Fork) -> Result<(), RunnerError> {
        let body = serde_json::to_vec(fork)?;
        self.call::<serde_json::Value>(Method::POST, &path(id, "/fork"), Some(body), SLOW)
            .await
            .map(drop)
    }

    pub async fn destroy(&self, id: Uuid) -> Result<(), RunnerError> {
        self.call::<serde_json::Value>(Method::DELETE, &path(id, ""), None, SLOW)
            .await
            .map(drop)
    }

    pub async fn state(&self, id: Uuid) -> Result<State, RunnerError> {
        self.call(Method::GET, &path(id, ""), None, QUICK).await
    }

    pub async fn list(&self) -> Result<Listing, RunnerError> {
        self.call(Method::GET, "/v1/playgrounds", None, QUICK).await
    }

    pub async fn health(&self) -> Result<Health, RunnerError> {
        self.call(Method::GET, "/v1/health", None, Duration::from_secs(10))
            .await
    }

    /// The playground volume's free and total bytes (read at most every
    /// 30 s by the runner).
    pub async fn space(&self) -> Result<Space, RunnerError> {
        self.call(Method::GET, "/v1/space", None, Duration::from_secs(30))
            .await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<T, RunnerError> {
        let (status, bytes) = request(&self.socket, method, path, body, timeout).await?;
        if status.is_success() {
            let bytes = if status == StatusCode::NO_CONTENT || bytes.is_empty() {
                &b"null"[..]
            } else {
                &bytes[..]
            };
            return Ok(serde_json::from_slice(bytes)?);
        }
        let body: ErrorBody = serde_json::from_slice(&bytes).unwrap_or_else(|_| ErrorBody {
            code: "runner".to_owned(),
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
        Err(RunnerError::Refused {
            status: status.as_u16(),
            code: body.code,
            message: body.message,
        })
    }
}

fn path(id: Uuid, rest: &str) -> String {
    format!("/v1/playgrounds/{id}{rest}")
}
