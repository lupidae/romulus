# romulus-fakedata

Fake rows for a schema: the generator behind playgrounds
([docs/playground.md](../../docs/playground.md), section 2). Given a remus
model, a size, a profile and a seed, it plans how many rows each table gets
and how every column is filled, and hands back an ordered list of steps to
run on the playground's Postgres, plus a report. No I/O, no async, no OS
randomness: it builds for `wasm32-unknown-unknown`.

```rust
pub fn plan(model: &remus_core::Schema, opts: &Options) -> Result<Plan, Error>
pub fn plan_with_rows(model, opts, rows: &BTreeMap<String, u64>) -> Result<Plan, Error> // "schema.table" → rows; the cap holds

pub struct Options { pub size: Size, pub profile: Profile, pub seed: u64, pub max_rows_per_table: Option<u64> }
pub enum Size { Small, Medium }
pub enum Profile { Realistic, EdgeCases }

impl Plan {
    pub fn steps(&self) -> Vec<Step>   // fresh, lazy row iterators on each call
    pub fn report(&self) -> Report     // serde::Serialize
    pub fn rows(&self) -> u64
    pub fn sample(&self, schema: &str, table: &str, limit: u64) -> Option<Sample>
}
pub enum Step {
    CopyTable(CopyTable),   // COPY copy.statement() FROM STDIN; rows encoded with copy_text()
    Sql(String),            // execute as is
    Outcome(String),        // one text value: report.record_outcome(&it)
}
pub struct CopyTable {
    pub schema: String, pub table: String,              // the COPY target, a staging table
    pub columns: Vec<String>,
    pub rows: Rows,                                     // Iterator<Item = Vec<Option<String>>> + ExactSizeIterator + Send
    pub target_schema: String, pub target_table: String // the table they are for
}
pub fn copy_text(row: &[Option<String>], out: &mut Vec<u8>)  // one line of COPY text format
pub const GENERATOR_VERSION: u32;                            // store it with the playground
```

## Running a plan

Run every step in order, on one session, as the schema's owner, on
PostgreSQL 16 or later, with `client_encoding` `UTF8`. Autocommit or one transaction both work. No step
fails because of the data: rows Postgres refuses are dropped and counted.

1. A staging schema (`romulus_fakedata_<suffix from the seed>`, never one
   the model has) with an outcome table.
2. Per table, parents first: a staging table of `text` columns, the `COPY`
   (which never fails on a value), then a `DO` block moving the rows in:
   `INSERT … SELECT` casting each column, keeping rows whose input is valid
   (`pg_input_is_valid`), whose CHECKs are not false and whose parents
   exist, `ON CONFLICT DO NOTHING`. If the bulk insert still raises, the
   block falls back to one row at a time, each in its own subtransaction.
3. After a cycle's tables: `UPDATE`s filling the foreign keys loaded NULL;
   sequences moved past explicit keys; matviews refreshed.
4. The `Outcome` query (what was inserted, dropped and why), then the
   staging schema dropped, then `ANALYZE` of the loaded tables.

`FORCE ROW LEVEL SECURITY` is switched off for the load and back on at the
end, so the owner's checks see every parent row.

## How rows are made

- **Determinism.** Every cell draws from a ChaCha8 RNG keyed by
  `sha256(seed ‖ schema ‖ table ‖ column)`, on its row's stream: a value is
  a pure function of (seed, table, column, row). Same model, options and
  `GENERATOR_VERSION`, same rows; adding a column leaves the others alone.
  "Now" is fixed per version (2026-10-01).
- **Keys** are unique by construction: one column per unique set carries the
  row number (`rhoda.wilkinson17@example.com`, `AB0017` under a pattern), or
  the set is drawn as a combination of its foreign keys and small domains
  from a permutation. Short types cap the row count (a unique `boolean`
  holds 2 rows).
- **Foreign keys** copy a parent row's key, computed for that row directly
  (no read-back); fan-out is skewed (Zipf). Identity and serial keys that
  others point at are written `1, 2, 3…`. Self-references form trees; a
  cycle breaks at a nullable key; a cycle `NOT NULL` both ways loads in one
  block with its keys held (deferred, or dropped and added back), rows left
  without a parent removed in every direction until none is.
- **CHECKs** shaped as bounds, `IN` lists, lengths, regexes, `a < b` and
  `num_nonnulls` hold by construction; other readable ones are evaluated per
  row (redrawn up to 8 times); Postgres judges all of them in the load.
- **Values** follow column-name heuristics checked against the type
  (`Heuristic` in the report), else the type: numbers within precision, text
  within length, timestamps in the last two years and ordered within a row,
  PostGIS EWKT from the typmod, unit vectors of the right dimension,
  citext, hstore, ltree, ranges, arrays, enums, domains, composites. Emails
  and URLs use reserved domains only.
- **Partitions**: range, list and default bounds are parsed; each row picks
  a leaf and its key values fall inside it. Hash partitions route
  themselves when every remainder exists; bounds not understood get no rows.
- **Edge cases** profile: 40 % NULLs, strings at their maximum length,
  empty and non-ASCII strings, extreme and NaN numbers, infinite timestamps.

## Tests

```bash
cargo test -p romulus-fakedata                   # fast, no Docker: the oracle skips, loudly
just sandbox                                     # then the oracle runs too:
cargo test -p romulus-fakedata --test oracle     # corpus × both profiles, hostile scenarios
```

The oracle builds every schema of the sandbox corpus (and remus's showcase)
on a scratch database, hands it to an owner role that is not a superuser,
runs the plan as that owner and checks that no step failed, that each table
holds what the report says, and that every missing row is one the report
explains. Realistic rows must all load; in CI (`CI` set) a missing sandbox
fails the test, as the migration oracle does.

## Performance

`examples/medium.rs` plans Medium for a 30-table schema (1 M rows, 136 MB of
COPY text) and streams every row, without a database:

```bash
/usr/bin/time -l cargo run --release -p romulus-fakedata --example medium   # macOS; -v on Linux
```

About 5 s of CPU and a 7 MB peak RSS on an M-series Mac: memory does not
grow with the row count (nothing is materialised). Loaded into the sandbox
(`tests/oracle/medium.rs`, ignored by default, run in release), the same
million rows take about 50 s, of which Postgres's side (COPY, the judged
inserts, ANALYZE) is about two thirds; nothing is dropped. A runner can
overlap generation with sending: `Rows` is `Send`.

## Not yet

- One locale (English).
- A unique column under a regex without digits is sampled, not numbered: a
  rare duplicate is dropped (and reported).
- Exclusion constraints other than `=` and `&&` are left to Postgres.
