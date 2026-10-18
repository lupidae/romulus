//! `romulus-playground`: the playground service, on `ROMULUS_PLAY_ADDR`,
//! until SIGTERM or ctrl-c. Configuration: `romulus_playground::server::Config`.

use std::{process::ExitCode, sync::Arc};

use romulus_play_runner::Runner;
use romulus_playground::server::{self, Config, Host, seed::FakeData};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // Development: the worktree's .env (ports, keys); the real environment
    // wins. A release build reads its real environment only.
    #[cfg(debug_assertions)]
    let _ = dotenvy::dotenv();
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("romulus_playground=info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if std::env::var("ROMULUS_LOG_FORMAT").is_ok_and(|f| f == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!("playground service: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("playground service: {err}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("configuration: {0}")]
    Config(#[from] server::host::ConfigError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

async fn run() -> Result<(), Error> {
    let config = Config::from_env()?;
    let runner = Runner::new(&config.runner_socket);
    let host = Host::new(config.clone(), runner, Arc::new(FakeData));
    match host.runner.health().await {
        Ok(health) if health.docker && health.runtime_available && health.image => {}
        Ok(health) => tracing::error!(?health, "the runner is not ready"),
        Err(err) => tracing::error!(error = %err, "the runner is not answering"),
    }
    host.stop_strays().await;
    let reaper = server::reaper(Arc::clone(&host));
    let listener = TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, "playground service: listening");
    let closing = Arc::clone(&host);
    axum::serve(listener, server::router(Arc::clone(&host)))
        .with_graceful_shutdown(async move {
            shutdown().await;
            // Console sessions outlive their request: close them.
            let _ = closing.closing.send(true);
        })
        .await?;
    reaper.abort();
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
