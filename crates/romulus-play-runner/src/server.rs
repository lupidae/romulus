//! The runner itself: its configuration, the fixed operations, and the HTTP
//! server on its Unix socket ([`serve`]).
//!
//! Each operation names its playground by id (a parsed UUID); the container
//! is `romulus-play-<id>`, labelled with the runner's scope so that several
//! runners on one Docker (local worktrees) never touch each other's.
//! Operations on one playground run one at a time.

#![allow(
    clippy::disallowed_types,
    reason = "romulus-api's JSON-error extractors are its own: this server answers protocol::ErrorBody"
)]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{
    docker::{Docker, DockerError},
    password::{self, Role},
    protocol::{
        Create, Endpoint, ErrorBody, Health, Listed, Listing, RUN_DIR_BYTES, Space, Started, State,
        is_plain_identifier,
    },
};

const LABEL_ID: &str = "romulus.playground";
const LABEL_SCOPE: &str = "romulus.play.scope";
/// Seconds Postgres gets for its fast shutdown before SIGKILL.
const STOP_GRACE: u32 = 20;

/// Where playgrounds keep their files (`pg/<id>/…`, `run/<id>/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    /// A directory of the Docker host: the XFS volume at
    /// `/var/lib/romulus-play` in the preview.
    Host(PathBuf),
    /// A named Docker volume (local development: Docker Desktop and its
    /// kin cannot bind-mount a socket made in their VM back to macOS).
    Volume(String),
}

impl Root {
    /// A mount of `sub` (or the whole root) at `target`.
    fn mount(&self, sub: Option<&str>, target: &str, read_only: bool) -> Value {
        match self {
            Self::Host(path) => {
                let source = sub.map_or_else(|| path.clone(), |s| path.join(s));
                json!({
                    "Type": "bind", "Source": source, "Target": target, "ReadOnly": read_only,
                    "BindOptions": { "Propagation": "rprivate" }
                })
            }
            Self::Volume(name) => {
                let mut mount = json!({
                    "Type": "volume", "Source": name, "Target": target, "ReadOnly": read_only,
                });
                if let Some(sub) = sub {
                    mount["VolumeOptions"] = json!({ "Subpath": sub });
                }
                mount
            }
        }
    }
}

/// The runner's configuration, from its environment only.
#[derive(Debug, Clone)]
pub struct Config {
    /// `ROMULUS_PLAY_RUNNER_SOCKET`: where it listens.
    pub listen: PathBuf,
    /// `ROMULUS_PLAY_DOCKER`: Docker's socket (`/var/run/docker.sock`).
    pub docker: PathBuf,
    /// `ROMULUS_PLAY_IMAGE`: the playground image (deploy/playground), which
    /// also runs the file helper.
    pub image: String,
    /// `ROMULUS_PLAY_QUOTA_IMAGE`: an image with `xfs_quota` (the runner's
    /// own, in the preview). Unset: no quotas.
    pub quota_image: Option<String>,
    /// `ROMULUS_PLAY_ROOT`: an absolute host path, or `volume:<name>`.
    pub root: Root,
    /// `ROMULUS_PLAY_RUNTIME`: `runsc-uds` (gVisor with host Unix sockets)
    /// unless set; `runc` in local development.
    pub runtime: String,
    /// `ROMULUS_PLAY_SCOPE`: the label value marking this runner's
    /// containers.
    pub scope: String,
    /// `ROMULUS_PLAY_SECRET`: the roles' passwords derive from it (the
    /// service holds the same).
    pub secret: Vec<u8>,
    /// `ROMULUS_PLAY_TCP=true`: local development only, refused with
    /// gVisor: Postgres also listens on a port published on loopback.
    pub tcp: bool,
    /// `ROMULUS_PLAY_MAX_DATA_KB`: each Postgres process's private memory.
    pub max_data_kb: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{var}: {reason}")]
pub struct ConfigError {
    pub var: &'static str,
    pub reason: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&'static str) -> Option<String>) -> Result<Self, ConfigError> {
        let err = |var, reason: &str| ConfigError {
            var,
            reason: reason.to_owned(),
        };
        let required = |var| get(var).ok_or_else(|| err(var, "not set"));
        let root = required("ROMULUS_PLAY_ROOT")?;
        let root = match root.strip_prefix("volume:") {
            Some(name) if !name.is_empty() => Root::Volume(name.to_owned()),
            Some(_) => return Err(err("ROMULUS_PLAY_ROOT", "volume: needs a name")),
            None if root.starts_with('/') => Root::Host(PathBuf::from(root)),
            None => {
                return Err(err(
                    "ROMULUS_PLAY_ROOT",
                    "an absolute path, or volume:<name>",
                ));
            }
        };
        let scope = get("ROMULUS_PLAY_SCOPE").unwrap_or_else(|| "default".to_owned());
        if scope.is_empty()
            || scope.len() > 60
            || !scope
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(err(
                "ROMULUS_PLAY_SCOPE",
                "letters, digits, - and _ only, at most 60",
            ));
        }
        let secret = required("ROMULUS_PLAY_SECRET")?;
        if secret.len() < 32 {
            return Err(err("ROMULUS_PLAY_SECRET", "at least 32 characters"));
        }
        let runtime = get("ROMULUS_PLAY_RUNTIME").unwrap_or_else(|| "runsc-uds".to_owned());
        let tcp = match get("ROMULUS_PLAY_TCP").as_deref() {
            None | Some("false" | "0") => false,
            Some("true" | "1") => true,
            Some(_) => return Err(err("ROMULUS_PLAY_TCP", "true or false")),
        };
        // gVisor is the isolation; TCP is for machines without it.
        if tcp && runtime.starts_with("runsc") {
            return Err(err(
                "ROMULUS_PLAY_TCP",
                "local development only: refused with a gVisor runtime",
            ));
        }
        let max_data_kb = match get("ROMULUS_PLAY_MAX_DATA_KB") {
            None => 262_144,
            Some(v) => v
                .parse()
                .map_err(|_| err("ROMULUS_PLAY_MAX_DATA_KB", "a number of KiB"))?,
        };
        Ok(Self {
            listen: PathBuf::from(required("ROMULUS_PLAY_RUNNER_SOCKET")?),
            docker: PathBuf::from(
                get("ROMULUS_PLAY_DOCKER").unwrap_or_else(|| "/var/run/docker.sock".to_owned()),
            ),
            image: required("ROMULUS_PLAY_IMAGE")?,
            quota_image: get("ROMULUS_PLAY_QUOTA_IMAGE"),
            root,
            runtime,
            scope,
            secret: secret.into_bytes(),
            tcp,
            max_data_kb,
        })
    }
}

/// The runner's state: Docker, its configuration, per-playground locks.
#[derive(Clone)]
pub struct Runner(Arc<Inner>);

struct Inner {
    config: Config,
    docker: Docker,
    locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    /// The volume's filesystem, asked once (a helper).
    filesystem: OnceCell<String>,
    /// The volume's space, read by a helper at most every [`SPACE_FOR`].
    space: tokio::sync::Mutex<Option<(Instant, Space)>>,
}

/// How long a reading of the volume's space is kept.
const SPACE_FOR: Duration = Duration::from_secs(30);

/// Why an operation failed: an [`ErrorBody`] and its status.
#[derive(Debug)]
pub struct Failure {
    status: StatusCode,
    body: ErrorBody,
}

impl Failure {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.to_owned(),
                message: message.into(),
            },
        }
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such playground")
    }
}

impl From<DockerError> for Failure {
    fn from(err: DockerError) -> Self {
        tracing::error!(error = %err, "runner: docker");
        Self::new(StatusCode::BAD_GATEWAY, "docker", err.to_string())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

type Answer<T> = Result<Json<T>, Failure>;

fn container(id: Uuid) -> String {
    format!("romulus-play-{id}")
}

/// A stable XFS project id per playground directory (no state to keep): a
/// collision would make two directories share one quota, never cross a
/// boundary. `which` is `pg` (data and copy) or `run` (the socket).
fn project_id(id: Uuid, which: &str) -> u32 {
    let mut hash = Sha256::new();
    hash.update(id.as_bytes());
    if which != "pg" {
        hash.update(which.as_bytes());
    }
    let digest = hash.finalize();
    10_000 + u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 2_000_000_000
}

impl Runner {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(Inner {
            docker: Docker::new(&config.docker),
            config,
            locks: Mutex::new(HashMap::new()),
            filesystem: OnceCell::new(),
            space: tokio::sync::Mutex::new(None),
        }))
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    async fn lock(&self, id: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .0
            .locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Readies the playground volume: a named volume (local development) is
    /// created if missing.
    pub async fn prepare(&self) -> Result<(), DockerError> {
        if let Root::Volume(name) = &self.0.config.root {
            self.0
                .docker
                .create_volume(name, &self.labels(None))
                .await?;
        }
        Ok(())
    }

    /// The playground volume's filesystem (`xfs`, `btrfs`…), asked once.
    pub async fn filesystem(&self) -> Result<&str, DockerError> {
        self.0
            .filesystem
            .get_or_try_init(|| async {
                let name = format!("romulus-play-files-fstype-{}", self.0.config.scope);
                Ok(self
                    .0
                    .docker
                    .run_once(&name, &self.files_spec("fstype", None))
                    .await?
                    .trim()
                    .to_owned())
            })
            .await
            .map(String::as_str)
    }

    /// The volume's free and total bytes, read by a helper at most every
    /// 30 s (the API refuses new playgrounds when it is nearly full).
    pub async fn space(&self) -> Result<Space, Failure> {
        let mut cached = self.0.space.lock().await;
        if let Some((at, space)) = *cached
            && at.elapsed() < SPACE_FOR
        {
            return Ok(space);
        }
        let name = format!("romulus-play-files-space-{}", self.0.config.scope);
        let text = self
            .0
            .docker
            .run_once(&name, &self.files_spec("space", None))
            .await?;
        let space = Space::parse(&text).ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "docker",
                format!("unreadable space: {text:?}"),
            )
        })?;
        *cached = Some((Instant::now(), space));
        Ok(space)
    }

    /// Quotas need XFS and an image with `xfs_quota`.
    pub async fn quotas(&self) -> bool {
        self.0.config.quota_image.is_some()
            && matches!(self.filesystem().await, Ok(fs) if fs == "xfs")
    }

    fn labels(&self, id: Option<Uuid>) -> Value {
        let mut labels = json!({ LABEL_SCOPE: self.0.config.scope });
        if let Some(id) = id {
            labels[LABEL_ID] = json!(id.to_string());
        }
        labels
    }

    fn log_config() -> Value {
        json!({ "Type": "json-file", "Config": { "max-size": "1m", "max-file": "1" } })
    }

    /// `play-files <op> [id]`: root, no network, three capabilities, the
    /// volume at /play, nothing else.
    fn files_spec(&self, op: &'static str, id: Option<Uuid>) -> Value {
        let mut cmd = vec![op.to_owned()];
        cmd.extend(id.map(|id| id.to_string()));
        json!({
            "Image": self.0.config.image,
            "User": "0:0",
            "Entrypoint": ["play-files"],
            "Cmd": cmd,
            "Labels": self.labels(None),
            "NetworkDisabled": true,
            "HostConfig": {
                "NetworkMode": "none",
                "CapDrop": ["ALL"],
                "CapAdd": ["CHOWN", "DAC_OVERRIDE", "FOWNER"],
                "SecurityOpt": ["no-new-privileges"],
                "ReadonlyRootfs": true,
                "PidsLimit": 64,
                "Memory": 268_435_456,
                "Mounts": [self.0.config.root.mount(None, "/play", false)],
                "LogConfig": Self::log_config(),
            },
        })
    }

    async fn files(&self, op: &'static str, id: Uuid) -> Result<String, DockerError> {
        let name = format!("romulus-play-files-{op}-{id}");
        self.0
            .docker
            .run_once(&name, &self.files_spec(op, Some(id)))
            .await
    }

    /// The project quotas on `pg/<id>` (data and generated copy, `bytes`)
    /// and `run/<id>` (the socket directory, [`RUN_DIR_BYTES`]), by
    /// `xfs_quota -x` in a short-lived privileged container: the only
    /// operation that needs `CAP_SYS_ADMIN`.
    async fn set_quota(&self, id: Uuid, bytes: u64) -> Result<(), DockerError> {
        let Some(image) = &self.0.config.quota_image else {
            return Ok(());
        };
        let (pg, run) = (project_id(id, "pg"), project_id(id, "run"));
        let spec = json!({
            "Image": image,
            "User": "0:0",
            "Entrypoint": ["xfs_quota"],
            "Cmd": [
                "-x",
                "-c", format!("project -s -p /play/pg/{id} {pg}"),
                "-c", format!("limit -p bhard={}m {pg}", bytes / (1024 * 1024)),
                "-c", format!("project -s -p /play/run/{id} {run}"),
                "-c", format!("limit -p bhard={}m {run}", RUN_DIR_BYTES / (1024 * 1024)),
                "/play",
            ],
            "Labels": self.labels(None),
            "NetworkDisabled": true,
            "HostConfig": {
                "Privileged": true,
                "NetworkMode": "none",
                "Mounts": [self.0.config.root.mount(None, "/play", false)],
                "LogConfig": Self::log_config(),
            },
        });
        self.0
            .docker
            .run_once(&format!("romulus-play-quota-{id}"), &spec)
            .await
            .map(drop)
    }

    /// The playground's container (stopped): the flags of
    /// docs/playground.md's threat table, all of them from here.
    fn playground_spec(&self, id: Uuid, create: &Create, extensions: &str) -> Value {
        let config = &self.0.config;
        let secret = &config.secret;
        let mut env = vec![
            format!(
                "PLAY_OWNER_PASSWORD={}",
                password::derive(secret, id, Role::Owner)
            ),
            format!(
                "PLAY_READ_PASSWORD={}",
                password::derive(secret, id, Role::Read)
            ),
            format!(
                "PLAY_SERVICE_PASSWORD={}",
                password::derive(secret, id, Role::Service)
            ),
            format!("PLAY_EXTENSIONS={extensions}"),
            format!("PLAY_MAX_DATA_KB={}", config.max_data_kb),
        ];
        let memory = create.size.memory_bytes();
        let mut host = json!({
            "Runtime": config.runtime,
            "NetworkMode": "none",
            "ReadonlyRootfs": true,
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges"],
            "PidsLimit": 128,
            "Memory": memory,
            "MemorySwap": memory,
            "NanoCpus": 1_000_000_000_i64,
            "ShmSize": 67_108_864,
            "Tmpfs": { "/tmp": "rw,nosuid,nodev,noexec,size=64m" },
            "Mounts": [
                config.root.mount(Some(&format!("pg/{id}/data")), "/var/lib/postgresql/data", false),
                config.root.mount(Some(&format!("run/{id}")), "/var/run/postgresql", false),
            ],
            "RestartPolicy": { "Name": "no" },
            "OomScoreAdj": 500,
            "LogConfig": Self::log_config(),
        });
        let mut spec = json!({
            "Image": config.image,
            "User": "999:999",
            "Labels": self.labels(Some(id)),
            "StopSignal": "SIGINT",
            "StopTimeout": STOP_GRACE,
        });
        if config.tcp {
            env.push("PLAY_TCP=1".to_owned());
            host["NetworkMode"] = json!("bridge");
            host["PortBindings"] =
                json!({ "5432/tcp": [{ "HostIp": "127.0.0.1", "HostPort": "" }] });
            spec["ExposedPorts"] = json!({ "5432/tcp": {} });
        } else {
            spec["NetworkDisabled"] = json!(true);
        }
        spec["Env"] = json!(env);
        spec["HostConfig"] = host;
        spec
    }

    /// The container, if it is this runner's.
    async fn inspect(&self, id: Uuid) -> Result<Option<crate::docker::Inspect>, Failure> {
        let inspect = self.0.docker.inspect(&container(id)).await?;
        Ok(inspect.filter(|i| {
            i.config
                .as_ref()
                .and_then(|c| c.labels.as_ref()?.get(LABEL_SCOPE))
                .is_some_and(|scope| *scope == self.0.config.scope)
        }))
    }

    pub async fn create(&self, id: Uuid, create: Create) -> Result<(), Failure> {
        let mut extensions = Vec::with_capacity(create.extensions.len());
        for extension in &create.extensions {
            let Some(name) = extension.allowed_name() else {
                return Err(Failure::new(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    format!("extension {:?} is not allowed", extension.name),
                ));
            };
            if !is_plain_identifier(&extension.schema) {
                return Err(Failure::new(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    format!("schema {:?} is not a plain identifier", extension.schema),
                ));
            }
            extensions.push(format!("{name}:{}", extension.schema));
        }
        let _guard = self.lock(id).await;
        if self.0.docker.inspect(&container(id)).await?.is_some() {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "exists",
                "the playground exists",
            ));
        }
        self.files("create", id).await?;
        if self.quotas().await {
            self.set_quota(id, create.size.disk_bytes()).await?;
        }
        let spec = self.playground_spec(id, &create, &extensions.join(" "));
        self.0.docker.create(&container(id), &spec).await?;
        tracing::info!(%id, size = ?create.size, "runner: created");
        Ok(())
    }

    pub async fn start(&self, id: Uuid) -> Result<Started, Failure> {
        let _guard = self.lock(id).await;
        let inspect = self.inspect(id).await?.ok_or_else(Failure::not_found)?;
        if !inspect.state.running {
            self.0.docker.start(&container(id)).await?;
        }
        let endpoint = if self.0.config.tcp {
            let port = self
                .inspect(id)
                .await?
                .and_then(|i| {
                    i.network
                        .as_ref()
                        .and_then(crate::docker::NetworkSettings::postgres_port)
                })
                .ok_or_else(|| {
                    Failure::new(StatusCode::BAD_GATEWAY, "docker", "no published port")
                })?;
            Endpoint::Tcp {
                host: "127.0.0.1".to_owned(),
                port,
            }
        } else {
            Endpoint::Socket
        };
        Ok(Started { endpoint })
    }

    pub async fn stop(&self, id: Uuid) -> Result<(), Failure> {
        let _guard = self.lock(id).await;
        self.stop_locked(id).await
    }

    async fn stop_locked(&self, id: Uuid) -> Result<(), Failure> {
        let inspect = self.inspect(id).await?.ok_or_else(Failure::not_found)?;
        if inspect.state.running {
            self.0.docker.stop(&container(id), STOP_GRACE).await?;
        }
        Ok(())
    }

    pub async fn snapshot(&self, id: Uuid) -> Result<(), Failure> {
        let _guard = self.lock(id).await;
        let inspect = self.inspect(id).await?.ok_or_else(Failure::not_found)?;
        if inspect.state.running {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "running",
                "stop the playground first",
            ));
        }
        self.files("snapshot", id).await?;
        Ok(())
    }

    pub async fn restore(&self, id: Uuid) -> Result<(), Failure> {
        let _guard = self.lock(id).await;
        self.stop_locked(id).await?;
        self.files("restore", id).await.map_err(|err| match err {
            DockerError::Helper(message) if message.contains("no generated copy") => {
                Failure::new(StatusCode::CONFLICT, "no_snapshot", "no generated copy")
            }
            other => other.into(),
        })?;
        Ok(())
    }

    pub async fn destroy(&self, id: Uuid) -> Result<(), Failure> {
        let _guard = self.lock(id).await;
        if self.0.docker.inspect(&container(id)).await?.is_some()
            && self.inspect(id).await?.is_none()
        {
            // Another runner's (scope): not ours to delete.
            return Err(Failure::not_found());
        }
        self.0.docker.remove(&container(id)).await?;
        self.files("destroy", id).await?;
        tracing::info!(%id, "runner: destroyed");
        Ok(())
    }

    pub async fn state(&self, id: Uuid) -> Result<State, Failure> {
        let Some(inspect) = self.inspect(id).await? else {
            return Ok(State::default());
        };
        let endpoint = match (&inspect.network, self.0.config.tcp) {
            _ if !inspect.state.running => None,
            (Some(network), true) => network.postgres_port().map(|port| Endpoint::Tcp {
                host: "127.0.0.1".to_owned(),
                port,
            }),
            (_, true) => None,
            (_, false) => Some(Endpoint::Socket),
        };
        Ok(State {
            exists: true,
            running: inspect.state.running,
            oom_killed: inspect.state.oom_killed,
            endpoint,
        })
    }

    pub async fn list(&self) -> Result<Listing, Failure> {
        let label = format!("{LABEL_SCOPE}={}", self.0.config.scope);
        let playgrounds = self
            .0
            .docker
            .list(&label)
            .await?
            .into_iter()
            .filter_map(|c| {
                let id = c.labels.get(LABEL_ID)?.parse().ok()?;
                Some(Listed {
                    id,
                    running: c.state == "running",
                })
            })
            .collect();
        Ok(Listing { playgrounds })
    }

    pub async fn health(&self) -> Health {
        let config = &self.0.config;
        let runtimes = self.0.docker.runtimes().await;
        let image = self.0.docker.image_exists(&config.image).await;
        let filesystem = match self.filesystem().await {
            Ok(fs) => Some(fs.to_owned()),
            Err(err) => {
                tracing::warn!(error = %err, "runner: cannot read the volume's filesystem");
                None
            }
        };
        Health {
            docker: runtimes.is_ok(),
            runtime_available: runtimes.is_ok_and(|r| r.contains(&config.runtime)),
            runtime: config.runtime.clone(),
            image: image.unwrap_or(false),
            quotas: config.quota_image.is_some() && filesystem.as_deref() == Some("xfs"),
            filesystem,
        }
    }
}

/// The routes of [`crate::protocol`].
pub fn router(runner: Runner) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/space", get(space))
        .route("/v1/playgrounds", get(list))
        .route(
            "/v1/playgrounds/{id}",
            post(create).get(state).delete(destroy),
        )
        .route("/v1/playgrounds/{id}/start", post(start))
        .route("/v1/playgrounds/{id}/stop", post(stop))
        .route("/v1/playgrounds/{id}/snapshot", post(snapshot))
        .route("/v1/playgrounds/{id}/restore", post(restore))
        .with_state(runner)
}

async fn health(AxumState(runner): AxumState<Runner>) -> Json<Health> {
    Json(runner.health().await)
}

async fn space(AxumState(runner): AxumState<Runner>) -> Answer<Space> {
    runner.space().await.map(Json)
}

async fn list(AxumState(runner): AxumState<Runner>) -> Answer<Listing> {
    runner.list().await.map(Json)
}

async fn create(
    AxumState(runner): AxumState<Runner>,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Result<StatusCode, Failure> {
    let create: Create = serde_json::from_slice(&body)
        .map_err(|e| Failure::new(StatusCode::BAD_REQUEST, "bad_request", e.to_string()))?;
    runner.create(id, create).await?;
    Ok(StatusCode::CREATED)
}

async fn state(AxumState(runner): AxumState<Runner>, Path(id): Path<Uuid>) -> Answer<State> {
    runner.state(id).await.map(Json)
}

async fn start(AxumState(runner): AxumState<Runner>, Path(id): Path<Uuid>) -> Answer<Started> {
    runner.start(id).await.map(Json)
}

async fn stop(
    AxumState(runner): AxumState<Runner>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    runner.stop(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn snapshot(
    AxumState(runner): AxumState<Runner>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    runner.snapshot(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore(
    AxumState(runner): AxumState<Runner>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    runner.restore(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn destroy(
    AxumState(runner): AxumState<Runner>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    runner.destroy(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Extension, Size};

    fn config(extra: &[(&'static str, &str)]) -> Result<Config, ConfigError> {
        let mut env: HashMap<&'static str, String> = [
            ("ROMULUS_PLAY_RUNNER_SOCKET", "/run/runner.sock"),
            ("ROMULUS_PLAY_IMAGE", "romulus-playground:test"),
            ("ROMULUS_PLAY_ROOT", "/var/lib/romulus-play"),
            ("ROMULUS_PLAY_SECRET", "0123456789abcdef0123456789abcdef"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_owned()))
        .collect();
        for (k, v) in extra {
            env.insert(k, (*v).to_owned());
        }
        Config::from_lookup(|name| env.get(name).cloned())
    }

    #[test]
    fn gvisor_is_the_default_runtime_and_tcp_is_refused_with_it() {
        let c = config(&[]).unwrap();
        assert_eq!(c.runtime, "runsc-uds");
        assert!(!c.tcp);
        assert_eq!(c.root, Root::Host(PathBuf::from("/var/lib/romulus-play")));
        let err = config(&[("ROMULUS_PLAY_TCP", "true")]).unwrap_err();
        assert_eq!(err.var, "ROMULUS_PLAY_TCP");
        let dev = config(&[
            ("ROMULUS_PLAY_TCP", "true"),
            ("ROMULUS_PLAY_RUNTIME", "runc"),
            ("ROMULUS_PLAY_ROOT", "volume:romulus-play-x"),
        ])
        .unwrap();
        assert!(dev.tcp);
        assert_eq!(dev.root, Root::Volume("romulus-play-x".to_owned()));
    }

    #[test]
    fn a_short_secret_or_a_relative_root_is_refused() {
        assert!(config(&[("ROMULUS_PLAY_SECRET", "short")]).is_err());
        assert!(config(&[("ROMULUS_PLAY_ROOT", "relative/path")]).is_err());
        assert!(config(&[("ROMULUS_PLAY_SCOPE", "a b")]).is_err());
    }

    #[test]
    fn the_container_is_locked_down_whatever_the_request() {
        let runner = Runner::new(config(&[]).unwrap());
        let id = Uuid::from_u128(7);
        let create = Create {
            size: Size::Small,
            extensions: vec![Extension {
                name: "vector".to_owned(),
                schema: "public".to_owned(),
            }],
        };
        let spec = runner.playground_spec(id, &create, "vector:public");
        let host = &spec["HostConfig"];
        assert_eq!(host["Runtime"], "runsc-uds");
        assert_eq!(host["NetworkMode"], "none");
        assert_eq!(spec["NetworkDisabled"], true);
        assert_eq!(host["ReadonlyRootfs"], true);
        assert_eq!(host["CapDrop"], json!(["ALL"]));
        assert!(host.get("CapAdd").is_none());
        assert!(host.get("Privileged").is_none());
        assert_eq!(host["SecurityOpt"], json!(["no-new-privileges"]));
        assert_eq!(host["PidsLimit"], 128);
        assert_eq!(host["Memory"], 536_870_912);
        assert_eq!(host["MemorySwap"], host["Memory"]);
        assert_eq!(host["NanoCpus"], 1_000_000_000_i64);
        assert!(host.get("PortBindings").is_none());
        assert_eq!(spec["User"], "999:999");
        let mounts = host["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(
            mounts[0]["Source"],
            format!("/var/lib/romulus-play/pg/{id}/data")
        );
        assert_eq!(
            mounts[1]["Source"],
            format!("/var/lib/romulus-play/run/{id}")
        );
        let env: Vec<&str> = spec["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(env.contains(&"PLAY_EXTENSIONS=vector:public"));
        assert!(!env.iter().any(|e| e.starts_with("PLAY_TCP")));
    }

    #[test]
    fn helpers_get_three_capabilities_and_no_network() {
        let runner = Runner::new(config(&[]).unwrap());
        let spec = runner.files_spec("restore", Some(Uuid::from_u128(7)));
        assert_eq!(spec["HostConfig"]["NetworkMode"], "none");
        assert_eq!(
            spec["HostConfig"]["CapAdd"],
            json!(["CHOWN", "DAC_OVERRIDE", "FOWNER"])
        );
        assert_eq!(
            spec["Cmd"],
            json!(["restore", "00000000-0000-0000-0000-000000000007"])
        );
    }

    #[test]
    fn project_ids_are_stable_and_out_of_the_low_range() {
        let id = Uuid::from_u128(42);
        assert_eq!(project_id(id, "pg"), project_id(id, "pg"));
        assert!(project_id(id, "pg") >= 10_000);
        assert_ne!(project_id(id, "pg"), project_id(Uuid::from_u128(43), "pg"));
        // The socket directory has a quota of its own.
        assert_ne!(project_id(id, "pg"), project_id(id, "run"));
        assert!(project_id(id, "run") >= 10_000);
    }

    #[test]
    fn space_reads_as_df_prints_it() {
        assert_eq!(
            Space::parse("1048576 4194304\n"),
            Some(Space {
                free_bytes: 1_048_576,
                total_bytes: 4_194_304
            })
        );
        assert_eq!(Space::parse("df: /play: no such file"), None);
    }
}
