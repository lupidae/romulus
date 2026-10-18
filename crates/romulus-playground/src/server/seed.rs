//! Generated rows (docs/playground.md, section 2). The generator is
//! `romulus-fakedata`, behind [`Seeder`] ([`FakeData`]): the seam has its
//! shape, so a test (or another generator) can stand in.
//!
//! | here | romulus-fakedata |
//! |---|---|
//! | [`Seeder::plan`] | `plan(&model, &Options { size, profile, seed, max_rows_per_table })` |
//! | [`Seeder::version`] | `GENERATOR_VERSION: u32` |
//! | [`Plan::steps`] | `Plan::steps(&self) -> Vec<Step>` (fresh, lazy row iterators) |
//! | [`Plan::report`] | `Plan::report(&self) -> Report` |
//! | [`Step::CopyTable`] | `Step::CopyTable(CopyTable)`: `COPY schema.table (columns) FROM STDIN`, rows for `target_schema.target_table` |
//! | [`Step::Sql`] | `Step::Sql(String)`: one statement, as is |
//! | [`Step::Outcome`] | `Step::Outcome(String)`: a query of one text value, given to `Report::record_outcome` |
//! | [`Report`] | `Report` (`Serialize`, `record_outcome(&mut self, &str)`) |
//!
//! [`load`] runs every step in order on one session as the schema's owner
//! (romulus-fakedata's contract: Postgres 16 or later, `client_encoding`
//! UTF8, which [`super::pg::Conn`] sets), rows generated on a blocking
//! thread a chunk ahead of `COPY`, and feeds each outcome to the report
//! before it is read. The report is stored as it is, with its sentences
//! (`lines()`) as `lines`, which the page lists.

use std::{sync::Arc, time::Duration};

use remus_core::Schema;
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::Instant};

use super::pg::{Conn, Event, PgError, quote_ident};
use crate::protocol::{Profile, Size};

/// What the generator is asked for (`romulus_fakedata::Options`).
#[derive(Debug, Clone)]
pub struct Options {
    pub size: Size,
    pub profile: Profile,
    pub seed: u64,
    /// A cap on every table (the size's cap across tables holds too).
    pub max_rows_per_table: Option<u64>,
}

/// Rows for `COPY schema.table (columns) FROM STDIN` (text format).
pub struct CopyTable {
    /// The `COPY` target (the generator's staging table).
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    /// Each row's values in column order, `None` for NULL, unescaped.
    pub rows: Box<dyn Iterator<Item = Vec<Option<String>>> + Send>,
    /// The table these rows are for (progress).
    pub target_schema: String,
    pub target_table: String,
}

/// One thing to run, in order.
pub enum Step {
    CopyTable(CopyTable),
    /// One statement (often a `DO` block), executed as is.
    Sql(String),
    /// A query returning one text value: given to [`Report::record_outcome`]
    /// before the report is read.
    Outcome(String),
}

/// What the generator decided, and what Postgres judged (its outcome).
pub trait Report: Send {
    fn record_outcome(&mut self, json: &str) -> Result<(), SeedError>;
    /// Stored and shown as it is.
    fn to_json(&self) -> Value;
}

/// The load of one schema's rows.
pub trait Plan: Send {
    /// The steps, in order; each call starts the row iterators afresh.
    fn steps(&self) -> Vec<Step>;
    fn report(&self) -> Box<dyn Report>;
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SeedError(pub String);

/// The generator.
pub trait Seeder: Send + Sync {
    fn plan(&self, model: &Schema, options: &Options) -> Result<Box<dyn Plan>, SeedError>;
    /// Stored with the playground (`GENERATOR_VERSION`): the same model,
    /// options and version give the same rows.
    fn version(&self) -> u32;
}

/// romulus-fakedata, as the seam takes it.
pub struct FakeData;

struct FakePlan(romulus_fakedata::Plan);

struct FakeReport(romulus_fakedata::Report);

impl Seeder for FakeData {
    fn plan(&self, model: &Schema, options: &Options) -> Result<Box<dyn Plan>, SeedError> {
        let options = romulus_fakedata::Options {
            size: match options.size {
                Size::Small => romulus_fakedata::Size::Small,
                Size::Medium => romulus_fakedata::Size::Medium,
            },
            profile: match options.profile {
                Profile::Realistic => romulus_fakedata::Profile::Realistic,
                Profile::EdgeCases => romulus_fakedata::Profile::EdgeCases,
            },
            seed: options.seed,
            max_rows_per_table: options.max_rows_per_table,
        };
        let plan = romulus_fakedata::plan(model, &options).map_err(|e| SeedError(e.to_string()))?;
        Ok(Box::new(FakePlan(plan)))
    }

    fn version(&self) -> u32 {
        romulus_fakedata::GENERATOR_VERSION
    }
}

impl Plan for FakePlan {
    fn steps(&self) -> Vec<Step> {
        self.0
            .steps()
            .into_iter()
            .map(|step| match step {
                romulus_fakedata::Step::CopyTable(c) => Step::CopyTable(CopyTable {
                    schema: c.schema,
                    table: c.table,
                    columns: c.columns,
                    rows: Box::new(c.rows),
                    target_schema: c.target_schema,
                    target_table: c.target_table,
                }),
                romulus_fakedata::Step::Sql(sql) => Step::Sql(sql),
                romulus_fakedata::Step::Outcome(query) => Step::Outcome(query),
            })
            .collect()
    }

    fn report(&self) -> Box<dyn Report> {
        Box::new(FakeReport(self.0.report()))
    }
}

impl Report for FakeReport {
    fn record_outcome(&mut self, json: &str) -> Result<(), SeedError> {
        self.0
            .record_outcome(json)
            .map_err(|e| SeedError(e.to_string()))
    }

    /// The report as romulus-fakedata serializes it, and its sentences as
    /// `lines` (what the page lists).
    fn to_json(&self) -> Value {
        let mut report = serde_json::to_value(&self.0).unwrap_or_default();
        if let Some(fields) = report.as_object_mut() {
            fields.insert("lines".to_owned(), json!(self.0.lines()));
        }
        report
    }
}

/// A value in `COPY`'s text format.
fn escape(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Postgres text cannot hold one (the generator never makes it).
            '\0' => {}
            c => out.push(c),
        }
    }
}

fn copy_line(row: &[Option<String>], out: &mut String) {
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        match value {
            Some(value) => escape(value, out),
            None => out.push_str("\\N"),
        }
    }
    out.push('\n');
}

/// What the generating thread hands the loader.
enum Load {
    Copy {
        statement: String,
        target: (String, String),
    },
    Data(String),
    EndCopy,
    Sql(String),
    Outcome(String),
}

/// Rows copied per table (by the table they are for), and the report.
pub struct Loaded {
    pub tables: Vec<(String, String, u64)>,
    pub report: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Seed(#[from] SeedError),
    /// A step ran past its deadline (the build then fails, and its
    /// container goes with whatever still ran).
    #[error("{step} took more than {secs} s")]
    Slow { step: String, secs: u64 },
    #[error("{schema}.{table}: {source}")]
    Table {
        schema: String,
        table: String,
        source: PgError,
    },
    #[error("{0}")]
    Pg(#[from] PgError),
}

/// `COPY "schema"."table" ("a", "b") FROM STDIN`.
fn copy_statement(copy: &CopyTable) -> String {
    let columns: Vec<String> = copy.columns.iter().map(|c| quote_ident(c)).collect();
    format!(
        "COPY {}.{} ({}) FROM STDIN",
        quote_ident(&copy.schema),
        quote_ident(&copy.table),
        columns.join(", ")
    )
}

/// Generates the plan's steps on this (blocking) thread, a chunk ahead.
fn generate(plan: &dyn Plan, tx: &mpsc::Sender<Load>) {
    const CHUNK: usize = 256 * 1024;
    for step in plan.steps() {
        let sent = match step {
            Step::Sql(sql) => tx.blocking_send(Load::Sql(sql)),
            Step::Outcome(query) => tx.blocking_send(Load::Outcome(query)),
            Step::CopyTable(copy) => {
                let start = Load::Copy {
                    statement: copy_statement(&copy),
                    target: (copy.target_schema.clone(), copy.target_table.clone()),
                };
                if tx.blocking_send(start).is_err() {
                    return;
                }
                let mut chunk = String::with_capacity(CHUNK + 4096);
                for row in copy.rows {
                    copy_line(&row, &mut chunk);
                    if chunk.len() >= CHUNK
                        && tx
                            .blocking_send(Load::Data(std::mem::take(&mut chunk)))
                            .is_err()
                    {
                        return;
                    }
                }
                if !chunk.is_empty() && tx.blocking_send(Load::Data(chunk)).is_err() {
                    return;
                }
                tx.blocking_send(Load::EndCopy)
            }
        };
        if sent.is_err() {
            return;
        }
    }
}

/// What a step is, for the person reading why a build failed: a plan's SQL
/// starts with a `-- romulus_fakedata: …` line saying it.
fn step_name(sql: &str) -> String {
    let first = sql.lines().next().unwrap_or_default();
    let name = first
        .strip_prefix("-- romulus_fakedata: ")
        .unwrap_or("a step of the rows' load");
    name.chars().take(120).collect()
}

/// Plans with `seeder` and loads the plan on `conn` (the schema's owner),
/// every step in order, each within `limit` (a `COPY` with its rows);
/// `on_table` hears each table's rows as they are in.
pub async fn load(
    seeder: Arc<dyn Seeder>,
    model: Schema,
    options: Options,
    conn: &mut Conn,
    limit: Duration,
    mut on_table: impl FnMut(&str, &str, u64),
) -> Result<Loaded, LoadError> {
    let plan = seeder.plan(&model, &options)?;
    let mut report = plan.report();
    let (tx, mut rx) = mpsc::channel::<Load>(4);
    let generator = tokio::task::spawn_blocking(move || generate(plan.as_ref(), &tx));

    let mut tables = Vec::new();
    let mut copying: Option<(String, String)> = None;
    // The running step's deadline, and what it is.
    let mut step = (Instant::now() + limit, String::new());
    let slow = |name: &str| LoadError::Slow {
        step: name.to_owned(),
        secs: limit.as_secs(),
    };
    loop {
        // A copy's rows come from the generator within the copy's deadline.
        let next = if copying.is_some() {
            tokio::time::timeout_at(step.0, rx.recv())
                .await
                .map_err(|_| slow(&step.1))?
        } else {
            rx.recv().await
        };
        let Some(load) = next else {
            break;
        };
        match load {
            Load::Sql(sql) => {
                step = (Instant::now() + limit, step_name(&sql));
                tokio::time::timeout_at(step.0, conn.query(&sql))
                    .await
                    .map_err(|_| slow(&step.1))??;
            }
            Load::Outcome(query) => {
                step = (Instant::now() + limit, step_name(&query));
                let value = tokio::time::timeout_at(step.0, conn.query(&query))
                    .await
                    .map_err(|_| slow(&step.1))??
                    .into_iter()
                    .next()
                    .and_then(|row| row.into_iter().next().flatten())
                    .unwrap_or_default();
                report.record_outcome(&value)?;
            }
            Load::Copy { statement, target } => {
                step = (
                    Instant::now() + limit,
                    format!("the rows of {}.{}", target.0, target.1),
                );
                conn.send_query(&statement).await?;
                let started = tokio::time::timeout_at(step.0, conn.next())
                    .await
                    .map_err(|_| slow(&step.1))??;
                match started {
                    Event::CopyIn => {}
                    Event::Error(error) => {
                        drain(conn).await?;
                        return Err(LoadError::Table {
                            schema: target.0,
                            table: target.1,
                            source: PgError::Db(Box::new(error)),
                        });
                    }
                    _ => return Err(PgError::Protocol("COPY did not start".into()).into()),
                }
                copying = Some(target);
            }
            Load::Data(chunk) => tokio::time::timeout_at(step.0, conn.copy_data(chunk.as_bytes()))
                .await
                .map_err(|_| slow(&step.1))??,
            Load::EndCopy => {
                let Some((schema, table)) = copying.take() else {
                    continue;
                };
                let rows = end_copy(conn, step.0).await.map_err(|err| match err {
                    Ended::Slow => slow(&step.1),
                    Ended::Failed(source) => LoadError::Table {
                        schema: schema.clone(),
                        table: table.clone(),
                        source,
                    },
                })?;
                on_table(&schema, &table, rows);
                tables.push((schema, table, rows));
            }
        }
    }
    generator
        .await
        .map_err(|e| SeedError(format!("the generator stopped: {e}")))?;
    Ok(Loaded {
        tables,
        report: report.to_json(),
    })
}

/// Why a `COPY` did not end well.
enum Ended {
    Slow,
    Failed(PgError),
}

/// Ends the `COPY` in progress by `until`: the rows it took.
async fn end_copy(conn: &mut Conn, until: Instant) -> Result<u64, Ended> {
    conn.copy_done().await.map_err(Ended::Failed)?;
    let mut rows = 0;
    let mut failed = None;
    loop {
        let event = tokio::time::timeout_at(until, conn.next())
            .await
            .map_err(|_| Ended::Slow)?
            .map_err(Ended::Failed)?;
        match event {
            Event::Done(tag) => {
                rows = tag
                    .strip_prefix("COPY ")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
            Event::Error(error) => failed = Some(error),
            Event::Ready(_) => break,
            _ => {}
        }
    }
    match failed {
        Some(error) => Err(Ended::Failed(PgError::Db(Box::new(error)))),
        None => Ok(rows),
    }
}

async fn drain(conn: &mut Conn) -> Result<(), PgError> {
    loop {
        if let Event::Ready(_) = conn.next().await? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_lines_escape_and_mark_nulls() {
        let mut out = String::new();
        copy_line(
            &[
                Some("a\tb".to_owned()),
                None,
                Some("back\\slash\nline".to_owned()),
            ],
            &mut out,
        );
        assert_eq!(out, "a\\tb\t\\N\tback\\\\slash\\nline\n");
    }

    #[test]
    fn a_slow_step_is_named_by_what_the_plan_says_it_does() {
        assert_eq!(
            step_name(
                "-- romulus_fakedata: refresh shop.daily_totals\nREFRESH MATERIALIZED VIEW x"
            ),
            "refresh shop.daily_totals"
        );
        assert_eq!(step_name("UPDATE t SET a = 1"), "a step of the rows' load");
    }

    #[test]
    fn a_copy_names_its_staging_table_and_quotes_everything() {
        let copy = CopyTable {
            schema: "romulus_fakedata_1".to_owned(),
            table: "t3_or\"ders".to_owned(),
            columns: vec!["id".to_owned(), "Total".to_owned()],
            rows: Box::new(std::iter::empty()),
            target_schema: "public".to_owned(),
            target_table: "orders".to_owned(),
        };
        assert_eq!(
            copy_statement(&copy),
            r#"COPY "romulus_fakedata_1"."t3_or""ders" ("id", "Total") FROM STDIN"#
        );
    }

    struct Said;
    impl Report for Said {
        fn record_outcome(&mut self, _: &str) -> Result<(), SeedError> {
            Ok(())
        }
        fn to_json(&self) -> Value {
            json!({})
        }
    }

    struct Steps;
    impl Plan for Steps {
        fn steps(&self) -> Vec<Step> {
            vec![
                Step::Sql("-- romulus_fakedata: staging".to_owned()),
                Step::CopyTable(CopyTable {
                    schema: "s".to_owned(),
                    table: "t".to_owned(),
                    columns: vec!["a".to_owned()],
                    rows: Box::new((0..3).map(|i| vec![Some(i.to_string())])),
                    target_schema: "public".to_owned(),
                    target_table: "t".to_owned(),
                }),
                Step::Outcome("SELECT '{}'".to_owned()),
            ]
        }
        fn report(&self) -> Box<dyn Report> {
            Box::new(Said)
        }
    }

    #[test]
    fn the_generator_thread_sends_every_step_in_order() {
        let (tx, mut rx) = mpsc::channel(16);
        generate(&Steps, &tx);
        drop(tx);
        let mut seen = Vec::new();
        while let Ok(load) = rx.try_recv() {
            seen.push(match load {
                Load::Sql(_) => "sql".to_owned(),
                Load::Copy { statement, target } => {
                    format!("copy {statement} for {}.{}", target.0, target.1)
                }
                Load::Data(data) => format!("data {data:?}"),
                Load::EndCopy => "end".to_owned(),
                Load::Outcome(q) => format!("outcome {q}"),
            });
        }
        assert_eq!(
            seen,
            [
                "sql",
                r#"copy COPY "s"."t" ("a") FROM STDIN for public.t"#,
                r#"data "0\n1\n2\n""#,
                "end",
                "outcome SELECT '{}'",
            ]
        );
    }
}
