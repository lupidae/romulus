use std::sync::Arc;

use axum::extract::FromRef;
use sqlx::PgPool;

use crate::{
    Config,
    auth::{events::SessionEvents, oidc::Oidc, password},
    collab::Hub,
    compile::SqlCompiler,
    imports::Turns,
    mail::Mailer,
    migration_tests::MigrationTests,
    orgs::events::MembershipEvents,
    playgrounds::{
        Playgrounds,
        host::{Host, Remote},
    },
    rate::Limits,
    versions::{diff::DiffCache, timeline::cache::TimelineCache},
};

/// Shared by every handler. Cheap to clone: a pool handle and `Arc`s.
///
/// Handlers can extract the parts directly (`State(db): State<PgPool>`,
/// `State(config): State<Arc<Config>>`) thanks to the `FromRef` impls.
#[derive(Clone, FromRef)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    /// The OIDC relying party and its discovery cache.
    pub oidc: Arc<Oidc>,
    /// Role changes and removals, for the collab server to close sockets.
    pub membership: MembershipEvents,
    /// Sign-outs, for the collab server to close the sockets of the session.
    pub sessions: SessionEvents,
    /// SQL → model, for uploads, SQL pushes and drafts.
    pub compiler: SqlCompiler,
    /// Per-token and per-person rate limits.
    pub limits: Arc<Limits>,
    /// The drafts open right now (the collab server); compiles with `compiler`.
    pub collab: Hub,
    /// Diffs between versions, computed once (versions never change).
    pub diffs: Arc<DiffCache>,
    /// Steps of version histories (the time machine), computed once per pair
    /// of versions.
    pub timelines: Arc<TimelineCache>,
    /// Migration imports waiting for the sandbox, organizations in turn.
    pub imports: Turns,
    /// The playground host, builds in progress, who has which console open.
    pub playgrounds: Playgrounds,
    /// Migration tests in flight: their queue, their progress.
    pub migration_tests: MigrationTests,
    /// Email (`ROMULUS_SMTP_*`), or off.
    pub mailer: Mailer,
    /// Password sign-in's limits per client, address and person.
    pub passwords: Arc<password::throttle::Limits>,
}

impl AppState {
    /// Without a SQL compiler (see [`AppState::with_compiler`]).
    pub fn new(db: PgPool, config: Config) -> Self {
        let config_playground = config.playground.clone();
        let mailer = Mailer::from_config(&config);
        Self {
            collab: Hub::new(db.clone(), SqlCompiler::unavailable()),
            db,
            oidc: Arc::new(Oidc::new(&config)),
            config: Arc::new(config),
            membership: MembershipEvents::new(),
            sessions: SessionEvents::new(),
            compiler: SqlCompiler::unavailable(),
            limits: Arc::new(Limits::default()),
            diffs: Arc::new(DiffCache::default()),
            timelines: Arc::new(TimelineCache::default()),
            imports: Turns::default(),
            playgrounds: Playgrounds::new(match &config_playground {
                Some(play) => Host::new(Remote::new(play.url.clone(), play.key.clone())),
                None => Host::off(),
            }),
            migration_tests: MigrationTests::default(),
            mailer,
            passwords: Arc::new(password::throttle::Limits::default()),
        }
    }

    /// A mailer of one's own (tests: `Mailer::memory`).
    #[must_use]
    pub fn with_mailer(mut self, mailer: Mailer) -> Self {
        self.mailer = mailer;
        self
    }

    /// A playground host of one's own (tests: a fake).
    #[must_use]
    pub fn with_playgrounds(mut self, host: Host) -> Self {
        self.playgrounds = Playgrounds::new(host);
        self
    }

    /// Uploads, pushes and drafts compile with `compiler` (the sandbox in
    /// production): one pool, whose prefix only one process may hold.
    #[must_use]
    pub fn with_compiler(mut self, compiler: SqlCompiler) -> Self {
        self.collab = Hub::new(self.db.clone(), compiler.clone());
        self.compiler = compiler;
        self
    }

    /// A collab hub of one's own (tests: shorter timings).
    #[must_use]
    pub fn with_collab(mut self, collab: Hub) -> Self {
        self.collab = collab;
        self
    }
}
