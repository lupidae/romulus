//! `romulus-api [serve | migrate | openapi | <admin command>]`.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use romulus_api::{
    AppState, Config,
    admin::{self, AdminError, SetupOutcome},
    app,
    compile::SqlCompiler,
    config::ConfigError,
    db,
    error::Chain,
    mail::MailError,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: romulus-api [command]
  serve       run the API (default); applies pending migrations first
  migrate     apply pending migrations to DATABASE_URL and exit
  openapi     print the OpenAPI document as JSON (no database needed)

password sign-in (ROMULUS_AUTH=password); links print on stdout, run on the server:
  setup --email EMAIL --name NAME --org ORG [--slug SLUG]
              the first admin and the organization; prints the link that sets
              the admin's password (24 h), or nothing once it is set
  password-link EMAIL      prints a one-time link to set a new password (1 h)
  unlock EMAIL             clears the sign-in lock of an address
  verify-email EMAIL       marks a password account's address verified
  auth-events [--email EMAIL] [--limit N]
                           the sign-in log, newest first, as JSON lines
  mail-test EMAIL          sends a test message through ROMULUS_SMTP_*

deleting on request (both: --dry-run first says what would go; the address,
or the admins, are emailed when it is done):
  delete-user EMAIL [--dry-run]
              every account with this address and what it alone holds (the
              organizations it is the only member of), its Zitadel user too
              (ROMULUS_ZITADEL_TOKEN); refused (exit 4) while it is the only
              admin of an organization others belong to
  delete-org SLUG [--dry-run]
              the organization and everything in it; refused (exit 4) while
              its subscription is live

hosted billing (no database needed):
  stripe-setup [--public-url URL] [--terms-url URL] [--privacy-url URL]
               [--webhook-url URL [--secret-file PATH]] [--live]
              products, prices (from the plan table) and the Customer Portal
              in the account of STRIPE_SECRET_KEY, idempotently; a live key
              needs --live. The webhook's secret goes to PATH, never stdout";

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
    #[error(transparent)]
    Admin(#[from] AdminError),
    #[error("stripe-setup")]
    StripeSetup(#[from] romulus_api::billing::setup::SetupError),
}

impl MainError {
    /// 2: `setup` on an install someone else set up. 3: `mail-test` with
    /// mail off. 4: a deletion refused until something is done first. 1:
    /// anything else.
    fn exit_code(&self) -> u8 {
        match self {
            Self::Admin(AdminError::AlreadyOwned(_)) => 2,
            Self::Admin(AdminError::Mail(MailError::Off)) => 3,
            Self::Admin(AdminError::Refused(_)) => 4,
            _ => 1,
        }
    }
}

enum Command {
    Serve,
    Migrate,
    Openapi,
    StripeSetup(romulus_api::billing::setup::Setup),
    Admin(AdminCommand),
}

enum AdminCommand {
    Setup(admin::Setup),
    PasswordLink(String),
    Unlock(String),
    VerifyEmail(String),
    AuthEvents { email: Option<String>, limit: i64 },
    MailTest(String),
    DeleteUser { email: String, dry_run: bool },
    DeleteOrg { slug: String, dry_run: bool },
}

/// `--flag value` pairs (and `--flag=value`) after the command, by name.
fn flags(
    args: &[String],
    allowed: &[&str],
) -> Result<std::collections::HashMap<String, String>, MainError> {
    let mut flags = std::collections::HashMap::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => (arg.clone(), args.next().ok_or(MainError::Usage)?.clone()),
        };
        let name = name.strip_prefix("--").ok_or(MainError::Usage)?;
        if !allowed.contains(&name) {
            return Err(MainError::Usage);
        }
        flags.insert(name.to_owned(), value);
    }
    Ok(flags)
}

fn parse(args: &[String]) -> Result<Command, MainError> {
    let rest = args.get(1..).unwrap_or_default();
    let one = || match rest {
        [value] => Ok(value.clone()),
        _ => Err(MainError::Usage),
    };
    Ok(match args.first().map(String::as_str) {
        None | Some("serve") => Command::Serve,
        Some("migrate") => Command::Migrate,
        Some("openapi") => Command::Openapi,
        Some("setup") => {
            let mut flags = flags(rest, &["email", "name", "org", "slug"])?;
            let mut take = |name| flags.remove(name).ok_or(MainError::Usage);
            Command::Admin(AdminCommand::Setup(admin::Setup {
                email: take("email")?,
                name: take("name")?,
                org: take("org")?,
                slug: take("slug").ok(),
            }))
        }
        Some("password-link") => Command::Admin(AdminCommand::PasswordLink(one()?)),
        Some("unlock") => Command::Admin(AdminCommand::Unlock(one()?)),
        Some("verify-email") => Command::Admin(AdminCommand::VerifyEmail(one()?)),
        Some("mail-test") => Command::Admin(AdminCommand::MailTest(one()?)),
        Some(command @ ("delete-user" | "delete-org")) => {
            let dry_run = rest.iter().any(|arg| arg == "--dry-run");
            let target = match rest
                .iter()
                .filter(|arg| *arg != "--dry-run")
                .collect::<Vec<_>>()
                .as_slice()
            {
                [target] if !target.starts_with("--") => (*target).clone(),
                _ => return Err(MainError::Usage),
            };
            Command::Admin(if command == "delete-user" {
                AdminCommand::DeleteUser {
                    email: target,
                    dry_run,
                }
            } else {
                AdminCommand::DeleteOrg {
                    slug: target,
                    dry_run,
                }
            })
        }
        Some("stripe-setup") => {
            let live = rest.iter().any(|arg| arg == "--live");
            let rest: Vec<String> = rest
                .iter()
                .filter(|arg| *arg != "--live")
                .cloned()
                .collect();
            let mut flags = flags(
                &rest,
                &[
                    "public-url",
                    "terms-url",
                    "privacy-url",
                    "webhook-url",
                    "secret-file",
                ],
            )?;
            Command::StripeSetup(romulus_api::billing::setup::Setup {
                public_url: flags.remove("public-url"),
                terms_url: flags.remove("terms-url"),
                privacy_url: flags.remove("privacy-url"),
                webhook_url: flags.remove("webhook-url"),
                secret_file: flags.remove("secret-file").map(Into::into),
                live,
            })
        }
        Some("auth-events") => {
            let flags = flags(rest, &["email", "limit"])?;
            let limit = match flags.get("limit") {
                Some(limit) => limit.parse().map_err(|_| MainError::Usage)?,
                None => 100,
            };
            Command::Admin(AdminCommand::AuthEvents {
                email: flags.get("email").cloned(),
                limit,
            })
        }
        Some(_) => return Err(MainError::Usage),
    })
}

fn main() -> ExitCode {
    // Before any thread exists: setting the environment is only sound then.
    #[cfg(debug_assertions)]
    load_dev_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = parse(&args);
    // An admin command talks to a person at a terminal: plain lines, even
    // where the server logs JSON.
    init_tracing(!matches!(
        command,
        Ok(Command::Admin(_) | Command::StripeSetup(_))
    ));
    let command = match command {
        Ok(command) => command,
        Err(err) => return fail(&err),
    };
    let result = match command {
        Command::Openapi => print_openapi(),
        command => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(MainError::from)
            .and_then(|runtime| {
                runtime.block_on(async {
                    match command {
                        Command::Migrate => migrate().await,
                        Command::Admin(command) => run_admin(command).await,
                        Command::StripeSetup(setup) => stripe_setup(&setup).await,
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
    ExitCode::from(err.exit_code())
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

fn init_tracing(server: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("romulus_api=debug,tower_http=info,sqlx=warn"));
    // stderr, so `romulus-api openapi > file` (and a printed link) stay clean.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr);
    if server && std::env::var("ROMULUS_LOG_FORMAT").is_ok_and(|format| format == "json") {
        builder.json().init();
    } else if server {
        builder.init();
    } else {
        builder.without_time().with_target(false).init();
    }
}

fn print_openapi() -> Result<(), MainError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &romulus_api::openapi())?;
    writeln!(stdout)?;
    Ok(())
}

/// `stripe-setup`: Stripe's products, prices, portal (and webhook) from the
/// plan table, in the account of `STRIPE_SECRET_KEY`.
async fn stripe_setup(setup: &romulus_api::billing::setup::Setup) -> Result<(), MainError> {
    let key = std::env::var("STRIPE_SECRET_KEY")
        .ok()
        .filter(|key| !key.is_empty());
    let mut stdout = io::stdout().lock();
    romulus_api::billing::setup::run(setup, key, &mut stdout).await?;
    Ok(())
}

async fn migrate() -> Result<(), MainError> {
    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::MIGRATOR.run(&pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// An admin command: on the database (migrated first, like `serve`), its
/// output on stdout, what it did in the log (stderr).
async fn run_admin(command: AdminCommand) -> Result<(), MainError> {
    let config = Config::from_env()?;
    if let AdminCommand::MailTest(to) = &command {
        admin::mail_test(&config, to).await?;
        tracing::info!("mail-test: the server accepted the message");
        return Ok(());
    }
    let pool = db::connect(&config.database_url).await?;
    db::MIGRATOR.run(&pool).await?;
    let mut stdout = io::stdout().lock();
    match command {
        AdminCommand::Setup(setup) => match admin::setup(&pool, &config, setup).await? {
            SetupOutcome::Link(link) => {
                tracing::info!(
                    "setup: open this link to set the admin's password (valid 24 hours, once)"
                );
                writeln!(stdout, "{link}")?;
            }
            SetupOutcome::AlreadySetUp { org } => {
                tracing::info!(%org, "setup: already set up, the admin has a password");
            }
        },
        AdminCommand::PasswordLink(email) => {
            let link = admin::password_link(&pool, &config, &email).await?;
            tracing::info!("password-link: valid 1 hour, once; hand it to its owner only");
            writeln!(stdout, "{link}")?;
        }
        AdminCommand::Unlock(email) => {
            let cleared = admin::unlock(&pool, &email).await?;
            tracing::info!(cleared, "unlock: done");
        }
        AdminCommand::VerifyEmail(email) => {
            let verified = admin::verify_email(&pool, &email).await?;
            tracing::info!(accounts = verified.accounts, "verify-email: done");
            if verified.skipped_sso > 0 {
                tracing::warn!(
                    accounts = verified.skipped_sso,
                    "verify-email: single sign-on accounts with this address were left alone (their identity provider verifies them)"
                );
            }
        }
        AdminCommand::AuthEvents { email, limit } => {
            for event in admin::auth_events(&pool, email.as_deref(), limit).await? {
                serde_json::to_writer(&mut stdout, &event)?;
                writeln!(stdout)?;
            }
        }
        // The plan first, refused or not; then the refusal (exit 4).
        AdminCommand::DeleteUser { email, dry_run } => {
            let mailer = romulus_api::mail::Mailer::from_config(&config);
            let done = admin::delete_user(&pool, &config, mailer, &email, dry_run).await?;
            for line in done.report() {
                writeln!(stdout, "{line}")?;
            }
            if let Some(refused) = done.refused() {
                return Err(AdminError::Refused(refused.to_owned()).into());
            }
        }
        AdminCommand::DeleteOrg { slug, dry_run } => {
            let mailer = romulus_api::mail::Mailer::from_config(&config);
            let done = admin::delete_org(&pool, &config, mailer, &slug, dry_run).await?;
            for line in done.report() {
                writeln!(stdout, "{line}")?;
            }
            if let Some(refused) = done.refused() {
                return Err(AdminError::Refused(refused.to_owned()).into());
            }
        }
        AdminCommand::MailTest(_) => unreachable!("handled above"),
    }
    Ok(())
}

async fn serve() -> Result<(), MainError> {
    let config = Config::from_env()?;
    if config.auth.oidc && config.oidc.is_none() {
        tracing::warn!("no OIDC client configured (run `just up` to provision Zitadel)");
    }
    if config.auth.password {
        tracing::info!(
            signup = ?config.signup,
            mail = config.mail.is_some(),
            "password sign-in is on"
        );
        let local = matches!(
            config.public_url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if config.public_url.scheme() != "https" && !local {
            tracing::warn!(
                public_url = %config.public_url,
                "passwords and session cookies travel in clear over http: serve romulus over https (docs/self-host/hardening.md)"
            );
        }
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
    let compiler = SqlCompiler::sandbox(&config);
    if config.sandbox_urls.is_empty() {
        tracing::warn!(
            "no sandbox configured (ROMULUS_SANDBOX_URL): SQL uploads answer 503, drafts do not compile"
        );
    } else {
        for major in romulus_api::domain::PgMajor::SUPPORTED {
            if !config.sandbox_urls.contains_key(&major) {
                tracing::warn!(%major, "no sandbox for this Postgres major: its databases do not compile");
            }
        }
    }
    let state = AppState::new(pool, config).with_compiler(compiler.clone());
    let collab = state.collab.clone();
    // Models imports left behind (a process stopped mid-import): now, and
    // from time to time.
    let sweeper = romulus_api::imports::staging::sweeper(
        state.db.clone(),
        state.imports.clone(),
        romulus_api::imports::staging::SWEEP_EVERY,
    );
    // Playgrounds past their lifetime, builds that will never finish,
    // orphans on the host: now, and every 5 minutes.
    let playground_sweeper = romulus_api::playgrounds::sweeper::sweeper(
        state.clone(),
        romulus_api::playgrounds::sweeper::SWEEP_EVERY,
    );
    let playgrounds = state.playgrounds.clone();
    // Password sign-in's stale failures, expired links, old events.
    let password_sweeper = state.config.auth.password.then(|| {
        romulus_api::auth::password::sweeper(
            state.db.clone(),
            romulus_api::auth::password::SWEEP_EVERY,
        )
    });
    // Billing: playground hours metered every minute, the emails it owes
    // (renewal reminders, payment notices) every hour.
    let billing_tasks = romulus_api::billing::tasks(&state);
    // Versions removed more than 30 days ago: now, and every hour.
    let purger = romulus_api::versions::removal::purger(
        state.db.clone(),
        romulus_api::versions::removal::PURGE_EVERY,
    );
    // Deleted databases and expired invitations 30 days on, inactive
    // accounts (when on): now, and every hour.
    let retention =
        romulus_api::retention::sweeper(state.clone(), romulus_api::retention::SWEEP_EVERY);
    if state.config.delete_inactive_accounts {
        tracing::info!("accounts unused for 3 years are deleted, after an email 30 days before");
    }
    // What a deletion ended, from any process (`romulus-api delete-org`
    // too): drafts closed, sockets of removed members.
    let deletions = romulus_api::retention::live::listen(state.clone());
    // The peer address reaches handlers as `ConnectInfo` (the sign-in limiter).
    let app = app(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Websockets outlive their request: close them (the clients
            // reconnect after the restart) and write every open draft.
            playgrounds.shutdown();
            collab.shutdown().await;
        })
        .await?;
    sweeper.abort();
    purger.abort();
    retention.abort();
    deletions.abort();
    if let Some(password_sweeper) = password_sweeper {
        password_sweeper.abort();
    }
    playground_sweeper.abort();
    for task in billing_tasks {
        task.abort();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn deletions_take_one_target_and_a_dry_run_anywhere() {
        for line in [
            "delete-user ada@acme.io --dry-run",
            "delete-user --dry-run ada@acme.io",
        ] {
            assert!(matches!(
                parse(&args(line)),
                Ok(Command::Admin(AdminCommand::DeleteUser { ref email, dry_run: true })) if email == "ada@acme.io"
            ));
        }
        assert!(matches!(
            parse(&args("delete-org acme")),
            Ok(Command::Admin(AdminCommand::DeleteOrg { ref slug, dry_run: false })) if slug == "acme"
        ));
        for wrong in ["delete-user", "delete-org a b", "delete-org --force acme"] {
            assert!(
                matches!(parse(&args(wrong)), Err(MainError::Usage)),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_refused_deletion_exits_4() {
        let refused = MainError::Admin(AdminError::Refused("cancel it first".to_owned()));
        assert_eq!(refused.exit_code(), 4);
        assert_eq!(MainError::Usage.exit_code(), 1);
    }
}
