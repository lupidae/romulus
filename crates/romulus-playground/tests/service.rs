//! The service against real playgrounds: the runner in process (Docker and
//! the playground image, `just playground-image`), the service's HTTP server
//! on a loopback port, calls signed as the API signs them.
//!
//! Skipped, loudly, without Docker or the image; in CI (`CI` set) that is a
//! failure. Each test has its own Docker volume and runner scope, removed at
//! the end. Locally the playgrounds run with `runc` and reach the service by
//! TCP (`ROMULUS_PLAY_RUNTIME`, `ROMULUS_PLAY_TCP` from `.env`).

use std::{
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use romulus_play_runner::server::{
    Config as RunnerConfig, Root, Runner as RunnerServer, router as runner_router,
};
use romulus_playground::{
    protocol::{
        Access, Build, BuildEvent, ConsoleEvent, ConsoleRequest, Migrate, MigrationEvent,
        MigrationStatement, Profile, RunStatus, Size, StatementStatus,
    },
    server::{
        Config, Host,
        seed::{CopyTable, FakeData, Options, Plan, Report, SeedError, Seeder, Step},
    },
    signing::{Claims, Op},
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, UnixListener};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use uuid::Uuid;

const KEY: &[u8] = b"test-key-0123456789abcdef0123456789";
const SECRET: &[u8] = b"test-secret-0123456789abcdef012345";

/// 300 customers, every tenth without an email, loaded as
/// `romulus-fakedata` loads: through a staging table, then SQL, then the
/// outcome query the report records. `slow`: a row every 100 ms (a build to
/// cancel).
struct Customers {
    slow: bool,
}

struct CustomersPlan {
    slow: bool,
}

struct Outcome(Option<Value>);

impl Report for Outcome {
    fn record_outcome(&mut self, json: &str) -> Result<(), SeedError> {
        self.0 = Some(serde_json::from_str(json).map_err(|e| SeedError(e.to_string()))?);
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({ "generator": "test", "notes": [], "outcome": self.0 })
    }
}

impl Plan for CustomersPlan {
    fn steps(&self) -> Vec<Step> {
        let slow = self.slow;
        vec![
            Step::Sql(
                "CREATE SCHEMA staging; CREATE TABLE staging.customers (LIKE public.customers)"
                    .to_owned(),
            ),
            Step::CopyTable(CopyTable {
                schema: "staging".to_owned(),
                table: "customers".to_owned(),
                columns: vec!["id".to_owned(), "email".to_owned(), "name".to_owned()],
                rows: Box::new((1..=300).map(move |i| {
                    if slow {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    vec![
                        Some(i.to_string()),
                        (i % 10 != 0).then(|| format!("c{i}@example.com")),
                        Some(format!("Customer\t{i}")),
                    ]
                })),
                target_schema: "public".to_owned(),
                target_table: "customers".to_owned(),
            }),
            Step::Sql("INSERT INTO public.customers SELECT * FROM staging.customers".to_owned()),
            Step::Outcome(
                "SELECT json_build_object('rows', count(*))::text FROM staging.customers"
                    .to_owned(),
            ),
            Step::Sql("DROP SCHEMA staging CASCADE".to_owned()),
        ]
    }

    fn report(&self) -> Box<dyn Report> {
        Box::new(Outcome(None))
    }
}

impl Seeder for Customers {
    fn plan(&self, _: &remus_core::Schema, _: &Options) -> Result<Box<dyn Plan>, SeedError> {
        Ok(Box::new(CustomersPlan { slow: self.slow }))
    }

    fn version(&self) -> u32 {
        1
    }
}

const SCHEMA: &str = "CREATE TABLE customers (id bigint PRIMARY KEY, email text, name text NOT NULL);\n\
                      CREATE TABLE orders (id bigint PRIMARY KEY, customer_id bigint REFERENCES customers);\n";

fn model() -> Value {
    json!({ "remus_version": "0.1", "database": "app", "server_version": "17", "entities": [] })
}

fn docker(args: &[&str]) -> bool {
    Command::new("docker")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

struct Stack {
    addr: std::net::SocketAddr,
    host: Arc<Host>,
    org: Uuid,
    scope: String,
    volume: String,
    socket: std::path::PathBuf,
    http: reqwest::Client,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let label = format!("label=romulus.play.scope={}", self.scope);
        if let Ok(out) = Command::new("docker")
            .args(["ps", "-aq", "--filter", &label])
            .output()
        {
            for id in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                docker(&["rm", "-f", id]);
            }
        }
        docker(&["volume", "rm", "-f", &self.volume]);
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A runner and a service of their own, or `None` (skipped) without
/// Docker or the image.
async fn stack(tweak: impl FnOnce(&mut Config)) -> Option<Stack> {
    stack_with(tweak, Customers { slow: false }).await
}

#[allow(clippy::print_stderr)] // a skipped suite must say so
async fn stack_with(
    tweak: impl FnOnce(&mut Config),
    seeder: impl Seeder + 'static,
) -> Option<Stack> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    let image =
        std::env::var("ROMULUS_PLAY_IMAGE").unwrap_or_else(|_| "romulus-playground:dev".to_owned());
    if !docker(&["image", "inspect", &image]) {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must build the playground image ({image})"
        );
        eprintln!("SKIPPED: no Docker or no {image} (just playground-image)");
        return None;
    }
    let short = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let scope = format!("test-{short}");
    let volume = format!("romulus-play-test-{short}");
    assert!(
        docker(&["volume", "create", &volume]),
        "docker volume create"
    );
    let socket = std::path::PathBuf::from(format!("/tmp/rpt-{short}.sock"));
    let runtime = std::env::var("ROMULUS_PLAY_RUNTIME").unwrap_or_else(|_| "runc".to_owned());
    let tcp = !runtime.starts_with("runsc");
    let runner = RunnerServer::new(RunnerConfig {
        listen: socket.clone(),
        docker: std::env::var("ROMULUS_PLAY_DOCKER")
            .unwrap_or_else(|_| "/var/run/docker.sock".to_owned())
            .into(),
        image,
        quota_image: None,
        root: Root::Volume(volume.clone()),
        runtime,
        scope: scope.clone(),
        secret: SECRET.to_vec(),
        tcp,
        max_data_kb: 262_144,
    });
    let listener = UnixListener::bind(&socket).expect("bind the runner's socket");
    tokio::spawn(async move { axum::serve(listener, runner_router(runner)).await });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut config = Config {
        addr,
        key: KEY.to_vec(),
        secret: SECRET.to_vec(),
        runner_socket: socket.clone(),
        sockets: "/run/play".into(),
        max_running: 3,
        max_running_per_org: 2,
        statements: 4,
        idle: Duration::from_mins(15),
        read_deadline: Duration::from_secs(30),
        develop_deadline: Duration::from_secs(120),
        migration_deadline: Duration::from_mins(10),
    };
    tweak(&mut config);
    let host = Host::new(
        config,
        romulus_play_runner::Runner::new(&socket),
        Arc::new(seeder),
    );
    let service = Arc::clone(&host);
    tokio::spawn(async move {
        axum::serve(listener, romulus_playground::server::router(service)).await
    });
    Some(Stack {
        addr,
        host,
        org: Uuid::new_v4(),
        scope,
        volume,
        socket,
        http: reqwest::Client::new(),
    })
}

fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

impl Stack {
    fn sign(&self, op: Op, id: Option<Uuid>, access: Access, body: Option<&[u8]>) -> String {
        let mut claims = Claims::new(op, id, access, now())
            .org(self.org)
            .user(Uuid::from_u128(1));
        if let Some(body) = body {
            claims = claims.body(body);
        }
        claims.sign(KEY)
    }

    async fn ndjson<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        op: Op,
        id: Uuid,
        body: &impl serde::Serialize,
    ) -> Vec<T> {
        let body = serde_json::to_vec(body).unwrap();
        let response = self
            .http
            .post(format!("http://{}{path}", self.addr))
            .header(
                "authorization",
                self.sign(op, Some(id), Access::Develop, Some(&body)),
            )
            .body(body)
            .send()
            .await
            .expect("the service answers");
        assert_eq!(response.status(), 200, "{path}");
        let text = response.text().await.unwrap();
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("an event"))
            .collect()
    }

    /// A ready playground holding [`SCHEMA`] and 300 customers.
    async fn build(&self) -> Uuid {
        let id = Uuid::now_v7();
        let events: Vec<BuildEvent> = self
            .ndjson(
                &format!("/playgrounds/{id}/build"),
                Op::Build,
                id,
                &Build {
                    size: Size::Small,
                    profile: Profile::Realistic,
                    seed: 7,
                    schema_sql: SCHEMA.to_owned(),
                    model: model(),
                    extensions: Vec::new(),
                },
            )
            .await;
        match events.last() {
            Some(BuildEvent::Ready { tables, report, .. }) => {
                let customers = tables.iter().find(|t| t.table == "customers").unwrap();
                assert_eq!(customers.rows, 300);
                // The outcome query ran before the staging schema went.
                assert_eq!(report["outcome"], json!({ "rows": 300 }));
                // Progress names the table the rows are for.
                assert!(events.iter().any(|e| matches!(
                    e,
                    BuildEvent::Table { schema, table, rows: 300, .. } if schema == "public" && table == "customers"
                )));
            }
            other => panic!("the build did not finish: {other:?} ({events:?})"),
        }
        id
    }

    async fn post(&self, path: &str, op: Op, id: Uuid, body: Option<Vec<u8>>) -> reqwest::Response {
        let mut request = self
            .http
            .post(format!("http://{}{path}", self.addr))
            .header(
                "authorization",
                self.sign(op, Some(id), Access::Develop, body.as_deref()),
            );
        if let Some(body) = body {
            request = request.body(body);
        }
        request.send().await.expect("the service answers")
    }

    /// A console that may not reach `ready`: its first events as they come.
    async fn console_raw(&self, id: Uuid, access: Access) -> Console {
        let mut request = format!("ws://{}/playgrounds/{id}/console", self.addr)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            self.sign(Op::Console, Some(id), access, None)
                .parse()
                .unwrap(),
        );
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("the console opens");
        Console { socket, next: 0 }
    }

    async fn console(&self, id: Uuid, access: Access) -> Console {
        let mut console = self.console_raw(id, access).await;
        console
            .until(|e| matches!(e, ConsoleEvent::Ready { .. }))
            .await;
        console
    }
}

struct Console {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next: u32,
}

impl Console {
    async fn event(&mut self) -> ConsoleEvent {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(60), self.socket.next())
                .await
                .expect("an event within a minute")
                .expect("the socket is open")
                .expect("a frame");
            if let tungstenite::Message::Text(text) = frame {
                return serde_json::from_str(&text).expect("a console event");
            }
        }
    }

    async fn until(&mut self, done: impl Fn(&ConsoleEvent) -> bool) -> Vec<ConsoleEvent> {
        let mut seen = Vec::new();
        loop {
            let event = self.event().await;
            let last = done(&event);
            seen.push(event);
            if last {
                return seen;
            }
        }
    }

    async fn send(&mut self, request: &ConsoleRequest) {
        self.socket
            .send(tungstenite::Message::Text(
                serde_json::to_string(request).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    /// Runs `sql`; every event up to its `finished`.
    async fn run(&mut self, sql: &str) -> Vec<ConsoleEvent> {
        self.next += 1;
        let id = format!("r{}", self.next);
        self.send(&ConsoleRequest::Run {
            id: id.clone(),
            sql: sql.to_owned(),
            explain: false,
        })
        .await;
        self.until(|e| matches!(e, ConsoleEvent::Finished { id: done, .. } if *done == id))
            .await
    }
}

fn finished(events: &[ConsoleEvent]) -> (RunStatus, Option<String>) {
    match events.last() {
        Some(ConsoleEvent::Finished { status, error, .. }) => (*status, error.clone()),
        other => panic!("no finished event: {other:?}"),
    }
}

fn errors(events: &[ConsoleEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ConsoleEvent::Error { error, .. } => {
                Some(format!("{} {}", error.sqlstate, error.message))
            }
            _ => None,
        })
        .collect()
}

fn rows(events: &[ConsoleEvent]) -> Vec<Vec<Option<String>>> {
    events
        .iter()
        .filter_map(|e| match e {
            ConsoleEvent::Rows { rows, .. } => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
async fn the_console_reads_and_its_role_cannot_write_however_it_asks() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let mut console = stack.console(id, Access::Read).await;

    let read = console
        .run("SELECT id, email, name FROM customers WHERE id <= 2 ORDER BY id")
        .await;
    assert_eq!(finished(&read).0, RunStatus::Ok);
    assert_eq!(
        rows(&read),
        vec![
            vec![
                Some("1".into()),
                Some("c1@example.com".into()),
                Some("Customer\t1".into())
            ],
            vec![
                Some("2".into()),
                Some("c2@example.com".into()),
                Some("Customer\t2".into())
            ],
        ]
    );
    let columns = read.iter().find_map(|e| match e {
        ConsoleEvent::Columns { columns, .. } => Some(columns.clone()),
        _ => None,
    });
    let columns = columns.expect("columns");
    assert_eq!(columns[0].type_name, "bigint");
    assert!(columns[0].numeric);
    assert_eq!(columns[1].type_name, "text");

    // Privileges, not parsing: each of these is refused by Postgres itself,
    // whether written plainly, hidden in a DO block, or with the read-only
    // convenience switched off.
    for write in [
        "INSERT INTO customers VALUES (1000, 'x@example.com', 'x')",
        "UPDATE customers SET email = NULL",
        "DELETE FROM customers",
        "TRUNCATE customers",
        "SET default_transaction_read_only = off; INSERT INTO customers VALUES (1001, NULL, 'y')",
        "DO $$ BEGIN EXECUTE 'DELETE FROM customers'; END $$",
        "CREATE TABLE stolen (a int)",
        "CREATE TEMP TABLE scratch (a int)",
        "SELECT * INTO copied FROM customers",
        "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        "SELECT lo_from_bytea(0, 'x'::bytea)",
        "SELECT pg_terminate_backend(pg_backend_pid())",
        "CREATE EXTENSION hstore",
        "ALTER TABLE customers ADD COLUMN pwned int",
        "COPY customers TO PROGRAM 'id'",
        "SELECT pg_read_file('/etc/passwd')",
    ] {
        let events = console.run(write).await;
        let (status, error) = finished(&events);
        assert_eq!(status, RunStatus::Error, "{write} was refused");
        let errors = errors(&events);
        assert!(
            errors.iter().any(|e| e.starts_with("42501")
                || e.starts_with("25006")
                || e.starts_with("0A000")
                || e.starts_with("58P01")),
            "{write}: {errors:?} ({error:?})"
        );
    }
    let count = console
        .run("ROLLBACK; SELECT count(*) FROM customers")
        .await;
    assert_eq!(rows(&count).last().unwrap()[0].as_deref(), Some("300"));
}

#[tokio::test]
async fn a_cancelled_statement_stops_whatever_statement_timeout_says() {
    let Some(stack) = stack(|c| c.read_deadline = Duration::from_secs(3)).await else {
        return;
    };
    let id = stack.build().await;
    let mut console = stack.console(id, Access::Read).await;

    console
        .send(&ConsoleRequest::Run {
            id: "sleep".into(),
            sql: "SELECT pg_sleep(20)".into(),
            explain: false,
        })
        .await;
    console
        .until(|e| matches!(e, ConsoleEvent::Statement { .. }))
        .await;
    let started = Instant::now();
    console
        .send(&ConsoleRequest::Cancel { id: "sleep".into() })
        .await;
    let events = console
        .until(|e| matches!(e, ConsoleEvent::Finished { .. }))
        .await;
    assert_eq!(finished(&events).0, RunStatus::Cancelled);
    assert!(errors(&events)[0].starts_with("57014"), "{events:?}");
    assert!(started.elapsed() < Duration::from_secs(5));

    // Past the deadline, whatever statement_timeout says.
    let events = console
        .run("SET statement_timeout = 0; SELECT pg_sleep(20)")
        .await;
    let (status, error) = finished(&events);
    assert_eq!(status, RunStatus::Cancelled);
    assert!(error.unwrap().contains("3 s limit"), "{events:?}");

    // The session is still good.
    let events = console.run("SELECT 1 AS one").await;
    assert_eq!(rows(&events), vec![vec![Some("1".to_owned())]]);
}

fn done(events: &[ConsoleEvent], at: usize) -> (u64, bool) {
    events
        .iter()
        .find_map(|e| match e {
            ConsoleEvent::Done {
                index,
                rows,
                truncated,
                ..
            } if *index == at => Some((*rows, *truncated)),
            _ => None,
        })
        .expect("the statement finished")
}

fn rows_of(events: &[ConsoleEvent], at: usize) -> Vec<Vec<Option<String>>> {
    events
        .iter()
        .filter_map(|e| match e {
            ConsoleEvent::Rows { index, rows, .. } if *index == at => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
async fn rows_past_the_cap_are_dropped_without_cancelling_what_follows() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let mut console = stack.console(id, Access::Read).await;

    // Exactly the cap: nothing left out, and the next statement runs.
    let events = console
        .run("SELECT generate_series(1, 1000); SELECT 2 AS two")
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));
    assert_eq!(done(&events, 0), (1000, false));
    assert_eq!(rows_of(&events, 0).len(), 1000);
    assert_eq!(rows_of(&events, 1), vec![vec![Some("2".to_owned())]]);

    // Past it: the first 1 000 kept, and the statement stops at the 1 001st
    // row by itself (it does not run on to its end, holding a slot), with no
    // cancel to reach the statement after it.
    let started = Instant::now();
    let events = console
        .run("SELECT generate_series(1, 50000000); SELECT 3 AS three")
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));
    assert_eq!(done(&events, 0), (1000, true));
    assert_eq!(rows_of(&events, 0).len(), 1000);
    assert_eq!(rows_of(&events, 1), vec![vec![Some("3".to_owned())]]);
    // Ten billion rows of a cross join, the same.
    let started = Instant::now();
    let events = console
        .run("SELECT a, b FROM generate_series(1, 100000) a, generate_series(1, 100000) b")
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(done(&events, 0), (1000, true));
    let events = console.run("SELECT 4 AS four").await;
    assert_eq!(rows(&events), vec![vec![Some("4".to_owned())]]);
}

#[tokio::test]
async fn a_migration_run_reports_locks_rewrites_and_the_rows_that_break_it() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let statement = |sql: &str| MigrationStatement {
        sql: sql.to_owned(),
        hazard: None,
        outside_transaction: false,
    };
    let events: Vec<MigrationEvent> = stack
        .ndjson(
            &format!("/playgrounds/{id}/migration"),
            Op::Migrate,
            id,
            &Migrate {
                statements: vec![
                    statement("ALTER TABLE customers ADD COLUMN status text;"),
                    statement("ALTER TABLE customers ALTER COLUMN id TYPE numeric(20,0);"),
                    statement("UPDATE customers SET status = 'active' WHERE id < 100;"),
                    statement("ALTER TABLE customers ALTER COLUMN email SET NOT NULL;"),
                    statement("ALTER TABLE customers DROP COLUMN name;"),
                ],
            },
        )
        .await;
    let Some(MigrationEvent::Done { report }) = events.last() else {
        panic!("no report: {events:?}");
    };
    assert!(!report.passed);
    assert_eq!(report.failed_at, Some(3));
    let s = &report.statements;
    assert_eq!(s[0].status, StatementStatus::Passed);
    assert!(
        s[0].locks
            .iter()
            .any(|l| l.relation == "public.customers" && l.mode == "AccessExclusiveLock")
    );
    assert!(
        s[0].rewrote.is_empty(),
        "adding a nullable column rewrites nothing"
    );
    assert_eq!(
        s[1].rewrote,
        vec!["public.customers".to_owned()],
        "a type change rewrites"
    );
    assert_eq!(s[2].rows_changed, Some(99));
    assert_eq!(s[3].status, StatementStatus::Failed);
    assert_eq!(s[3].error.as_ref().unwrap().sqlstate, "23502");
    let offending = s[3].offending.as_ref().expect("the rows without an email");
    assert_eq!(offending.count, 30);
    assert_eq!(offending.sample.len(), 5);
    assert_eq!(s[4].status, StatementStatus::NotRun);

    // The migrated rows stay queryable (the first three statements committed),
    // until Reset puts the generated ones back.
    let mut console = stack.console(id, Access::Read).await;
    let events = console
        .run("SELECT count(*) FROM customers WHERE status = 'active'")
        .await;
    assert_eq!(rows(&events)[0][0].as_deref(), Some("99"));
    let reset = stack
        .http
        .post(format!("http://{}/playgrounds/{id}/reset", stack.addr))
        .header(
            "authorization",
            stack.sign(Op::Reset, Some(id), Access::Develop, None),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), 200);
    console
        .until(|e| matches!(e, ConsoleEvent::Ready { .. }))
        .await;
    let events = console
        .run("SELECT count(*) FROM customers WHERE status IS NOT NULL")
        .await;
    assert!(
        errors(&events)[0].contains("column \"status\" does not exist"),
        "{events:?}"
    );
}

#[tokio::test]
async fn calls_the_api_did_not_sign_are_refused() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = Uuid::now_v7();
    let url = format!("http://{}/playgrounds/{id}/reset", stack.addr);
    let unsigned = stack.http.post(&url).send().await.unwrap();
    assert_eq!(unsigned.status(), 401);
    // Signed for another playground, or with another key.
    let other = stack.sign(Op::Reset, Some(Uuid::now_v7()), Access::Develop, None);
    let response = stack
        .http
        .post(&url)
        .header("authorization", other)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let forged = Claims::new(Op::Reset, Some(id), Access::Develop, now())
        .sign(b"not the key, but 32 bytes long....");
    let response = stack
        .http
        .post(&url)
        .header("authorization", forged)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    // A reviewer's signature does not run migrations.
    let body = serde_json::to_vec(&Migrate { statements: vec![] }).unwrap();
    let response = stack
        .http
        .post(format!("http://{}/playgrounds/{id}/migration", stack.addr))
        .header(
            "authorization",
            stack.sign(Op::Migrate, Some(id), Access::Read, Some(&body)),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn open_tabs_do_not_hold_a_playground_and_a_failed_start_says_why() {
    let Some(stack) = stack(|c| {
        c.max_running_per_org = 1;
        c.read_deadline = Duration::from_secs(20);
    })
    .await
    else {
        return;
    };
    let first = stack.build().await;
    // Building the second stopped the first.
    let second = stack.build().await;
    assert!(!stack.host.status(first).await.unwrap().running);
    assert!(stack.host.status(second).await.unwrap().running);

    // A statement running in the second holds it: the first cannot start,
    // and its console says why (not "comes back by itself").
    let mut busy = stack.console(second, Access::Read).await;
    busy.send(&ConsoleRequest::Run {
        id: "sleep".into(),
        sql: "SELECT pg_sleep(4)".into(),
        explain: false,
    })
    .await;
    busy.until(|e| matches!(e, ConsoleEvent::Statement { .. }))
        .await;
    let mut console = stack.console_raw(first, Access::Read).await;
    let events = console
        .until(|e| {
            matches!(
                e,
                ConsoleEvent::Unavailable { .. } | ConsoleEvent::Ready { .. }
            )
        })
        .await;
    match events.last() {
        Some(ConsoleEvent::Unavailable { code, reason }) => {
            assert_eq!(code, "org_full");
            assert!(reason.contains("in use"), "{reason}");
        }
        other => panic!("the first started past the cap: {other:?}"),
    }
    busy.until(|e| matches!(e, ConsoleEvent::Finished { .. }))
        .await;

    // Try again: the second has an open tab but no statement, so it makes
    // room, and its tab hears it was stopped.
    console.send(&ConsoleRequest::Open).await;
    console
        .until(|e| matches!(e, ConsoleEvent::Ready { .. }))
        .await;
    let events = busy
        .until(|e| matches!(e, ConsoleEvent::Stopped { .. }))
        .await;
    let Some(ConsoleEvent::Stopped { reason }) = events.last() else {
        unreachable!()
    };
    assert!(reason.contains("make room"), "{reason}");
    assert!(!stack.host.status(second).await.unwrap().running);

    // A run in the stopped tab starts its playground again (stopping the
    // first, idle now).
    let events = busy.run("SELECT count(*) FROM customers").await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{events:?}");
    assert!(events.iter().any(|e| matches!(e, ConsoleEvent::Starting)));
    assert_eq!(rows(&events)[0][0].as_deref(), Some("300"));
    console
        .until(|e| matches!(e, ConsoleEvent::Stopped { .. }))
        .await;
}

#[tokio::test]
async fn idleness_counts_from_the_last_statement_whatever_tabs_are_open() {
    let Some(stack) = stack(|c| c.idle = Duration::from_secs(1)).await else {
        return;
    };
    let id = stack.build().await;
    let mut console = stack.console(id, Access::Read).await;
    let events = console.run("SELECT 1").await;
    assert_eq!(finished(&events).0, RunStatus::Ok);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The tab is still open: the playground stops anyway, and says why.
    stack.host.reap().await;
    let events = console
        .until(|e| matches!(e, ConsoleEvent::Stopped { .. }))
        .await;
    let Some(ConsoleEvent::Stopped { reason }) = events.last() else {
        unreachable!()
    };
    assert!(reason.contains("without a query"), "{reason}");
    assert!(!stack.host.status(id).await.unwrap().running);

    // Its next run starts it again.
    let events = console.run("SELECT count(*) FROM customers").await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{events:?}");
    assert_eq!(rows(&events)[0][0].as_deref(), Some("300"));

    // A statement running is not idleness.
    console
        .send(&ConsoleRequest::Run {
            id: "long".into(),
            sql: "SELECT pg_sleep(3)".into(),
            explain: false,
        })
        .await;
    console
        .until(|e| matches!(e, ConsoleEvent::Statement { .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    stack.host.reap().await;
    assert!(stack.host.status(id).await.unwrap().running);
    let events = console
        .until(|e| matches!(e, ConsoleEvent::Finished { .. }))
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{events:?}");
}

#[tokio::test]
async fn a_busy_playground_answers_at_once_and_delete_cancels_what_runs() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let statement = |sql: &str| MigrationStatement {
        sql: sql.to_owned(),
        hazard: None,
        outside_transaction: false,
    };
    let body = serde_json::to_vec(&Migrate {
        statements: vec![statement("SELECT pg_sleep(60);")],
    })
    .unwrap();
    let running = stack
        .post(
            &format!("/playgrounds/{id}/migration"),
            Op::Migrate,
            id,
            Some(body.clone()),
        )
        .await;
    assert_eq!(running.status(), 200);
    let deadline = Instant::now() + Duration::from_secs(30);
    while stack.host.status(id).await.unwrap().busy.as_deref() != Some("migrating") {
        assert!(Instant::now() < deadline, "the migration started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Reset and another migration: refused now, not queued.
    let started = Instant::now();
    let reset = stack
        .post(&format!("/playgrounds/{id}/reset"), Op::Reset, id, None)
        .await;
    assert_eq!(reset.status(), 409);
    let again = stack
        .post(
            &format!("/playgrounds/{id}/migration"),
            Op::Migrate,
            id,
            Some(body),
        )
        .await;
    assert_eq!(again.status(), 409);
    assert!(started.elapsed() < Duration::from_secs(3));

    // Delete cancels the run and destroys it.
    let started = Instant::now();
    let response = stack
        .http
        .delete(format!("http://{}/playgrounds/{id}", stack.addr))
        .header(
            "authorization",
            stack.sign(Op::Destroy, Some(id), Access::Develop, None),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "{:?}",
        started.elapsed()
    );
    let _ = running.text().await;
    assert!(!docker(&["inspect", &format!("romulus-play-{id}")]));
}

#[tokio::test]
async fn deleting_a_playground_while_it_builds_cancels_the_build() {
    let Some(stack) = stack_with(|_| {}, Customers { slow: true }).await else {
        return;
    };
    let id = Uuid::now_v7();
    let body = serde_json::to_vec(&Build {
        size: Size::Small,
        profile: Profile::Realistic,
        seed: 7,
        schema_sql: SCHEMA.to_owned(),
        model: model(),
        extensions: Vec::new(),
    })
    .unwrap();
    let building = stack
        .post(
            &format!("/playgrounds/{id}/build"),
            Op::Build,
            id,
            Some(body),
        )
        .await;
    assert_eq!(building.status(), 200);
    // Into its rows (30 s of them).
    let deadline = Instant::now() + Duration::from_secs(60);
    while !stack.host.status(id).await.unwrap().running {
        assert!(Instant::now() < deadline, "the build started its database");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let started = Instant::now();
    let response = stack
        .http
        .delete(format!("http://{}/playgrounds/{id}", stack.addr))
        .header(
            "authorization",
            stack.sign(Op::Destroy, Some(id), Access::Develop, None),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "{:?}",
        started.elapsed()
    );
    // The stream ends without a `ready`.
    let text = building.text().await.unwrap();
    assert!(!text.contains("\"ready\""), "{text}");
    assert!(!docker(&["inspect", &format!("romulus-play-{id}")]));
}

#[tokio::test]
async fn a_role_changed_from_inside_is_put_back_by_a_restart() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let mut console = stack.console(id, Access::Read).await;
    // Postgres lets any role do both to itself.
    let events = console
        .run(
            "SET default_transaction_read_only = off; \
             ALTER ROLE play_read SET statement_timeout = '1ms'",
        )
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));
    let events = console
        .run("ALTER ROLE play_read PASSWORD 'locked-out'")
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));

    // The next sessions (two at once, as a page opening twice does) restart
    // the playground once, and both work.
    let (mut other, mut third) = tokio::join!(
        stack.console_raw(id, Access::Read),
        stack.console_raw(id, Access::Read)
    );
    let (opened, also) = tokio::join!(
        other.until(|e| matches!(
            e,
            ConsoleEvent::Ready { .. } | ConsoleEvent::Unavailable { .. }
        )),
        third.until(|e| matches!(
            e,
            ConsoleEvent::Ready { .. } | ConsoleEvent::Unavailable { .. }
        )),
    );
    for events in [&opened, &also] {
        assert!(
            matches!(events.last(), Some(ConsoleEvent::Ready { .. })),
            "{events:?}"
        );
    }
    let events = third.run("SELECT 1").await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{events:?}");
    let events = other
        .run("SELECT count(*), current_setting('statement_timeout') FROM customers")
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{events:?}");
    assert_eq!(
        rows(&events)[0],
        vec![Some("300".to_owned()), Some("0".to_owned())]
    );
    // The first tab was paused for the restart and is back.
    console
        .until(|e| matches!(e, ConsoleEvent::Ready { .. }))
        .await;
    // Exactly one of the two sessions restarted it (the API audits that).
    let restarts = [&opened, &also]
        .iter()
        .flat_map(|events| events.iter())
        .filter(|e| matches!(e, ConsoleEvent::Restarted { .. }))
        .count();
    assert_eq!(restarts, 1);

    // Again within the minute: not restarted again. A setting stays until
    // the next start; a login changed again says to try in a minute.
    let events = console
        .run(
            "SET default_transaction_read_only = off; \
             ALTER ROLE play_read PASSWORD 'locked-out-again'",
        )
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));
    let mut late = stack.console_raw(id, Access::Read).await;
    let events = late
        .until(|e| {
            matches!(
                e,
                ConsoleEvent::Ready { .. } | ConsoleEvent::Unavailable { .. }
            )
        })
        .await;
    match events.last() {
        Some(ConsoleEvent::Unavailable { reason, .. }) => {
            assert!(reason.contains("Try again in a minute"), "{reason}");
        }
        other => panic!("restarted twice within a minute: {other:?}"),
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ConsoleEvent::Restarted { .. }))
    );
}

/// The rows of `sql`'s first column, as text.
fn column(events: &[ConsoleEvent]) -> Vec<String> {
    rows(events)
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect()
}

/// The script the API sends for `sql` (`compile::playground_script`):
/// compiled in the sandbox (Postgres 17, the playground's), its policy
/// roles made PUBLIC, its extensions left to the runner. `None` (skipped)
/// without `ROMULUS_SANDBOX_URL`; in CI that is a failure.
#[allow(clippy::print_stderr)] // a skipped test must say so
async fn playground_script(sql: &str) -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    let Ok(url) = std::env::var("ROMULUS_SANDBOX_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must run the sandbox (ROMULUS_SANDBOX_URL)"
        );
        eprintln!("SKIPPED: no ROMULUS_SANDBOX_URL (just sandbox)");
        return None;
    };
    let compiler = romulus_sandbox::Compiler::start(romulus_sandbox::SandboxConfig {
        major: Some(17),
        pool_size: 1,
        prefix: "romulus_playtest".to_owned(),
        ..romulus_sandbox::SandboxConfig::new(url)
    })
    .await
    .expect("the sandbox starts");
    let compilation = compiler.compile(sql).await.expect("the sandbox compiles");
    assert!(compilation.is_ok(), "{:?}", compilation.diagnostics);
    Some(romulus_sandbox::playground::script(
        sql,
        &compilation,
        compiler.catalog(),
    ))
}

#[tokio::test]
async fn romulus_fakedata_fills_a_real_schema() {
    // remus's showcase: schemas, enums, domains, a partitioned table, views,
    // a materialized view, foreign keys, a policy, two extensions.
    let model =
        remus_core::Schema::from_json(remus_core::fixtures::SHOWCASE).expect("the showcase");
    let Some(schema_sql) = playground_script(&romulus_sandbox::render_sql(&model)).await else {
        return;
    };
    let Some(stack) = stack_with(|_| {}, FakeData).await else {
        return;
    };
    let id = Uuid::now_v7();
    let events: Vec<BuildEvent> = stack
        .ndjson(
            &format!("/playgrounds/{id}/build"),
            Op::Build,
            id,
            &Build {
                size: Size::Small,
                profile: Profile::Realistic,
                seed: 0x7f_3a91,
                schema_sql,
                model: serde_json::from_str(remus_core::fixtures::SHOWCASE).unwrap(),
                extensions: model
                    .extensions
                    .iter()
                    .map(|e| romulus_playground::protocol::Extension {
                        name: e.name.clone(),
                        schema: e.schema.clone(),
                    })
                    .collect(),
            },
        )
        .await;
    let Some(BuildEvent::Ready {
        tables,
        report,
        generator,
        ..
    }) = events.last()
    else {
        panic!("the build did not finish: {events:?}");
    };
    assert_eq!(*generator, romulus_fakedata::GENERATOR_VERSION.to_string());
    let rows_in = |schema: &str, table: &str| {
        tables
            .iter()
            .find(|t| t.schema == schema && t.table == table)
            .map_or(0, |t| t.rows)
    };
    assert!(rows_in("shop", "customers") > 0, "{tables:?}");
    assert!(rows_in("shop", "orders") > 0, "{tables:?}");
    // The partitioned table counts its partitions' rows.
    assert!(rows_in("audit", "events") > 0, "{tables:?}");
    // The outcome went into the report before it was read, and its
    // sentences are there for the page.
    assert!(
        report["rows_inserted"].as_u64().unwrap_or(0) > 0,
        "{report}"
    );
    assert!(report["lines"].is_array(), "{report}");
    // Progress named the tables the rows are for, not the staging ones.
    assert!(events.iter().any(|e| matches!(
        e,
        BuildEvent::Table { schema, .. } if schema == "shop"
    )));

    // The console reads them, and the foreign keys hold (stock and
    // products: no row-level security there).
    let mut console = stack.console(id, Access::Read).await;
    let events = console
        .run(
            "SELECT count(*) FROM shop.stock s \
             WHERE NOT EXISTS (SELECT 1 FROM shop.products p WHERE p.id = s.product_id)",
        )
        .await;
    assert_eq!(finished(&events).0, RunStatus::Ok, "{:?}", errors(&events));
    assert_eq!(column(&events), ["0"]);
    // Orders force row-level security: the console sees what its policies
    // let it, as the application would, once their setting is there.
    let events = console.run("SELECT count(*) FROM shop.orders").await;
    assert!(errors(&events)[0].contains("app.customer_id"), "{events:?}");
    // Nothing of the load is left behind (its staging schema).
    let events = console
        .run("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'romulus_fakedata%'")
        .await;
    assert_eq!(column(&events), ["0"]);
}

#[tokio::test]
async fn a_migration_statement_returning_millions_of_rows_streams_through() {
    let Some(stack) = stack(|_| {}).await else {
        return;
    };
    let id = stack.build().await;
    let statement = |sql: &str| MigrationStatement {
        sql: sql.to_owned(),
        hazard: None,
        outside_transaction: false,
    };
    // Fifty million rows nobody reads: none is kept (the service has 512 MB).
    let events: Vec<MigrationEvent> = stack
        .ndjson(
            &format!("/playgrounds/{id}/migration"),
            Op::Migrate,
            id,
            &Migrate {
                statements: vec![
                    statement("SELECT generate_series(1, 50000000);"),
                    statement("ALTER TABLE customers ADD COLUMN status text;"),
                ],
            },
        )
        .await;
    let Some(MigrationEvent::Done { report }) = events.last() else {
        panic!("no report: {events:?}");
    };
    assert!(report.passed, "{report:?}");
    assert_eq!(report.statements[0].tag.as_deref(), Some("SELECT 50000000"));
    assert_eq!(report.statements[1].status, StatementStatus::Passed);
    // The service is still up, and the playground answers.
    let health = stack
        .http
        .get(format!("http://{}/health", stack.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let mut console = stack.console(id, Access::Read).await;
    let events = console.run("SELECT count(status) FROM customers").await;
    assert_eq!(rows(&events), vec![vec![Some("0".to_owned())]]);
}

#[tokio::test]
async fn a_slow_seeding_step_fails_its_build_promptly() {
    // Rows every 100 ms (30 s of them) against a 2 s deadline per step.
    let Some(stack) = stack_with(
        |c| c.develop_deadline = Duration::from_secs(2),
        Customers { slow: true },
    )
    .await
    else {
        return;
    };
    let id = Uuid::now_v7();
    let started = Instant::now();
    let events: Vec<BuildEvent> = stack
        .ndjson(
            &format!("/playgrounds/{id}/build"),
            Op::Build,
            id,
            &Build {
                size: Size::Small,
                profile: Profile::Realistic,
                seed: 7,
                schema_sql: SCHEMA.to_owned(),
                model: model(),
                extensions: Vec::new(),
            },
        )
        .await;
    match events.last() {
        Some(BuildEvent::Failed { stage, message }) => {
            assert_eq!(*stage, romulus_playground::protocol::Stage::Rows);
            assert!(
                message.contains("the rows of public.customers took more than 2 s"),
                "{message}"
            );
        }
        other => panic!("the build did not fail: {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "{:?}",
        started.elapsed()
    );
    // A failed build leaves nothing.
    assert!(!docker(&["inspect", &format!("romulus-play-{id}")]));
}
