//! `romulus-api [serve | migrate | openapi]`.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use romulus_api::{
    AppState, Config, app, compile::SqlCompiler, config::ConfigError, db, error::Chain,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: romulus-api [serve | migrate | openapi]
  serve     run the API (default); applies pending migrations first
  migrate   apply pending migrations to DATABASE_URL and exit
  openapi   print the OpenAPI document as JSON (no database needed)";

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{USAGE}")]
    Usage,
    #[error("configuration")]
    Config(#[from] ConfigError),
    #[error("database")]
    Database(#[from] sqlx::Error),
    #[error("migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("i/o")]
    Io(#[from] io::Error),
    #[error("serializing the OpenAPI document")]
    Json(#[from] serde_json::Error),
}

enum Command {
    Serve,
    Migrate,
    Openapi,
}

fn main() -> ExitCode {
    // Before any thread exists: setting the environment is only sound then.
    #[cfg(debug_assertions)]
    load_dev_env();
    init_tracing();

    let command = match std::env::args().nth(1).as_deref() {
        None | Some("serve") => Command::Serve,
        Some("migrate") => Command::Migrate,
        Some("openapi") => Command::Openapi,
        Some(_) => return fail(&MainError::Usage),
    };
    let result = match command {
        Command::Openapi => print_openapi(),
        Command::Serve | Command::Migrate => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(MainError::from)
            .and_then(|runtime| {
                runtime.block_on(async {
                    match command {
                        Command::Migrate => migrate().await,
                        _ => serve().await,
                    }
                })
            }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&err),
    }
}

fn fail(err: &MainError) -> ExitCode {
    tracing::error!("{}", Chain(err));
    ExitCode::FAILURE
}

/// Dev convenience, debug builds only: `.env` (ports, from `scripts/dev-env`)
/// and the OIDC client the Zitadel provisioner wrote. Neither overrides a
/// variable already set. A release build reads its real environment only, so
/// a stray `.env` next to a deployed binary changes nothing.
#[cfg(debug_assertions)]
fn load_dev_env() {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename(".dev/zitadel/oidc.env");
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("romulus_api=debug,tower_http=info,sqlx=warn"));
    // stderr, so `romulus-api openapi > file` stays clean.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr);
    if std::env::var("ROMULUS_LOG_FORMAT").is_ok_and(|format| format == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

fn print_openapi() -> Result<(), MainError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &romulus_api::openapi())?;
    writeln!(stdout)?;
    Ok(())
}

async fn migrate() -> Result<(), MainError> {
    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::MIGRATOR.run(&pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn serve() -> Result<(), MainError> {
    let config = Config::from_env()?;
    if config.oidc.is_none() {
        tracing::warn!("no OIDC client configured (run `just up` to provision Zitadel)");
    }
    let pool = db::connect(&config.database_url).await?;
    // One API instance in the MVP, so it can own the migrations.
    db::MIGRATOR.run(&pool).await?;

    let listener = TcpListener::bind(config.api_addr).await?;
    tracing::info!(
        "listening on http://{} (public URL {})",
        listener.local_addr()?,
        config.public_url
    );
    // The peer address reaches handlers as `ConnectInfo` (the sign-in limiter).
    let compiler = SqlCompiler::sandbox(&config);
    if config.sandbox_url.is_none() {
        tracing::warn!("no sandbox configured (ROMULUS_SANDBOX_URL): SQL uploads answer 503");
    }
    let app = app(AppState::new(pool, config).with_compiler(compiler.clone()))
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    compiler.close().await;
    tracing::info!("shut down cleanly");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("cannot listen for ctrl-c: {err}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!("cannot listen for SIGTERM: {err}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down: finishing in-flight requests");
}
