//! A migration test, from its turn to its report, in a task of its own
//! (leaving the page does not stop it; Cancel does):
//!
//! 1. **its turn** ([`super::queue`]), its place in line live;
//! 2. **room on the host**: the playgrounds live and the tests running fit
//!    [`MAX_TOTAL`], and the volume has the playground's quota free; else
//!    it waits ([`ROOM_WAIT`] at most, saying why);
//! 3. **its playground**, built as any (the base version's schema, rows
//!    generated with the test's size, profile and seed); a host whose
//!    running playgrounds are all in use (`org_full`, `host_full`) is
//!    asked again until there is room;
//! 4. **the test** on it (`romulus-playground`'s `test`: the engine), its
//!    progress kept in memory for the page;
//! 5. **the report** stored, the verdict as its status;
//! 6. **the playground deleted**, whatever happened.

use std::time::Duration;

use romulus_migration_check::{DataSource, Event, Statement, Verdict};
use romulus_playground::protocol::{Access, Build, BuildEvent, Profile, Size, Test, TestEvent};
use uuid::Uuid;

use super::{TestProgress, TestStage, plan::Planned, queue::Place};
use crate::{
    AppState,
    domain::{
        MigrationTestStatus, MigrationTestVerdict, PlaygroundProfile, PlaygroundSize,
        PlaygroundStatus,
    },
    playgrounds::{
        BuildProgress, DISK_RESERVE_BYTES, MAJOR, MAX_TOTAL,
        build::{CREATE_LOCK, extensions, room},
        host::{Backend, Caller, HostError},
    },
};

/// How long a test waits for room on the host, at most.
pub const ROOM_WAIT: Duration = Duration::from_mins(10);
/// Between two looks for room.
const ROOM_EVERY: Duration = Duration::from_secs(10);
/// Rows sampled per table.
pub const SAMPLE: usize = 25;

/// The rows a test runs on.
pub enum Data {
    /// Generated for it (the test's size, profile and seed).
    Generated,
    /// A copy of this playground's (the source is never changed).
    Playground { source: Uuid, label: String },
}

/// A test to run.
pub struct Job {
    pub state: AppState,
    pub id: Uuid,
    pub org: Uuid,
    pub user: (Uuid, String),
    pub planned: Planned,
    pub data: Data,
    pub size: PlaygroundSize,
    pub profile: PlaygroundProfile,
    pub seed: i64,
    pub place: Place,
}

/// No room on the host yet: why.
struct Full(String);

/// How a test ended.
enum Outcome {
    Report(Box<romulus_migration_check::Report>),
    Error(String),
}

impl Job {
    pub async fn run(self) {
        let Job {
            state,
            id,
            org,
            user,
            planned,
            data,
            size,
            profile,
            seed,
            place,
        } = self;
        state
            .migration_tests
            .set_progress(id, Some(TestProgress::stage(TestStage::Queued)));
        // 1. Its turn.
        let turn = place.turn().await;
        let running = Running {
            state: state.clone(),
            id,
            org,
            user,
            planned,
            data,
            size,
            profile,
            seed,
        };
        let outcome = running.on_turn().await;
        drop(turn);
        state.migration_tests.set_progress(id, None);
        state.migration_tests.forget(id);
        let (status, verdict, rows_checked, report, failure) = match outcome {
            Outcome::Report(report) => (
                match report.verdict {
                    Verdict::Passed | Verdict::PassedUpOnly => MigrationTestStatus::Passed,
                    Verdict::NeedsAcknowledgement => MigrationTestStatus::NeedsAcknowledgement,
                    Verdict::Failed => MigrationTestStatus::Failed,
                },
                Some(MigrationTestVerdict::from(report.verdict)),
                !report.nothing_checked,
                serde_json::to_value(&report).ok(),
                None,
            ),
            Outcome::Error(message) => {
                (MigrationTestStatus::Error, None, true, None, Some(message))
            }
        };
        let recorded = sqlx::query!(
            "UPDATE migration_tests SET status = $2, verdict = $3, rows_checked = $4, report = $5,
                    failure = $6, finished_at = now()
             WHERE id = $1 AND status IN ('queued', 'running')",
            id,
            status as MigrationTestStatus,
            verdict as Option<MigrationTestVerdict>,
            rows_checked,
            report,
            failure.map(|f| f.chars().take(2000).collect::<String>()),
        )
        .execute(&state.db)
        .await;
        if let Err(err) = recorded {
            tracing::error!(test = %id, error = %err, "cannot record a migration test");
        }
    }
}

/// A job whose turn came.
struct Running {
    state: AppState,
    id: Uuid,
    org: Uuid,
    user: (Uuid, String),
    planned: Planned,
    data: Data,
    size: PlaygroundSize,
    profile: PlaygroundProfile,
    seed: i64,
}

impl Running {
    fn caller(&self) -> Caller {
        Caller {
            org: self.org,
            user: Some(self.user.clone()),
            access: Access::Develop,
        }
    }

    fn progress(&self, progress: TestProgress) {
        self.state
            .migration_tests
            .set_progress(self.id, Some(progress));
    }

    fn host_size(&self) -> Size {
        match self.size {
            PlaygroundSize::Small => Size::Small,
            PlaygroundSize::Medium => Size::Medium,
        }
    }

    /// Takes room for one more playground on the host and marks the test
    /// running on `playground`, in one transaction under the lock creating
    /// a playground takes ([`CREATE_LOCK`]): counted with the live
    /// playgrounds and the running tests, under [`MAX_TOTAL`], their disk
    /// quotas and this one within the volume. `Ok(false)`: the test was
    /// stopped meanwhile; `Err`: no room yet (why).
    async fn claim_room(&self, playground: Uuid) -> Result<bool, Full> {
        let room = room(&self.state).await.map_err(|e| Full(e.to_string()))?;
        let claimed = async {
            let mut tx = self.state.db.begin().await?;
            sqlx::query!(
                r#"SELECT 1 AS "locked!" FROM pg_advisory_xact_lock($1)"#,
                CREATE_LOCK
            )
            .fetch_one(&mut *tx)
            .await?;
            let held = sqlx::query!(
                r#"WITH live AS (
                       SELECT size FROM playgrounds WHERE status IN ('building', 'ready')
                       UNION ALL
                       SELECT size FROM migration_tests
                       WHERE status = 'running' AND playground_id IS NOT NULL
                   )
                   SELECT count(*) AS "live!",
                          count(*) FILTER (WHERE size = 'small') AS "small!",
                          count(*) FILTER (WHERE size = 'medium') AS "medium!"
                   FROM live"#
            )
            .fetch_one(&mut *tx)
            .await?;
            if held.live >= MAX_TOTAL {
                return Ok::<_, sqlx::Error>(Err(Full(
                    "The playground host holds as many playgrounds as it can.".to_owned(),
                )));
            }
            let quota = |n: i64, size: Size| u64::try_from(n).unwrap_or(0) * size.disk_bytes();
            let promised = quota(held.small, Size::Small) + quota(held.medium, Size::Medium);
            let wanted = self.host_size().disk_bytes();
            if promised + wanted > room.total.saturating_sub(DISK_RESERVE_BYTES)
                || room.free < wanted + DISK_RESERVE_BYTES
            {
                return Ok(Err(Full("The playground host is short of disk.".to_owned())));
            }
            let marked = sqlx::query!(
                "UPDATE migration_tests SET status = 'running', started_at = now(), playground_id = $2
                 WHERE id = $1 AND status = 'queued'",
                self.id,
                playground,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(Ok(marked.rows_affected() == 1))
        }
        .await;
        claimed.map_err(|e| Full(e.to_string()))?
    }

    async fn on_turn(&self) -> Outcome {
        let started = tokio::time::Instant::now();
        let playground = match super::new_id(&self.state.db).await {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(%error, "migration test: no playground id");
                return Outcome::Error("The test could not start: run it again.".to_owned());
            }
        };
        // 2. Room, taken with the test marked running.
        loop {
            match self.claim_room(playground).await {
                Ok(true) => break,
                // Cancelled meanwhile.
                Ok(false) => return Outcome::Error("The test was stopped.".to_owned()),
                Err(Full(why)) if started.elapsed() < ROOM_WAIT => {
                    let mut progress = TestProgress::stage(TestStage::Queued);
                    progress.waiting = Some(format!("{why} Waiting for room…"));
                    self.progress(progress);
                    tokio::time::sleep(ROOM_EVERY).await;
                }
                Err(Full(why)) => {
                    return Outcome::Error(format!(
                        "{why} The test waited {} minutes for room and gave up: try again later.",
                        ROOM_WAIT.as_secs() / 60
                    ));
                }
            }
        }
        let outcome = self.on_playground(playground).await;
        // 6. The playground goes, whatever happened.
        self.destroy(playground).await;
        outcome
    }

    async fn destroy(&self, playground: Uuid) {
        if let Ok(backend) = self.state.playgrounds.host.backend() {
            match backend.destroy(playground, self.caller()).await {
                Ok(()) => {}
                Err(err) if err.code() == Some("not_found") => {}
                Err(err) => {
                    // The sweeper takes what is left (its row says no test holds it).
                    tracing::warn!(playground = %playground, error = %err, "cannot delete a test's playground yet");
                }
            }
        }
        let _ = sqlx::query!(
            "UPDATE migration_tests SET playground_id = NULL WHERE id = $1",
            self.id
        )
        .execute(&self.state.db)
        .await;
    }

    async fn on_playground(&self, playground: Uuid) -> Outcome {
        let backend = match self.state.playgrounds.host.backend() {
            Ok(backend) => backend,
            Err(err) => return Outcome::Error(err.to_string()),
        };
        let (build, tables_total) = match self.spec().await {
            Ok(spec) => spec,
            Err(message) => return Outcome::Error(message),
        };
        let rows = match &self.data {
            Data::Playground { source, .. } => self
                .fork_into(backend, playground, *source, &build)
                .await
                .map(|()| String::new()),
            Data::Generated => {
                self.build_rows(backend, playground, &build, tables_total)
                    .await
            }
        };
        let test = match rows.and_then(|generator| self.test(build.model, generator)) {
            Ok(test) => test,
            Err(message) => return Outcome::Error(message),
        };
        self.run_test(backend, playground, test).await
    }

    /// What its playground holds (step 3): the base version's schema (as a
    /// playground's script), its extensions and model; and how many tables.
    async fn spec(&self) -> Result<(Build, usize), String> {
        let sql = self.planned.base.sql().map_err(|e| e.to_string())?;
        let Some(script) = self
            .state
            .compiler
            .playground_script(MAJOR, &sql)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Err(format!(
                "{}'s SQL no longer compiles here, so no playground can hold it.",
                self.planned.base_label
            ));
        };
        let base_schema = self.planned.base.schema().map_err(|e| e.to_string())?;
        let tables_total = base_schema
            .entities
            .iter()
            .filter(|e| e.kind.is_table())
            .count();
        let build = Build {
            size: self.host_size(),
            profile: match self.profile {
                PlaygroundProfile::Realistic => Profile::Realistic,
                PlaygroundProfile::EdgeCases => Profile::EdgeCases,
            },
            seed: u64::try_from(self.seed).unwrap_or(0),
            schema_sql: script,
            extensions: extensions(&base_schema),
            model: self.planned.base.model.clone(),
        };
        Ok((build, tables_total))
    }

    /// A copy of someone's playground instead (step 3; it pauses a moment):
    /// still at the base version as built, and not copied in the last
    /// minute ([`super::FORK_EVERY`]).
    async fn fork_into(
        &self,
        backend: &dyn Backend,
        playground: Uuid,
        source: Uuid,
        build: &Build,
    ) -> Result<(), String> {
        let wait = self.state.migration_tests.fork_turn(source);
        if !wait.is_zero() {
            let mut progress = TestProgress::stage(TestStage::Building);
            progress.waiting = Some(
                "That playground was copied a moment ago: waiting, so it is not paused twice in a row…"
                    .to_owned(),
            );
            self.progress(progress);
            tokio::time::sleep(wait).await;
        }
        // As it is now, not as it was when the test was asked for.
        let now = sqlx::query!(
            r#"SELECT status AS "status: PlaygroundStatus", version_id, migrated_by_run IS NOT NULL AS "migrated!"
               FROM playgrounds WHERE id = $1"#,
            source,
        )
        .fetch_optional(&self.state.db)
        .await
        .map_err(|e| e.to_string())?;
        let fresh = "Test on fresh generated data instead.";
        match now {
            None => return Err(format!("The playground to copy is gone. {fresh}")),
            Some(row) if row.status != PlaygroundStatus::Ready => {
                return Err(format!("The playground to copy is not ready. {fresh}"));
            }
            Some(row) if row.version_id != self.planned.base.id => {
                return Err(format!(
                    "The playground to copy is at another version than {} now. {fresh}",
                    self.planned.base_label
                ));
            }
            Some(row) if row.migrated => {
                return Err(format!(
                    "The playground to copy holds the rows a migration run left, not {}'s. {fresh}",
                    self.planned.base_label
                ));
            }
            Some(_) => {}
        }
        let mut progress = TestProgress::stage(TestStage::Building);
        progress.waiting = Some("Copying the playground's rows…".to_owned());
        self.progress(progress);
        let fork = romulus_playground::protocol::Fork {
            source,
            size: build.size,
            extensions: build.extensions.clone(),
        };
        backend
            .fork(playground, self.caller(), fork)
            .await
            .map_err(|err| {
                format!(
                    "The playground's rows could not be copied: {}",
                    refused(&err)
                )
            })
    }

    /// Its own rows, generated (step 3), waiting (up to [`ROOM_WAIT`])
    /// while every running playground is in use: the generator's version.
    async fn build_rows(
        &self,
        backend: &dyn Backend,
        playground: Uuid,
        build: &Build,
        tables_total: usize,
    ) -> Result<String, String> {
        let built_at = tokio::time::Instant::now();
        loop {
            let mut progress = TestProgress::stage(TestStage::Building);
            progress.build = Some(BuildProgress {
                stage: romulus_playground::protocol::Stage::Starting,
                tables_done: 0,
                tables_total,
                table: None,
            });
            self.progress(progress);
            let mut events = backend
                .build(playground, self.caller(), build.clone())
                .await
                .map_err(|err| refused(&err))?;
            let mut failed = None;
            while let Some(event) = events.recv().await {
                match event {
                    BuildEvent::Stage { stage } => {
                        let mut progress = TestProgress::stage(TestStage::Building);
                        progress.build = Some(BuildProgress {
                            stage,
                            tables_done: 0,
                            tables_total,
                            table: None,
                        });
                        self.progress(progress);
                    }
                    BuildEvent::Table {
                        schema,
                        table,
                        done,
                        total,
                        ..
                    } => {
                        let mut progress = TestProgress::stage(TestStage::Building);
                        progress.build = Some(BuildProgress {
                            stage: romulus_playground::protocol::Stage::Rows,
                            tables_done: done,
                            tables_total: total.max(done),
                            table: Some(format!("{schema}.{table}")),
                        });
                        self.progress(progress);
                    }
                    BuildEvent::Ready { generator, .. } => return Ok(generator),
                    BuildEvent::Failed { message, .. } => {
                        failed = Some(message);
                        break;
                    }
                }
            }
            let message = failed
                .unwrap_or_else(|| "The playground host stopped during the build.".to_owned());
            // Every running playground is in use: wait for one to be free.
            let full = message.contains("in use") && built_at.elapsed() < ROOM_WAIT;
            if !full {
                return Err(format!("Its playground could not be built: {message}"));
            }
            let mut progress = TestProgress::stage(TestStage::Queued);
            progress.waiting = Some(format!("{message} Waiting for room…"));
            self.progress(progress);
            tokio::time::sleep(ROOM_EVERY).await;
        }
    }

    /// What the service runs (step 4): the statements (policy roles
    /// `PUBLIC`, as the playground's schema has them), the models, the
    /// rows' source.
    fn test(&self, base: serde_json::Value, generator: String) -> Result<Test, String> {
        let public = |m: &romulus_migration_check::Migration| -> Vec<Statement> {
            m.statements
                .iter()
                .map(|s| Statement {
                    sql: romulus_sandbox::playground::migration(&s.sql),
                    ..s.clone()
                })
                .collect()
        };
        Ok(Test {
            base,
            target: serde_json::to_value(&self.planned.target).map_err(|e| e.to_string())?,
            up: public(&self.planned.up),
            up_reordered: self.planned.up.reordered.clone(),
            down: self.planned.down.as_ref().map(public),
            down_reordered: self
                .planned
                .down
                .as_ref()
                .map(|d| d.reordered.clone())
                .unwrap_or_default(),
            sample: SAMPLE,
            base_label: self.planned.base_label.clone(),
            target_label: self.planned.target_label.clone(),
            data: match &self.data {
                Data::Playground { label, .. } => DataSource::Existing {
                    label: label.clone(),
                },
                Data::Generated => DataSource::Generated {
                    size: match self.size {
                        PlaygroundSize::Small => "small".to_owned(),
                        PlaygroundSize::Medium => "medium".to_owned(),
                    },
                    profile: match self.profile {
                        PlaygroundProfile::Realistic => "realistic".to_owned(),
                        PlaygroundProfile::EdgeCases => "edge_cases".to_owned(),
                    },
                    seed: format!("{:06x}", self.seed),
                    generator,
                },
            },
        })
    }

    /// The test (step 4), its progress followed: the report.
    async fn run_test(&self, backend: &dyn Backend, playground: Uuid, test: Test) -> Outcome {
        let mut progress = TestProgress::stage(TestStage::Testing);
        self.progress(progress.clone());
        let mut events = match backend.test(playground, self.caller(), test).await {
            Ok(events) => events,
            Err(err) => return Outcome::Error(refused(&err)),
        };
        while let Some(event) = events.recv().await {
            match event {
                TestEvent::Progress { event } => {
                    progress.follow(&event);
                    self.progress(progress.clone());
                }
                TestEvent::Done { report } => return Outcome::Report(report),
                TestEvent::Failed { message } => return Outcome::Error(message),
            }
        }
        Outcome::Error("The playground host stopped during the test.".to_owned())
    }
}

fn refused(err: &HostError) -> String {
    match err {
        HostError::Refused { message, .. } => message.clone(),
        HostError::Off => "Playgrounds are not available on this server.".to_owned(),
        HostError::Unavailable(_) => {
            "The playground host is not answering. Try again in a moment.".to_owned()
        }
    }
}

impl TestProgress {
    /// Follows what the engine says.
    fn follow(&mut self, event: &Event) {
        match event {
            Event::Phase { phase } => {
                self.phase = Some(*phase);
                self.statement = None;
            }
            Event::Running {
                direction,
                index,
                total,
            } => {
                self.direction = Some(*direction);
                self.statement = Some(*index);
                self.statements = Some(*total);
            }
            Event::Statement { .. } => {}
            Event::PhaseDone { phase } => self.done.push(super::PhaseDone {
                phase: phase.phase,
                status: phase.status,
            }),
        }
    }
}
