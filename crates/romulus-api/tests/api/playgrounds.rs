//! Playgrounds: who may create, read, reset, run migrations and delete them;
//! the caps; the shared history and the audit log; the console's socket.
//! The playground host is a fake ([`FakeHost`]): the service itself is
//! tested in `crates/romulus-playground/tests`.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::http::StatusCode;
use futures_util::{SinkExt, StreamExt};
use romulus_api::{
    domain::{PlaygroundRunKind, PlaygroundRunStatus, Role},
    playgrounds::{
        MAX_PER_ORG, MAX_PER_PERSON, MAX_TOTAL,
        host::{Backend, BoxFuture, Caller, ConsoleLink, Host, HostError},
        runs::{self, CONSOLE_RUNS_KEPT, NewRun},
        sweeper,
    },
    rate::{Limiter, Limits},
};
use romulus_playground::protocol::{
    Access, Build, BuildEvent, ConsoleEvent, ConsoleRequest, Health, Listed, Listing, Migrate,
    MigrationEvent, MigrationReport, RunStatus, Stage, StatementResult, StatementStatus, Status,
    TableRows, TextSpan, Txn,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http::HeaderValue};
use uuid::Uuid;

use super::{
    harness::{ORIGIN, Session, Team, TestApp, database, member},
    live::{Client, live_app, quick, serve, version},
};

/// What the fake host was asked.
#[derive(Default)]
struct Calls {
    builds: Vec<(Uuid, Build, Caller)>,
    migrations: Vec<(Uuid, Migrate, Caller)>,
    resets: Vec<Uuid>,
    destroyed: Vec<Uuid>,
    consoles: Vec<(Uuid, Access)>,
    live: HashSet<Uuid>,
    /// The host's free and total disk (100 and 200 GiB unless set).
    free_bytes: Option<u64>,
    total_bytes: Option<u64>,
    /// The host does not say its disk space.
    space_unknown: bool,
    /// Resets and migration runs answer 409 `busy`.
    busy: bool,
}

fn busy() -> HostError {
    HostError::Refused {
        status: 409,
        code: "busy".into(),
        message: "the playground is busy migrating".into(),
    }
}

#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<Calls>>);

impl FakeHost {
    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn result(index: usize, sql: &str, status: StatementStatus) -> StatementResult {
    StatementResult {
        index,
        sql: sql.to_owned(),
        status,
        hazard: None,
        duration_ms: 3,
        locks: Vec::new(),
        locks_documented: false,
        rewrote: Vec::new(),
        rows_changed: Some(2),
        tag: Some("CREATE TABLE".to_owned()),
        error: None,
        offending: None,
    }
}

impl Backend for FakeHost {
    fn build(
        &self,
        id: Uuid,
        caller: Caller,
        build: Build,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<BuildEvent>, HostError>> {
        let tables: Vec<TableRows> = build.model["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| TableRows {
                schema: e["schema"].as_str().unwrap_or_default().to_owned(),
                table: e["name"].as_str().unwrap_or_default().to_owned(),
                rows: 10,
            })
            .collect();
        // Seed 0xbad: the build fails.
        let fails = build.seed == 0xbad;
        let mut calls = self.calls();
        calls.builds.push((id, build, caller));
        calls.live.insert(id);
        drop(calls);
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(8);
            tokio::spawn(async move {
                for stage in [Stage::Starting, Stage::Schema, Stage::Rows] {
                    let _ = tx.send(BuildEvent::Stage { stage }).await;
                }
                if fails {
                    let _ = tx
                        .send(BuildEvent::Failed {
                            stage: Stage::Rows,
                            message: "the rows do not load".to_owned(),
                        })
                        .await;
                    return;
                }
                let _ = tx
                    .send(BuildEvent::Ready {
                        tables,
                        report: json!({ "generator": "fake", "notes": [] }),
                        generator: "fake-1".to_owned(),
                        duration_ms: 5,
                    })
                    .await;
            });
            Ok(rx)
        })
    }

    fn migrate(
        &self,
        id: Uuid,
        caller: Caller,
        migrate: Migrate,
    ) -> BoxFuture<'_, Result<mpsc::Receiver<MigrationEvent>, HostError>> {
        let refused = self.calls().busy;
        if !refused {
            self.calls().migrations.push((id, migrate.clone(), caller));
        }
        Box::pin(async move {
            if refused {
                return Err(busy());
            }
            let (tx, rx) = mpsc::channel(8);
            tokio::spawn(async move {
                let _ = tx.send(MigrationEvent::Resetting).await;
                let statements: Vec<StatementResult> = migrate
                    .statements
                    .iter()
                    .enumerate()
                    .map(|(i, s)| result(i, &s.sql, StatementStatus::Passed))
                    .collect();
                let _ = tx
                    .send(MigrationEvent::Done {
                        report: MigrationReport {
                            passed: true,
                            statements,
                            failed_at: None,
                            duration_ms: 7,
                        },
                    })
                    .await;
            });
            Ok(rx)
        })
    }

    fn reset(&self, id: Uuid, _: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        let refused = self.calls().busy;
        if !refused {
            self.calls().resets.push(id);
        }
        Box::pin(async move {
            if refused {
                return Err(busy());
            }
            Ok(Status {
                running: true,
                ..Status::default()
            })
        })
    }

    fn destroy(&self, id: Uuid, _: Caller) -> BoxFuture<'_, Result<(), HostError>> {
        let mut calls = self.calls();
        calls.destroyed.push(id);
        calls.live.remove(&id);
        Box::pin(async { Ok(()) })
    }

    fn status(&self, _: Uuid, _: Caller) -> BoxFuture<'_, Result<Status, HostError>> {
        Box::pin(async {
            Ok(Status {
                running: true,
                ..Status::default()
            })
        })
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

    fn console(&self, id: Uuid, caller: Caller) -> BoxFuture<'_, Result<ConsoleLink, HostError>> {
        self.calls().consoles.push((id, caller.access));
        Box::pin(async move {
            let (to_host, mut requests) = mpsc::channel::<String>(8);
            let (events, from_host) = mpsc::channel::<String>(8);
            tokio::spawn(async move {
                let send = |event: ConsoleEvent| serde_json::to_string(&event).unwrap();
                let _ = events
                    .send(send(ConsoleEvent::Ready { txn: Txn::Idle }))
                    .await;
                while let Some(frame) = requests.recv().await {
                    let (id, sql) = match serde_json::from_str(&frame) {
                        Ok(ConsoleRequest::Run { id, sql, .. }) => (id, sql),
                        // A held run ends when cancelled.
                        Ok(ConsoleRequest::Cancel { id }) => {
                            let _ = events
                                .send(send(ConsoleEvent::Finished {
                                    id,
                                    status: RunStatus::Cancelled,
                                    statements: 1,
                                    rows: None,
                                    ms: 9,
                                    error: Some("canceling statement due to user request".into()),
                                }))
                                .await;
                            continue;
                        }
                        _ => continue,
                    };
                    let span = TextSpan::of(&sql, 0..sql.len());
                    if sql == "hold" {
                        let _ = events
                            .send(send(ConsoleEvent::Statement { id, index: 0, span }))
                            .await;
                        continue;
                    }
                    if sql == "restart" {
                        let _ = events
                            .send(send(ConsoleEvent::Restarted {
                                reason: "a role was changed".into(),
                            }))
                            .await;
                    }
                    if sql == "syntax error" {
                        let _ = events
                            .send(send(ConsoleEvent::Finished {
                                id,
                                status: RunStatus::Error,
                                statements: 0,
                                rows: None,
                                ms: 0,
                                error: Some("syntax error at end of input".into()),
                            }))
                            .await;
                        continue;
                    }
                    for event in [
                        ConsoleEvent::Statement {
                            id: id.clone(),
                            index: 0,
                            span,
                        },
                        ConsoleEvent::Rows {
                            id: id.clone(),
                            index: 0,
                            rows: vec![vec![Some("1".to_owned())]],
                        },
                        ConsoleEvent::Finished {
                            id: id.clone(),
                            status: RunStatus::Ok,
                            statements: 1,
                            rows: Some(1),
                            ms: 4,
                            error: None,
                        },
                    ] {
                        let _ = events.send(send(event)).await;
                    }
                }
            });
            Ok(ConsoleLink { to_host, from_host })
        })
    }

    fn health(&self) -> BoxFuture<'_, Result<Health, HostError>> {
        let calls = self.calls();
        let space = (!calls.space_unknown).then(|| {
            (
                calls.free_bytes.unwrap_or(100 * GIB),
                calls.total_bytes.unwrap_or(200 * GIB),
            )
        });
        drop(calls);
        Box::pin(async move {
            Ok(Health {
                ok: true,
                free_bytes: space.map(|s| s.0),
                total_bytes: space.map(|s| s.1),
                ..Health::default()
            })
        })
    }
}

const V1: &str = "CREATE TABLE public.customers (id bigint PRIMARY KEY);\n";
const GIB: u64 = 1024 * 1024 * 1024;

/// An app with a fake host and the live collab (drafts compile), team
/// `acme`, database `shop` at v1.
async fn setup(db: &PgPool) -> (TestApp, Team, FakeHost) {
    setup_with(db, Limits::default()).await
}

async fn setup_with(db: &PgPool, limits: Limits) -> (TestApp, Team, FakeHost) {
    let live = live_app(db.clone(), quick());
    let host = FakeHost::default();
    let mut state = live.state.clone().with_playgrounds(Host::new(host.clone()));
    // Uploads and playground scripts compile with the hub's (fake) compiler.
    state.compiler = state.collab.compiler().clone();
    state.limits = Arc::new(limits);
    let app = TestApp::with_state(state);
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({})).await;
    (app, team, host)
}

fn path(team: &Team, rest: &str) -> String {
    team.path(&format!("/databases/shop/playgrounds{rest}"))
}

async fn create(
    app: &TestApp,
    team: &Team,
    who: &Session,
    body: Value,
) -> super::harness::TestResponse {
    app.as_user(who).post(&path(team, ""), &body).await
}

/// Creates a playground of v1 as the developer and waits until it is ready.
async fn ready(app: &TestApp, team: &Team, body: Value) -> Uuid {
    let created = create(app, team, &team.developer, body).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let id: Uuid = created.json()["id"].as_str().unwrap().parse().unwrap();
    for _ in 0..100 {
        let got = app
            .as_user(&team.developer)
            .get(&path(team, &format!("/{id}")))
            .await
            .json();
        if got["status"] == "ready" {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the playground never became ready");
}

async fn audit(db: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_events WHERE action::text = $1"#,
        action
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_developer_creates_a_playground_of_a_version_and_it_builds(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let created = create(&app, &team, &team.developer, json!({ "seed": "7f3a91" })).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let body = created.json();
    assert_eq!(body["status"], "building");
    assert_eq!(body["version"], 1);
    assert_eq!(body["seed"], "7f3a91");
    assert_eq!(body["size"], "small");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

    let id = {
        for _ in 0..100 {
            let got = app
                .as_user(&team.reviewer)
                .get(&path(&team, &format!("/{id}")))
                .await
                .json();
            if got["status"] == "ready" {
                assert_eq!(
                    got["tables"],
                    json!([{ "schema": "public", "table": "customers", "rows": 10 }])
                );
                assert_eq!(got["generator"], "fake-1");
                assert_eq!(got["live"]["running"], true);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        id
    };
    {
        let calls = host.calls();
        let (built, build, caller) = &calls.builds[0];
        assert_eq!(*built, id);
        assert_eq!(build.seed, 0x7f_3a91);
        // The schema went through the compiler's playground script (the fake
        // compiler answers the text as it is).
        assert_eq!(build.schema_sql, V1);
        assert_eq!(caller.access, Access::Develop);
        assert_eq!(caller.org, team.org);
    }
    assert_eq!(audit(&db, "playground_created").await, 1);

    let listed = app.as_user(&team.reviewer).get(&path(&team, "")).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json().as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reviewers_read_playgrounds_but_cannot_create_reset_migrate_or_delete(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let refused = create(&app, &team, &team.reviewer, json!({})).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let id = ready(&app, &team, json!({})).await;

    let reviewer = app.as_user(&team.reviewer);
    assert_eq!(reviewer.get(&path(&team, "")).await.status, StatusCode::OK);
    assert_eq!(
        reviewer.get(&path(&team, &format!("/{id}"))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        reviewer
            .get(&path(&team, &format!("/{id}/runs")))
            .await
            .status,
        StatusCode::OK
    );
    for (method, rest, body) in [
        ("post", "/reset", json!({})),
        ("post", "/migration", json!({ "sha256": "x" })),
        ("delete", "", json!({})),
    ] {
        let url = path(&team, &format!("/{id}{rest}"));
        let response = match method {
            "post" => reviewer.post(&url, &body).await,
            _ => reviewer.delete(&url).await,
        };
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{method} {rest}");
    }
    let calls = host.calls();
    assert!(calls.resets.is_empty() && calls.migrations.is_empty() && calls.destroyed.is_empty());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn outsiders_and_other_organizations_see_nothing(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let other = Team::new(&db, "globex").await;
    database(&db, other.org, "shop").await;
    for who in [&team.outsider, &other.developer, &other.admin] {
        let as_them = app.as_user(who);
        assert_eq!(
            as_them.get(&path(&team, "")).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            as_them.get(&path(&team, &format!("/{id}"))).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            as_them
                .post(&path(&team, &format!("/{id}/reset")), &json!({}))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
    // Another organization's own path does not reach it either.
    let theirs = format!("/api/orgs/globex/databases/shop/playgrounds/{id}");
    assert_eq!(
        app.as_user(&other.developer).get(&theirs).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.as_user(&other.developer).delete(&theirs).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_playground_per_version_and_the_caps_hold(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let shop: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'shop'")
        .fetch_one(&db)
        .await
        .unwrap();
    for n in 2..=4 {
        version(&db, shop, n, V1, json!({})).await;
    }
    ready(&app, &team, json!({ "version": 1 })).await;
    let again = create(&app, &team, &team.developer, json!({ "version": 1 })).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "playground_exists");
    ready(&app, &team, json!({ "version": 2 })).await;
    ready(&app, &team, json!({ "version": 3 })).await;
    let fourth = create(&app, &team, &team.admin, json!({ "version": 4 })).await;
    assert_eq!(fourth.status, StatusCode::CONFLICT);
    assert_eq!(fourth.code(), "playground_limit");

    // Five per organization, across its databases.
    for name in ["mall", "depot"] {
        let other = database(&db, team.org, name).await;
        version(&db, other, 1, V1, json!({})).await;
        let created = app
            .as_user(&team.admin)
            .post(
                &team.path(&format!("/databases/{name}/playgrounds")),
                &json!({}),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    }
    let sixth_db = database(&db, team.org, "annex").await;
    version(&db, sixth_db, 1, V1, json!({})).await;
    let sixth = app
        .as_user(&team.admin)
        .post(&team.path("/databases/annex/playgrounds"), &json!({}))
        .await;
    assert_eq!(sixth.status, StatusCode::CONFLICT);
    assert_eq!(sixth.code(), "playground_limit");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn bad_requests_are_refused_with_their_code(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let bad_seed = create(&app, &team, &team.developer, json!({ "seed": "not hex!" })).await;
    assert_eq!(bad_seed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad_seed.code(), "validation");
    let missing = create(&app, &team, &team.developer, json!({ "version": 9 })).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    database(&db, team.org, "empty").await;
    let empty = app
        .as_user(&team.developer)
        .post(&team.path("/databases/empty/playgrounds"), &json!({}))
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(empty.code(), "no_version");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_a_host_playgrounds_answer_host_unavailable(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let created = create(&app, &team, &team.developer, json!({})).await;
    assert_eq!(created.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(created.code(), "host_unavailable");
    let health = app.get("/api/health").await.json();
    assert_eq!(health["playgrounds"], "off");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_playground_runs_the_reviewed_migration_and_reset_puts_rows_back(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let draft: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let id = ready(&app, &team, json!({ "draft_id": draft })).await;
    let got = app
        .as_user(&team.developer)
        .get(&path(&team, &format!("/{id}")))
        .await
        .json();
    assert_eq!(got["draft"]["id"], draft.to_string());
    assert_eq!(got["draft"]["moved"], false);
    assert_eq!(got["version"], 1);

    // The draft adds a table.
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
            roots
                .sql
                .push(txn, "CREATE TABLE public.orders (id bigint PRIMARY KEY);\n");
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

    // Not the text reviewed: refused.
    let stale = app
        .as_user(&team.developer)
        .post(
            &path(&team, &format!("/{id}/migration")),
            &json!({ "sha256": "0".repeat(64) }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "draft_changed");

    let run = app
        .as_user(&team.developer)
        .post(
            &path(&team, &format!("/{id}/migration")),
            &json!({ "sha256": sha256 }),
        )
        .await;
    assert_eq!(run.status, StatusCode::OK);
    let events: Vec<Value> = String::from_utf8_lossy(&run.body)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["type"], "done", "{events:?}");
    {
        let calls = host.calls();
        let (_, migrate, caller) = &calls.migrations[0];
        assert_eq!(caller.access, Access::Develop);
        assert!(
            migrate
                .statements
                .iter()
                .any(|s| s.sql.contains("CREATE TABLE public.orders"))
        );
    }

    // The run is recorded once the job ends.
    let mut got = Value::Null;
    for _ in 0..100 {
        got = app
            .as_user(&team.reviewer)
            .get(&path(&team, &format!("/{id}")))
            .await
            .json();
        if !got["migrated_by"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(got["migrated_by"]["kind"], "migration");
    assert_eq!(got["migrated_by"]["report"]["passed"], true);
    assert_eq!(audit(&db, "playground_migration_run").await, 1);

    let reset = app
        .as_user(&team.developer)
        .post(&path(&team, &format!("/{id}/reset")), &json!({}))
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.json());
    assert!(reset.json()["migrated_by"].is_null());
    assert_eq!(host.calls().resets, vec![id]);
    assert_eq!(audit(&db, "playground_reset").await, 1);
    let runs = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{id}/runs")))
        .await
        .json();
    let kinds: Vec<&str> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["reset", "migration"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_playground_has_no_migration_to_run(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let run = app
        .as_user(&team.developer)
        .post(
            &path(&team, &format!("/{id}/migration")),
            &json!({ "sha256": "x" }),
        )
        .await;
    assert_eq!(run.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(run.code(), "no_migration");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_destroys_it_on_the_host_and_is_audited(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let deleted = app
        .as_user(&team.developer)
        .delete(&path(&team, &format!("/{id}")))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(host.calls().destroyed, vec![id]);
    assert_eq!(audit(&db, "playground_deleted").await, 1);
    assert_eq!(
        app.as_user(&team.developer)
            .get(&path(&team, &format!("/{id}")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

async fn console(
    addr: std::net::SocketAddr,
    url: &str,
    who: Option<&Session>,
    origin: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    u16,
> {
    let mut request = format!("ws://{addr}{url}").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_str(origin).unwrap());
    if let Some(who) = who {
        request
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(&who.cookie()).unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(err) => panic!("{err}"),
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_console_checks_origin_and_membership_and_records_the_shared_history(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let addr = serve(&app).await;
    let url = format!("/api/orgs/acme/databases/shop/playgrounds/{id}/console");

    assert_eq!(
        console(addr, &url, Some(&team.reviewer), "https://evil.example")
            .await
            .err(),
        Some(403)
    );
    assert_eq!(
        console(addr, &url, Some(&team.outsider), ORIGIN)
            .await
            .err(),
        Some(404)
    );
    assert_eq!(console(addr, &url, None, ORIGIN).await.err(), Some(401));

    let mut socket = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("opens");
    socket
        .send(tungstenite::Message::Text(
            json!({ "type": "run", "id": "r1", "sql": "SELECT 1" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut finished = false;
    let mut history = false;
    while !(finished && history) {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an event")
            .expect("open")
            .expect("a frame");
        let tungstenite::Message::Text(text) = frame else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        finished |= event["type"] == "finished";
        history |= event["type"] == "history" && event["run"]["sql"] == "SELECT 1";
    }
    // Reviewers get read access (the shorter deadline); developers develop.
    assert_eq!(host.calls().consoles, vec![(id, Access::Read)]);
    let mut developer = console(addr, &url, Some(&team.developer), ORIGIN)
        .await
        .expect("opens");
    let _ = tokio::time::timeout(Duration::from_secs(5), developer.next()).await;
    assert_eq!(host.calls().consoles[1], (id, Access::Develop));

    let runs = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{id}/runs")))
        .await
        .json();
    assert_eq!(runs["runs"][0]["kind"], "query");
    assert_eq!(runs["runs"][0]["by"]["name"], "Reviewer-acme");
    assert_eq!(runs["runs"][0]["rows"], 1);

    // A reset is announced to everyone, with who did it (by id: names repeat).
    let reset = app
        .as_user(&team.developer)
        .post(&path(&team, &format!("/{id}/reset")), &json!({}))
        .await;
    assert_eq!(reset.status, StatusCode::OK);
    let heard = frames_until(&mut socket, |e| {
        e["type"] == "announcement" && e["event"] == "reset"
    })
    .await;
    assert_eq!(
        heard.last().unwrap()["by"]["id"],
        team.developer.user.to_string()
    );

    // Anything but a console request closes the socket.
    socket
        .send(tungstenite::Message::Text(
            json!({ "type": "terminate", "pid": 1 }).to_string().into(),
        ))
        .await
        .unwrap();
    let closed = loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("closes")
        {
            Some(Ok(tungstenite::Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            _ => break None,
        }
    };
    assert_eq!(closed, Some(1003));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_person_opens_at_most_three_consoles_of_a_playground(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let addr = serve(&app).await;
    let url = format!("/api/orgs/acme/databases/shop/playgrounds/{id}/console");
    let mut open = Vec::new();
    for _ in 0..3 {
        let mut socket = console(addr, &url, Some(&team.reviewer), ORIGIN)
            .await
            .expect("opens");
        let _ = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
        open.push(socket);
    }
    let mut fourth = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("upgrades");
    let closed = loop {
        match tokio::time::timeout(Duration::from_secs(5), fourth.next())
            .await
            .expect("closes")
        {
            Some(Ok(tungstenite::Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            _ => break None,
        }
    };
    assert_eq!(closed, Some(4429));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn unused_playgrounds_expire_and_orphans_are_destroyed(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let orphan = Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0042);
    host.calls().live.insert(orphan);
    sqlx::query!(
        "UPDATE playgrounds SET expires_at = now() - interval '1 minute' WHERE id = $1",
        id
    )
    .execute(&db)
    .await
    .unwrap();
    let swept = sweeper::sweep(&app.state, chrono::Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(swept.expired, 1);
    assert_eq!(swept.orphans, 1);
    let destroyed = host.calls().destroyed.clone();
    assert!(destroyed.contains(&id) && destroyed.contains(&orphan));
    let got = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{id}")))
        .await
        .json();
    assert_eq!(got["status"], "expired");
    let refused = app
        .as_user(&team.developer)
        .post(&path(&team, &format!("/{id}/reset")), &json!({}))
        .await;
    assert_eq!(refused.code(), "playground_expired");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn concurrent_creates_cannot_pass_the_caps_together(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    // Six databases at v1, created at once by two people (three each, their
    // own cap): the organization's cap (five) holds.
    let mut urls = Vec::new();
    for i in 0..6 {
        let name = format!("db{i}");
        let id = database(&db, team.org, &name).await;
        version(&db, id, 1, V1, json!({})).await;
        urls.push(team.path(&format!("/databases/{name}/playgrounds")));
    }
    let creates = urls.iter().enumerate().map(|(i, url)| {
        let who = if i % 2 == 0 {
            &team.developer
        } else {
            &team.admin
        };
        let app = &app;
        async move { app.as_user(who).post(url, &json!({})).await }
    });
    let answers = futures_util::future::join_all(creates).await;
    let created = answers
        .iter()
        .filter(|a| a.status == StatusCode::CREATED)
        .count();
    assert_eq!(created, usize::try_from(MAX_PER_ORG).unwrap());
    for refused in answers.iter().filter(|a| a.status != StatusCode::CREATED) {
        assert_eq!(refused.status, StatusCode::CONFLICT);
        assert_eq!(refused.code(), "playground_limit");
    }
    let live = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM playgrounds WHERE org_id = $1 AND status IN ('building', 'ready')"#,
        team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(live, MAX_PER_ORG);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_person_makes_a_few_playgrounds_across_organizations(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    ready(&app, &team, json!({})).await;
    for name in ["mall", "depot"] {
        let id = database(&db, team.org, name).await;
        version(&db, id, 1, V1, json!({})).await;
        let created = app
            .as_user(&team.developer)
            .post(
                &team.path(&format!("/databases/{name}/playgrounds")),
                &json!({}),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    }
    // Their own organization, made to go past their cap: refused.
    let other = Team::new(&db, "globex").await;
    let shop = database(&db, other.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    member(&db, other.org, team.developer.user, Role::Developer).await;
    let url = "/api/orgs/globex/databases/shop/playgrounds";
    let fourth = app.as_user(&team.developer).post(url, &json!({})).await;
    assert_eq!(fourth.status, StatusCode::CONFLICT);
    assert_eq!(fourth.code(), "playground_limit");
    assert!(
        fourth.json()["message"]
            .as_str()
            .unwrap()
            .contains(&format!("You made {MAX_PER_PERSON}"))
    );
    // Someone else there is not counted against them.
    let theirs = app.as_user(&other.developer).post(url, &json!({})).await;
    assert_eq!(theirs.status, StatusCode::CREATED, "{}", theirs.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_full_host_or_a_nearly_full_disk_refuses_new_playgrounds(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    // 1 GiB free: a small playground's quota, not the reserve.
    host.calls().free_bytes = Some(GIB);
    let refused = create(&app, &team, &team.developer, json!({})).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.code(), "playground_host_full");
    host.calls().free_bytes = None;

    // Every organization's together: the host's own cap.
    let other = Team::new(&db, "globex").await;
    let big = database(&db, other.org, "big").await;
    for n in 1..=i32::try_from(MAX_TOTAL).unwrap() {
        version(&db, big, n, V1, json!({})).await;
    }
    sqlx::query!(
        "INSERT INTO playgrounds (org_id, database_id, version_id, model_sha256, size, profile, seed, status, ready_at)
         SELECT $1, $2, v.id, sha256(''::bytea), 'small', 'realistic', 1, 'ready', now()
         FROM versions v WHERE v.database_id = $2",
        other.org,
        big,
    )
    .execute(&db)
    .await
    .unwrap();
    let full = create(&app, &team, &team.developer, json!({})).await;
    assert_eq!(full.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(full.code(), "playground_host_full");
}

/// The text frames of a console until one matches `done`.
async fn frames_until(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    done: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an event")
            .expect("open")
            .expect("a frame");
        let tungstenite::Message::Text(text) = frame else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        let last = done(&event);
        seen.push(event);
        if last {
            return seen;
        }
    }
}

async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request: Value,
) {
    socket
        .send(tungstenite::Message::Text(request.to_string().into()))
        .await
        .unwrap();
}

fn finished_run(id: &str) -> impl Fn(&Value) -> bool + '_ {
    move |e| e["type"] == "finished" && e["id"] == id
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_person_has_a_few_runs_in_flight_and_runs_that_ran_nothing_are_not_kept(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let addr = serve(&app).await;
    let url = format!("/api/orgs/acme/databases/shop/playgrounds/{id}/console");
    let mut first = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("opens");
    let mut second = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("opens");

    // Two held runs, in two tabs: a third, in either, is refused here.
    send(
        &mut first,
        json!({ "type": "run", "id": "h1", "sql": "hold" }),
    )
    .await;
    frames_until(&mut first, |e| e["type"] == "statement").await;
    send(
        &mut second,
        json!({ "type": "run", "id": "h2", "sql": "hold" }),
    )
    .await;
    frames_until(&mut second, |e| e["type"] == "statement").await;
    send(
        &mut first,
        json!({ "type": "run", "id": "r3", "sql": "SELECT 1" }),
    )
    .await;
    let refused = frames_until(&mut first, finished_run("r3")).await;
    let refused = refused.last().unwrap();
    assert_eq!(refused["status"], "error");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("runs in progress"),
        "{refused}"
    );

    // One ends: the next run goes through.
    send(&mut first, json!({ "type": "cancel", "id": "h1" })).await;
    frames_until(&mut first, finished_run("h1")).await;
    send(
        &mut first,
        json!({ "type": "run", "id": "r4", "sql": "SELECT 1" }),
    )
    .await;
    let ran = frames_until(&mut first, finished_run("r4")).await;
    assert_eq!(ran.last().unwrap()["status"], "ok");

    // A run that ran no statement (a syntax error) is the person's alone.
    send(
        &mut first,
        json!({ "type": "run", "id": "r5", "sql": "syntax error" }),
    )
    .await;
    frames_until(&mut first, finished_run("r5")).await;
    let runs = app
        .as_user(&team.reviewer)
        .get(&path(&team, &format!("/{id}/runs")))
        .await
        .json();
    let kept: Vec<(&str, &str)> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["sql"].as_str().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!(kept, [("SELECT 1", "ok"), ("hold", "cancelled")]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn console_runs_are_rate_limited_per_person(db: PgPool) {
    let limits = Limits {
        playground_console: Limiter::new(2, Duration::from_mins(1)),
        ..Limits::default()
    };
    let (app, team, _) = setup_with(&db, limits).await;
    let id = ready(&app, &team, json!({})).await;
    let addr = serve(&app).await;
    let url = format!("/api/orgs/acme/databases/shop/playgrounds/{id}/console");
    let mut socket = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("opens");
    for run in ["r1", "r2"] {
        send(
            &mut socket,
            json!({ "type": "run", "id": run, "sql": "SELECT 1" }),
        )
        .await;
        let events = frames_until(&mut socket, finished_run(run)).await;
        assert_eq!(events.last().unwrap()["status"], "ok");
    }
    send(
        &mut socket,
        json!({ "type": "run", "id": "r3", "sql": "SELECT 1" }),
    )
    .await;
    let events = frames_until(&mut socket, finished_run("r3")).await;
    let refused = events.last().unwrap();
    assert_eq!(refused["status"], "error");
    assert!(
        refused["error"].as_str().unwrap().contains("faster"),
        "{refused}"
    );
    // Someone else is not slowed down.
    let mut other = console(addr, &url, Some(&team.developer), ORIGIN)
        .await
        .expect("opens");
    send(
        &mut other,
        json!({ "type": "run", "id": "o1", "sql": "SELECT 1" }),
    )
    .await;
    let events = frames_until(&mut other, finished_run("o1")).await;
    assert_eq!(events.last().unwrap()["status"], "ok");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_history_keeps_the_newest_console_runs_and_every_reset(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let run = |kind, sql: &'static str| NewRun {
        playground: id,
        user: (team.developer.user, "Developer-acme"),
        kind,
        sql: Some(sql),
        duration_ms: 1,
        status: PlaygroundRunStatus::Ok,
        rows: Some(1),
        error: None,
        report: None,
        migration_input_sha256: None,
    };
    runs::record(&db, run(PlaygroundRunKind::Reset, "-"))
        .await
        .unwrap();
    runs::record_console(&db, run(PlaygroundRunKind::Query, "SELECT 'oldest'"))
        .await
        .unwrap();
    for _ in 0..CONSOLE_RUNS_KEPT {
        runs::record_console(&db, run(PlaygroundRunKind::Query, "SELECT 1"))
            .await
            .unwrap();
    }
    let kept = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE kind = 'query') AS "queries!",
                  count(*) FILTER (WHERE kind = 'reset') AS "resets!",
                  count(*) FILTER (WHERE sql = 'SELECT ''oldest''') AS "oldest!"
           FROM playground_runs WHERE playground_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (kept.queries, kept.resets, kept.oldest),
        (CONSOLE_RUNS_KEPT, 1, 0)
    );
}

/// A playground made for a draft that adds a table, and the SHA-256 of the
/// draft's text as the person reviewed it.
async fn draft_playground(app: &TestApp, team: &Team) -> (Uuid, String) {
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let draft: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let id = ready(app, team, json!({ "draft_id": draft })).await;
    let addr = serve(app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &team.developer,
    )
    .await;
    alice
        .edit(|roots, txn| {
            use yrs::Text;
            roots
                .sql
                .push(txn, "CREATE TABLE public.orders (id bigint PRIMARY KEY);\n");
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
    (id, preview["sha256"].as_str().unwrap().to_owned())
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_busy_playground_or_a_second_run_of_yours_is_refused_and_not_recorded(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let (id, sha256) = draft_playground(&app, &team).await;
    let dev = app.as_user(&team.developer);
    let migration = path(&team, &format!("/{id}/migration"));

    // The host is busy: 409 before anything streams, nothing recorded.
    host.calls().busy = true;
    let run = dev.post(&migration, &json!({ "sha256": sha256 })).await;
    assert_eq!(run.status, StatusCode::CONFLICT);
    assert_eq!(run.code(), "playground_busy");
    let reset = dev
        .post(&path(&team, &format!("/{id}/reset")), &json!({}))
        .await;
    assert_eq!(reset.status, StatusCode::CONFLICT);
    assert_eq!(reset.code(), "playground_busy");
    host.calls().busy = false;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let runs = dev.get(&path(&team, &format!("/{id}/runs"))).await.json();
    assert_eq!(runs["runs"], json!([]));
    assert_eq!(audit(&db, "playground_migration_run").await, 0);
    assert_eq!(audit(&db, "playground_reset").await, 0);

    // One run at a time per person, wherever it is.
    let held = app
        .state
        .playgrounds
        .start_migration(team.developer.user)
        .expect("their slot");
    let run = dev.post(&migration, &json!({ "sha256": sha256 })).await;
    assert_eq!(run.status, StatusCode::CONFLICT);
    assert_eq!(run.code(), "migration_in_progress");
    drop(held);
    let run = dev.post(&migration, &json!({ "sha256": sha256 })).await;
    assert_eq!(run.status, StatusCode::OK);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_a_failed_playground_cleans_up_on_the_host_too(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    let created = create(&app, &team, &team.developer, json!({ "seed": "bad" })).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let id: Uuid = created.json()["id"].as_str().unwrap().parse().unwrap();
    for _ in 0..100 {
        let got = app
            .as_user(&team.developer)
            .get(&path(&team, &format!("/{id}")))
            .await
            .json();
        if got["status"] == "failed" {
            assert_eq!(got["failure"], "the rows do not load");
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let deleted = app
        .as_user(&team.developer)
        .delete(&path(&team, &format!("/{id}")))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(host.calls().destroyed, vec![id]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn quotas_are_never_promised_past_the_volume_and_an_unknown_volume_refuses(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    // 4 GiB in all: the 2 GiB reserve leaves 2, two small playgrounds.
    host.calls().total_bytes = Some(4 * GIB);
    let shop: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'shop'")
        .fetch_one(&db)
        .await
        .unwrap();
    for n in 2..=3 {
        version(&db, shop, n, V1, json!({})).await;
    }
    ready(&app, &team, json!({ "version": 1 })).await;
    ready(&app, &team, json!({ "version": 2 })).await;
    // Free space says yes (100 GiB: the files are sparse); the quotas say no.
    let third = create(&app, &team, &team.developer, json!({ "version": 3 })).await;
    assert_eq!(third.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(third.code(), "playground_host_full");
    // A medium one counts double.
    host.calls().total_bytes = Some(5 * GIB);
    let medium = create(
        &app,
        &team,
        &team.developer,
        json!({ "version": 3, "size": "medium" }),
    )
    .await;
    assert_eq!(medium.status, StatusCode::SERVICE_UNAVAILABLE);
    let small = create(&app, &team, &team.developer, json!({ "version": 3 })).await;
    assert_eq!(small.status, StatusCode::CREATED, "{}", small.json());

    // A host that cannot say its disk space: no new playground.
    host.calls().total_bytes = None;
    host.calls().space_unknown = true;
    let other = database(&db, team.org, "mall").await;
    version(&db, other, 1, V1, json!({})).await;
    let unknown = app
        .as_user(&team.admin)
        .post(&team.path("/databases/mall/playgrounds"), &json!({}))
        .await;
    assert_eq!(unknown.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unknown.code(), "host_unavailable");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_restart_for_a_changed_role_is_audited_once(db: PgPool) {
    let (app, team, _) = setup(&db).await;
    let id = ready(&app, &team, json!({})).await;
    let addr = serve(&app).await;
    let url = format!("/api/orgs/acme/databases/shop/playgrounds/{id}/console");
    let mut socket = console(addr, &url, Some(&team.reviewer), ORIGIN)
        .await
        .expect("opens");
    send(
        &mut socket,
        json!({ "type": "run", "id": "r1", "sql": "restart" }),
    )
    .await;
    let events = frames_until(&mut socket, finished_run("r1")).await;
    assert!(
        events.iter().any(|e| e["type"] == "restarted"),
        "{events:?}"
    );
    assert_eq!(audit(&db, "playground_restarted").await, 1);
    let actor = sqlx::query_scalar!(
        "SELECT actor_id FROM audit_events WHERE action = 'playground_restarted'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(actor, None, "romulus itself restarted it");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_on_another_postgres_major_gets_no_playground_yet(db: PgPool) {
    let (app, team, host) = setup(&db).await;
    sqlx::query!("UPDATE databases SET pg_major = 15 WHERE slug = 'shop'")
        .execute(&db)
        .await
        .unwrap();
    let refused = create(&app, &team, &team.developer, json!({})).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "major_unsupported");
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("Postgres 15")
    );
    assert!(host.calls().builds.is_empty());
}
