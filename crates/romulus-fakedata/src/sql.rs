//! The load's SQL: what Postgres runs around the `COPY`s (docs/playground.md,
//! "Pipeline: generate in Rust, let Postgres judge").
//!
//! Rows are copied into staging tables of text columns (`COPY` never fails
//! on a value), then moved into the real table by an `INSERT … SELECT` that
//! casts each column, keeps the rows whose input is valid
//! (`pg_input_is_valid`), whose CHECKs are not false and whose parents
//! exist, and skips duplicates (`ON CONFLICT DO NOTHING`). Each move is a
//! `DO` block: if the bulk insert still fails (a CHECK that raises, a row
//! fitting no partition), it falls back to one row at a time, each in its
//! own subtransaction, so a bad row is dropped and the load goes on. What
//! was dropped, and why, goes to an outcome table read back at the end.

use std::fmt::Write as _;

use crate::model::{Fill, Fk, Model, Table, Unit, qualified_sql};

pub const ORD: &str = "romulus_ord";

pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A `DO` block, dollar-quoted with a tag its body does not hold.
pub fn do_block(label: &str, body: &str) -> String {
    let mut tag = "romulus".to_owned();
    let mut n = 0;
    while body.contains(&format!("${tag}$")) {
        n += 1;
        tag = format!("romulus{n}");
    }
    format!(
        "-- romulus_fakedata: {label}\nDO ${tag}$\n#variable_conflict use_column\n{body}\n${tag}$"
    )
}

fn outcome(m: &Model) -> String {
    format!("{}.\"outcome\"", quote(&m.staging))
}

pub fn staging_table(m: &Model, t: &Table) -> String {
    format!("{}.{}", quote(&m.staging), quote(&t.staging))
}

fn table_sql(t: &Table) -> String {
    qualified_sql(&t.schema, &t.name)
}

/// `INSERT INTO outcome …` with SQL expressions for rows and message.
fn record(
    m: &Model,
    t: &Table,
    kind: &str,
    name: Option<&str>,
    rows: &str,
    message: &str,
) -> String {
    format!(
        "INSERT INTO {} (\"schema\", \"table\", kind, name, rows, message) VALUES ({}, {}, {}, {}, {rows}, {message});",
        outcome(m),
        literal(&t.schema),
        literal(&t.name),
        literal(kind),
        name.map_or_else(|| "NULL".to_owned(), literal),
    )
}

/// The staging schema, then the outcome table: one statement each.
pub fn setup(m: &Model) -> [String; 2] {
    [
        format!(
            "-- romulus_fakedata: staging schema\nCREATE SCHEMA {};",
            quote(&m.staging)
        ),
        format!(
            "-- romulus_fakedata: outcome table\n\
             CREATE UNLOGGED TABLE {} (\n    seq bigint GENERATED ALWAYS AS IDENTITY,\n    \"schema\" text,\n    \"table\" text,\n    kind text NOT NULL,\n    name text,\n    rows bigint NOT NULL,\n    message text\n);",
            outcome(m),
        ),
    ]
}

pub fn create_staging(m: &Model, t: &Table) -> String {
    let mut cols = vec![format!("{ORD} bigint")];
    for c in t.columns.iter().filter(|c| c.written()) {
        cols.push(format!("{} text", quote(&c.name)));
    }
    format!(
        "-- romulus_fakedata: staging for {}\nCREATE UNLOGGED TABLE {} (\n    {}\n);",
        t.qualified(),
        staging_table(m, t),
        cols.join(",\n    ")
    )
}

pub fn copy_columns(t: &Table) -> Vec<String> {
    std::iter::once(ORD.to_owned())
        .chain(
            t.columns
                .iter()
                .filter(|c| c.written())
                .map(|c| c.name.clone()),
        )
        .collect()
}

pub fn drop_staging(m: &Model, tables: &[&Table]) -> String {
    let names: Vec<String> = tables.iter().map(|t| staging_table(m, t)).collect();
    format!(
        "-- romulus_fakedata: staging dropped\nDROP TABLE IF EXISTS {};",
        names.join(", ")
    )
}

/// Whether the column is in the typed row (`r`).
fn available(t: &Table, name: &str, generated_inline: &[usize]) -> bool {
    t.column(name)
        .is_some_and(|i| t.columns[i].written() || generated_inline.contains(&i))
}

/// Generated columns the CHECKs read, computed in the typed row so the
/// CHECKs can be judged before the insert.
fn generated_inline(t: &Table) -> Vec<usize> {
    let mut out = Vec::new();
    for check in &t.checks {
        for name in &check.columns {
            let Some(i) = t.column(name) else { continue };
            let col = &t.columns[i];
            if let (Fill::Database(_), Some(expr)) = (&col.fill, &col.expression)
                && col
                    .expression_columns
                    .iter()
                    .all(|n| t.column(n).is_some_and(|j| t.columns[j].written()))
                && !out.contains(&i)
                && !expr.is_empty()
            {
                out.push(i);
            }
        }
    }
    out
}

/// The CHECKs judged in the WHERE: those whose columns are all in `r`.
fn judged_checks<'t>(t: &'t Table, inline: &[usize]) -> Vec<&'t crate::model::Check> {
    t.checks
        .iter()
        .filter(|c| !c.columns.is_empty() && c.columns.iter().all(|n| available(t, n, inline)))
        .collect()
}

fn deferred_column(t: &Table, c: usize) -> bool {
    t.fks.iter().any(|f| f.deferred && f.columns.contains(&c))
}

/// The typed row: each written column cast from the staging text (NULL
/// for a deferred foreign key), `source` the staging row's alias.
fn typed_columns(t: &Table, source: &str) -> Vec<String> {
    t.columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.written())
        .map(|(i, c)| {
            if deferred_column(t, i) {
                format!("NULL::{} AS {}", c.type_text, quote(&c.name))
            } else {
                format!(
                    "{source}.{}::{} AS {}",
                    quote(&c.name),
                    c.type_text,
                    quote(&c.name)
                )
            }
        })
        .collect()
}

/// Written `NOT NULL` columns (a deferred foreign key is loaded NULL).
fn required(t: &Table) -> impl Iterator<Item = &crate::model::Col> {
    t.columns
        .iter()
        .enumerate()
        .filter(|(i, c)| c.written() && c.not_null && !deferred_column(t, *i))
        .map(|(_, c)| c)
}

/// The staged row's checks before any cast: a value in every `NOT NULL`
/// column (a NULL cast to a `NOT NULL` domain raises), valid input where
/// Postgres must judge it.
fn validity(t: &Table) -> Vec<String> {
    let present = required(t).map(|c| format!("s.{} IS NOT NULL", quote(&c.name)));
    let valid = t
        .columns
        .iter()
        .enumerate()
        .filter(|(i, c)| c.written() && c.validate && !deferred_column(t, *i))
        .map(|(_, c)| {
            let col = format!("s.{}", quote(&c.name));
            format!(
                "({col} IS NULL OR pg_input_is_valid({col}, {}))",
                literal(&c.type_text)
            )
        });
    present.chain(valid).collect()
}

/// The condition a foreign key puts on the typed row `r`.
fn fk_condition(m: &Model, t: &Table, fk: &Fk, row: &str) -> Option<String> {
    if fk.deferred || !fk.exists_check {
        return None;
    }
    let cols: Vec<String> = fk
        .columns
        .iter()
        .map(|&c| format!("{row}.{}", quote(&t.columns[c].name)))
        .collect();
    let Some(p) = fk
        .parent
        .filter(|&p| m.tables[p].skip.is_none() && m.tables[p].rows > 0)
    else {
        // No parent rows: only NULL references pass.
        return Some(format!(
            "({})",
            cols.iter()
                .map(|c| format!("{c} IS NULL"))
                .collect::<Vec<_>>()
                .join(" OR ")
        ));
    };
    let parent = &m.tables[p];
    let matches: Vec<String> = fk
        .parent_columns
        .iter()
        .zip(&cols)
        .map(|(&pc, c)| format!("romulus_p.{} = {c}", quote(&parent.columns[pc].name)))
        .collect();
    let exists = format!(
        "EXISTS (SELECT 1 FROM {} AS romulus_p WHERE {})",
        table_sql(parent),
        matches.join(" AND ")
    );
    let nulls = if fk.match_full {
        format!(
            "({})",
            cols.iter()
                .map(|c| format!("{c} IS NULL"))
                .collect::<Vec<_>>()
                .join(" AND ")
        )
    } else {
        cols.iter()
            .map(|c| format!("{c} IS NULL"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    Some(format!("({nulls} OR {exists})"))
}

struct Insert {
    /// The bulk `INSERT … SELECT` from staging.
    bulk: String,
    /// When every parent row went in (the outcome says so), the same
    /// insert without probing the parents: (condition, insert).
    fast: Option<(String, String)>,
    /// One staged row (`romulus_row`), for the fallback.
    single: String,
    /// The conditions, for the counts.
    conditions: Vec<(String, String, String)>,
    /// The typed row from staging, for the counts.
    typed: String,
}

#[allow(clippy::too_many_lines)]
fn insert(m: &Model, t: &Table, held: &[usize]) -> Insert {
    let written: Vec<&str> = t
        .columns
        .iter()
        .filter(|c| c.written())
        .map(|c| c.name.as_str())
        .collect();
    let target_cols: Vec<String> = written.iter().map(|n| quote(n)).collect();
    let overriding = if t.columns.iter().any(|c| c.identity_always && c.written()) {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    let inline = generated_inline(t);
    let mut conditions: Vec<(String, String, String)> = Vec::new();
    for check in judged_checks(t, &inline) {
        conditions.push((
            "check".to_owned(),
            check.name.clone(),
            format!("({}) IS NOT FALSE", check.sql),
        ));
    }
    for (fi, fk) in t.fks.iter().enumerate() {
        if held.contains(&fi) {
            continue;
        }
        if let Some(c) = fk_condition(m, t, fk, "r") {
            conditions.push(("foreign_key".to_owned(), fk.name.clone(), c));
        }
    }
    let wrap = |inner: String| -> String {
        if inline.is_empty() {
            return inner;
        }
        let extra: Vec<String> = inline
            .iter()
            .map(|&i| {
                let c = &t.columns[i];
                format!(
                    "({}) AS {}",
                    c.expression.clone().unwrap_or_default(),
                    quote(&c.name)
                )
            })
            .collect();
        format!("SELECT r1.*, {} FROM ({inner}) AS r1", extra.join(", "))
    };
    let valid = validity(t);
    let typed = format!(
        "SELECT {} FROM {} AS s{} OFFSET 0",
        std::iter::once(format!("s.{ORD}"))
            .chain(typed_columns(t, "s"))
            .collect::<Vec<_>>()
            .join(", "),
        staging_table(m, t),
        if valid.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", valid.join(" AND "))
        }
    );
    let typed = wrap(typed);
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(
            "\nWHERE {}",
            conditions
                .iter()
                .map(|(_, _, c)| c.as_str())
                .collect::<Vec<_>>()
                .join("\n  AND ")
        )
    };
    let conflict = if t.deferrable_unique {
        ""
    } else {
        "\nON CONFLICT DO NOTHING"
    };
    // Parents checked with EXISTS: when each holds all its planned rows,
    // the probes can only pass.
    let probed: Vec<usize> = {
        let mut p: Vec<usize> = t
            .fks
            .iter()
            .enumerate()
            .filter(|(fi, fk)| !held.contains(fi) && !fk.deferred && fk.exists_check)
            .filter_map(|(_, fk)| fk.parent)
            .filter(|&p| m.tables[p].skip.is_none() && m.tables[p].rows > 0)
            .collect();
        p.sort_unstable();
        p.dedup();
        p
    };
    let select_cols: Vec<String> = written.iter().map(|n| format!("r.{}", quote(n))).collect();
    // A table with nothing to write (identity and generated columns only)
    // takes rows of no column: every value its default.
    let target = if target_cols.is_empty() {
        String::new()
    } else {
        format!(" ({})", target_cols.join(", "))
    };
    let bulk = format!(
        "INSERT INTO {}{target}{overriding}\nSELECT {}\nFROM ({typed}) AS r{where_clause}\nORDER BY r.{ORD}{conflict}",
        table_sql(t),
        select_cols.join(", "),
    );
    let fast = (!probed.is_empty()).then(|| {
        let kept: Vec<&str> = conditions
            .iter()
            .filter(|(kind, _, c)| kind != "foreign_key" || !c.contains("EXISTS"))
            .map(|(_, _, c)| c.as_str())
            .collect();
        let where_fast = if kept.is_empty() {
            String::new()
        } else {
            format!("\nWHERE {}", kept.join("\n  AND "))
        };
        let complete = probed
            .iter()
            .map(|&p| {
                let parent = &m.tables[p];
                format!(
                    "(SELECT coalesce(sum(o.rows), 0) FROM {} AS o WHERE o.\"schema\" = {} AND o.\"table\" = {} AND o.kind = 'inserted') = {}",
                    outcome(m),
                    literal(&parent.schema),
                    literal(&parent.name),
                    parent.rows
                )
            })
            .collect::<Vec<_>>()
            .join("\n        AND ");
        let insert = format!(
            "INSERT INTO {}{target}{overriding}\nSELECT {}\nFROM ({typed}) AS r{where_fast}\nORDER BY r.{ORD}{conflict}",
            table_sql(t),
            select_cols.join(", "),
        );
        (complete, insert)
    });
    let single_typed = wrap(format!(
        "SELECT {}",
        typed_columns(t, "romulus_row").join(", ")
    ));
    let single = format!(
        "INSERT INTO {}{target}{overriding}\nSELECT {}\nFROM ({single_typed}) AS r{where_clause}{conflict}",
        table_sql(t),
        select_cols.join(", "),
    );
    Insert {
        bulk,
        fast,
        single,
        conditions,
        typed,
    }
}

/// The counts recorded when rows were dropped: invalid input per column,
/// then each condition over the valid rows.
fn counts(m: &Model, t: &Table, ins: &Insert) -> String {
    let mut out = String::new();
    let mut invalid: Vec<(String, String, String)> = required(t)
        .map(|c| {
            let col = format!("s.{}", quote(&c.name));
            (
                "null".to_owned(),
                c.name.clone(),
                format!("count(*) FILTER (WHERE {col} IS NULL)"),
            )
        })
        .collect();
    invalid.extend(
        t.columns
            .iter()
            .enumerate()
            .filter(|(i, c)| c.written() && c.validate && !deferred_column(t, *i))
            .map(|(_, c)| {
                let col = format!("s.{}", quote(&c.name));
                (
                    "invalid".to_owned(),
                    c.name.clone(),
                    format!("count(*) FILTER (WHERE {col} IS NOT NULL AND NOT pg_input_is_valid({col}, {}))", literal(&c.type_text)),
                )
            }),
    );
    let lateral = |items: &[(String, String, String)]| -> String {
        items
            .iter()
            .enumerate()
            .map(|(i, (kind, name, _))| format!("({}, {}, x.c{i})", literal(kind), literal(name)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !invalid.is_empty() {
        let items = invalid;
        let _ = writeln!(
            out,
            "INSERT INTO {} (\"schema\", \"table\", kind, name, rows) SELECT {}, {}, v.kind, v.name, v.n FROM (SELECT {} FROM {} AS s) AS x, LATERAL (VALUES {}) AS v(kind, name, n) WHERE v.n > 0;",
            outcome(m),
            literal(&t.schema),
            literal(&t.name),
            items
                .iter()
                .enumerate()
                .map(|(i, (_, _, e))| format!("{e} AS c{i}"))
                .collect::<Vec<_>>()
                .join(", "),
            staging_table(m, t),
            lateral(&items),
        );
    }
    let mut items: Vec<(String, String, String)> = ins
        .conditions
        .iter()
        .map(|(kind, name, cond)| {
            (
                kind.clone(),
                name.clone(),
                format!("count(*) FILTER (WHERE NOT ({cond}))"),
            )
        })
        .collect();
    items.push(("valid".to_owned(), String::new(), "count(*)".to_owned()));
    let all = if ins.conditions.is_empty() {
        "true".to_owned()
    } else {
        ins.conditions
            .iter()
            .map(|(_, _, c)| format!("({c})"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    items.push((
        "judged".to_owned(),
        String::new(),
        format!("count(*) FILTER (WHERE NOT ({all}))"),
    ));
    let _ = writeln!(
        out,
        "INSERT INTO {} (\"schema\", \"table\", kind, name, rows) SELECT {}, {}, v.kind, nullif(v.name, ''), v.n FROM (SELECT {} FROM ({}) AS r) AS x, LATERAL (VALUES {}) AS v(kind, name, n) WHERE v.n > 0 OR v.kind = 'valid';",
        outcome(m),
        literal(&t.schema),
        literal(&t.name),
        items
            .iter()
            .enumerate()
            .map(|(i, (_, _, e))| format!("{e} AS c{i}"))
            .collect::<Vec<_>>()
            .join(", "),
        ins.typed,
        lateral(&items),
    );
    out
}

/// The move of one table's staged rows into it.
pub fn load_table(m: &Model, t: &Table) -> String {
    let ins = insert(m, t, &[]);
    let staged = t.rows;
    let bulk = match &ins.fast {
        Some((complete, fast)) => format!(
            "IF {complete} THEN\n            {};\n        ELSE\n            {};\n        END IF",
            indent(fast, 12),
            indent(&ins.bulk, 12)
        ),
        None => indent(&ins.bulk, 8),
    };
    let body = format!(
        "DECLARE\n    romulus_inserted bigint := 0;\n    romulus_k bigint := 0;\n    romulus_failed bigint := 0;\n    romulus_error text;\n    romulus_row record;\n\
         BEGIN\n    SET CONSTRAINTS ALL IMMEDIATE;\n    BEGIN\n        {bulk};\n        GET DIAGNOSTICS romulus_inserted = ROW_COUNT;\n    \
         EXCEPTION WHEN OTHERS THEN\n        romulus_error := SQLERRM;\n        romulus_inserted := 0;\n        \
         FOR romulus_row IN SELECT * FROM {staging} ORDER BY {ORD} LOOP\n            BEGIN\n                romulus_k := 0;\n                \
         IF {present} THEN\n                    {single};\n                    GET DIAGNOSTICS romulus_k = ROW_COUNT;\n                END IF;\n                \
         romulus_inserted := romulus_inserted + romulus_k;\n            \
         EXCEPTION WHEN OTHERS THEN\n                romulus_failed := romulus_failed + 1;\n                \
         IF romulus_failed = 1 THEN romulus_error := SQLERRM; END IF;\n            END;\n        END LOOP;\n        \
         {fallback}\n    END;\n    {inserted}\n    \
         IF romulus_failed > 0 THEN {errors} END IF;\n    \
         IF romulus_inserted + romulus_failed < {staged} THEN\n        BEGIN\n            {counts}\n        EXCEPTION WHEN OTHERS THEN NULL;\n        END;\n    END IF;\nEND",
        single = indent(&ins.single, 20),
        present = {
            let present: Vec<String> = required(t)
                .map(|c| format!("romulus_row.{} IS NOT NULL", quote(&c.name)))
                .collect();
            if present.is_empty() {
                "true".to_owned()
            } else {
                present.join(" AND ")
            }
        },
        staging = staging_table(m, t),
        fallback = record(m, t, "fallback", None, "0", "romulus_error"),
        inserted = record(m, t, "inserted", None, "romulus_inserted", "NULL"),
        errors = record(m, t, "error", None, "romulus_failed", "romulus_error"),
        counts = indent(counts(m, t, &ins).trim_end(), 12),
    );
    do_block(
        &format!("rows of {} ({staged} staged)", t.qualified()),
        &body,
    )
}

fn indent(s: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    s.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.to_owned()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A cycle loaded with its `NOT NULL` foreign keys: every foreign key
/// between its tables (but those loaded NULL and filled after) is held,
/// deferred or dropped, while all of them load; then rows left without a
/// parent are deleted, in every direction, until a pass deletes none, and
/// the keys are put back. Each table records the rows it ends with. Any
/// failure leaves the cycle's tables empty, reported.
#[allow(clippy::too_many_lines)]
pub fn load_held(m: &Model, unit: &Unit) -> String {
    let members: Vec<usize> = unit
        .tables
        .iter()
        .copied()
        .filter(|&t| m.tables[t].skip.is_none() && m.tables[t].rows > 0)
        .collect();
    let inner: Vec<(usize, usize)> = unit
        .tables
        .iter()
        .flat_map(|&ti| {
            m.tables[ti]
                .fks
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.deferred && f.parent.is_some_and(|p| unit.tables.contains(&p)))
                .map(move |(fi, _)| (ti, fi))
        })
        .collect();
    let deferrable = |ti: usize, fi: usize| m.tables[ti].fks[fi].definition.contains("DEFERRABLE");
    let mut body =
        String::from("DECLARE\n    romulus_k bigint := 0;\n    romulus_pass bigint := 0;\n");
    for &ti in &members {
        let _ = writeln!(
            body,
            "    romulus_ins_{ti} bigint := 0;\n    romulus_rm_{ti} bigint := 0;"
        );
    }
    body.push_str("BEGIN\n");
    for &(ti, fi) in &inner {
        let t = &m.tables[ti];
        let fk = &t.fks[fi];
        if deferrable(ti, fi) {
            let _ = writeln!(
                body,
                "    SET CONSTRAINTS {}.{} DEFERRED;",
                quote(&t.schema),
                quote(&fk.name)
            );
        } else {
            let _ = writeln!(
                body,
                "    ALTER TABLE {} DROP CONSTRAINT {};",
                table_sql(t),
                quote(&fk.name)
            );
        }
    }
    for &ti in &members {
        let t = &m.tables[ti];
        let held: Vec<usize> = inner
            .iter()
            .filter(|(x, _)| *x == ti)
            .map(|(_, f)| *f)
            .collect();
        let ins = insert(m, t, &held);
        let _ = writeln!(
            body,
            "    {};\n    GET DIAGNOSTICS romulus_ins_{ti} = ROW_COUNT;\n    IF romulus_ins_{ti} < {} THEN\n        BEGIN\n            {}\n        EXCEPTION WHEN OTHERS THEN NULL;\n        END;\n    END IF;",
            indent(&ins.bulk, 4),
            t.rows,
            indent(counts(m, t, &ins).trim_end(), 12),
        );
    }
    // Rows whose parent did not make it, in every direction of the cycle.
    body.push_str("    LOOP\n        romulus_pass := 0;\n");
    for &(ti, fi) in &inner {
        let t = &m.tables[ti];
        let fk = &t.fks[fi];
        let Some(parent) = fk.parent.map(|p| &m.tables[p]) else {
            continue;
        };
        if !members.contains(&ti) {
            continue;
        }
        let cols: Vec<String> = fk
            .columns
            .iter()
            .map(|&c| format!("romulus_c.{}", quote(&t.columns[c].name)))
            .collect();
        let matches: Vec<String> = fk
            .parent_columns
            .iter()
            .zip(&cols)
            .map(|(&pc, c)| format!("romulus_p.{} = {c}", quote(&parent.columns[pc].name)))
            .collect();
        let _ = writeln!(
            body,
            "        DELETE FROM {} AS romulus_c WHERE {} AND NOT EXISTS (SELECT 1 FROM {} AS romulus_p WHERE {});\n        GET DIAGNOSTICS romulus_k = ROW_COUNT;\n        IF romulus_k > 0 THEN\n            romulus_rm_{ti} := romulus_rm_{ti} + romulus_k;\n            romulus_pass := romulus_pass + romulus_k;\n            {}\n        END IF;",
            table_sql(t),
            cols.iter()
                .map(|c| format!("{c} IS NOT NULL"))
                .collect::<Vec<_>>()
                .join(" AND "),
            table_sql(parent),
            matches.join(" AND "),
            record(m, t, "removed", Some(&fk.name), "romulus_k", "NULL"),
        );
    }
    body.push_str("        EXIT WHEN romulus_pass = 0;\n    END LOOP;\n");
    for &(ti, fi) in &inner {
        let t = &m.tables[ti];
        let fk = &t.fks[fi];
        if deferrable(ti, fi) {
            let _ = writeln!(
                body,
                "    SET CONSTRAINTS {}.{} IMMEDIATE;",
                quote(&t.schema),
                quote(&fk.name)
            );
        } else {
            let _ = writeln!(
                body,
                "    ALTER TABLE {} ADD CONSTRAINT {} {};",
                table_sql(t),
                quote(&fk.name),
                fk.definition
            );
            if let Some(comment) = &fk.comment {
                let _ = writeln!(
                    body,
                    "    COMMENT ON CONSTRAINT {} ON {} IS {};",
                    quote(&fk.name),
                    table_sql(t),
                    literal(comment)
                );
            }
        }
    }
    // What each table ends with; rows an ON DELETE CASCADE took as well.
    for &ti in &members {
        let t = &m.tables[ti];
        let _ = writeln!(
            body,
            "    SELECT count(*) INTO romulus_k FROM {};\n    {}\n    IF romulus_ins_{ti} - romulus_rm_{ti} > romulus_k THEN {} END IF;",
            table_sql(t),
            record(m, t, "inserted", None, "romulus_k", "NULL"),
            record(
                m,
                t,
                "removed",
                None,
                &format!("romulus_ins_{ti} - romulus_rm_{ti} - romulus_k"),
                "NULL"
            ),
        );
    }
    body.push_str("EXCEPTION WHEN OTHERS THEN\n");
    for &ti in &members {
        let t = &m.tables[ti];
        let _ = writeln!(
            body,
            "    {}",
            record(m, t, "cycle_failed", None, "0", "SQLERRM")
        );
    }
    body.push_str("END");
    let names: Vec<String> = unit
        .tables
        .iter()
        .map(|&t| m.tables[t].qualified())
        .collect();
    do_block(&format!("rows of the cycle {}", names.join(", ")), &body)
}

/// Fills a deferred foreign key (a cycle, a self-reference) from staging.
#[allow(clippy::too_many_lines)]
pub fn fill_deferred(m: &Model, t: &Table, fi: usize) -> Option<String> {
    let fk = &t.fks[fi];
    let key = t.update_key.as_ref()?;
    let p = fk.parent?;
    let parent = &m.tables[p];
    if parent.skip.is_some() || parent.rows == 0 {
        return None;
    }
    let col = |c: usize, src: &str| {
        format!(
            "{src}.{}::{}",
            quote(&t.columns[c].name),
            t.columns[c].type_text
        )
    };
    let set = if fk.columns.len() == 1 {
        format!(
            "{} = {}",
            quote(&t.columns[fk.columns[0]].name),
            col(fk.columns[0], "romulus_s")
        )
    } else {
        format!(
            "({}) = ROW({})",
            fk.columns
                .iter()
                .map(|&c| quote(&t.columns[c].name))
                .collect::<Vec<_>>()
                .join(", "),
            fk.columns
                .iter()
                .map(|&c| col(c, "romulus_s"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let by_key = |src: &str| {
        key.iter()
            .map(|&k| format!("romulus_t.{} = {}", quote(&t.columns[k].name), col(k, src)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let present = |src: &str| {
        fk.columns
            .iter()
            .map(|&c| format!("{src}.{} IS NOT NULL", quote(&t.columns[c].name)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let exists = |src: &str| {
        format!(
            "EXISTS (SELECT 1 FROM {} AS romulus_p WHERE {})",
            table_sql(parent),
            fk.parent_columns
                .iter()
                .zip(&fk.columns)
                .map(|(&pc, &c)| format!(
                    "romulus_p.{} = {}",
                    quote(&parent.columns[pc].name),
                    col(c, src)
                ))
                .collect::<Vec<_>>()
                .join(" AND ")
        )
    };
    // CHECKs reading the foreign key, judged on the row as updated.
    let guards = |src: &str| -> String {
        let mut out = String::new();
        for check in &t.checks {
            let reads_fk = check
                .columns
                .iter()
                .any(|n| t.column(n).is_some_and(|i| fk.columns.contains(&i)));
            if !reads_fk
                || !check
                    .columns
                    .iter()
                    .all(|n| t.column(n).is_some_and(|i| t.columns[i].written()))
            {
                continue;
            }
            let row: Vec<String> = t
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.written())
                .map(|(i, c)| {
                    if fk.columns.contains(&i) {
                        format!("{} AS {}", col(i, src), quote(&c.name))
                    } else {
                        format!("romulus_t.{} AS {}", quote(&c.name), quote(&c.name))
                    }
                })
                .collect();
            let _ = write!(
                out,
                " AND EXISTS (SELECT 1 FROM (SELECT {}) AS r WHERE ({}) IS NOT FALSE)",
                row.join(", "),
                check.sql
            );
        }
        out
    };
    let update = |src: &str, from: &str| {
        format!(
            "UPDATE {} AS romulus_t SET {set_src}{from} WHERE {} AND {} AND {}{}",
            table_sql(t),
            by_key(src),
            present(src),
            exists(src),
            guards(src),
            set_src = if src == "romulus_s" {
                set.clone()
            } else {
                set.replace("romulus_s.", &format!("{src}."))
            },
        )
    };
    let bulk = update(
        "romulus_s",
        &format!(" FROM {} AS romulus_s", staging_table(m, t)),
    );
    let single = update("romulus_row", "");
    let body = format!(
        "DECLARE\n    romulus_n bigint := 0;\n    romulus_k bigint := 0;\n    romulus_failed bigint := 0;\n    romulus_error text;\n    romulus_row record;\n\
         BEGIN\n    SET CONSTRAINTS ALL IMMEDIATE;\n    BEGIN\n        {bulk};\n        GET DIAGNOSTICS romulus_n = ROW_COUNT;\n    \
         EXCEPTION WHEN OTHERS THEN\n        romulus_error := SQLERRM;\n        \
         FOR romulus_row IN SELECT * FROM {staging} ORDER BY {ORD} LOOP\n            BEGIN\n                {single};\n                \
         GET DIAGNOSTICS romulus_k = ROW_COUNT;\n                romulus_n := romulus_n + romulus_k;\n            \
         EXCEPTION WHEN OTHERS THEN\n                romulus_failed := romulus_failed + 1;\n            END;\n        END LOOP;\n    END;\n    \
         {done}\n    IF romulus_failed > 0 THEN {failed} END IF;\nEND",
        staging = staging_table(m, t),
        done = record(
            m,
            t,
            "deferred",
            Some(&fk.name),
            "romulus_n",
            "romulus_error"
        ),
        failed = record(
            m,
            t,
            "deferred_failed",
            Some(&fk.name),
            "romulus_failed",
            "romulus_error"
        ),
    );
    Some(do_block(
        &format!("{} of {}, after its rows", fk.name, t.qualified()),
        &body,
    ))
}

pub fn sequences(m: &Model, t: &Table) -> Option<String> {
    if t.sequences.is_empty() || t.rows == 0 {
        return None;
    }
    let mut body = String::from("BEGIN\n");
    for (c, seq) in &t.sequences {
        let col = &t.columns[*c];
        let seq_expr = match seq {
            Some(s) => literal(s),
            None => format!(
                "pg_get_serial_sequence({}, {})",
                literal(&table_sql(t)),
                literal(&col.name)
            ),
        };
        let _ = writeln!(
            body,
            "    BEGIN\n        PERFORM setval({seq_expr}, romulus_m) FROM (SELECT max({}) AS romulus_m FROM {}) AS romulus_x WHERE romulus_m IS NOT NULL;\n    EXCEPTION WHEN OTHERS THEN\n        {}\n    END;",
            quote(&col.name),
            table_sql(t),
            record(m, t, "sequence_failed", Some(&col.name), "0", "SQLERRM"),
        );
    }
    body.push_str("END");
    Some(do_block(
        &format!("sequences of {} moved past the rows", t.qualified()),
        &body,
    ))
}

pub fn row_security(t: &Table, force: bool) -> String {
    do_block(
        &format!(
            "row level security of {} {}",
            t.qualified(),
            if force {
                "forced again"
            } else {
                "not forced while loading"
            }
        ),
        &format!(
            "BEGIN\n    ALTER TABLE {} {}FORCE ROW LEVEL SECURITY;\nEXCEPTION WHEN OTHERS THEN NULL;\nEND",
            table_sql(t),
            if force { "" } else { "NO " }
        ),
    )
}

pub fn refresh(m: &Model, matview: &str) -> String {
    do_block(
        &format!("refresh {matview}"),
        &format!(
            "BEGIN\n    REFRESH MATERIALIZED VIEW {matview};\nEXCEPTION WHEN OTHERS THEN\n    INSERT INTO {} (\"schema\", \"table\", kind, name, rows, message) VALUES (NULL, NULL, 'refresh_failed', {}, 0, SQLERRM);\nEND",
            outcome(m),
            literal(matview),
        ),
    )
}

pub fn outcome_query(m: &Model) -> String {
    format!(
        "-- romulus_fakedata: the outcome, one JSON array\nSELECT coalesce(json_agg(o ORDER BY o.seq), '[]')::text FROM {} AS o",
        outcome(m)
    )
}

pub fn cleanup(m: &Model) -> String {
    format!(
        "-- romulus_fakedata: staging schema dropped\nDROP SCHEMA IF EXISTS {} CASCADE;",
        quote(&m.staging)
    )
}

pub fn analyze(m: &Model) -> Option<String> {
    let tables: Vec<String> = m
        .tables
        .iter()
        .filter(|t| t.skip.is_none() && t.rows > 0)
        .map(table_sql)
        .collect();
    (!tables.is_empty()).then(|| {
        format!(
            "-- romulus_fakedata: statistics\nANALYZE {};",
            tables.join(", ")
        )
    })
}
