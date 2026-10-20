//! The engine on real databases: a scratch database of the sandbox
//! cluster per case (as its superuser), the base schema built there, the
//! models read back by remus's introspection, then a test run.
//!
//! Needs `ROMULUS_SANDBOX_URL` (`.env`, `just sandbox`): skipped, loudly,
//! without it on a laptop, and failed without it in CI (`CI` set).

use std::{
    future::Future,
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use remus_core::Schema;
use romulus_migration_check::{
    Check, DownStatus, Event, Flag, LossKind, Migration, NoteKind, Options, Phase, Report, Seed,
    StatementStatus, Status, Verdict, introspect,
    session::{Answer, DbError, Rows, Session, SessionError},
    statements::Statement,
};
use sqlx::{AssertSqlSafe, Connection, PgConnection, raw_sql};
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
});

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

fn with_sandbox<F, Fut>(body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "ROMULUS_SANDBOX_URL is not set in CI: the migration check's tests cannot run"
        );
        #[allow(clippy::print_stderr)]
        {
            eprintln!("SKIPPED: ROMULUS_SANDBOX_URL is not set, the migration check did not run");
        }
        return;
    };
    RUNTIME.block_on(body(url));
}

async fn scratch(admin: &str) -> (String, PgConnection) {
    let name = format!(
        "romulus_test_check_{}_{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    );
    let mut conn = PgConnection::connect(admin).await.expect("the sandbox");
    raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut conn)
        .await
        .expect("a scratch database");
    let (base, _) = admin.rsplit_once('/').expect("an admin URL");
    let db = PgConnection::connect(&format!("{base}/{name}"))
        .await
        .expect("the scratch database");
    (name, db)
}

async fn drop_scratch(admin: &str, name: &str, db: PgConnection) {
    let _ = db.close().await;
    if let Ok(mut conn) = PgConnection::connect(admin).await {
        let _ = raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
    }
}

async fn script(db: &mut PgConnection, sql: &str) {
    raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(&mut *db)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

/// The model `sql` builds, read back from a database.
async fn model_of(admin: &str, sql: &str) -> Schema {
    let (name, mut db) = scratch(admin).await;
    script(&mut db, sql).await;
    let model = introspect(&mut db, Duration::from_secs(30))
        .await
        .expect("introspection");
    drop_scratch(admin, &name, db).await;
    model
}

/// Runs `check` on a database holding `base_sql`.
async fn run(admin: &str, base_sql: &str, check: &Check) -> (Report, Vec<Event>) {
    let (name, mut db) = scratch(admin).await;
    script(&mut db, base_sql).await;
    let mut events = Vec::new();
    let report = romulus_migration_check::run(&mut db, check, &mut |e| events.push(e)).await;
    drop_scratch(admin, &name, db).await;
    (report, events)
}

fn planned(old: &Schema, new: &Schema) -> Migration {
    Migration::planned(&romulus_migration::migration(
        old,
        new,
        &romulus_migration::Options::default(),
    ))
}

fn check(
    base: &Schema,
    target: &Schema,
    up: Migration,
    down: Option<Migration>,
    seed: Seed,
) -> Check {
    Check {
        base: base.clone(),
        target: target.clone(),
        up,
        down,
        seed,
        options: Options {
            sample: 10,
            base_label: "v1".to_owned(),
            target_label: "v2".to_owned(),
            ..Options::default()
        },
    }
}

fn generated(seed: u64) -> Seed {
    Seed::Generate(romulus_fakedata::Options::new(
        romulus_fakedata::Size::Small,
        romulus_fakedata::Profile::Realistic,
        seed,
    ))
}

fn explain(report: &Report) -> String {
    serde_json::to_string_pretty(report).unwrap_or_default()
}

fn notes(report: &Report, phase: Phase) -> Vec<(NoteKind, Option<String>, Status)> {
    report
        .phase(phase)
        .into_iter()
        .flat_map(|p| &p.checks)
        .flat_map(|c| &c.tables)
        .flat_map(|t| &t.notes)
        .map(|n| (n.kind, n.column.clone(), n.status))
        .collect()
}

const SHOP: &str = "
CREATE TABLE customers (id integer PRIMARY KEY, email text NOT NULL UNIQUE, name text, legacy_code text);
CREATE TABLE orders (
    id bigint PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    total numeric(10,2) NOT NULL,
    note text,
    created_at timestamptz NOT NULL
);
CREATE TABLE events (at timestamptz, kind text);
";

#[test]
fn a_migration_that_keeps_everything_passes_up_and_down() {
    with_sandbox(|admin| async move {
        let base = model_of(&admin, SHOP).await;
        let target = model_of(
            &admin,
            &format!("{SHOP}\nALTER TABLE orders ADD COLUMN shipped_at timestamptz;\nCREATE INDEX ON orders (created_at);"),
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let (report, events) = run(
            &admin,
            SHOP,
            &check(&base, &target, up, Some(down), generated(7)),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        assert_eq!(report.down_status, DownStatus::Ran);
        assert!(report.rows > 100, "rows were generated: {}", report.rows);
        assert!(report.data_loss.is_empty(), "{:?}", report.data_loss);
        for phase in [
            Phase::Seed,
            Phase::Before,
            Phase::Up,
            Phase::AfterUp,
            Phase::Down,
            Phase::AfterDown,
        ] {
            assert_eq!(
                report.phase(phase).map(|p| p.status),
                Some(Status::Passed),
                "{phase:?}: {}",
                explain(&report)
            );
        }
        // Each statement measured: an ADD COLUMN takes ACCESS EXCLUSIVE.
        let add = &report.up.statements[0];
        assert_eq!(add.status, StatementStatus::Passed);
        assert!(
            add.locks
                .iter()
                .any(|l| l.relation == "public.orders" && l.mode == "AccessExclusiveLock"),
            "{:?}",
            add.locks
        );
        // The table without a key is counted, not followed.
        assert!(
            notes(&report, Phase::AfterUp).contains(&(NoteKind::NotCompared, None, Status::Passed)),
            "{:?}",
            notes(&report, Phase::AfterUp)
        );
        assert!(events.iter().any(|e| matches!(e, Event::Statement { .. })));
        assert!(
            report.headline.ends_with("Everything came back."),
            "{}",
            report.headline
        );
    });
}

#[test]
fn a_renamed_column_keeps_its_values_and_comes_back() {
    with_sandbox(|admin| async move {
        let base = model_of(&admin, SHOP).await;
        let target = model_of(
            &admin,
            &format!("{SHOP}\nALTER TABLE customers RENAME COLUMN name TO full_name;\nALTER TABLE orders RENAME TO purchases;"),
        )
        .await;
        let up = Migration::written(
            "ALTER TABLE customers RENAME COLUMN name TO full_name;\nALTER TABLE orders RENAME TO purchases;",
        )
        .unwrap();
        let down = Migration::written(
            "ALTER TABLE purchases RENAME TO orders;\nALTER TABLE customers RENAME COLUMN full_name TO name;",
        )
        .unwrap();
        let (report, _) = run(
            &admin,
            SHOP,
            &check(&base, &target, up, Some(down), generated(11)),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Renamed, Some("full_name".into()), Status::Passed)),
            "{after_up:?}"
        );
        let renamed_table = report
            .phase(Phase::AfterUp)
            .unwrap()
            .checks
            .iter()
            .flat_map(|c| &c.tables)
            .find(|t| t.table == "public.purchases")
            .expect("the renamed table is listed");
        assert_eq!(renamed_table.from.as_deref(), Some("public.orders"));
        assert_eq!(renamed_table.before, renamed_table.after);
        assert!(report.data_loss.is_empty(), "{:?}", report.data_loss);
    });
}

#[test]
fn a_conversion_is_compared_through_its_cast_and_rounding_is_data_loss() {
    with_sandbox(|admin| async move {
        let base_sql =
            "CREATE TABLE prices (id integer PRIMARY KEY, amount numeric(10,2), label text);";
        let rows = "INSERT INTO prices VALUES (1, 1.60, 'a'), (2, 2.00, 'b'), (3, 3.49, 'c'), (4, NULL, 'd');";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TABLE prices (id integer PRIMARY KEY, amount integer, label text);",
        )
        .await;
        let up = planned(&base, &target);
        assert!(
            up.statements
                .iter()
                .any(|s| s.sql.contains("USING") && s.hazard.as_deref() == Some("deletes_data")),
            "{:?}",
            up.statements
        );
        let down = planned(&target, &base);
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(rows.to_owned())),
        )
        .await;
        // Rounding is flagged: the test passes, and says what is lost.
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Converted, Some("amount".into()), Status::Passed)),
            "{after_up:?}"
        );
        assert!(
            after_up.contains(&(NoteKind::Rounded, Some("amount".into()), Status::Warning)),
            "{after_up:?}"
        );
        let loss = report
            .data_loss
            .iter()
            .find(|l| l.column.as_deref() == Some("amount"))
            .unwrap_or_else(|| panic!("a loss on amount: {:?}", report.data_loss));
        assert_eq!(loss.kind, LossKind::ValuesChanged);
        assert_eq!(loss.rows, 2, "1.60 and 3.49 do not come back: {loss:?}");
        assert!(loss.expected);
        assert_eq!(loss.cause.as_ref().map(|c| c.flag), Some(Flag::Flagged));
        assert!(
            loss.examples
                .iter()
                .any(|e| e.before.as_deref() == Some("1.60") && e.after.as_deref() == Some("2.00")),
            "{loss:?}"
        );
    });
}

#[test]
fn a_dropped_column_is_data_loss_after_down_as_its_flag_says() {
    with_sandbox(|admin| async move {
        let base = model_of(&admin, SHOP).await;
        let target_sql = SHOP.replace(", legacy_code text", "");
        let target = model_of(&admin, &target_sql).await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let (report, _) = run(
            &admin,
            SHOP,
            &check(&base, &target, up, Some(down), generated(3)),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        let loss = report
            .data_loss
            .iter()
            .find(|l| l.column.as_deref() == Some("legacy_code"))
            .unwrap_or_else(|| panic!("a loss on legacy_code: {}", explain(&report)));
        assert_eq!(loss.kind, LossKind::ColumnDropped);
        assert!(loss.expected);
        assert!(loss.rows > 0);
        assert!(
            loss.message.contains("dropped by statement"),
            "{}",
            loss.message
        );
        assert_eq!(
            report.phase(Phase::AfterDown).unwrap().status,
            Status::Warning
        );
        // Without its flag, the same loss fails the test: the migration did
        // something it did not say.
        let mut unflagged = planned(&base, &target);
        for s in &mut unflagged.statements {
            s.hazard = None;
        }
        let (report, _) = run(
            &admin,
            SHOP,
            &check(
                &base,
                &target,
                unflagged,
                Some(planned(&target, &base)),
                generated(3),
            ),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        assert!(
            report
                .data_loss
                .iter()
                .any(|l| !l.expected && l.cause.as_ref().map(|c| c.flag) == Some(Flag::NotFlagged))
        );
    });
}

#[test]
fn a_failing_statement_stops_the_test_with_postgres_error_and_rows() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE people (id integer PRIMARY KEY, nickname text);";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TABLE people (id integer PRIMARY KEY, nickname text NOT NULL);",
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let rows = "INSERT INTO people VALUES (1, 'ok'), (2, NULL), (3, NULL);";
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed);
        assert_eq!(report.up.failed_at, Some(0));
        let (_, failed) = report
            .failed_statement()
            .expect("the statement that failed");
        let error = failed.error.as_ref().expect("its error");
        assert_eq!(error.sqlstate, "23502");
        assert_eq!(failed.offending.as_ref().map(|o| o.count), Some(2));
        assert_eq!(report.down_status, DownStatus::NotRun);
        assert!(
            report
                .headline
                .starts_with("Failed at statement 1 of 1 of the up migration"),
            "{}",
            report.headline
        );
    });
}

#[test]
fn a_written_update_explains_its_changes_and_a_trigger_does_not() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE notes (id integer PRIMARY KEY, body text, shout text);";
        let rows = "INSERT INTO notes VALUES (1, 'hello', 'a'), (2, 'world', 'b');";
        let base = model_of(&admin, base_sql).await;
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &base,
                Migration::written("UPDATE notes SET body = upper(body);").unwrap(),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        assert_eq!(report.down_status, DownStatus::NotProvided);
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Changed, Some("body".into()), Status::Warning)),
            "{after_up:?}"
        );
        assert!(
            report.headline.contains("No down migration"),
            "{}",
            report.headline
        );

        // A trigger changing another column: nothing in the text says so.
        let up = Migration::written(
            "CREATE FUNCTION shout() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.shout := 'changed'; RETURN NEW; END $$;\n\
             CREATE TRIGGER shout BEFORE UPDATE ON notes FOR EACH ROW EXECUTE FUNCTION shout();\n\
             UPDATE notes SET body = upper(body);\n\
             DROP TRIGGER shout ON notes;\n\
             DROP FUNCTION shout();",
        )
        .unwrap();
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &base, up, None, Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Unexplained, Some("shout".into()), Status::Failed)),
            "{after_up:?}"
        );
    });
}

#[test]
fn a_database_not_at_the_base_schema_is_refused_before_anything_runs() {
    with_sandbox(|admin| async move {
        let base = model_of(&admin, SHOP).await;
        let (report, _) = run(
            &admin,
            "CREATE TABLE something_else (id int PRIMARY KEY);",
            &check(
                &base,
                &base,
                Migration::written("SELECT 1;").unwrap(),
                None,
                Seed::Existing {
                    label: "Alice's playground".to_owned(),
                },
            ),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed);
        assert_eq!(
            report.phase(Phase::Before).map(|p| p.status),
            Some(Status::Failed)
        );
        assert!(report.up.statements.is_empty());
        assert!(report.headline.contains("not at v1"), "{}", report.headline);
    });
}

#[test]
fn a_table_dropped_and_created_again_loses_its_rows_where_flagged() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE log (id integer PRIMARY KEY, line text);";
        let base = model_of(&admin, base_sql).await;
        // A plain table becoming partitioned is dropped and created again.
        let target = model_of(
            &admin,
            "CREATE TABLE log (id integer, line text) PARTITION BY HASH (id);",
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let rows = "INSERT INTO log SELECT g, 'line ' || g FROM generate_series(1, 30) g;";
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        let loss = report
            .data_loss
            .iter()
            .find(|l| l.kind == LossKind::RowsDeleted)
            .unwrap_or_else(|| panic!("rows lost: {}", explain(&report)));
        assert!(loss.expected);
        assert_eq!((loss.count_before, loss.count_after), (Some(30), Some(0)));
    });
}

/// The checks of a phase, by kind: status and summary.
fn check_of(
    report: &Report,
    phase: Phase,
    kind: romulus_migration_check::CheckKind,
) -> Option<(Status, String)> {
    report
        .phase(phase)?
        .checks
        .iter()
        .find(|c| c.kind == kind)
        .map(|c| (c.status, c.summary.clone()))
}

#[test]
fn a_partitioned_table_s_renamed_column_is_followed_in_its_partitions() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE events (id integer, at date, kind text, PRIMARY KEY (id, at)) PARTITION BY RANGE (at);\n\
             CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');";
        let rows = "INSERT INTO events SELECT n, date '2026-01-01' + n, 'k' || n FROM generate_series(1, 40) n;";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            &format!("{base_sql}\nALTER TABLE events RENAME COLUMN kind TO category;"),
        )
        .await;
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &target,
                Migration::written("ALTER TABLE events RENAME COLUMN kind TO category;").unwrap(),
                Some(
                    Migration::written("ALTER TABLE events RENAME COLUMN category TO kind;")
                        .unwrap(),
                ),
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Renamed, Some("category".into()), Status::Passed)),
            "{after_up:?}"
        );
    });
}

#[test]
fn a_table_without_a_key_is_compared_as_wholes_and_never_passes_blind() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE log (at timestamptz, kind text, body text);\n\
             CREATE FUNCTION stamp() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.kind := 'stamped'; RETURN NEW; END $$;";
        let rows = "INSERT INTO log SELECT now(), 'k', 'b' || n FROM generate_series(1, 30) n;";
        let base = model_of(&admin, base_sql).await;
        // An UPDATE of one column: that column cannot be followed, the
        // others compare as wholes.
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &base,
                Migration::written("UPDATE log SET body = upper(body);").unwrap(),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::NotVerified, Some("body".into()), Status::Warning)),
            "{after_up:?}"
        );
        assert!(
            after_up.contains(&(NoteKind::NotCompared, None, Status::Passed)),
            "{after_up:?}"
        );
        // A trigger changing another column, which nothing in the text
        // says: the wholes differ.
        let up = Migration::written(
            "CREATE TRIGGER stamp BEFORE UPDATE ON log FOR EACH ROW EXECUTE FUNCTION stamp();\n\
             UPDATE log SET body = upper(body);\n\
             DROP TRIGGER stamp ON log;",
        )
        .unwrap();
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &base, up, None, Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Unexplained, Some("kind".into()), Status::Failed)),
            "{after_up:?}"
        );
    });
}

#[test]
fn written_loss_it_does_not_declare_and_a_do_block_need_acknowledgement() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE prices (id integer PRIMARY KEY, amount numeric(10,2));";
        let rows = "INSERT INTO prices VALUES (1, 1.60), (2, 2.00), (3, 3.49);";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TABLE prices (id integer PRIMARY KEY, amount integer);",
        )
        .await;
        // Written by hand: the conversion rounds, and nothing says so.
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &target,
                Migration::written("ALTER TABLE prices ALTER COLUMN amount TYPE integer;").unwrap(),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::NeedsAcknowledgement,
            "{}",
            explain(&report)
        );
        assert!(
            !report.headline.contains("as flagged"),
            "{}",
            report.headline
        );
        let loss = &report.data_loss[0];
        assert_eq!(loss.status, Status::NeedsAcknowledgement, "{loss:?}");
        assert_eq!(
            loss.cause.as_ref().map(|c| c.flag),
            Some(Flag::WrittenUndeclared)
        );

        // A DO block deleting rows: it may explain it, nothing verifies it.
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &base,
                Migration::written("DO $$ BEGIN DELETE FROM prices WHERE id = 1; END $$;").unwrap(),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::NeedsAcknowledgement,
            "{}",
            explain(&report)
        );
        assert!(
            report
                .data_loss
                .iter()
                .all(|l| l.cause.as_ref().map(|c| c.flag) == Some(Flag::Opaque)),
            "{:?}",
            report.data_loss
        );
    });
}

#[test]
fn rows_are_found_again_by_another_unique_key_when_the_key_column_goes() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE members (id integer PRIMARY KEY, email text NOT NULL UNIQUE, nickname text);";
        let rows = "INSERT INTO members SELECT n, 'm' || n || '@x.test', 'n' || n FROM generate_series(1, 20) n;";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TABLE members (email text NOT NULL UNIQUE, nickname text);",
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(rows.to_owned())),
        )
        .await;
        // The key goes, the email finds the rows: nickname compared, id lost
        // as flagged.
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Dropped, Some("id".into()), Status::Warning)),
            "{after_up:?}"
        );
        assert!(
            !after_up.iter().any(|(k, _, _)| *k == NoteKind::NotVerified),
            "{after_up:?}"
        );
        assert!(
            report
                .data_loss
                .iter()
                .any(|l| l.column.as_deref() == Some("id") && l.expected),
            "{}",
            explain(&report)
        );
    });
}

#[test]
fn a_type_created_under_another_name_and_renamed_is_recomputed() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TYPE mood AS ENUM ('ok', 'sad');\n\
             CREATE TABLE people (id integer PRIMARY KEY, feeling mood);";
        let rows = "INSERT INTO people VALUES (1, 'ok'), (2, 'sad'), (3, NULL);";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TYPE mood AS ENUM ('ok', 'sad', 'blue');\n\
             CREATE TABLE people (id integer PRIMARY KEY, feeling mood);",
        )
        .await;
        let up = Migration::written(
            "CREATE TYPE mood_new AS ENUM ('ok', 'sad', 'blue');\n\
             ALTER TABLE people ALTER COLUMN feeling TYPE mood_new USING feeling::text::mood_new;\n\
             DROP TYPE mood;\n\
             ALTER TYPE mood_new RENAME TO mood;",
        )
        .unwrap();
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, None, Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Converted, Some("feeling".into()), Status::Passed)),
            "{after_up:?}"
        );
    });
}

#[test]
fn a_down_migration_restarting_a_sequence_fails() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE tickets (id serial PRIMARY KEY, title text);";
        let rows = "INSERT INTO tickets (title) SELECT 't' || n FROM generate_series(1, 20) n;";
        let base = model_of(&admin, base_sql).await;
        let up = Migration::written("COMMENT ON TABLE tickets IS 'Support tickets';").unwrap();
        let down = Migration::written(
            "COMMENT ON TABLE tickets IS NULL;\nALTER SEQUENCE tickets_id_seq RESTART WITH 1;",
        )
        .unwrap();
        let target = model_of(
            &admin,
            &format!("{base_sql}\nCOMMENT ON TABLE tickets IS 'Support tickets';"),
        )
        .await;
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(rows.to_owned())),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        let (status, summary) = check_of(
            &report,
            Phase::AfterDown,
            romulus_migration_check::CheckKind::Sequences,
        )
        .expect("a sequences check");
        assert_eq!(status, Status::Failed, "{summary}");
    });
}

#[test]
fn rows_more_after_up_that_nothing_explains_fail() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE notes (id integer PRIMARY KEY, body text);\n\
             CREATE TABLE audit (id serial PRIMARY KEY, what text);\n\
             CREATE FUNCTION log_it() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO audit (what) VALUES (NEW.body); RETURN NEW; END $$;\n\
             CREATE TRIGGER log_it AFTER UPDATE ON notes FOR EACH ROW EXECUTE FUNCTION log_it();";
        let rows = "INSERT INTO notes VALUES (1, 'a'), (2, 'b'); INSERT INTO audit (what) VALUES ('seed');";
        let base = model_of(&admin, base_sql).await;
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &base,
                Migration::written("UPDATE notes SET body = upper(body);").unwrap(),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        let (status, _) = check_of(
            &report,
            Phase::AfterUp,
            romulus_migration_check::CheckKind::RowCounts,
        )
        .unwrap();
        assert_eq!(status, Status::Failed);
    });
}

#[test]
fn empty_tables_are_said_and_a_run_on_none_checks_nothing() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE notes (id integer PRIMARY KEY, body text);\n\
             CREATE TABLE tags (id integer PRIMARY KEY, name text);";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            &format!("{base_sql}\nALTER TABLE notes ADD COLUMN pinned boolean;"),
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(String::new())),
        )
        .await;
        // A nullable column: nothing it does depends on rows, so an empty
        // table leaves nothing unchecked (said, still).
        assert_eq!(report.verdict, Verdict::Passed, "{}", explain(&report));
        assert!(!report.nothing_checked, "{}", explain(&report));
        assert_eq!(report.empty_tables, ["public.notes"]);
        let (status, summary) = check_of(
            &report,
            Phase::AfterUp,
            romulus_migration_check::CheckKind::Coverage,
        )
        .unwrap();
        assert_eq!(status, Status::Warning);
        assert!(summary.contains("depends on rows"), "{summary}");

        // `SET NOT NULL` would fail on rows holding NULLs: on none, nothing
        // was checked.
        let target = model_of(
            &admin,
            "CREATE TABLE notes (id integer PRIMARY KEY, body text NOT NULL);\n\
             CREATE TABLE tags (id integer PRIMARY KEY, name text);",
        )
        .await;
        let up = planned(&base, &target);
        let down = planned(&target, &base);
        let (report, _) = run(
            &admin,
            base_sql,
            &check(&base, &target, up, Some(down), Seed::Sql(String::new())),
        )
        .await;
        assert!(report.nothing_checked, "{}", explain(&report));
        assert!(
            report.headline.contains("nothing was checked"),
            "{}",
            report.headline
        );
    });
}

/// A migration romulus-migration would have planned, without flags.
fn unflagged(sql: &[&str]) -> Migration {
    Migration {
        statements: sql
            .iter()
            .map(|sql| Statement {
                sql: (*sql).to_owned(),
                hazard: None,
                planned: true,
                outside_transaction: false,
            })
            .collect(),
        reordered: Vec::new(),
    }
}

#[test]
fn a_keyless_column_converted_is_compared_cast_back_and_rounding_fails() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE readings (at timestamptz, value numeric(10,2), n integer);";
        let rows = "INSERT INTO readings SELECT now(), n + 0.25, n FROM generate_series(1, 30) n;";
        let base = model_of(&admin, base_sql).await;
        let case = |target_sql: &'static str, up: Migration| {
            let admin = admin.clone();
            let base = base.clone();
            async move {
                let target = model_of(&admin, target_sql).await;
                run(
                    &admin,
                    base_sql,
                    &check(&base, &target, up, None, Seed::Sql(rows.to_owned())),
                )
                .await
                .0
            }
        };
        // Planned without its flag, a conversion that rounds: data lost
        // the migration did not say, found without a key.
        let report = case(
            "CREATE TABLE readings (at timestamptz, value integer, n integer);",
            unflagged(&[
                "ALTER TABLE public.readings ALTER COLUMN value TYPE integer USING value::integer;",
            ]),
        )
        .await;
        assert_eq!(report.verdict, Verdict::Failed, "{}", explain(&report));
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Rounded, Some("value".into()), Status::Failed)),
            "{after_up:?}"
        );
        assert_eq!(report.data_loss.len(), 1, "{}", explain(&report));
        assert_eq!(report.data_loss[0].kind, LossKind::ValuesChanged);

        // One that keeps the text: the same as a whole.
        let report = case(
            "CREATE TABLE readings (at timestamptz, value numeric(10,2), n bigint);",
            unflagged(&["ALTER TABLE public.readings ALTER COLUMN n TYPE bigint;"]),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );

        // One that changes the text but not the values: they convert back.
        let report = case(
            "CREATE TABLE readings (at timestamptz, value numeric(12,3), n integer);",
            unflagged(&["ALTER TABLE public.readings ALTER COLUMN value TYPE numeric(12,3);"]),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(NoteKind::Converted, Some("value".into()), Status::Passed)),
            "{after_up:?}"
        );

        // Values changed by a planned statement without its flag, which no
        // key follows: a person looks, it never passes as a warning.
        let report = case(
            base_sql,
            unflagged(&["UPDATE public.readings SET n = n + 1;"]),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::NeedsAcknowledgement,
            "{}",
            explain(&report)
        );
        let after_up = notes(&report, Phase::AfterUp);
        assert!(
            after_up.contains(&(
                NoteKind::NotVerified,
                Some("n".into()),
                Status::NeedsAcknowledgement
            )),
            "{after_up:?}"
        );
    });
}

#[test]
fn a_keyless_enum_s_renamed_labels_map_back() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TYPE mood AS ENUM ('ok', 'sad');\n\
             CREATE TABLE moods (at timestamptz, m mood, ms mood[]);";
        let rows = "INSERT INTO moods SELECT now(), CASE WHEN n % 2 = 0 THEN 'ok' ELSE 'sad' END::mood, \
             ARRAY['sad', 'ok']::mood[] FROM generate_series(1, 20) n;\n\
             INSERT INTO moods VALUES (now(), NULL, '{}'), (now(), 'ok', NULL);";
        let base = model_of(&admin, base_sql).await;
        let target = model_of(
            &admin,
            "CREATE TYPE mood AS ENUM ('ok', 'blue');\n\
             CREATE TABLE moods (at timestamptz, m mood, ms mood[]);",
        )
        .await;
        let (report, _) = run(
            &admin,
            base_sql,
            &check(
                &base,
                &target,
                unflagged(&["ALTER TYPE public.mood RENAME VALUE 'sad' TO 'blue';"]),
                None,
                Seed::Sql(rows.to_owned()),
            ),
        )
        .await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        let after_up = notes(&report, Phase::AfterUp);
        for column in ["m", "ms"] {
            assert!(
                after_up.contains(&(NoteKind::Changed, Some(column.into()), Status::Passed)),
                "{after_up:?}"
            );
        }
    });
}

/// A session whose reads of `"big_log"` as a whole run past their limit
/// (a table too large): Postgres's cancel, as a playground's session gives.
struct TooLarge<'a> {
    db: &'a mut PgConnection,
}

impl Session for TooLarge<'_> {
    async fn run(
        &mut self,
        sql: &str,
        limit: Duration,
        rows: Rows,
    ) -> Result<Answer, SessionError> {
        if sql.contains("string_agg") && sql.contains("\"big_log\"") {
            return Err(SessionError::from(DbError {
                code: "57014".to_owned(),
                message: "canceling statement due to user request".to_owned(),
                ..DbError::default()
            }));
        }
        Session::run(&mut *self.db, sql, limit, rows).await
    }

    async fn copy_in(
        &mut self,
        statement: &str,
        data: &mut (dyn Iterator<Item = Vec<u8>> + Send),
        limit: Duration,
    ) -> Result<u64, SessionError> {
        Session::copy_in(&mut *self.db, statement, data, limit).await
    }
}

#[test]
fn a_keyless_table_too_large_is_not_verified_and_needs_a_person_when_changed() {
    with_sandbox(|admin| async move {
        let base_sql = "CREATE TABLE big_log (at timestamptz, body text);\n\
             CREATE TABLE notes (id integer PRIMARY KEY, body text);";
        let rows = "INSERT INTO big_log SELECT now(), 'b' || n FROM generate_series(1, 30) n;\n\
             INSERT INTO notes SELECT n, 'n' || n FROM generate_series(1, 30) n;";
        let base = model_of(&admin, base_sql).await;
        let on = |up: &'static str| {
            let admin = admin.clone();
            let base = base.clone();
            async move {
                let (name, mut db) = scratch(&admin).await;
                script(&mut db, base_sql).await;
                let check = check(
                    &base,
                    &base,
                    Migration::written(up).unwrap(),
                    None,
                    Seed::Sql(rows.to_owned()),
                );
                let report = romulus_migration_check::run(
                    &mut TooLarge { db: &mut db },
                    &check,
                    &mut |_| {},
                )
                .await;
                drop_scratch(&admin, &name, db).await;
                report
            }
        };
        let too_large = |report: &Report| {
            report
                .phase(Phase::AfterUp)
                .into_iter()
                .flat_map(|p| &p.checks)
                .flat_map(|c| &c.tables)
                .filter(|t| t.table == "public.big_log")
                .flat_map(|t| &t.notes)
                .find(|n| n.message.contains("too large"))
                .map(|n| n.status)
        };
        // Not changed by the migration: said, a warning.
        let report = on("UPDATE notes SET body = upper(body);").await;
        assert_eq!(
            report.verdict,
            Verdict::PassedUpOnly,
            "{}",
            explain(&report)
        );
        assert_eq!(
            too_large(&report),
            Some(Status::Warning),
            "{}",
            explain(&report)
        );
        // Changed by it: a person acknowledges what could not be verified.
        let report = on("UPDATE big_log SET body = upper(body);").await;
        assert_eq!(
            report.verdict,
            Verdict::NeedsAcknowledgement,
            "{}",
            explain(&report)
        );
        assert_eq!(
            too_large(&report),
            Some(Status::NeedsAcknowledgement),
            "{}",
            explain(&report)
        );
        assert!(
            report.headline.contains("could not be verified"),
            "{}",
            report.headline
        );
    });
}
