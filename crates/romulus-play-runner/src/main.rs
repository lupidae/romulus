//! `romulus-play-runner`: serves the runner's operations on its Unix socket
//! (`ROMULUS_PLAY_RUNNER_SOCKET`) until SIGTERM or ctrl-c. Configuration:
//! `romulus_play_runner::server::Config`.

use std::{os::unix::fs::PermissionsExt, process::ExitCode};

use romulus_play_runner::server::{Config, Runner, router};
use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("romulus_play_runner=info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if std::env::var("ROMULUS_LOG_FORMAT").is_ok_and(|f| f == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!("runner: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("runner: {err}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("configuration: {0}")]
    Config(#[from] romulus_play_runner::server::ConfigError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

async fn run() -> Result<(), Error> {
    let config = Config::from_env()?;
    let runner = Runner::new(config.clone());
    if let Err(err) = runner.prepare().await {
        tracing::error!(error = %err, "runner: cannot prepare the playground volume");
    }
    let health = runner.health().await;
    if !health.docker {
        tracing::error!(docker = %config.docker.display(), "runner: Docker is not answering");
    } else if !health.runtime_available {
        tracing::error!(runtime = %config.runtime, "runner: Docker has no such runtime");
    }
    if !health.image {
        tracing::error!(image = %config.image, "runner: the playground image is missing");
    }
    match (&health.filesystem, health.quotas) {
        (_, true) => tracing::info!("runner: XFS project quotas on"),
        (Some(fs), false) => tracing::warn!(
            filesystem = %fs,
            "runner: no disk quotas (the playground volume is not XFS, or no quota image): local development only"
        ),
        (None, false) => {
            tracing::warn!("runner: no disk quotas (the volume's filesystem is unknown)");
        }
    }

    if let Some(dir) = config.listen.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket left by a runner that did not stop cleanly.
    let _ = std::fs::remove_file(&config.listen);
    let listener = UnixListener::bind(&config.listen)?;
    // Whoever mounts the socket's directory may call: only the service does.
    std::fs::set_permissions(&config.listen, std::fs::Permissions::from_mode(0o666))?;
    tracing::info!(
        socket = %config.listen.display(),
        runtime = %config.runtime,
        scope = %config.scope,
        tcp = config.tcp,
        "runner: listening"
    );
    axum::serve(listener, router(runner))
        .with_graceful_shutdown(shutdown())
        .await?;
    let _ = std::fs::remove_file(&config.listen);
    Ok(())
}

async fn shutdown() {
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        () = async {
            match terminate.as_mut() {
                Some(signal) => { signal.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {},
    }
}
