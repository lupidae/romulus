//! Migration tests: who may run and read them, what each kind tests (a
//! draft as reviewed, two versions, a version's own migration), the queue
//! and Cancel, the report kept, the playground deleted after, and a test
//! left behind by a restart. The host is a fake ([`TestHost`]): the engine
//! is tested in `crates/romulus-migration-check`, the service's `test`
//! in `crates/romulus-playground/tests`.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::http::StatusCode;
use romulus_api::playgrounds::{
    host::{Backend, BoxFuture, Caller, ConsoleLink, Host, HostError},
    sweeper,
};
use romulus_migration_check::{
    DataLoss, DataSource, Direction, DownStatus, Event, Flag, LossKind, MigrationReport, Phase,
    PhaseReport, Report, StatementRef, StatementResult, StatementStatus, Status as CheckStatus,
    Verdict,
};
use romulus_playground::protocol::{
    Build, BuildEvent, Fork, Health, Listed, Listing, Migrate, MigrationEvent, Stage, Status,
    TableRows, Test, TestEvent,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use super::{
    harness::{Team, TestApp, database},
    live::{Client, live_app, model_of, quick, serve, version},
};

#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "switches each test flips")]
struct Calls {
    builds: Vec<(Uuid, Build)>,
    forks: Vec<(Uuid, Fork)>,
    tests: Vec<(Uuid, Test, Caller)>,
    destroyed: Vec<Uuid>,
    live: Vec<Uuid>,
    /// Builds fail with this message.
    build_fails: Option<String>,
    /// Tests wait for `release` before answering.
    hold: bool,
    /// Tests answer with a failed report.
    fail: bool,
    /// Tests answer with this verdict (found undeclared loss…).
    verdict: Option<Verdict>,
    /// Tests find data loss no statement declares (the verdict the
    /// engine's own rule gives).
    undeclared: bool,
    /// Tests answer that every table touched was empty.
    nothing_checked: bool,
    /// Tests fail at the first up statement containing this.
    fail_on: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct TestHost {
    calls: Arc<Mutex<Calls>>,
    release: Arc<Notify>,
}

impl TestHost {
    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tests fail at the first up statement containing `marker`.
    pub(super) fn fail_on(&self, marker: &str) {
        self.calls().fail_on = Some(marker.to_owned());
    }

    /// The up migrations tested so far, in order.
    pub(super) fn tested(&self) -> Vec<Vec<String>> {
        self.calls()
            .tests
            .iter()
            .map(|(_, test, _)| test.up.iter().map(|s| s.sql.clone()).collect())
            .collect()
    }
}

/// What a hand-written statement lost without saying so.
fn undeclared_loss() -> DataLoss {
    DataLoss {
        table: "public.customers".to_owned(),
        column: Some("id".to_owned()),
        kind: LossKind::ValuesChanged,
        rows: 3,
        sampled: 10,
        count_before: None,
        count_after: None,
        examples: Vec::new(),
        expected: false,
        status: CheckStatus::NeedsAcknowledgement,
        message: "public.customers.id: 3 of 10 sampled values do not convert back.".to_owned(),
        cause: Some(StatementRef {
            direction: Direction::Up,
            index: 0,
            hazard: None,
            flag: Flag::WrittenUndeclared,
        }),
    }
}

/// The report a test of `test` gives, failing at up statement `failed_at`;
/// its verdict the engine's own rule ([`romulus_migration_check::verdict`]).
fn report(test: &Test, failed_at: Option<usize>, undeclared: bool) -> Report {
    let failed = failed_at.is_some();
    let at = failed_at.unwrap_or(usize::MAX);
    let results = |statements: &[romulus_migration_check::Statement]| MigrationReport {
        passed: !failed,
        statements: statements
            .iter()
            .enumerate()
            .map(|(index, s)| StatementResult {
                index,
                sql: s.sql.clone(),
                status: match index.cmp(&at) {
                    std::cmp::Ordering::Less => StatementStatus::Passed,
                    std::cmp::Ordering::Equal => StatementStatus::Failed,
                    std::cmp::Ordering::Greater => StatementStatus::NotRun,
                },
                hazard: s.hazard.clone(),
                duration_ms: 2,
                locks: Vec::new(),
                locks_documented: false,
                rewrote: Vec::new(),
                rows_changed: None,
                tag: None,
                error: (index == at).then(|| romulus_migration_check::SqlError {
                    sqlstate: "23502".to_owned(),
                    message: "column \"email\" contains null values".to_owned(),
                    ..Default::default()
                }),
                offending: None,
            })
            .collect(),
        failed_at,
        duration_ms: 4,
    };
    let phases = if failed {
        vec![PhaseReport {
            phase: Phase::Up,
            status: CheckStatus::Failed,
            duration_ms: 4,
            summary: "Statement 1 failed.".to_owned(),
            checks: Vec::new(),
        }]
    } else {
        Vec::new()
    };
    let data_loss = if undeclared && !failed {
        vec![undeclared_loss()]
    } else {
        Vec::new()
    };
    let down_status = if failed || test.down.is_none() {
        DownStatus::NotProvided
    } else {
        DownStatus::Ran
    };
    Report {
        verdict: romulus_migration_check::verdict(&phases, &data_loss, down_status),
        headline: if failed {
            format!(
                "Failed at statement {} of {} of the up migration: column \"email\" contains \
                 null values",
                at + 1,
                test.up.len()
            )
        } else {
            "Up and down ran on 10 rows. Everything came back.".to_owned()
        },
        data: test.data.clone(),
        sample: test.sample,
        tables: 1,
        rows: 10,
        phases,
        up: results(&test.up),
        down_status,
        down: (!failed)
            .then(|| test.down.as_deref().map(results))
            .flatten(),
        data_loss,
        nothing_checked: false,
        empty_tables: Vec::new(),
        not_compared: Vec::new(),
        locks_from: "pg_locks".to_owned(),
        duration_ms: 9,
    }
}

impl Backend for TestHost {
    fn build(
        &self,
        id: Uuid,
        _: Caller,
        build: Build,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<BuildEvent>, HostError>> {
        let mut calls = self.calls();
        calls.builds.push((id, build));
        calls.live.push(id);
        let fails = calls.build_fails.clone();
        drop(calls);
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(8);
            tokio::spawn(async move {
                let _ = tx.send(BuildEvent::Stage { stage: Stage::Rows }).await;
                let _ = tx
                    .send(match fails {
                        Some(message) => BuildEvent::Failed {
                            stage: Stage::Rows,
                            message,
                        },
                        None => BuildEvent::Ready {
                            tables: vec![TableRows {
                                schema: "public".into(),
                                table: "customers".into(),
                                rows: 10,
                            }],
                            report: json!({}),
                            generator: "1".to_owned(),
                            duration_ms: 3,
                        },
                    })
                    .await;
            });
            Ok(rx)
        })
    }

    fn test(
        &self,
        id: Uuid,
        caller: Caller,
        test: Test,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<TestEvent>, HostError>> {
        let mut calls = self.calls();
        calls.tests.push((id, test.clone(), caller));
        let hold = calls.hold;
        let fail = if calls.fail {
            Some(0)
        } else {
            calls
                .fail_on
                .as_ref()
                .and_then(|marker| test.up.iter().position(|s| s.sql.contains(marker)))
        };
        let (verdict, nothing_checked, undeclared) =
            (calls.verdict, calls.nothing_checked, calls.undeclared);
        drop(calls);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(8);
            tokio::spawn(async move {
                let _ = tx
                    .send(TestEvent::Progress {
                        event: Event::Phase { phase: Phase::Up },
                    })
                    .await;
                let _ = tx
                    .send(TestEvent::Progress {
                        event: Event::Running {
                            direction: Direction::Up,
                            index: 0,
                            total: test.up.len(),
                        },
                    })
                    .await;
                if hold {
                    release.notified().await;
                }
                let mut report = report(&test, fail, undeclared);
                if let Some(verdict) = verdict {
                    report.verdict = verdict;
                }
                report.nothing_checked = nothing_checked;
                let _ = tx
                    .send(TestEvent::Done {
                        report: Box::new(report),
                    })
                    .await;
            });
            Ok(rx)
        })
    }

    fn fork(&self, id: Uuid, _: Caller, fork: Fork) -> BoxFuture<'_, Result<(), HostError>> {
        let mut calls = self.calls();
        calls.forks.push((id, fork));
        calls.live.push(id);
        Box::pin(async { Ok(()) })
    }

    fn migrate(
        &self,
        _: Uuid,
        _: Caller,
        _: Migrate,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<MigrationEvent>, HostError>> {
        Box::pin(async { Err(HostError::Off) })
    }

    fn reset(&self, _: Uuid, _: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        Box::pin(async { Err(HostError::Off) })
    }

    fn destroy(&self, id: Uuid, _: Caller) -> BoxFuture<'_, Result<(), HostError>> {
        let mut calls = self.calls();
        calls.destroyed.push(id);
        calls.live.retain(|l| *l != id);
        Box::pin(async { Ok(()) })
    }

    fn status(&self, _: Uuid, _: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        Box::pin(async { Ok(Status::default()) })
    }

    fn list(&self) -> BoxFuture<'_, Result<Listing, HostError>> {
        let playgrounds = self
            .calls()
            .live
            .iter()
            .map(|id| Listed {
                id: *id,
                running: true,
            })
            .collect();
        Box::pin(async move { Ok(Listing { playgrounds }) })
    }

    fn console(&self, _: Uuid, _: Caller) -> BoxFuture<'_, Result<ConsoleLink, HostError>> {
        Box::pin(async { Err(HostError::Off) })
    }

    fn health(&self) -> BoxFuture<'_, Result<Health, HostError>> {
        Box::pin(async {
            Ok(Health {
                ok: true,
                free_bytes: Some(100 << 30),
                total_bytes: Some(200 << 30),
                ..Health::default()
            })
        })
    }
}

const V1: &str = "CREATE TABLE public.customers (id bigint PRIMARY KEY);\n";
const V2: &str = "CREATE TABLE public.customers (id bigint PRIMARY KEY);\nCREATE TABLE public.orders (id bigint PRIMARY KEY);\n";

/// An app with the fake host, team `acme`, database `shop` at v1 and v2.
pub(super) async fn setup(db: &PgPool) -> (TestApp, Team, TestHost) {
    let live = live_app(db.clone(), quick());
    let host = TestHost::default();
    let mut state = live.state.clone().with_playgrounds(Host::new(host.clone()));
    state.compiler = state.collab.compiler().clone();
    let app = TestApp::with_state(state);
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({})).await;
    version(db, shop, 2, V2, json!({})).await;
    (app, team, host)
}

fn path(team: &Team, rest: &str) -> String {
    team.path(&format!("/databases/shop/migration-tests{rest}"))
}

/// Polls the test until it is over.
async fn finished(app: &TestApp, team: &Team, id: &str) -> Value {
    finished_in(app, team, "shop", id).await
}

/// Polls a test of database `db` until it is over.
async fn finished_in(app: &TestApp, team: &Team, db: &str, id: &str) -> Value {
    for _ in 0..200 {
        let got = app
            .as_user(&team.reviewer)
            .get(&team.path(&format!("/databases/{db}/migration-tests/{id}")))
            .await
            .json();
        if !matches!(got["status"].as_str(), Some("queued" | "running")) {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the test never finished");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn developers_test_two_versions_and_everyone_reads_the_report(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let body = json!({ "from": 1, "to": 2, "seed": "7f3a91" });
    for who in [&team.reviewer, &team.outsider] {
        let refused = app.as_user(who).post(&path(&team, ""), &body).await;
        assert!(
            matches!(
                refused.status,
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "{}",
            refused.status
        );
    }
    let created = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &body)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let test = created.json();
    assert_eq!(test["source"], "versions");
    assert_eq!(
        (test["from"].as_i64(), test["to"].as_i64()),
        (Some(1), Some(2))
    );
    assert_eq!(test["down"], "generated");
    assert_eq!(test["data"]["seed"], "7f3a91");
    assert!(
        test["up_sql"].as_str().unwrap().contains("orders"),
        "{test}"
    );
    let id = test["id"].as_str().unwrap().to_owned();

    let done = finished(&app, &team, &id).await;
    assert_eq!(done["status"], "passed", "{done}");
    assert_eq!(done["report"]["verdict"], "passed");
    assert_eq!(done["summary"]["up_statements"], 1);
    assert_eq!(done["created_by"]["name"], "Developer-acme");

    // What the host was asked: a playground of v1 built, the migration
    // tested on it both ways, the playground deleted after.
    {
        let calls = host.calls();
        let (playground, build) = &calls.builds[0];
        assert_eq!(build.seed, 0x7f_3a91);
        let (tested, test, caller) = &calls.tests[0];
        assert_eq!(tested, playground);
        assert_eq!(caller.access, romulus_playground::protocol::Access::Develop);
        assert_eq!(
            (test.base_label.as_str(), test.target_label.as_str()),
            ("v1", "v2")
        );
        assert!(
            test.up[0].sql.contains("CREATE TABLE public.orders"),
            "{:?}",
            test.up
        );
        assert!(
            test.down.as_ref().unwrap()[0]
                .sql
                .contains("DROP TABLE public.orders")
        );
        assert!(test.up[0].planned);
        assert!(matches!(&test.data, DataSource::Generated { seed, .. } if seed == "7f3a91"));
        assert_eq!(calls.destroyed, vec![*playground]);
    }

    // Everyone in the organization sees it in the pair's history; outsiders
    // do not.
    let listed = app
        .as_user(&team.reviewer)
        .get(&path(&team, "?from=1&to=2"))
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(listed[0]["report"].is_null(), "lists carry summaries only");
    assert_eq!(listed[0]["summary"]["verdict"], "passed");
    let other = app
        .as_user(&team.reviewer)
        .get(&path(&team, "?from=2&to=1"))
        .await
        .json();
    assert_eq!(other.as_array().unwrap().len(), 0);
    let outsider = app
        .as_user(&team.outsider)
        .get(&path(&team, &format!("/{id}")))
        .await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_is_tested_as_reviewed(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let draft: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let addr = serve(&app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &team.developer,
    )
    .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(
                txn,
                "CREATE TABLE public.invoices (id bigint PRIMARY KEY);\n",
            );
        })
        .await;
    alice
        .until(|c| {
            let expected = romulus_api::collab::compile::sha256_hex(&c.sql());
            c.events
                .iter()
                .any(|e| e["type"] == "compile" && e["sha256"] == expected)
        })
        .await;
    let preview = app
        .as_user(&team.developer)
        .get(&team.path(&format!("/databases/shop/drafts/{draft}/publish")))
        .await
        .json();
    let sha256 = preview["sha256"].as_str().unwrap().to_owned();

    // Not what was reviewed: refused, nothing queued.
    let stale = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": "0".repeat(64) }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "draft_changed");
    let missing = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "draft_id": draft }))
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);

    let created = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": sha256 }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let test = created.json();
    assert_eq!(test["source"], "draft");
    assert_eq!(test["draft"]["id"], draft.to_string());
    assert_eq!(test["sha256"], sha256);
    assert_eq!(test["from"], 2);
    let done = finished(&app, &team, test["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "passed", "{done}");
    {
        let calls = host.calls();
        let (_, test, _) = &calls.tests[0];
        assert!(
            test.target_label.starts_with("the draft \""),
            "{}",
            test.target_label
        );
        assert_eq!(test.base_label, "v2");
        assert!(
            test.up.iter().any(|s| s.sql.contains("invoices")),
            "{:?}",
            test.up
        );
    }

    // The publish dialog says a test of exactly this migration passed.
    let preview = app
        .as_user(&team.developer)
        .get(&team.path(&format!("/databases/shop/drafts/{draft}/publish")))
        .await
        .json();
    assert_eq!(preview["test_gate"]["required"], false, "{preview}");
    let history = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("?draft={draft}")))
        .await
        .json();
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["current"], true, "{history}");

    // Edited since: the test is of another text.
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots
                .sql
                .push(txn, "CREATE TABLE public.notes (id bigint PRIMARY KEY);\n");
        })
        .await;
    alice
        .until(|c| {
            let expected = romulus_api::collab::compile::sha256_hex(&c.sql());
            c.events
                .iter()
                .any(|e| e["type"] == "compile" && e["sha256"] == expected)
        })
        .await;
    let history = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("?draft={draft}")))
        .await
        .json();
    assert_eq!(history[0]["current"], false, "{history}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_s_written_migration_runs_up_only_unless_a_down_is_asked_for(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let shop: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'shop'")
        .fetch_one(&db)
        .await
        .unwrap();
    // An uploaded version has no migration of its own.
    let refused = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "version": 2 }))
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "no_written_migration");
    let first = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "version": 1 }))
        .await;
    assert_eq!(first.code(), "no_written_migration");

    // v3, published from a migration written by hand (versions are
    // immutable: written with it).
    let model = serde_json::to_value(model_of(
        "CREATE TABLE public.customers (id bigint PRIMARY KEY);\nCREATE TABLE public.orders (id bigint PRIMARY KEY);\nCREATE TABLE public.refunds (id bigint PRIMARY KEY);\n",
    ))
    .unwrap();
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                               migration_sql)
         VALUES ($1, 3, $2, 'CREATE TABLE refunds ();', 'sql', 'draft', '{}', $3)",
        shop,
        model,
        "CREATE TABLE public.refunds (id bigint PRIMARY KEY);\nUPDATE public.orders SET id = id;",
    )
    .execute(&db)
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let created = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "version": 3 }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let test = created.json();
    assert_eq!(
        (test["from"].as_i64(), test["to"].as_i64()),
        (Some(2), Some(3))
    );
    assert_eq!(test["down"], "none");
    assert!(test["down_sql"].is_null());
    finished(&app, &team, test["id"].as_str().unwrap()).await;
    {
        let calls = host.calls();
        let (_, test, _) = calls.tests.last().unwrap();
        assert_eq!(test.up.len(), 2, "as written: {:?}", test.up);
        assert!(!test.up[0].planned);
        assert!(test.down.is_none());
    }

    let created = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "version": 3, "down": "generate" }),
        )
        .await;
    assert_eq!(created.json()["down"], "generated");
    finished(&app, &team, created.json()["id"].as_str().unwrap()).await;
    let calls = host.calls();
    let (_, test, _) = calls.tests.last().unwrap();
    assert!(
        test.down.as_ref().unwrap()[0]
            .sql
            .contains("DROP TABLE public.refunds")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tests_wait_their_turn_and_cancel_deletes_the_playground(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    host.calls().hold = true;
    let first = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "from": 1, "to": 2 }))
        .await
        .json();
    let first_id = first["id"].as_str().unwrap().to_owned();
    // The first one reaches its test (held there).
    for _ in 0..200 {
        if !host.calls().tests.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Only the first one holds.
    host.calls().hold = false;
    let running = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{first_id}")))
        .await
        .json();
    assert_eq!(running["status"], "running");
    assert_eq!(running["progress"]["stage"], "testing");
    assert_eq!(running["progress"]["phase"], "up");
    // One per organization at a time: the second waits, its place said.
    let second = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "from": 2, "to": 1 }))
        .await
        .json();
    let second_id = second["id"].as_str().unwrap().to_owned();
    assert_eq!(second["status"], "queued");
    assert_eq!(second["position"], 1);

    // Reviewers cannot stop it; developers can.
    let refused = app
        .as_user(&team.reviewer)
        .post_empty(&path(&team, &format!("/{first_id}/cancel")))
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let cancelled = app
        .as_user(&team.developer)
        .post_empty(&path(&team, &format!("/{first_id}/cancel")))
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.json());
    assert_eq!(cancelled.json()["status"], "cancelled");
    let playground = host.calls().tests[0].0;
    assert!(host.calls().destroyed.contains(&playground));
    let again = app
        .as_user(&team.developer)
        .post_empty(&path(&team, &format!("/{first_id}/cancel")))
        .await;
    assert_eq!(again.code(), "test_finished");

    // The second one's turn came.
    let done = finished(&app, &team, &second_id).await;
    assert_eq!(done["status"], "passed", "{done}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_failed_build_and_a_failed_statement_say_why(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    host.calls().build_fails = Some("the rows do not load".to_owned());
    let created = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "from": 1, "to": 2 }))
        .await
        .json();
    let done = finished(&app, &team, created["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "error");
    assert!(
        done["failure"]
            .as_str()
            .unwrap()
            .contains("the rows do not load"),
        "{done}"
    );
    assert_eq!(host.calls().destroyed.len(), 1, "nothing is left behind");

    host.calls().build_fails = None;
    host.calls().fail = true;
    let created = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "from": 1, "to": 2 }))
        .await
        .json();
    let done = finished(&app, &team, created["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["summary"]["failed"]["sqlstate"], "23502");
    assert_eq!(done["summary"]["failed"]["direction"], "up");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn what_a_test_asks_for_is_checked(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    for (body, status, code) in [
        (json!({}), StatusCode::UNPROCESSABLE_ENTITY, "validation"),
        (
            json!({ "from": 1, "to": 1 }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation",
        ),
        (
            json!({ "from": 1 }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation",
        ),
        (
            json!({ "from": 1, "to": 9 }),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            json!({ "from": 1, "to": 2, "seed": "xyz" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation",
        ),
    ] {
        let got = app
            .as_user(&team.developer)
            .post(&path(&team, ""), &body)
            .await;
        assert_eq!((got.status, got.code().as_str()), (status, code), "{body}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_test_left_running_by_a_restart_ends_as_an_error_and_its_playground_goes(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    host.calls().hold = true;
    let created = app
        .as_user(&team.developer)
        .post(&path(&team, ""), &json!({ "from": 1, "to": 2 }))
        .await
        .json();
    let id = created["id"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        if !host.calls().tests.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let playground = host.calls().tests[0].0;
    // A sweep of this process spares the running test's playground.
    let swept = sweeper::sweep(&app.state, chrono::Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!((swept.orphans, swept.tests), (0, 0));
    assert!(!host.calls().destroyed.contains(&playground));
    // A sweep of a process started after it: the test was left behind.
    let swept = sweeper::sweep(
        &app.state,
        chrono::Utc::now() + chrono::Duration::seconds(1),
    )
    .await
    .unwrap();
    // Its own playground goes with it, not as an orphan later.
    assert_eq!((swept.tests, swept.orphans), (1, 0));
    assert!(host.calls().destroyed.contains(&playground));
    let got = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{id}")))
        .await
        .json();
    assert_eq!(got["status"], "error");
    assert!(
        got["failure"].as_str().unwrap().contains("restarted"),
        "{got}"
    );
    host.release.notify_waiters();
}

/// A ready playground of `version` made by the developer.
async fn playground(db: &PgPool, team: &Team, version: i32) -> Uuid {
    sqlx::query_scalar!(
        "INSERT INTO playgrounds (org_id, database_id, version_id, model_sha256, size, profile, seed,
                                  status, ready_at, created_by)
         SELECT d.org_id, d.id, v.id, sha256('model'), 'small', 'realistic', 1194641, 'ready', now(),
                (SELECT user_id FROM memberships WHERE org_id = d.org_id AND role = 'developer' LIMIT 1)
         FROM databases d JOIN versions v ON v.database_id = d.id
         WHERE d.org_id = $1 AND d.slug = 'shop' AND v.number = $2
         RETURNING id",
        team.org,
        version,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_test_runs_on_a_copy_of_someone_s_playground_at_the_base_version(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let at_v1 = playground(&db, &team, 1).await;
    let at_v2 = playground(&db, &team, 2).await;
    // Another version than the migration starts from: refused, said why.
    let refused = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "from": 1, "to": 2, "playground": at_v2 }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "playground_other_version");
    assert!(refused.json()["message"].as_str().unwrap().contains("v2"));

    let created = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "from": 1, "to": 2, "playground": at_v1 }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let test = created.json();
    assert_eq!(test["data"]["playground"]["id"], at_v1.to_string());
    assert_eq!(test["data"]["seed"], "123a91");
    let label = test["data"]["playground"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        label.ends_with("playground (small, seed 123a91)"),
        "{label}"
    );
    let done = finished(&app, &team, test["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "passed", "{done}");

    // Copied, never built; the test told the rows are someone's; the copy
    // deleted after, the source never touched.
    let calls = host.calls();
    assert!(calls.builds.is_empty());
    let (copy, fork) = &calls.forks[0];
    assert_eq!(fork.source, at_v1);
    let (tested, test, _) = &calls.tests[0];
    assert_eq!(tested, copy);
    assert!(matches!(&test.data, DataSource::Existing { label: l } if *l == label));
    assert_eq!(calls.destroyed, vec![*copy]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_test_runs_the_renames_the_migration_view_shows(db: PgPool) {
    use super::renames::{KEY, RENAMED, V1 as USERS};

    let (app, team, host) = setup(&db).await;
    let crm = database(&db, team.org, "crm").await;
    super::renames::version(&db, crm, 1, USERS).await;
    super::renames::version(&db, crm, 2, RENAMED).await;
    let tests = team.path("/databases/crm/migration-tests");
    let sql = |statements: &[romulus_migration_check::Statement]| {
        statements.iter().map(|s| s.sql.clone()).collect::<Vec<_>>()
    };

    // Detected and on: renamed up, renamed back down; the rows follow.
    let created = app
        .as_user(&team.developer)
        .post(&tests, &json!({ "from": 1, "to": 2 }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let done = finished_in(&app, &team, "crm", created.json()["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "passed", "{done}");
    let (_, test, _) = host.calls().tests.last().cloned().unwrap();
    assert_eq!(
        sql(&test.up),
        ["ALTER TABLE public.users RENAME COLUMN email TO email_address;"]
    );
    assert_eq!(
        sql(test.down.as_deref().unwrap()),
        ["ALTER TABLE public.users RENAME COLUMN email_address TO email;"]
    );

    // Turned off on screen: dropped and added, as the view then shows it.
    let created = app
        .as_user(&team.developer)
        .post(
            &tests,
            &json!({ "from": 1, "to": 2, "renames": { KEY: false } }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    finished_in(&app, &team, "crm", created.json()["id"].as_str().unwrap()).await;
    let (_, test, _) = host.calls().tests.last().cloned().unwrap();
    assert_eq!(
        sql(&test.up),
        [
            "ALTER TABLE public.users ADD COLUMN email_address text;",
            "ALTER TABLE public.users DROP COLUMN email;",
        ]
    );
    assert_eq!(test.up[1].hazard.as_deref(), Some("deletes_data"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_organization_can_require_a_passing_test_before_publishing(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    // Admins only, and in the audit log.
    let on = json!({ "require_passing_test": true });
    let denied = app
        .as_user(&team.developer)
        .patch(&team.path(""), &on)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let set = app.as_user(&team.admin).patch(&team.path(""), &on).await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    assert_eq!(set.json()["require_passing_test"], true);
    let got = app.as_user(&team.reviewer).get(&team.path("")).await.json();
    assert_eq!(got["require_passing_test"], true);
    let audited = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_events
           WHERE org_id = $1 AND action = 'passing_test_required_changed'"#,
        team.org,
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // A draft adding a table: not published until a test of it passed.
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let draft: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let addr = serve(&app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &team.developer,
    )
    .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(
                txn,
                "CREATE TABLE public.invoices (id bigint PRIMARY KEY);\n",
            );
        })
        .await;
    alice
        .until(|c| {
            let expected = romulus_api::collab::compile::sha256_hex(&c.sql());
            c.events
                .iter()
                .any(|e| e["type"] == "compile" && e["sha256"] == expected)
        })
        .await;
    let publish = team.path(&format!("/databases/shop/drafts/{draft}/publish"));
    let sha256 = app.as_user(&team.developer).get(&publish).await.json()["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = app
        .as_user(&team.developer)
        .post(&publish, &json!({ "sha256": sha256 }))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.json());
    assert_eq!(refused.code(), "passing_test_required");

    // A failed test is not enough.
    host.calls().fail = true;
    let failed = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": sha256 }),
        )
        .await
        .json();
    finished(&app, &team, failed["id"].as_str().unwrap()).await;
    let refused = app
        .as_user(&team.developer)
        .post(&publish, &json!({ "sha256": sha256 }))
        .await;
    assert_eq!(refused.code(), "passing_test_required");

    host.calls().fail = false;
    let passed = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": sha256 }),
        )
        .await
        .json();
    let done = finished(&app, &team, passed["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "passed", "{done}");
    let published = app
        .as_user(&team.developer)
        .post(&publish, &json!({ "sha256": sha256 }))
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_written_by_hand_runs_up_only_unless_a_down_is_asked_for(db: PgPool) {
    use romulus_api::{AppState, Config, collab::Hub};

    let host = TestHost::default();
    let hub = Hub::with_settings(db.clone(), super::draft_modes::mode_compiler(), quick());
    let mut state = AppState::new(db.clone(), Config::for_tests())
        .with_collab(hub)
        .with_playgrounds(Host::new(host.clone()));
    state.compiler = state.collab.compiler().clone();
    let app = TestApp::with_state(state);
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let draft = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/shop/drafts"),
            &json!({ "mode": "migration" }),
        )
        .await
        .json();
    let draft = draft["id"].as_str().unwrap().to_owned();
    let addr = serve(&app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &team.developer,
    )
    .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            let len = roots.migration.len(txn);
            roots.migration.insert(
                txn,
                len,
                "CREATE TABLE public.coupons (id bigint PRIMARY KEY);\n",
            );
        })
        .await;
    // The tab has the live compile's hash of the text on screen: enough.
    alice
        .until(|c| {
            c.last_event("compile").is_some_and(|e| {
                e["state"] == "ok"
                    && e["sha256"] == romulus_api::collab::compile::sha256_hex(&c.migration())
            })
        })
        .await;
    let sha256 = alice.last_event("compile").unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_owned();

    let created = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": sha256 }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let test = created.json();
    assert_eq!(test["down"], "none");
    finished(&app, &team, test["id"].as_str().unwrap()).await;
    let (_, run, _) = host.calls().tests.last().cloned().unwrap();
    assert_eq!(run.up.len(), 1, "as written: {:?}", run.up);
    assert!(!run.up[0].planned);
    assert!(run.down.is_none());

    // Asked for: a generated down, back to v1.
    let created = app
        .as_user(&team.developer)
        .post(
            &path(&team, ""),
            &json!({ "draft_id": draft, "sha256": sha256, "down": "generate" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    assert_eq!(created.json()["down"], "generated");
    finished(&app, &team, created.json()["id"].as_str().unwrap()).await;
    let (_, run, _) = host.calls().tests.last().cloned().unwrap();
    let down = run.down.expect("a generated down");
    assert!(
        down.iter()
            .any(|s| s.sql.contains("DROP TABLE public.coupons")),
        "{down:?}"
    );
}

/// An app compiling with `compiler`, with the fake host.
pub(super) fn app_with(
    db: &PgPool,
    compiler: romulus_api::collab::compile::SqlCompiler,
) -> (TestApp, TestHost) {
    use romulus_api::{AppState, Config, collab::Hub};
    let host = TestHost::default();
    let hub = Hub::with_settings(db.clone(), compiler, quick());
    let mut state = AppState::new(db.clone(), Config::for_tests())
        .with_collab(hub)
        .with_playgrounds(Host::new(host.clone()));
    state.compiler = state.collab.compiler().clone();
    (TestApp::with_state(state), host)
}

/// The organization publishes only what passed a test.
async fn require_tests(app: &TestApp, team: &Team) {
    let set = app
        .as_user(&team.admin)
        .patch(&team.path(""), &json!({ "require_passing_test": true }))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
}

/// A new draft of `shop` (in `mode`), its developer in it.
pub(super) async fn new_draft(app: &TestApp, team: &Team, mode: &str) -> (String, Client) {
    let draft = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases/shop/drafts"),
            &json!({ "mode": mode }),
        )
        .await
        .json();
    let id = draft["id"].as_str().unwrap().to_owned();
    let addr = serve(app).await;
    let editor = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{id}/ws"),
        &team.developer,
    )
    .await;
    (id, editor)
}

/// Replaces the draft's source (its SQL, or its migration) and waits for
/// the room to compile it.
pub(super) async fn write_source(editor: &mut Client, migration: bool, text: &str) {
    use yrs::{GetString, Text};
    let text = text.to_owned();
    editor
        .edit(move |roots, txn| {
            let source = if migration {
                &roots.migration
            } else {
                &roots.sql
            };
            let len = u32::try_from(source.get_string(txn).encode_utf16().count()).unwrap();
            source.remove_range(txn, 0, len);
            source.insert(txn, 0, &text);
        })
        .await;
    editor
        .until(move |c| {
            let now = if migration { c.migration() } else { c.sql() };
            c.events.iter().any(|e| {
                e["type"] == "compile"
                    && e["state"] == "ok"
                    && e["sha256"] == romulus_api::collab::compile::sha256_hex(&now)
            })
        })
        .await;
}

/// The publish preview, and its `sha256`.
async fn preview_of(app: &TestApp, team: &Team, draft: &str) -> (Value, String) {
    let preview = app
        .as_user(&team.developer)
        .get(&team.path(&format!("/databases/shop/drafts/{draft}/publish")))
        .await
        .json();
    let sha = preview["sha256"].as_str().unwrap().to_owned();
    (preview, sha)
}

/// Starts a test of the draft and waits for it: the test.
async fn tested(app: &TestApp, team: &Team, body: Value) -> Value {
    let created = app
        .as_user(&team.developer)
        .post(&path(team, ""), &body)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    finished(app, team, created.json()["id"].as_str().unwrap()).await
}

async fn publish(
    app: &TestApp,
    team: &Team,
    draft: &str,
    body: Value,
) -> super::harness::TestResponse {
    app.as_user(&team.developer)
        .post(
            &team.path(&format!("/databases/shop/drafts/{draft}/publish")),
            &body,
        )
        .await
}

/// A migration written by hand: a table, a column backfilled before it
/// becomes NOT NULL (in the author's own transaction), an index built
/// concurrently, then a constraint using that index.
pub(super) const WRITTEN: &str = "\
CREATE TABLE public.coupons (id bigint PRIMARY KEY);
BEGIN;
ALTER TABLE public.customers ADD COLUMN email text;

-- Backfill before NOT NULL.
UPDATE public.customers SET email = 'unknown@example.com' WHERE email IS NULL;
ALTER TABLE public.customers ALTER COLUMN email SET NOT NULL;
COMMIT;
CREATE UNIQUE INDEX CONCURRENTLY customers_email ON public.customers (email);
ALTER TABLE public.customers ADD CONSTRAINT customers_email_key UNIQUE USING INDEX customers_email;
";

/// What the written statements say, in their order.
pub(super) const WRITTEN_IN_ORDER: [&str; 6] = [
    "CREATE TABLE public.coupons",
    "ADD COLUMN email text",
    "UPDATE public.customers SET email",
    "ALTER COLUMN email SET NOT NULL",
    "CREATE UNIQUE INDEX CONCURRENTLY customers_email",
    "USING INDEX customers_email",
];

/// Where each of `needles` is in `text`, panicking when one is missing or
/// out of order.
pub(super) fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        let Some(at) = text[from..].find(needle) else {
            panic!("{needle:?} is missing, or before what precedes it, in:\n{text}");
        };
        from += at + needle.len();
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_written_migration_s_files_hold_its_statements_as_the_test_runs_them(db: PgPool) {
    let (app, host) = app_with(&db, super::draft_modes::mode_compiler());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let (draft, mut editor) = new_draft(&app, &team, "migration").await;
    write_source(&mut editor, true, WRITTEN).await;
    let (_, sha) = preview_of(&app, &team, &draft).await;
    let dev = app.as_user(&team.developer);
    let migration_of = |format: &str| {
        team.path(&format!(
            "/databases/shop/drafts/{draft}/migration?format={format}&version=20261024090000&sha256={sha}"
        ))
    };
    let files = |migration: &Value, role: &str| -> Vec<(String, String)> {
        migration["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["role"] == role)
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_owned(),
                    f["content"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    for format in [
        "plain",
        "sqlx",
        "flyway",
        "prisma",
        "dbmate",
        "golang_migrate",
    ] {
        let migration = dev.get(&migration_of(format)).await;
        assert_eq!(migration.status, StatusCode::OK, "{}", migration.json());
        let migration = migration.json();
        // The up files, in order, are the statements as written: the
        // backfill between the column and its NOT NULL.
        let up: String = files(&migration, "up")
            .into_iter()
            .map(|(_, content)| content)
            .collect();
        in_order(&up, &WRITTEN_IN_ORDER);
        assert!(
            up.contains("-- Backfill before NOT NULL."),
            "{format}: {up}"
        );
        assert!(up.contains("written by hand"), "{format}: {up}");
        // The author's BEGIN and COMMIT give way to the tool's own, one
        // per part: before the BEGIN, inside, after the concurrent index.
        let transactions = up.matches("BEGIN;").count();
        let expected = if matches!(format, "plain" | "golang_migrate") {
            3
        } else {
            0
        };
        assert_eq!(transactions, expected, "{format}: {up}");
        assert_eq!(migration["written"], true, "{format}");
        // Read from the statements: SET NOT NULL, the unique index and the
        // constraint can fail on existing rows; the backfill of a column
        // the migration added changes nothing that was there.
        let hazards: Vec<&Value> = migration["statements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| &s["hazard"])
            .collect();
        assert_eq!(
            hazards,
            [
                &Value::Null,
                &Value::Null,
                &Value::Null,
                &json!("can_fail"),
                &json!("can_fail"),
                &json!("can_fail"),
            ],
            "{format}"
        );
        assert_eq!(migration["destructive"], 3, "{format}");
        let statements: Vec<&str> = migration["statements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["sql"].as_str().unwrap())
            .collect();
        assert_eq!(statements.len(), 6, "{format}: {statements:?}");
        // The down migration is still the generated one, back to v1.
        if matches!(format, "dbmate" | "golang_migrate") {
            let all: String = migration["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["content"].as_str().unwrap())
                .collect();
            assert!(all.contains("DROP TABLE public.coupons"), "{format}: {all}");
        }
    }
    // Where the tool runs each file in one transaction: a part per
    // transaction the author wrote, the concurrent index in a file of its
    // own, in order.
    let sqlx = dev.get(&migration_of("sqlx")).await.json();
    let paths: Vec<String> = files(&sqlx, "up").into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        paths,
        [
            "20261024090000_migration_from_v1.sql",
            "20261024090001_migration_from_v1_part_2.sql",
            "20261024090002_migration_from_v1_create_index_customers_email.sql",
            "20261024090003_migration_from_v1_part_3.sql",
        ],
        "{sqlx}"
    );
    let concurrent = &files(&sqlx, "up")[2].1;
    assert!(
        concurrent.starts_with("-- no-transaction\n"),
        "{concurrent}"
    );

    // A test of the draft runs exactly the statements the files hold.
    let test = tested(
        &app,
        &team,
        json!({ "draft_id": draft, "sha256": sha, "down": "generate" }),
    )
    .await;
    assert_eq!(test["status"], "passed", "{test}");
    let run = host.tested().pop().unwrap();
    let plain = dev.get(&migration_of("plain")).await.json();
    let plain = &files(&plain, "up")[0].1;
    let ran: Vec<&str> = run
        .iter()
        .map(String::as_str)
        .filter(|sql| !matches!(*sql, "BEGIN;" | "COMMIT;"))
        .collect();
    in_order(plain, &ran);
    assert_eq!(ran.len(), 6, "{run:?}");

    // A ROLLBACK has no place in files that run whole: refused, said why.
    write_source(
        &mut editor,
        true,
        "ALTER TABLE public.customers ADD COLUMN note text;\nROLLBACK;\n",
    )
    .await;
    let (_, sha) = preview_of(&app, &team, &draft).await;
    let refused = dev
        .get(&team.path(&format!(
            "/databases/shop/drafts/{draft}/migration?format=sqlx&sha256={sha}"
        )))
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "migration_unreadable");
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("line 2: ROLLBACK cannot go in a migration tool's files"),
        "{}",
        refused.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_gate_counts_a_test_of_exactly_the_migration_publishing_runs(db: PgPool) {
    use super::renames::{KEY, RENAMED, V1 as USERS};

    let (app, _host) = app_with(&db, super::renames::rename_compiler());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    super::renames::version(&db, shop, 1, USERS).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "schema").await;
    write_source(&mut editor, false, RENAMED).await;
    let (preview, sha) = preview_of(&app, &team, &draft).await;
    assert_eq!(preview["test_gate"]["required"], true, "{preview}");
    assert!(preview["test_gate"]["passing"].is_null());

    // Tested with the detected rename off on screen: publishing runs it on.
    let off = tested(
        &app,
        &team,
        json!({ "draft_id": draft, "sha256": sha, "renames": { KEY: false } }),
    )
    .await;
    assert_eq!(off["status"], "passed", "{off}");
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.json());
    assert_eq!(refused.code(), "passing_test_required");

    // Tested as the draft has it: it counts.
    let on = tested(&app, &team, json!({ "draft_id": draft, "sha256": sha })).await;
    let (preview, _) = preview_of(&app, &team, &draft).await;
    assert_eq!(preview["test_gate"]["passing"], on["id"], "{preview}");
    let published = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_written_migration_needs_a_tested_down_and_its_base_as_tested(db: PgPool) {
    let (app, host) = app_with(&db, super::draft_modes::mode_compiler());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "migration").await;
    // Data only: no schema change, still something to test.
    write_source(&mut editor, true, "UPDATE public.customers SET id = id;\n").await;
    let (preview, sha) = preview_of(&app, &team, &draft).await;
    assert_eq!(preview["test_gate"]["nothing_to_test"], false, "{preview}");

    // Up only: not enough.
    let up_only = tested(&app, &team, json!({ "draft_id": draft, "sha256": sha })).await;
    assert_eq!(up_only["verdict"], "passed_up_only", "{up_only}");
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(refused.code(), "passing_test_required");
    assert!(
        refused.json()["message"].as_str().unwrap().contains("down"),
        "{}",
        refused.json()
    );

    // Nothing checked on rows: not enough either.
    host.calls().nothing_checked = true;
    tested(
        &app,
        &team,
        json!({ "draft_id": draft, "sha256": sha, "down": "generate" }),
    )
    .await;
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(refused.code(), "passing_test_required");
    host.calls().nothing_checked = false;

    // With its generated down, on rows: it counts.
    let both = tested(
        &app,
        &team,
        json!({ "draft_id": draft, "sha256": sha, "down": "generate" }),
    )
    .await;
    assert_eq!(both["verdict"], "passed", "{both}");
    let (preview, _) = preview_of(&app, &team, &draft).await;
    assert_eq!(preview["test_gate"]["passing"], both["id"], "{preview}");

    // Another version since: updated onto it, the same text is another
    // migration (another base).
    version(&db, shop, 2, V2, json!({})).await;
    let updated = app
        .as_user(&team.developer)
        .post_empty(&team.path(&format!("/databases/shop/drafts/{draft}/update")))
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    editor
        .until(|c| c.events.iter().any(|e| e["type"] == "updated"))
        .await;
    let (preview, sha) = preview_of(&app, &team, &draft).await;
    assert!(preview["test_gate"]["passing"].is_null(), "{preview}");
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(
        refused.code(),
        "passing_test_required",
        "{}",
        refused.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn undeclared_loss_counts_once_a_developer_acknowledges_it(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "schema").await;
    write_source(
        &mut editor,
        false,
        &format!("{V2}CREATE TABLE public.invoices (id bigint PRIMARY KEY);\n"),
    )
    .await;
    let (_, sha) = preview_of(&app, &team, &draft).await;
    host.calls().verdict = Some(Verdict::NeedsAcknowledgement);
    let test = tested(&app, &team, json!({ "draft_id": draft, "sha256": sha })).await;
    assert_eq!(test["status"], "needs_acknowledgement", "{test}");
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(refused.code(), "passing_test_required");

    let id = test["id"].as_str().unwrap();
    let acknowledge = path(&team, &format!("/{id}/acknowledge"));
    let denied = app.as_user(&team.reviewer).post_empty(&acknowledge).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    // The organization requires passing tests: not the draft's author.
    let own = app.as_user(&team.developer).post_empty(&acknowledge).await;
    assert_eq!(own.status, StatusCode::FORBIDDEN);
    assert_eq!(own.code(), "own_draft");
    let other = super::harness::user(&db, "Other-acme").await;
    super::harness::member(&db, team.org, other, romulus_api::domain::Role::Developer).await;
    let other = super::harness::session(&db, other).await;
    let done = app.as_user(&other).post_empty(&acknowledge).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    assert_eq!(done.json()["status"], "passed");
    assert_eq!(done.json()["verdict"], "needs_acknowledgement");
    assert_eq!(done.json()["acknowledged"]["by"]["name"], "Other-acme");
    let again = app.as_user(&team.admin).post_empty(&acknowledge).await;
    assert_eq!(again.code(), "nothing_to_acknowledge");
    let audited = sqlx::query_scalar!(
        r#"SELECT details->>'test_id' AS "test_id" FROM audit_events WHERE action = 'migration_test_acknowledged'"#
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(audited, vec![Some(id.to_owned())]);
    let published = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_editor_who_just_typed_cannot_acknowledge_before_their_edit_is_written(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "schema").await;
    write_source(
        &mut editor,
        false,
        &format!("{V2}CREATE TABLE public.invoices (id bigint PRIMARY KEY);\n"),
    )
    .await;
    let (_, sha) = preview_of(&app, &team, &draft).await;
    host.calls().verdict = Some(Verdict::NeedsAcknowledgement);
    let test = tested(&app, &team, json!({ "draft_id": draft, "sha256": sha })).await;
    assert_eq!(test["status"], "needs_acknowledgement", "{test}");
    let acknowledge = path(
        &team,
        &format!("/{}/acknowledge", test["id"].as_str().unwrap()),
    );

    // Another developer types: an editor in the room at once, while the
    // `draft_editors` row the room writes behind it waits (a lock holds it).
    let other = super::harness::user(&db, "Other-acme").await;
    super::harness::member(&db, team.org, other, romulus_api::domain::Role::Developer).await;
    let other_session = super::harness::session(&db, other).await;
    let mut hold = db.begin().await.unwrap();
    sqlx::query("LOCK TABLE draft_editors IN SHARE MODE")
        .execute(&mut *hold)
        .await
        .unwrap();
    let mut typist = Client::join(
        serve(&app).await,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &other_session,
    )
    .await;
    typist
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, "-- mine\n");
        })
        .await;
    typist
        .until(|c| {
            c.last_event("review").is_some_and(|e| {
                e["editors"]
                    .as_array()
                    .is_some_and(|editors| editors.contains(&json!(other)))
            })
        })
        .await;
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_editors WHERE user_id = $1")
        .bind(other)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(written, 0, "the row waits");

    // The room says they edited it: not theirs to acknowledge.
    let refused = app.as_user(&other_session).post_empty(&acknowledge).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.json());
    assert_eq!(refused.code(), "own_draft");

    // Written, the same.
    hold.rollback().await.unwrap();
    app.state.collab.shutdown().await;
    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_editors WHERE user_id = $1")
        .bind(other)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(written, 1);
    let refused = app.as_user(&other_session).post_empty(&acknowledge).await;
    assert_eq!(refused.code(), "own_draft");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_acknowledged_up_only_test_never_opens_the_gate(db: PgPool) {
    let (app, host) = app_with(&db, super::draft_modes::mode_compiler());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "migration").await;
    write_source(
        &mut editor,
        true,
        "ALTER TABLE public.customers ALTER COLUMN id TYPE integer;\n",
    )
    .await;
    let (_, sha) = preview_of(&app, &team, &draft).await;
    // Written by hand, up only: the loss it does not declare is found, and
    // the engine's rule gives the verdict.
    host.calls().undeclared = true;
    let test = tested(&app, &team, json!({ "draft_id": draft, "sha256": sha })).await;
    assert_eq!(test["status"], "needs_acknowledgement", "{test}");
    assert_eq!(test["down"], "none", "{test}");

    // An admin acknowledges it: up only it stays, and publishing waits.
    let id = test["id"].as_str().unwrap();
    let done = app
        .as_user(&team.admin)
        .post_empty(&path(&team, &format!("/{id}/acknowledge")))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    assert_eq!(done.json()["status"], "passed");
    assert_eq!(done.json()["verdict"], "passed_up_only");
    let (preview, _) = preview_of(&app, &team, &draft).await;
    assert!(preview["test_gate"]["passing"].is_null(), "{preview}");
    assert!(
        preview["test_gate"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("up only")),
        "{preview}"
    );
    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(
        refused.code(),
        "passing_test_required",
        "{}",
        refused.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn where_tests_cannot_run_an_admin_may_publish_saying_why(db: PgPool) {
    use romulus_api::{AppState, Config};

    // No playground host on this server.
    let live = live_app(db.clone(), quick());
    let mut state =
        AppState::new(db.clone(), Config::for_tests()).with_collab(live.state.collab.clone());
    state.compiler = state.collab.compiler().clone();
    let app = TestApp::with_state(state);
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    require_tests(&app, &team).await;
    let (draft, mut editor) = new_draft(&app, &team, "schema").await;
    write_source(&mut editor, false, V2).await;
    let (preview, sha) = preview_of(&app, &team, &draft).await;
    assert_eq!(preview["test_gate"]["available"], false, "{preview}");

    let refused = publish(&app, &team, &draft, json!({ "sha256": sha })).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "tests_unavailable");
    // A developer cannot override; an admin can, saying why.
    let refused = publish(
        &app,
        &team,
        &draft,
        json!({ "sha256": sha, "test_override_reason": "Hotfix" }),
    )
    .await;
    assert_eq!(refused.code(), "tests_unavailable");
    let published = app
        .as_user(&team.admin)
        .post(
            &team.path(&format!("/databases/shop/drafts/{draft}/publish")),
            &json!({ "sha256": sha, "test_override_reason": "Hotfix, no playgrounds here" }),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
    let reason = sqlx::query_scalar!(
        "SELECT reason FROM audit_events WHERE action = 'passing_test_overridden'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(reason.as_deref(), Some("Hotfix, no playgrounds here"));
}
