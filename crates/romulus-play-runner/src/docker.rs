//! The few calls of Docker's Engine API the runner makes, over its socket.

use std::{fmt::Write as _, path::PathBuf, time::Duration};

use hyper::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::unix::{UnixHttpError, request};

/// Subpath volume mounts (local development) need 1.45.
const API: &str = "/v1.45";
const CALL: Duration = Duration::from_secs(60);
/// A helper copying a Medium cluster without reflinks.
const HELPER: Duration = Duration::from_mins(5);

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("Docker is unavailable: {0}")]
    Unavailable(#[from] UnixHttpError),
    #[error("Docker answered {status}: {message}")]
    Api { status: u16, message: String },
    #[error("unreadable answer from Docker: {0}")]
    Answer(#[from] serde_json::Error),
    #[error("{0}")]
    Helper(String),
}

impl DockerError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Api { status: 404, .. })
    }
}

#[derive(Clone)]
pub struct Docker {
    socket: PathBuf,
}

/// What `GET /containers/{name}/json` tells the runner.
#[derive(Debug, Deserialize)]
pub struct Inspect {
    #[serde(rename = "State")]
    pub state: InspectState,
    #[serde(rename = "NetworkSettings", default)]
    pub network: Option<NetworkSettings>,
    #[serde(rename = "Config", default)]
    pub config: Option<ContainerConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ContainerConfig {
    #[serde(rename = "Labels", default)]
    pub labels: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct InspectState {
    #[serde(rename = "Running")]
    pub running: bool,
    #[serde(rename = "OOMKilled", default)]
    pub oom_killed: bool,
}

#[derive(Debug, Deserialize)]
pub struct NetworkSettings {
    #[serde(rename = "Ports", default)]
    pub ports: Option<serde_json::Map<String, Value>>,
}

impl NetworkSettings {
    /// The host port Postgres's 5432 is published on (local development).
    pub fn postgres_port(&self) -> Option<u16> {
        self.ports
            .as_ref()?
            .get("5432/tcp")?
            .as_array()?
            .iter()
            .find_map(|binding| binding.get("HostPort")?.as_str()?.parse().ok())
    }
}

#[derive(Debug, Deserialize)]
pub struct Summary {
    #[serde(rename = "Labels", default)]
    pub labels: std::collections::HashMap<String, String>,
    #[serde(rename = "State", default)]
    pub state: String,
}

impl Docker {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<(StatusCode, bytes::Bytes), DockerError> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let (status, bytes) =
            request(&self.socket, method, &format!("{API}{path}"), body, timeout).await?;
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            return Ok((status, bytes));
        }
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("message")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        Err(DockerError::Api {
            status: status.as_u16(),
            message,
        })
    }

    pub async fn create(&self, name: &str, spec: &Value) -> Result<(), DockerError> {
        self.call(
            Method::POST,
            &format!("/containers/create?name={name}"),
            Some(spec),
            CALL,
        )
        .await
        .map(drop)
    }

    pub async fn start(&self, name: &str) -> Result<(), DockerError> {
        self.call(
            Method::POST,
            &format!("/containers/{name}/start"),
            None,
            CALL,
        )
        .await
        .map(drop)
    }

    /// Stops with the image's signal (SIGINT: Postgres's fast shutdown),
    /// then SIGKILL after `grace` seconds.
    pub async fn stop(&self, name: &str, grace: u32) -> Result<(), DockerError> {
        self.call(
            Method::POST,
            &format!("/containers/{name}/stop?t={grace}"),
            None,
            CALL + Duration::from_secs(grace.into()),
        )
        .await
        .map(drop)
    }

    pub async fn remove(&self, name: &str) -> Result<(), DockerError> {
        match self
            .call(
                Method::DELETE,
                &format!("/containers/{name}?force=true&v=true"),
                None,
                CALL,
            )
            .await
        {
            Err(err) if err.is_not_found() => Ok(()),
            other => other.map(drop),
        }
    }

    pub async fn inspect(&self, name: &str) -> Result<Option<Inspect>, DockerError> {
        match self
            .call(Method::GET, &format!("/containers/{name}/json"), None, CALL)
            .await
        {
            Ok((_, bytes)) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Every container carrying `label` (`key=value`), running or not.
    pub async fn list(&self, label: &str) -> Result<Vec<Summary>, DockerError> {
        let filters = json!({ "label": [label] }).to_string();
        let query = url_encode(&filters);
        let (_, bytes) = self
            .call(
                Method::GET,
                &format!("/containers/json?all=true&filters={query}"),
                None,
                CALL,
            )
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// A named volume, labelled (creating an existing one changes nothing).
    pub async fn create_volume(&self, name: &str, labels: &Value) -> Result<(), DockerError> {
        self.call(
            Method::POST,
            "/volumes/create",
            Some(&json!({ "Name": name, "Labels": labels })),
            CALL,
        )
        .await
        .map(drop)
    }

    pub async fn image_exists(&self, image: &str) -> Result<bool, DockerError> {
        match self
            .call(
                Method::GET,
                &format!("/images/{}/json", url_encode(image)),
                None,
                CALL,
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(err) if err.is_not_found() => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// The runtimes Docker knows (`runc`, `runsc`, `runsc-uds`…).
    pub async fn runtimes(&self) -> Result<Vec<String>, DockerError> {
        let (_, bytes) = self.call(Method::GET, "/info", None, CALL).await?;
        let info: Value = serde_json::from_slice(&bytes)?;
        Ok(info
            .get("Runtimes")
            .and_then(Value::as_object)
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// Runs a one-shot container to completion and removes it: its exit
    /// code must be 0. Answers what it printed on stdout.
    pub async fn run_once(&self, name: &str, spec: &Value) -> Result<String, DockerError> {
        // A leftover from a runner that died mid-helper.
        self.remove(name).await?;
        self.create(name, spec).await?;
        let outcome = async {
            self.start(name).await?;
            let (_, bytes) = self
                .call(
                    Method::POST,
                    &format!("/containers/{name}/wait"),
                    None,
                    HELPER,
                )
                .await?;
            let code = serde_json::from_slice::<Value>(&bytes)?
                .get("StatusCode")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let (_, logs) = self
                .call(
                    Method::GET,
                    &format!("/containers/{name}/logs?stdout=true&stderr=true"),
                    None,
                    CALL,
                )
                .await?;
            let (stdout, stderr) = demultiplex(&logs);
            if code == 0 {
                Ok(stdout)
            } else {
                Err(DockerError::Helper(format!(
                    "{name} exited with {code}: {}",
                    stderr.trim()
                )))
            }
        }
        .await;
        let removed = self.remove(name).await;
        let stdout = outcome?;
        removed?;
        Ok(stdout)
    }
}

/// Docker's log stream without a TTY: frames of an 8-byte header (stream,
/// 0, 0, 0, big-endian length) and the bytes.
fn demultiplex(mut bytes: &[u8]) -> (String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while bytes.len() >= 8 {
        let len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let end = (8 + len).min(bytes.len());
        match bytes[0] {
            2 => err.extend_from_slice(&bytes[8..end]),
            _ => out.extend_from_slice(&bytes[8..end]),
        }
        bytes = &bytes[end..];
    }
    (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// Percent-encodes everything but unreserved characters (query values).
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_are_split_into_stdout_and_stderr() {
        let mut frames = vec![1, 0, 0, 0, 0, 0, 0, 3];
        frames.extend_from_slice(b"xfs");
        frames.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 4]);
        frames.extend_from_slice(b"oops");
        assert_eq!(demultiplex(&frames), ("xfs".to_owned(), "oops".to_owned()));
    }

    #[test]
    fn the_published_port_is_read() {
        let settings: NetworkSettings = serde_json::from_value(json!({
            "Ports": { "5432/tcp": [{ "HostIp": "127.0.0.1", "HostPort": "55001" }] }
        }))
        .unwrap();
        assert_eq!(settings.postgres_port(), Some(55001));
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(
            url_encode(r#"{"a":["b=c"]}"#),
            "%7B%22a%22%3A%5B%22b%3Dc%22%5D%7D"
        );
    }
}
