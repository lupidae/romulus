//! The rows a check looks at: every table's count, and a sample of rows by
//! primary key, each value as Postgres prints it.
//!
//! The sample is the same rows on every run of the same data: those whose
//! key hashes lowest (`md5` of the key's text), a spread across the table
//! rather than its first rows. A value longer than [`LONG`] bytes is kept
//! as its hash (compared, never shown). A table without a primary key (or a
//! unique key of `NOT NULL` columns) is counted, not sampled: its rows have
//! no identity to follow; its columns are compared as wholes instead
//! ([`Aggregate`]: values counted and hashed in order). Sequences' last
//! values are read too, to tell one that went back.
//!
//! Reads run in a transaction rolled back, names resolving in
//! `pg_catalog`; a table forcing row level security on its owner is read
//! with that off in the same transaction (its policies are for the people
//! querying it).

use std::collections::{HashMap, HashSet};

use remus_core::{
    Schema,
    model::{ConstraintKind, EntityKind, TypeKind},
};

use crate::{
    session::{Session, SessionError, qualified, quote_ident, quote_literal},
    statements::{Limits, read, scalar},
};

/// Values longer than this many bytes are compared by their hash.
pub const LONG: usize = 4096;

/// A column a check compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// As `format_type` prints it (qualified unless in `pg_catalog`).
    pub ty: String,
    /// Its enum type, when it is one (or an array of one).
    pub enum_type: Option<(String, String)>,
    pub is_array: bool,
}

/// A table holding rows: a plain table, or a partitioned one read across
/// its partitions (remus's model lists partitions under their parent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    /// The columns its rows are found by: the primary key, or a unique key
    /// of `NOT NULL` columns; empty when it has none.
    pub key: Vec<String>,
    /// Every key rows can be found by, `key` first (the primary key, then
    /// unique keys of `NOT NULL` columns): another is tried when the
    /// migration drops or converts one of `key`'s columns.
    pub keys: Vec<Vec<String>>,
    /// Every column but generated ones (computed, their expression is the
    /// schema check's).
    pub columns: Vec<ColumnInfo>,
    pub forced: bool,
    /// Partitioned: its rows are its partitions' (read without `ONLY`).
    pub partitioned: bool,
}

impl TableInfo {
    pub fn label(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }

    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// The tables of `model` that hold rows, in the model's order.
pub fn tables(model: &Schema) -> Vec<TableInfo> {
    let mut model = model.clone();
    romulus_migration::without_extension_objects(&mut model);
    let keys_of = |schema: &str, name: &str| -> Vec<Vec<String>> {
        // A partition's keys are its parent's (the model lists the parent's).
        let mut at = (schema.to_owned(), name.to_owned());
        for _ in 0..8 {
            let Some(entity) = model
                .entities
                .iter()
                .find(|e| e.schema == at.0 && e.name == at.1)
            else {
                return Vec::new();
            };
            let not_null = |c: &String| {
                entity
                    .columns
                    .iter()
                    .any(|col| col.name == *c && col.not_null)
            };
            let keys: Vec<Vec<String>> = entity
                .constraints
                .iter()
                .filter(|c| c.kind == ConstraintKind::PrimaryKey)
                .chain(entity.constraints.iter().filter(|c| {
                    c.kind == ConstraintKind::Unique
                        && !c.columns.is_empty()
                        && c.columns.iter().all(not_null)
                }))
                .map(|c| c.columns.clone())
                .collect();
            if !keys.is_empty() {
                return keys;
            }
            match model
                .partitions
                .iter()
                .find(|p| p.child_schema == at.0 && p.child == at.1)
            {
                Some(p) => at = (p.parent_schema.clone(), p.parent.clone()),
                None => return Vec::new(),
            }
        }
        Vec::new()
    };
    model
        .entities
        .iter()
        .filter(|e| e.kind.is_table())
        .map(|e| {
            let columns: Vec<ColumnInfo> = e
                .columns
                .iter()
                .filter(|c| c.generated.is_none())
                .map(|c| ColumnInfo {
                    name: c.name.clone(),
                    ty: c.r#type.clone(),
                    enum_type: (c.type_kind == TypeKind::Enum)
                        .then(|| (c.type_schema.clone(), c.type_name.clone())),
                    is_array: c.is_array || c.r#type.ends_with("[]"),
                })
                .collect();
            let mut keys = keys_of(&e.schema, &e.name);
            // A key the table lacks (a model out of step) is no key; one
            // over a generated column is not sampled.
            keys.retain(|key| {
                key.iter()
                    .all(|k| columns.iter().any(|c: &ColumnInfo| c.name == *k))
            });
            TableInfo {
                schema: e.schema.clone(),
                name: e.name.clone(),
                key: keys.first().cloned().unwrap_or_default(),
                keys,
                columns,
                forced: e.rls_forced,
                partitioned: e.kind == EntityKind::PartitionedTable,
            }
        })
        .collect()
}

/// A value as read: NULL, its text, or the hash of a long one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cell {
    Null,
    Text(String),
    /// `md5` of a value longer than [`LONG`] bytes.
    Hash(String),
}

impl Cell {
    fn read(raw: Option<String>) -> Self {
        match raw {
            None => Self::Null,
            Some(s) => match s.split_at_checked(1) {
                Some(("h", hash)) => Self::Hash(hash.to_owned()),
                Some(("v", text)) => Self::Text(text.to_owned()),
                _ => Self::Text(s),
            },
        }
    }

    /// For a report: the text, cut; `NULL` as `None`.
    pub fn shown(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Text(text) if text.chars().count() > 120 => {
                Some(format!("{}…", text.chars().take(120).collect::<String>()))
            }
            Self::Text(text) => Some(text.clone()),
            Self::Hash(_) => Some(format!("(a value over {LONG} bytes)")),
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// One sampled row: its key's text (one per key column) and its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: Vec<String>,
    /// In the order of the columns asked for.
    pub values: Vec<Cell>,
}

impl Row {
    /// The key as a report shows it: `(42)`, `(7, en)`.
    pub fn key_label(&self) -> String {
        format!("({})", self.key.join(", "))
    }
}

/// `c` as a sampled value: tagged text, or the tagged hash of a long one.
fn cell(column: &str) -> String {
    let c = quote_ident(column);
    format!(
        "CASE WHEN {c} IS NULL THEN NULL \
         WHEN pg_catalog.octet_length({c}::text) > {LONG} THEN 'h' || pg_catalog.md5({c}::text) \
         ELSE 'v' || {c}::text END"
    )
}

fn no_force(schema: &str, name: &str, forced: bool) -> String {
    if forced {
        format!(
            "ALTER TABLE {} NO FORCE ROW LEVEL SECURITY; ",
            qualified(schema, name)
        )
    } else {
        String::new()
    }
}

/// How a table is read: forcing row level security on its owner, and
/// partitioned (its rows are its partitions').
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scope {
    pub forced: bool,
    pub partitioned: bool,
}

impl Scope {
    pub fn of(table: &TableInfo) -> Self {
        Self {
            forced: table.forced,
            partitioned: table.partitioned,
        }
    }
}

/// `ONLY` for a table of its own rows (a plain table's partitions, if
/// any, are counted on their own); nothing for a partitioned one.
fn only(scope: Scope) -> &'static str {
    if scope.partitioned { "" } else { "ONLY " }
}

/// Rows of `schema.name` (its own; a partitioned table's, its partitions').
pub async fn count<S: Session>(
    session: &mut S,
    schema: &str,
    name: &str,
    scope: Scope,
    limits: Limits,
) -> Result<i64, SessionError> {
    let rows = read(
        session,
        &format!(
            "{}SELECT pg_catalog.count(*)::text FROM {}{}",
            no_force(schema, name, scope.forced),
            only(scope),
            qualified(schema, name)
        ),
        limits.read(),
    )
    .await?;
    Ok(scalar(&rows).and_then(|n| n.parse().ok()).unwrap_or(0))
}

/// A table to read rows of, named as it is now.
#[derive(Debug, Clone, Copy)]
pub struct Reading<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    /// The key's columns.
    pub key: &'a [String],
    /// The columns whose values to read.
    pub columns: &'a [String],
    pub scope: Scope,
}

/// Up to `n` rows of a table, by key (see the module documentation).
pub async fn sample<S: Session>(
    session: &mut S,
    table: Reading<'_>,
    n: usize,
    limits: Limits,
) -> Result<Vec<Row>, SessionError> {
    let Reading {
        schema,
        name,
        key,
        columns,
        scope,
    } = table;
    let keys: Vec<String> = key
        .iter()
        .map(|k| format!("{}::text", quote_ident(k)))
        .collect();
    let values: Vec<String> = columns.iter().map(|c| cell(c)).collect();
    let row_of_key = format!("ROW({})", keys.join(", "));
    let sql = format!(
        "{}SELECT {}, {} FROM {}{} ORDER BY pg_catalog.md5({row_of_key}::text) LIMIT {n}",
        no_force(schema, name, scope.forced),
        keys.join(", "),
        if values.is_empty() {
            "NULL".to_owned()
        } else {
            values.join(", ")
        },
        only(scope),
        qualified(schema, name)
    );
    let rows = read(session, &sql, limits.read()).await?;
    Ok(rows
        .into_iter()
        .map(|row| split(row, key.len(), columns.len()))
        .collect())
}

fn split(row: Vec<Option<String>>, keys: usize, columns: usize) -> Row {
    let mut row = row.into_iter();
    let key = (&mut row)
        .take(keys)
        .map(Option::unwrap_or_default)
        .collect();
    let values = row.take(columns).map(Cell::read).collect();
    Row { key, values }
}

/// The rows of a table whose keys' texts are `wanted` (each a key's
/// columns' texts), by key.
pub async fn lookup<S: Session>(
    session: &mut S,
    table: Reading<'_>,
    wanted: &[Vec<String>],
    limits: Limits,
) -> Result<HashMap<Vec<String>, Row>, SessionError> {
    let Reading {
        schema,
        name,
        key,
        columns,
        scope,
    } = table;
    if wanted.is_empty() || key.is_empty() {
        return Ok(HashMap::new());
    }
    let keys: Vec<String> = key
        .iter()
        .map(|k| format!("{}::text", quote_ident(k)))
        .collect();
    let values: Vec<String> = columns.iter().map(|c| cell(c)).collect();
    let list: Vec<String> = wanted
        .iter()
        .map(|k| {
            format!(
                "({})",
                k.iter()
                    .map(|v| quote_literal(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    let matched = if keys.len() == 1 {
        keys[0].clone()
    } else {
        format!("ROW({})", keys.join(", "))
    };
    let sql = format!(
        "{}SELECT {}, {} FROM {}{} WHERE {matched} IN (VALUES {})",
        no_force(schema, name, scope.forced),
        keys.join(", "),
        if values.is_empty() {
            "NULL".to_owned()
        } else {
            values.join(", ")
        },
        only(scope),
        qualified(schema, name),
        list.join(", ")
    );
    let rows = read(session, &sql, limits.read()).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let row = split(row, key.len(), columns.len());
            (row.key.clone(), row)
        })
        .collect())
}

/// One column of a table without a key, as a whole: how many values it
/// holds, and the hash of all of them in order (`None`: none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregate {
    pub non_null: i64,
    pub digest: Option<String>,
}

/// The columns' [`Aggregate`]s of `schema.name` (its own rows), in the
/// order of `columns`.
pub async fn aggregates<S: Session>(
    session: &mut S,
    schema: &str,
    name: &str,
    columns: &[String],
    scope: Scope,
    limits: Limits,
) -> Result<Vec<Aggregate>, SessionError> {
    let columns: Vec<String> = columns.iter().map(|c| quote_ident(c)).collect();
    aggregates_of(session, schema, name, &columns, scope, limits).await
}

/// [`Aggregate`]s of expressions over the rows of `schema.name` (a column
/// cast back, labels mapped back), as [`aggregates`] reads a column: the
/// count of values and the hash of their sorted texts.
pub async fn aggregates_of<S: Session>(
    session: &mut S,
    schema: &str,
    name: &str,
    expressions: &[String],
    scope: Scope,
    limits: Limits,
) -> Result<Vec<Aggregate>, SessionError> {
    let columns = expressions;
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<String> = columns
        .iter()
        .map(|c| {
            let c = format!("({c})");
            format!(
                "pg_catalog.count({c})::text, pg_catalog.md5(pg_catalog.string_agg({c}::text, E'\\x1f' ORDER BY {c}::text COLLATE \"C\"))"
            )
        })
        .collect();
    let rows = read(
        session,
        &format!(
            "{}SELECT {} FROM {}{}",
            no_force(schema, name, scope.forced),
            parts.join(", "),
            only(scope),
            qualified(schema, name)
        ),
        limits.read(),
    )
    .await?;
    let row = rows.into_iter().next().unwrap_or_default();
    let mut values = row.into_iter();
    Ok(columns
        .iter()
        .map(|_| Aggregate {
            non_null: values
                .next()
                .flatten()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            digest: values.next().flatten(),
        })
        .collect())
}

/// A sequence's next value, and the table owning it (a serial's, an
/// identity's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceValue {
    pub schema: String,
    pub name: String,
    /// What `nextval` would give now.
    pub next: i64,
    pub increment: i64,
    pub owner: Option<(String, String)>,
}

/// A sequence as listed: schema, name, increment, owning table.
type Listed = (String, String, i64, Option<(String, String)>);

/// Every user sequence's next value.
pub async fn sequences<S: Session>(
    session: &mut S,
    limits: Limits,
) -> Result<Vec<SequenceValue>, SessionError> {
    let listed = read(
        session,
        "SELECT s.schemaname::text, s.sequencename::text, s.increment_by::text, \
                tn.nspname::text, t.relname::text \
         FROM pg_catalog.pg_sequences s \
         JOIN pg_catalog.pg_namespace n ON n.nspname = s.schemaname \
         JOIN pg_catalog.pg_class c ON c.relname = s.sequencename AND c.relnamespace = n.oid \
         LEFT JOIN pg_catalog.pg_depend d ON d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass \
              AND d.objid = c.oid AND d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass \
              AND d.deptype IN ('a', 'i') \
         LEFT JOIN pg_catalog.pg_class t ON t.oid = d.refobjid \
         LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.relnamespace \
         WHERE s.schemaname NOT IN ('pg_catalog', 'information_schema') \
         ORDER BY 1, 2",
        limits.read(),
    )
    .await?;
    let listed: Vec<Listed> = listed
        .into_iter()
        .filter_map(|row| {
            let mut row = row.into_iter();
            let schema = row.next()??;
            let name = row.next()??;
            let increment = row.next()??.parse().ok()?;
            let owner = match (row.next().flatten(), row.next().flatten()) {
                (Some(s), Some(t)) => Some((s, t)),
                _ => None,
            };
            Some((schema, name, increment, owner))
        })
        .collect();
    if listed.is_empty() {
        return Ok(Vec::new());
    }
    // `last_value` and whether it was handed out: `pg_sequences` says NULL
    // for a sequence set with `setval(…, false)`.
    let reads: Vec<String> = listed
        .iter()
        .enumerate()
        .map(|(i, (schema, name, _, _))| {
            format!(
                "SELECT {i}::text, last_value::text, is_called::text FROM {}",
                qualified(schema, name)
            )
        })
        .collect();
    let values = read(session, &reads.join(" UNION ALL "), limits.read()).await?;
    let mut next: HashMap<usize, i64> = HashMap::new();
    for row in values {
        let mut row = row.into_iter();
        let (Some(Some(i)), Some(Some(last)), Some(Some(called))) =
            (row.next(), row.next(), row.next())
        else {
            continue;
        };
        if let (Ok(i), Ok(last)) = (i.parse::<usize>(), last.parse::<i64>()) {
            let increment = listed.get(i).map_or(1, |l| l.2);
            next.insert(
                i,
                if called == "true" {
                    last.saturating_add(increment)
                } else {
                    last
                },
            );
        }
    }
    Ok(listed
        .into_iter()
        .enumerate()
        .filter_map(|(i, (schema, name, increment, owner))| {
            Some(SequenceValue {
                schema,
                name,
                next: *next.get(&i)?,
                increment,
                owner,
            })
        })
        .collect())
}

/// Counts and samples of the tables, before the migration.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// By `(schema, name)`; a table that could not be counted is absent.
    pub counts: HashMap<(String, String), i64>,
    /// By `(schema, name)`, the rows sampled; values in the order of the
    /// table's [`TableInfo::columns`].
    pub samples: HashMap<(String, String), Vec<Row>>,
    /// By `(schema, name)`, for tables without a key holding rows: each
    /// column as a whole, in the order of [`TableInfo::columns`].
    pub aggregates: HashMap<(String, String), Vec<Aggregate>>,
    /// Tables without a key too large to read as wholes within the limit:
    /// not verified, and the report says so.
    pub too_large: HashSet<(String, String)>,
    pub sequences: Vec<SequenceValue>,
}

impl Snapshot {
    pub fn rows(&self) -> i64 {
        self.counts.values().sum()
    }

    pub fn sampled(&self) -> usize {
        self.samples.values().map(Vec::len).sum()
    }
}

/// Counts every table and samples `n` rows of each with a key.
pub async fn take<S: Session>(
    session: &mut S,
    tables: &[TableInfo],
    n: usize,
    limits: Limits,
) -> Result<Snapshot, SessionError> {
    let mut snapshot = Snapshot::default();
    for table in tables {
        let id = (table.schema.clone(), table.name.clone());
        let rows = count(
            session,
            &table.schema,
            &table.name,
            Scope::of(table),
            limits,
        )
        .await?;
        snapshot.counts.insert(id.clone(), rows);
        if rows == 0 || n == 0 {
            continue;
        }
        let columns: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        if table.key.is_empty() {
            match aggregates(
                session,
                &table.schema,
                &table.name,
                &columns,
                Scope::of(table),
                limits,
            )
            .await
            {
                Ok(wholes) => {
                    snapshot.aggregates.insert(id, wholes);
                }
                Err(err) if err.timed_out() => {
                    snapshot.too_large.insert(id);
                }
                Err(err) => return Err(err),
            }
            continue;
        }
        let reading = Reading {
            schema: &table.schema,
            name: &table.name,
            key: &table.key,
            columns: &columns,
            scope: Scope::of(table),
        };
        let sampled = sample(session, reading, n, limits).await?;
        snapshot.samples.insert(id, sampled);
    }
    snapshot.sequences = sequences(session, limits).await?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_read_back_their_tags() {
        assert_eq!(Cell::read(None), Cell::Null);
        assert_eq!(Cell::read(Some("v42".into())), Cell::Text("42".into()));
        assert_eq!(Cell::read(Some("v".into())), Cell::Text(String::new()));
        assert_eq!(Cell::read(Some("habc".into())), Cell::Hash("abc".into()));
        assert_eq!(
            Cell::Text("x".repeat(200)).shown().unwrap().chars().count(),
            121
        );
    }

    #[test]
    fn rows_split_into_key_and_values() {
        let row = split(
            vec![
                Some("7".into()),
                Some("en".into()),
                Some("vhello".into()),
                None,
            ],
            2,
            2,
        );
        assert_eq!(row.key, ["7", "en"]);
        assert_eq!(row.values, [Cell::Text("hello".into()), Cell::Null]);
        assert_eq!(row.key_label(), "(7, en)");
    }
}
