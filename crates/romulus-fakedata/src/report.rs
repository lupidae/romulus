//! What the generator did and could not do: per table, the rows planned
//! and inserted, how each column was filled, the constraints that forced a
//! fallback, and the rows Postgres dropped, with why.

use serde::{Deserialize, Serialize};

use crate::{
    Error, Profile, Size,
    heuristics::Heuristic,
    model::{Fallback, FallbackKind, Fill, FkMode, Model, Role},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub generator_version: u32,
    pub size: Size,
    pub profile: Profile,
    pub seed: u64,
    pub rows_planned: u64,
    /// A rough size of the rows on disk, indexes included.
    pub estimated_bytes: u64,
    /// Filled by [`Report::record_outcome`] once the load ran.
    pub rows_inserted: Option<u64>,
    pub tables: Vec<TableReport>,
    /// Relations not loaded at all (foreign tables, an extension's).
    pub skipped: Vec<Skipped>,
    /// What happened outside any one table (a materialized view that
    /// failed to refresh).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skipped {
    pub relation: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableReport {
    pub schema: String,
    pub table: String,
    pub role: Role,
    pub rows_planned: u64,
    pub rows_inserted: Option<u64>,
    pub columns: Vec<ColumnReport>,
    pub fallbacks: Vec<Fallback>,
    pub dropped: Vec<Dropped>,
    /// Why the table gets no rows, when it gets none by design.
    pub skipped: Option<String>,
    /// The bulk insert failed and the rows went in one at a time (slow:
    /// a sign of a value the generator got wrong).
    #[serde(default)]
    pub row_by_row: bool,
    /// What the load reported besides drops (a cycle's foreign key filled).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnReport {
    pub name: String,
    #[serde(flatten)]
    pub source: Source,
}

/// Where a column's values come from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Source {
    /// A column-name heuristic.
    Heuristic { heuristic: Heuristic },
    /// Its type alone (within its CHECKs).
    Type,
    /// Its constant default, mixed with typed values.
    Default,
    /// Shaped by CHECKs (bounds, a list, a pattern).
    Check { constraints: Vec<String> },
    /// Copied from a parent row.
    ForeignKey { constraint: String, how: String },
    /// Inside the row's partition.
    PartitionKey,
    /// `1, 2, 3…` written explicitly (a key others point at).
    Sequence,
    /// Left to Postgres.
    Database { reason: String },
    /// Always NULL.
    Null,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dropped {
    pub reason: DropReason,
    /// The constraint or column concerned.
    pub name: Option<String>,
    pub rows: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DropReason {
    /// A CHECK was false for them (a row can break several).
    Check,
    /// Their parent row was dropped.
    ForeignKey,
    /// A unique or exclusion constraint already held their key.
    Duplicate,
    /// No value for a `NOT NULL` column (named).
    Null,
    /// Not valid input for the column's type or domain.
    InvalidValue,
    /// Postgres raised on them one at a time (the message is the first).
    Error,
    /// A cycle of NOT NULL foreign keys could not be loaded: its tables
    /// stay empty.
    Cycle,
}

/// One row of the outcome table.
#[derive(Debug, Deserialize)]
struct Outcome {
    schema: Option<String>,
    table: Option<String>,
    kind: String,
    name: Option<String>,
    rows: i64,
    message: Option<String>,
}

/// What one table's outcome rows add up to.
struct Counts {
    valid: Option<u64>,
    judged: u64,
    errors: u64,
    /// Inserted, then removed (a cycle's rows left without a parent).
    removed: u64,
    bulk_only: bool,
}

pub fn build(m: &Model) -> Report {
    let tables: Vec<TableReport> = m
        .tables
        .iter()
        .map(|t| TableReport {
            schema: t.schema.clone(),
            table: t.name.clone(),
            role: t.role,
            rows_planned: t.rows,
            rows_inserted: None,
            columns: t
                .columns
                .iter()
                .map(|c| ColumnReport {
                    name: c.name.clone(),
                    source: match &c.fill {
                        Fill::Database(reason) => Source::Database {
                            reason: (*reason).to_owned(),
                        },
                        Fill::Sequence { .. } => Source::Sequence,
                        Fill::Foreign { fk, .. } => {
                            let f = &t.fks[*fk];
                            Source::ForeignKey {
                                constraint: f.name.clone(),
                                how: match f.mode {
                                    FkMode::Skewed => "skewed fan-out",
                                    FkMode::OneToOne => "one row per parent",
                                    FkMode::Tree => "a tree: an earlier row",
                                    FkMode::Chain => "a chain: the previous row",
                                    FkMode::Combo { .. } => "every combination once",
                                }
                                .to_owned()
                                    + if f.deferred {
                                        ", filled after the rows"
                                    } else {
                                        ""
                                    },
                            }
                        }
                        Fill::Partition => Source::PartitionKey,
                        Fill::Null => Source::Null,
                        Fill::Value => match c.heuristic {
                            Some(heuristic) => Source::Heuristic { heuristic },
                            None if !c.bounds.shaped_by.is_empty() => Source::Check {
                                constraints: c.bounds.shaped_by.clone(),
                            },
                            None if c.default.is_some() => Source::Default,
                            None => Source::Type,
                        },
                    },
                })
                .collect(),
            fallbacks: t.fallbacks.clone(),
            dropped: Vec::new(),
            skipped: t.skip.clone(),
            row_by_row: false,
            notes: Vec::new(),
        })
        .collect();
    let rows_planned = tables.iter().map(|t| t.rows_planned).sum();
    // Rows, plus about half again for indexes.
    let estimated_bytes = m
        .tables
        .iter()
        .map(|t| t.rows * t.bytes_per_row * 3 / 2)
        .sum();
    Report {
        generator_version: crate::GENERATOR_VERSION,
        size: m.options.size,
        profile: m.options.profile,
        seed: m.options.seed,
        rows_planned,
        estimated_bytes,
        rows_inserted: None,
        tables,
        skipped: m
            .skipped
            .iter()
            .map(|(relation, reason)| Skipped {
                relation: relation.clone(),
                reason: reason.clone(),
            })
            .collect(),
        notes: Vec::new(),
    }
}

impl Report {
    /// Folds in what Postgres judged: the single text value
    /// [`crate::Step::Outcome`]'s query returns.
    #[allow(clippy::too_many_lines)]
    pub fn record_outcome(&mut self, json: &str) -> Result<(), Error> {
        let rows: Vec<Outcome> =
            serde_json::from_str(json).map_err(|e| Error::Outcome(e.to_string()))?;
        let mut counts: Vec<Counts> = self
            .tables
            .iter()
            .map(|_| Counts {
                valid: None,
                judged: 0,
                errors: 0,
                removed: 0,
                bulk_only: true,
            })
            .collect();
        for t in &mut self.tables {
            t.rows_inserted = Some(0);
        }
        for o in rows {
            let n = u64::try_from(o.rows).unwrap_or(0);
            let Some(i) = self.tables.iter().position(|t| {
                Some(&t.schema) == o.schema.as_ref() && Some(&t.table) == o.table.as_ref()
            }) else {
                if o.kind == "refresh_failed" {
                    self.notes.push(format!(
                        "{} was not refreshed: {}",
                        o.name.unwrap_or_default(),
                        o.message.unwrap_or_default()
                    ));
                }
                continue;
            };
            let t = &mut self.tables[i];
            let c = &mut counts[i];
            // One line per reason and constraint (a cycle's passes add up).
            let drop = |t: &mut TableReport, reason, name: Option<String>, rows, message| match t
                .dropped
                .iter_mut()
                .find(|existing| existing.reason == reason && existing.name == name)
            {
                Some(existing) => existing.rows += rows,
                None => t.dropped.push(Dropped {
                    reason,
                    name,
                    rows,
                    message,
                }),
            };
            match o.kind.as_str() {
                "inserted" => t.rows_inserted = Some(t.rows_inserted.unwrap_or(0) + n),
                "invalid" => drop(t, DropReason::InvalidValue, o.name, n, None),
                "check" => drop(t, DropReason::Check, o.name, n, None),
                "null" => drop(t, DropReason::Null, o.name, n, None),
                "removed" => {
                    c.removed += n;
                    drop(t, DropReason::ForeignKey, o.name, n, None);
                }
                "foreign_key" => {
                    drop(t, DropReason::ForeignKey, o.name, n, None);
                }
                "valid" => c.valid = Some(n),
                "judged" => c.judged = n,
                "error" => {
                    c.errors = n;
                    drop(t, DropReason::Error, None, n, o.message);
                }
                "fallback" => {
                    c.bulk_only = false;
                    t.row_by_row = true;
                    t.notes.push(format!(
                        "loaded one row at a time after the bulk insert failed: {}",
                        o.message.unwrap_or_default()
                    ));
                }
                "cycle_failed" => {
                    t.rows_inserted = Some(0);
                    drop(t, DropReason::Cycle, None, t.rows_planned, o.message);
                }
                "deferred" => t.notes.push(format!(
                    "{}: filled on {n} rows after the load",
                    o.name.unwrap_or_default()
                )),
                "deferred_failed" => t.notes.push(format!(
                    "{}: {n} rows kept NULL ({})",
                    o.name.unwrap_or_default(),
                    o.message.unwrap_or_default()
                )),
                "sequence_failed" => t.notes.push(format!(
                    "the sequence of {} was not moved: {}",
                    o.name.unwrap_or_default(),
                    o.message.unwrap_or_default()
                )),
                _ => {}
            }
        }
        for (t, c) in self.tables.iter_mut().zip(&counts) {
            let inserted = t.rows_inserted.unwrap_or(0);
            if let Some(valid) = c.valid {
                let duplicates = valid
                    .saturating_sub(c.judged)
                    .saturating_sub(inserted)
                    .saturating_sub(c.removed)
                    .saturating_sub(c.errors);
                if duplicates > 0 && (c.bulk_only || c.errors == 0) {
                    t.dropped.push(Dropped {
                        reason: DropReason::Duplicate,
                        name: None,
                        rows: duplicates,
                        message: None,
                    });
                }
            }
        }
        self.rows_inserted = Some(self.tables.iter().filter_map(|t| t.rows_inserted).sum());
        Ok(())
    }

    /// Rows planned but not inserted, per table (after the outcome).
    pub fn rows_dropped(&self) -> u64 {
        self.tables
            .iter()
            .map(|t| {
                t.rows_planned
                    .saturating_sub(t.rows_inserted.unwrap_or(t.rows_planned))
            })
            .sum()
    }

    /// The report as sentences, for a progress log or a page: what was
    /// dropped and why, what could not be built.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.tables {
            let name = format!("{}.{}", t.schema, t.table);
            if let Some(why) = &t.skipped {
                out.push(format!("{name}: no rows, {why}"));
                continue;
            }
            for d in &t.dropped {
                let what = match (d.reason, &d.name) {
                    (DropReason::Check, Some(n)) => format!("broke `{n}`"),
                    (DropReason::ForeignKey, Some(n)) => format!("lost their parent row (`{n}`)"),
                    (DropReason::ForeignKey, None) => "lost their parent row".to_owned(),
                    (DropReason::Null, Some(n)) => format!("had no value for `{n}` (NOT NULL)"),
                    (DropReason::InvalidValue, Some(n)) => {
                        format!("had a value `{n}` does not accept")
                    }
                    (DropReason::Duplicate, _) => "repeated a unique key".to_owned(),
                    (DropReason::Cycle, _) => {
                        "belong to a cycle of foreign keys that could not be loaded".to_owned()
                    }
                    _ => "were refused".to_owned(),
                };
                let message = d
                    .message
                    .as_deref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default();
                out.push(format!(
                    "{name}: {} of {} rows {what} and were dropped{message}",
                    d.rows, t.rows_planned
                ));
            }
            for f in t
                .fallbacks
                .iter()
                .filter(|f| !matches!(f.kind, FallbackKind::Redrawn | FallbackKind::Judged))
            {
                match &f.constraint {
                    Some(c) => out.push(format!("{name}: `{c}`: {}", f.reason)),
                    None => out.push(format!("{name}: {}", f.reason)),
                }
            }
            out.extend(t.notes.iter().map(|n| format!("{name}: {n}")));
        }
        for s in &self.skipped {
            out.push(format!("{}: not loaded, {}", s.relation, s.reason));
        }
        out.extend(self.notes.iter().cloned());
        out
    }
}
