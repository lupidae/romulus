//! The checks after each direction.
//!
//! **After up**: the schema is the target's; every table kept has its rows
//! (counts), and its sampled rows, found again by key (renamed columns
//! followed, converted keys converted, another unique key when a key's
//! column goes), hold the values they held: the same text, or for a
//! converted column what the migration's own cast gives (its `USING`,
//! evaluated on the old values, types under their names after the
//! migration), and for an enum whose label was renamed the new label
//! (arrays of it too). A conversion's values are then cast back: those that
//! do not come back are what it rounded or cut. A table without a key has
//! its unchanged columns compared as wholes (count and hash of every
//! value): what changes in it cannot be verified, and says so.
//!
//! **After down**: the schema is the base's again, the counts are, and the
//! sampled rows are back with their original values, by their original
//! keys and names. What is not back is data loss ([`DataLoss`]). No
//! sequence went back.
//!
//! Every difference is explained by a statement, or not: a value changed
//! by an `UPDATE`, a row gone with a `DELETE`, a column dropped, a value
//! rounded by a conversion. Planned statements explain loss only when
//! romulus-migration flagged them `deletes_data`; a hand-written one when
//! it says it does it (a `DROP COLUMN` its values going, an `UPDATE` its
//! values changing: [`declares`]), else a developer acknowledges it, as
//! for a `DO` block whose effects cannot be read. Loss nothing explains, or
//! that a planned statement explains without its flag, fails the test: the
//! migration did something it did not say.

use std::collections::BTreeMap;

use remus_core::Schema;

use crate::{
    MAX_TABLES_LISTED,
    compare::{SchemaOptions, differences, introspect},
    effects::{Conversion, Effect, Walk},
    report::{
        CheckKind, CheckReport, DataLoss, Direction, Example, Flag, LossKind, Note, NoteKind,
        StatementRef, Status, TableCheck,
    },
    session::{Rows, Session, SessionError, quote_ident, quote_literal},
    snapshot::{self, Cell, Row, Scope, SequenceValue, Snapshot, TableInfo},
    statements::{Limits, Statement, read},
};

/// What a change is, to tell whether a statement written by hand says it
/// does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Rows gone.
    RowsGone,
    /// More rows.
    RowsAdded,
    /// A column's values gone with it.
    ColumnDropped,
    /// Values changed (or not back).
    Values,
}

/// Whether a statement doing `effect` says it makes a change of `shape`: a
/// `DELETE` that rows go, a `DROP COLUMN` that its values do, an `UPDATE`
/// that values change. A conversion does not say it rounds.
pub fn declares(effect: &Effect, shape: Shape) -> bool {
    match shape {
        Shape::RowsGone => matches!(
            effect,
            Effect::TableDropped { .. } | Effect::RowsDeleted { .. }
        ),
        Shape::RowsAdded => matches!(effect, Effect::RowsAdded { .. }),
        Shape::ColumnDropped => matches!(
            effect,
            Effect::ColumnDropped { .. } | Effect::TableDropped { .. }
        ),
        Shape::Values => matches!(
            effect,
            Effect::ValuesChanged { .. }
                | Effect::LabelRenamed { .. }
                | Effect::ColumnDropped { .. }
                | Effect::TableDropped { .. }
                | Effect::RowsDeleted { .. }
        ),
    }
}

/// An effect, where it comes from.
#[derive(Debug, Clone)]
pub struct Located {
    pub direction: Direction,
    pub index: usize,
    pub effect: Effect,
    pub planned: bool,
    pub hazard: Option<String>,
}

impl Located {
    pub fn all(
        direction: Direction,
        statements: &[Statement],
        effects: Vec<Vec<Effect>>,
    ) -> Vec<Self> {
        effects
            .into_iter()
            .enumerate()
            .flat_map(|(index, effects)| {
                let statement = &statements[index];
                effects.into_iter().map(move |effect| Self {
                    direction,
                    index,
                    effect,
                    planned: statement.planned,
                    hazard: statement.hazard.clone(),
                })
            })
            .collect()
    }

    /// The statement, as explaining a change of `shape`.
    pub fn reference(&self, shape: Shape) -> StatementRef {
        StatementRef {
            direction: self.direction,
            index: self.index,
            hazard: self.hazard.clone(),
            flag: if matches!(self.effect, Effect::Opaque) {
                Flag::Opaque
            } else if matches!(
                self.effect,
                Effect::Detached { .. } | Effect::Attached { .. }
            ) {
                Flag::Moved
            } else if self.planned {
                if self.hazard.as_deref() == Some("deletes_data") {
                    Flag::Flagged
                } else {
                    Flag::NotFlagged
                }
            } else if declares(&self.effect, shape) {
                Flag::Written
            } else {
                Flag::WrittenUndeclared
            },
        }
    }
}

/// Whether `effect` explains values of `column` (walk index, of `table`;
/// `None`: the table's rows) changing, or `enum_type` values changing. A
/// `DO` block may explain anything: its flag ([`Flag::Opaque`]) says it is
/// not verifiable.
pub fn explains_values(
    effect: &Effect,
    table: usize,
    column: Option<usize>,
    enum_type: Option<usize>,
) -> bool {
    match effect {
        Effect::ColumnConverted {
            table: t,
            column: c,
            ..
        }
        | Effect::ColumnDropped {
            table: t,
            column: c,
        } => *t == table && Some(*c) == column,
        Effect::ValuesChanged { table: t, columns } => {
            *t == table
                && match (columns, column) {
                    (None, _) | (_, None) => true,
                    (Some(columns), Some(c)) => columns.contains(&c),
                }
        }
        Effect::LabelRenamed { enum_type: e, .. } => enum_type == Some(*e),
        Effect::TableDropped { table: t } | Effect::RowsDeleted { table: t, .. } => *t == table,
        Effect::Opaque => true,
        _ => false,
    }
}

/// Whether `effect` explains rows of a table going (or more coming):
/// `family` is the table and every partition it has had (a partition
/// dropped, emptied or detached takes the parent's rows with it).
pub fn explains_rows(effect: &Effect, family: &[usize], shape: Shape) -> bool {
    match effect {
        Effect::TableDropped { table: t } | Effect::RowsDeleted { table: t, .. } => {
            shape == Shape::RowsGone && family.contains(t)
        }
        Effect::RowsAdded { table: t } => shape == Shape::RowsAdded && family.contains(t),
        Effect::Detached { parent, .. } => shape == Shape::RowsGone && family.contains(parent),
        Effect::Attached { parent, .. } => shape == Shape::RowsAdded && family.contains(parent),
        Effect::Opaque => true,
        _ => false,
    }
}

/// The status of something a statement explains (or that none does).
pub fn status_of(cause: Option<&StatementRef>) -> Status {
    match cause.map(|c| c.flag) {
        Some(Flag::Flagged | Flag::Written | Flag::Moved) => Status::Warning,
        Some(Flag::WrittenUndeclared | Flag::Opaque) => Status::NeedsAcknowledgement,
        Some(Flag::NotFlagged) | None => Status::Failed,
    }
}

/// The explaining statement to name: the one saying it best (flagged, then
/// written saying so, then written without saying, then unreadable, then
/// planned without its flag), the first of those.
pub fn pick<'a>(
    candidates: impl IntoIterator<Item = &'a Located>,
    shape: Shape,
) -> Option<StatementRef> {
    let rank = |flag: Flag| match flag {
        Flag::Flagged => 0,
        Flag::Moved | Flag::Written => 1,
        Flag::WrittenUndeclared => 2,
        Flag::Opaque => 3,
        Flag::NotFlagged => 4,
    };
    candidates
        .into_iter()
        .map(|l| l.reference(shape))
        .enumerate()
        .min_by_key(|(i, r)| (rank(r.flag), *i))
        .map(|(_, r)| r)
}

/// `statement 4 of the up migration (flagged: deletes data)`.
pub fn describe(cause: &StatementRef) -> String {
    let flag = match cause.flag {
        Flag::Flagged => " (flagged: deletes data)",
        Flag::NotFlagged => match cause.hazard.as_deref() {
            Some("can_fail") => " (flagged only: can fail)",
            Some("widens_access") => " (flagged only: widens access)",
            _ => " (not flagged)",
        },
        Flag::Written => "",
        Flag::WrittenUndeclared => " (written: it does not say it does this)",
        Flag::Opaque => " (its effects cannot be read: not verifiable)",
        Flag::Moved => " (rows moved with a partition, not lost)",
    };
    format!(
        "statement {} of the {} migration{flag}",
        cause.index + 1,
        cause.direction.word()
    )
}

/// `12,400`.
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        thousands(i64::try_from(n).unwrap_or(i64::MAX)),
        if n == 1 { one } else { many }
    )
}

fn count_of(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// A loss, its `expected` from its status.
fn lost(mut loss: DataLoss, status: Status) -> DataLoss {
    loss.expected = status == Status::Warning;
    loss.status = status;
    loss
}

/// What the checks share.
pub struct Context<'a> {
    pub tables: &'a [TableInfo],
    pub before: &'a Snapshot,
    /// The walk where the up migration left it.
    pub up: &'a Walk,
    /// Every effect, the up migration's then the down's.
    pub effects: &'a [Located],
    /// The base tables (`schema`, `name`) the up migration changes:
    /// something rows matter to touches them.
    pub changed: &'a [(String, String)],
    pub limits: Limits,
    pub schema: SchemaOptions,
}

impl Context<'_> {
    /// Whether the up migration changes `table`.
    fn changes(&self, table: &TableInfo) -> bool {
        self.changed
            .iter()
            .any(|(s, n)| *s == table.schema && *n == table.name)
    }

    fn table_index(&self, table: &TableInfo) -> Option<usize> {
        self.up.tables.iter().position(|t| {
            t.base
                .as_ref()
                .is_some_and(|(s, n)| *s == table.schema && *n == table.name)
        })
    }

    fn column_index(&self, table: usize, name: &str) -> Option<usize> {
        self.up.tables[table]
            .columns
            .iter()
            .position(|c| c.base.as_deref() == Some(name))
    }

    fn enum_index(&self, enum_type: Option<&(String, String)>) -> Option<usize> {
        let wanted = enum_type?;
        self.up
            .types
            .iter()
            .position(|e| e.base.as_ref() == Some(wanted))
    }

    fn of(&self, direction: Option<Direction>) -> impl Iterator<Item = &Located> {
        self.effects
            .iter()
            .filter(move |l| direction.is_none_or(|d| l.direction == d))
    }
}

/// The schema check: `actual` against `expected`, named `label`.
pub fn schema_check(
    actual: &Schema,
    expected: &Schema,
    options: &SchemaOptions,
    label: &str,
) -> CheckReport {
    let differences = differences(actual, expected, options);
    if differences.is_empty() {
        CheckReport {
            kind: CheckKind::Schema,
            status: Status::Passed,
            summary: format!("The schema is {label}, exactly."),
            details: Vec::new(),
            tables: Vec::new(),
        }
    } else {
        CheckReport {
            kind: CheckKind::Schema,
            status: Status::Failed,
            summary: format!(
                "The schema is not {label}: {}.",
                plural(differences.len(), "difference", "differences")
            ),
            details: differences.into_iter().take(50).collect(),
            tables: Vec::new(),
        }
    }
}

/// Introspects and checks the schema against `expected`.
pub async fn check_schema<S: Session>(
    session: &mut S,
    expected: &Schema,
    options: &SchemaOptions,
    label: &str,
    limits: Limits,
) -> CheckReport {
    match introspect(session, limits.read()).await {
        Ok(actual) => schema_check(&actual, expected, options, label),
        Err(err) => CheckReport {
            kind: CheckKind::Schema,
            status: Status::Failed,
            summary: format!("The schema could not be read: {err}"),
            details: Vec::new(),
            tables: Vec::new(),
        },
    }
}

/// A column's values after the up migration, as they should be.
struct Planned<'a> {
    info: &'a snapshot::ColumnInfo,
    /// Walk index of the column, if the walk knows it.
    column: Option<usize>,
    /// Its name now; `None`: dropped.
    now: Option<String>,
    conversions: Vec<(&'a Located, &'a Conversion)>,
    labels: Vec<(&'a str, &'a str)>,
}

/// The expected texts of `rows`' column `at` after `conversions`, evaluated
/// by Postgres (`None` for a row it could not say: a value kept as its
/// hash), or why it could not. `walk[i]` is the walk's index of the table's
/// column `i`, by which each conversion names the columns of its moment.
#[allow(
    clippy::too_many_arguments,
    reason = "the rows, the column, its conversions and the walk naming their types"
)]
async fn convert<S: Session>(
    session: &mut S,
    info: &TableInfo,
    rows: &[Row],
    at: usize,
    walk: &[Option<usize>],
    conversions: &[(&Located, &Conversion)],
    types_at_end: &Walk,
    limits: Limits,
) -> Result<Vec<Option<Cell>>, String> {
    // The column's values as they go through each conversion; the other
    // columns' as they were. Types under their names after the migration
    // (one created `x_new` and renamed `x` is `x` now).
    let mut current: Vec<Option<Cell>> = rows.iter().map(|r| Some(r.values[at].clone())).collect();
    let mut ty = info.columns[at].ty.clone();
    for (_, conversion) in conversions {
        let (to, using) = conversion.resolved(types_at_end);
        let step = Step {
            ty: &ty,
            to: &to,
            using: using.as_deref(),
            names: &conversion.names,
        };
        current = evaluate(session, info, rows, at, walk, &current, step, limits).await?;
        ty = to;
    }
    Ok(current)
}

/// One conversion to evaluate: from type `ty` to `to`, by `using` (the
/// column cast when `None`), under the columns' `names` of its moment.
#[derive(Clone, Copy)]
struct Step<'a> {
    ty: &'a str,
    to: &'a str,
    using: Option<&'a str>,
    names: &'a [(String, usize)],
}

/// One conversion on the sampled rows: `(USING)::type` over the row's
/// values under the names the columns had when it ran, typed as they were
/// (then, if an old type is gone, as text).
#[allow(
    clippy::too_many_arguments,
    reason = "the rows, the column and the step"
)]
async fn evaluate<S: Session>(
    session: &mut S,
    info: &TableInfo,
    rows: &[Row],
    at: usize,
    walk: &[Option<usize>],
    current: &[Option<Cell>],
    step: Step<'_>,
    limits: Limits,
) -> Result<Vec<Option<Cell>>, String> {
    let Step {
        ty,
        to,
        using,
        names,
    } = step;
    let name_then = |i: usize| {
        walk[i].and_then(|w| {
            names
                .iter()
                .find(|(_, index)| *index == w)
                .map(|(name, _)| name.clone())
        })
    };
    let own = name_then(at).unwrap_or_else(|| info.columns[at].name.clone());
    // The other columns of the moment (under their names then).
    let others: Vec<(usize, String)> = (0..info.columns.len())
        .filter(|&i| i != at)
        .filter_map(|i| name_then(i).map(|name| (i, name)))
        .filter(|(_, name)| *name != own)
        .collect();
    let literal = |cell: &Cell| match cell {
        Cell::Null => Some("NULL".to_owned()),
        Cell::Text(t) => Some(quote_literal(t)),
        Cell::Hash(_) => None,
    };
    let mut list = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(Some(value)) = current.get(i) else {
            continue;
        };
        let Some(value) = literal(value) else {
            continue;
        };
        let mut fields = vec![i.to_string(), value];
        for (ci, _) in &others {
            fields.push(literal(&row.values[*ci]).unwrap_or_else(|| "NULL".to_owned()));
        }
        list.push(format!("({})", fields.join(", ")));
    }
    if list.is_empty() {
        return Ok(vec![None; rows.len()]);
    }
    let mut aliases = vec!["o".to_owned(), "x".to_owned()];
    aliases.extend((1..=others.len()).map(|n| format!("y{n}")));
    let value = using.map_or_else(|| quote_ident(&own), str::to_owned);
    let mut last_error = String::new();
    for typed in [true, false] {
        let cast = |v: &str, t: &str| {
            if typed {
                format!("{v}::{t}")
            } else {
                v.to_owned()
            }
        };
        let mut projected = vec![format!("{} AS {}", cast("v.x", ty), quote_ident(&own))];
        for (n, (ci, name)) in others.iter().enumerate() {
            projected.push(format!(
                "{} AS {}",
                cast(&format!("v.y{}", n + 1), &info.columns[*ci].ty),
                quote_ident(name)
            ));
        }
        // Not in pg_catalog only: the USING resolves as it did when the
        // migration ran it.
        let sql = format!(
            "BEGIN; SELECT v.o::text, (SELECT ({value})::{to} FROM (SELECT {projected}) AS romulus_row)::text \
             FROM (VALUES {list}) AS v({aliases}); ROLLBACK",
            projected = projected.join(", "),
            list = list.join(", "),
            aliases = aliases.join(", ")
        );
        match session.run(&sql, limits.read(), Rows::READ).await {
            Ok(answer) => {
                let mut out = vec![None; rows.len()];
                for row in answer.rows {
                    let mut row = row.into_iter();
                    let (Some(Some(o)), value) = (row.next(), row.next().flatten()) else {
                        continue;
                    };
                    if let Ok(i) = o.parse::<usize>()
                        && i < out.len()
                    {
                        out[i] = Some(value.map_or(Cell::Null, Cell::Text));
                    }
                }
                return Ok(out);
            }
            Err(err) => {
                let _ = session.run("ROLLBACK", limits.read(), Rows::Skip).await;
                last_error = err.to_string();
            }
        }
    }
    Err(last_error)
}

/// Each value cast back to `ty` (the column's type before): the texts, or
/// why not.
async fn cast_back<S: Session>(
    session: &mut S,
    values: &[(usize, String)],
    ty: &str,
    limits: Limits,
) -> Result<BTreeMap<usize, String>, String> {
    if values.is_empty() {
        return Ok(BTreeMap::new());
    }
    let list: Vec<String> = values
        .iter()
        .map(|(i, v)| format!("({i}, {})", quote_literal(v)))
        .collect();
    let sql = format!(
        "SELECT v.o::text, (v.x::{ty})::text FROM (VALUES {}) AS v(o, x)",
        list.join(", ")
    );
    let rows = read(session, &sql, limits.read())
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut r = r.into_iter();
            Some((r.next()??.parse().ok()?, r.next()??))
        })
        .collect())
}

fn example(row: &Row, before: &Cell, after: &Cell) -> Example {
    Example {
        key: row.key_label(),
        before: before.shown(),
        after: after.shown(),
    }
}

fn label_of(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

/// A value after enum labels renamed (in order): the label, or each
/// element of a one-dimensional array of them, as Postgres prints it.
pub fn relabel(text: &str, is_array: bool, labels: &[(&str, &str)]) -> String {
    let map = |value: String| {
        labels.iter().fold(value, |value, (from, to)| {
            if value == *from {
                (*to).to_owned()
            } else {
                value
            }
        })
    };
    if !is_array {
        return map(text.to_owned());
    }
    let Some(inner) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}')) else {
        return text.to_owned();
    };
    let mut items: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut in_quotes) = (false, false);
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' if !in_quotes => {
                items.push((std::mem::take(&mut current), quoted));
                quoted = false;
            }
            // More than one dimension: left as it is.
            '{' | '}' if !in_quotes => return text.to_owned(),
            _ => current.push(c),
        }
    }
    if !inner.is_empty() {
        items.push((current, quoted));
    }
    let printed: Vec<String> = items
        .into_iter()
        .map(|(value, quoted)| {
            if !quoted && value == "NULL" {
                return value;
            }
            let value = map(value);
            let plain = !value.is_empty()
                && !value.eq_ignore_ascii_case("null")
                && !value
                    .chars()
                    .any(|c| matches!(c, '{' | '}' | ',' | '"' | '\\') || c.is_whitespace());
            if plain {
                value
            } else {
                format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect();
    format!("{{{}}}", printed.join(","))
}

/// What the up migration did to the rows: the row counts and the sampled
/// values, and what of the data is lost if nothing brings it back.
pub struct AfterUp {
    pub counts: CheckReport,
    pub values: CheckReport,
    /// What the up migration loses, should no down migration run.
    pub losses: Vec<DataLoss>,
}

/// A note for a column the up migration dropped, and the loss of the
/// sampled values it held.
fn dropped_column(
    cx: &Context<'_>,
    info: &TableInfo,
    t: usize,
    plan: &Planned<'_>,
    ci: usize,
    rows: &[Row],
) -> (Note, Option<DataLoss>) {
    let cause = pick(
        cx.of(Some(Direction::Up))
            .filter(|l| explains_values(&l.effect, t, plan.column, None)),
        Shape::ColumnDropped,
    );
    let status = status_of(cause.as_ref());
    let held = rows.iter().filter(|r| r.values[ci] != Cell::Null).count();
    let message = match &cause {
        Some(c) => format!("Dropped by {}.", describe(c)),
        None => "Dropped, and no statement says so.".to_owned(),
    };
    let note = Note {
        kind: NoteKind::Dropped,
        column: Some(plan.info.name.clone()),
        from: None,
        rows: held,
        status,
        message: message.clone(),
        statement: cause.clone(),
        examples: Vec::new(),
    };
    let loss = (held > 0).then(|| {
        lost(
            DataLoss {
                table: info.label(),
                column: Some(plan.info.name.clone()),
                kind: LossKind::ColumnDropped,
                rows: held,
                sampled: rows.len(),
                count_before: None,
                count_after: None,
                examples: rows
                    .iter()
                    .filter(|r| r.values[ci] != Cell::Null)
                    .take(3)
                    .map(|r| example(r, &r.values[ci], &Cell::Null))
                    .collect(),
                expected: false,
                status,
                message: format!(
                    "{}.{}: its values are gone. {message}",
                    info.label(),
                    plan.info.name
                ),
                cause,
            },
            status,
        )
    });
    (note, loss)
}

/// How a changed column of a table without a key is compared after the
/// up migration, when its values are not simply the same.
enum Back {
    /// Converted only: its values cast back to the type they had.
    Cast,
    /// Enum labels renamed only: mapped back to the labels they had.
    Labels(Vec<(String, String)>),
    /// Changed otherwise (an `UPDATE`, code it cannot read): it cannot be
    /// followed without a key.
    None,
}

/// A column the up migration changes, in a table without a key.
struct Changed {
    /// Its index in [`TableInfo::columns`].
    at: usize,
    now_name: String,
    cause: Option<StatementRef>,
    back: Back,
}

/// What each label renamed (in order) was at first: `(now, then)`, the
/// labels the renames leave as they were left out. The labels there at
/// first are those first named as a rename's `from` (a `to` is new).
fn labels_back(labels: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut seen: Vec<&str> = Vec::new();
    let mut first: Vec<&str> = Vec::new();
    for (from, to) in labels {
        if !seen.contains(from) {
            first.push(from);
        }
        seen.extend([*from, *to]);
    }
    first
        .into_iter()
        .filter_map(|then| {
            let now = labels
                .iter()
                .fold(then, |v, (from, to)| if v == *from { to } else { v });
            (now != then).then(|| (now.to_owned(), then.to_owned()))
        })
        .collect()
}

/// The expression giving a column's values as they read before the up
/// migration: cast back to `ty`, or its labels mapped back (each element
/// of a one-dimensional array; others as they are).
fn back_expression(column: &str, back: &Back, ty: &str, is_array: bool) -> Option<String> {
    let c = quote_ident(column);
    match back {
        Back::Cast => Some(format!("{c}::{ty}")),
        Back::Labels(map) => {
            let case = |v: &str| {
                let arms = map.iter().fold(String::new(), |mut arms, (now, then)| {
                    use std::fmt::Write as _;
                    let _ = write!(
                        arms,
                        " WHEN {} THEN {}",
                        quote_literal(now),
                        quote_literal(then)
                    );
                    arms
                });
                format!("CASE {v}::text{arms} ELSE {v}::text END")
            };
            Some(if is_array {
                format!(
                    "CASE WHEN {c} IS NULL THEN NULL \
                     WHEN pg_catalog.array_ndims({c}) IS DISTINCT FROM 1 THEN {c}::text \
                     ELSE (SELECT pg_catalog.array_agg({} ORDER BY u.o) \
                           FROM pg_catalog.unnest({c}) WITH ORDINALITY AS u(e, o))::text END",
                    case("u.e")
                )
            } else {
                case(&c)
            })
        }
        Back::None => None,
    }
}

/// What cannot be verified, explained by `cause`: a statement saying it
/// changes the data stands for it (a warning); anything else (planned
/// without the flag, written without saying so, unreadable, nothing)
/// needs a person.
fn unverifiable(cause: Option<&StatementRef>) -> Status {
    match cause.map(|c| c.flag) {
        Some(Flag::Flagged | Flag::Written | Flag::Moved) => Status::Warning,
        _ => Status::NeedsAcknowledgement,
    }
}

/// A table without a key not read as a whole (`err`, or too large before
/// the migration): not verified, which a person acknowledges when the
/// migration changes it.
fn unread(cx: &Context<'_>, info: &TableInfo, err: Option<&SessionError>, changes: bool) -> Note {
    let message = match err {
        Some(err) if !err.timed_out() => {
            format!("Not verified: its columns could not be read as wholes ({err}).")
        }
        _ => "Not verified (too large): reading it as a whole takes longer than a statement may."
            .to_owned(),
    };
    Note {
        kind: NoteKind::NotVerified,
        column: None,
        from: None,
        rows: 0,
        status: if changes || cx.changes(info) {
            Status::NeedsAcknowledgement
        } else {
            Status::Warning
        },
        message,
        statement: None,
        examples: Vec::new(),
    }
}

/// A table without a key after the up migration: every column compared as
/// a whole (count and hash of every value). A changed column whose values
/// are not the same is compared cast back to its type, or with its renamed
/// labels mapped back: what does not come back was rounded or cut.
/// What cannot be compared is not verified: a warning when a statement
/// says it changes the data, else for a person to acknowledge (never a
/// clean pass).
#[allow(clippy::too_many_lines, reason = "each column, in order")]
async fn keyless_after_up<S: Session>(
    session: &mut S,
    cx: &Context<'_>,
    info: &TableInfo,
    t: usize,
    now: (&str, &str, Scope),
    counts_kept: bool,
    losses: &mut Vec<DataLoss>,
) -> Option<TableCheck> {
    let (schema, name, forced) = now;
    let id = (info.schema.clone(), info.name.clone());
    let before = cx.before.counts.get(&id).copied();
    let mut check = TableCheck {
        table: label_of(schema, name),
        from: (schema != info.schema || name != info.name).then(|| info.label()),
        before,
        after: None,
        sampled: 0,
        status: Status::Passed,
        notes: Vec::new(),
    };
    let finish = |mut check: TableCheck| {
        check.status = check
            .notes
            .iter()
            .fold(Status::Passed, |s, n| s.and(n.status));
        Some(check)
    };
    let Some(wholes) = cx.before.aggregates.get(&id) else {
        if !cx.before.too_large.contains(&id) {
            return None;
        }
        check.notes.push(unread(cx, info, None, false));
        return finish(check);
    };
    if !counts_kept {
        check.notes.push(Note {
            kind: NoteKind::NotVerified,
            column: None,
            from: None,
            rows: 0,
            status: Status::Warning,
            message: "No key, and its row count changed: its values are not compared.".to_owned(),
            statement: None,
            examples: Vec::new(),
        });
        return finish(check);
    }
    let mut compared: Vec<(usize, String)> = Vec::new();
    let mut changed: Vec<Changed> = Vec::new();
    for (ci, column) in info.columns.iter().enumerate() {
        let c = cx.column_index(t, &column.name);
        let alive = c.and_then(|c| {
            let walk = &cx.up.tables[t].columns[c];
            walk.alive.then(|| walk.name.clone())
        });
        let Some(now_name) = alive else {
            let cause = pick(
                cx.of(Some(Direction::Up))
                    .filter(|l| explains_values(&l.effect, t, c, None)),
                Shape::ColumnDropped,
            );
            let status = status_of(cause.as_ref());
            let held = count_of(wholes[ci].non_null);
            check.notes.push(Note {
                kind: NoteKind::Dropped,
                column: Some(column.name.clone()),
                from: None,
                rows: held,
                status,
                message: cause.as_ref().map_or_else(
                    || "Dropped, and no statement says so.".to_owned(),
                    |c| format!("Dropped by {}.", describe(c)),
                ),
                statement: cause.clone(),
                examples: Vec::new(),
            });
            if held > 0 {
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: Some(column.name.clone()),
                        kind: LossKind::ColumnDropped,
                        rows: held,
                        sampled: count_of(before.unwrap_or(0)),
                        count_before: before,
                        count_after: None,
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!(
                            "{}.{}: its {} values are gone.",
                            info.label(),
                            column.name,
                            thousands(wholes[ci].non_null)
                        ),
                        cause,
                    },
                    status,
                ));
            }
            continue;
        };
        let enum_type = cx.enum_index(column.enum_type.as_ref());
        let changers: Vec<&Located> = cx
            .of(Some(Direction::Up))
            .filter(|l| explains_values(&l.effect, t, c, enum_type))
            .collect();
        if changers.is_empty() {
            compared.push((ci, now_name));
            continue;
        }
        let converted = changers
            .iter()
            .all(|l| matches!(l.effect, Effect::ColumnConverted { .. }));
        let labels: Vec<(&str, &str)> = changers
            .iter()
            .filter_map(|l| match &l.effect {
                Effect::LabelRenamed { from, to, .. } => Some((from.as_str(), to.as_str())),
                _ => None,
            })
            .collect();
        let back = if converted {
            Back::Cast
        } else if labels.len() == changers.len() {
            Back::Labels(labels_back(&labels))
        } else {
            Back::None
        };
        changed.push(Changed {
            at: ci,
            now_name,
            cause: pick(changers, Shape::Values),
            back,
        });
    }
    // Every column as a whole: the same as before?
    let names: Vec<String> = compared
        .iter()
        .map(|(_, n)| n.clone())
        .chain(changed.iter().map(|c| c.now_name.clone()))
        .collect();
    let after = if names.is_empty() {
        Vec::new()
    } else {
        match snapshot::aggregates(session, schema, name, &names, forced, cx.limits).await {
            Ok(after) => after,
            Err(err) => {
                check
                    .notes
                    .push(unread(cx, info, Some(&err), !changed.is_empty()));
                return finish(check);
            }
        }
    };
    let (unchanged, moved) = after.split_at(compared.len());
    let mut same = 0usize;
    for ((ci, now_name), whole) in compared.iter().zip(unchanged) {
        if *whole == wholes[*ci] {
            same += 1;
        } else {
            check.notes.push(Note {
                kind: NoteKind::Unexplained,
                column: Some(now_name.clone()),
                from: None,
                rows: 0,
                status: Status::Failed,
                message:
                    "Its values differ (compared as a whole: no key), and no statement explains it."
                        .to_owned(),
                statement: None,
                examples: Vec::new(),
            });
        }
    }
    // Changed columns: the same anyway, or compared brought back.
    let mut to_bring: Vec<(&Changed, String)> = Vec::new();
    for (column, whole) in changed.iter().zip(moved) {
        if *whole == wholes[column.at] {
            same += 1;
            continue;
        }
        let info_column = &info.columns[column.at];
        match back_expression(
            &column.now_name,
            &column.back,
            &info_column.ty,
            info_column.is_array,
        ) {
            Some(expression) => to_bring.push((column, expression)),
            None => check.notes.push(Note {
                kind: NoteKind::NotVerified,
                column: Some(column.now_name.clone()),
                from: None,
                rows: 0,
                status: unverifiable(column.cause.as_ref()),
                message: format!(
                    "Changed by {}: without a key, its values cannot be followed.",
                    column
                        .cause
                        .as_ref()
                        .map_or_else(|| "a statement".to_owned(), describe)
                ),
                statement: column.cause.clone(),
                examples: Vec::new(),
            }),
        }
    }
    if !to_bring.is_empty() {
        let expressions: Vec<String> = to_bring.iter().map(|(_, e)| e.clone()).collect();
        let brought =
            snapshot::aggregates_of(session, schema, name, &expressions, forced, cx.limits).await;
        for (i, (column, _)) in to_bring.iter().enumerate() {
            let original = &info.columns[column.at];
            let by = column
                .cause
                .as_ref()
                .map_or_else(|| "a statement".to_owned(), describe);
            let Ok(brought) = &brought else {
                let err = brought.as_ref().err();
                check.notes.push(Note {
                    kind: NoteKind::NotVerified,
                    column: Some(column.now_name.clone()),
                    from: None,
                    rows: 0,
                    status: unverifiable(column.cause.as_ref()),
                    message: match err {
                        Some(err) if err.timed_out() => format!(
                            "Changed by {by}: not verified (too large) as a whole without a key."
                        ),
                        Some(err) => format!(
                            "Changed by {by}: without a key, its values could not be compared as they were ({err})."
                        ),
                        None => format!("Changed by {by}: not verified."),
                    },
                    statement: column.cause.clone(),
                    examples: Vec::new(),
                });
                continue;
            };
            if brought.get(i) == Some(&wholes[column.at]) {
                check.notes.push(Note {
                    kind: if matches!(column.back, Back::Cast) {
                        NoteKind::Converted
                    } else {
                        NoteKind::Changed
                    },
                    column: Some(column.now_name.clone()),
                    from: None,
                    rows: 0,
                    status: Status::Passed,
                    message: if matches!(column.back, Back::Cast) {
                        format!(
                            "Converted by {by}: every value converts back to {} (compared as a whole: no key).",
                            original.ty
                        )
                    } else {
                        format!(
                            "Its labels renamed by {by}: every value maps back (compared as a whole: no key)."
                        )
                    },
                    statement: column.cause.clone(),
                    examples: Vec::new(),
                });
                continue;
            }
            if matches!(column.back, Back::Labels(_)) {
                // Beyond the labels, or arrays it cannot map: a person looks.
                check.notes.push(Note {
                    kind: NoteKind::NotVerified,
                    column: Some(column.now_name.clone()),
                    from: None,
                    rows: 0,
                    status: Status::NeedsAcknowledgement,
                    message: format!(
                        "Its labels renamed by {by}, but its values do not all map back (compared as a whole: no key)."
                    ),
                    statement: column.cause.clone(),
                    examples: Vec::new(),
                });
                continue;
            }
            // What does not convert back was rounded or cut.
            let status = status_of(column.cause.as_ref());
            let message = format!(
                "Not every value converts back to {} (compared as a whole: no key): {by} rounds or cuts them.",
                original.ty
            );
            check.notes.push(Note {
                kind: NoteKind::Rounded,
                column: Some(column.now_name.clone()),
                from: None,
                rows: 0,
                status,
                message: message.clone(),
                statement: column.cause.clone(),
                examples: Vec::new(),
            });
            losses.push(lost(
                DataLoss {
                    table: info.label(),
                    column: Some(original.name.clone()),
                    kind: LossKind::ValuesChanged,
                    rows: count_of(wholes[column.at].non_null),
                    sampled: count_of(before.unwrap_or(0)),
                    count_before: before,
                    count_after: before,
                    examples: Vec::new(),
                    expected: false,
                    status,
                    message: format!("{}.{}: {message}", info.label(), original.name),
                    cause: column.cause.clone(),
                },
                status,
            ));
        }
    }
    if same > 0 {
        check.notes.push(Note {
            kind: NoteKind::NotCompared,
            column: None,
            from: None,
            rows: 0,
            status: Status::Passed,
            message: format!(
                "No key: {} compared as wholes (count and hash of every value), the same.",
                plural(same, "column", "columns")
            ),
            statement: None,
            examples: Vec::new(),
        });
    }
    finish(check)
}

#[allow(clippy::too_many_lines, reason = "each table, each column, in order")]
pub async fn after_up<S: Session>(session: &mut S, cx: &Context<'_>, target: &Schema) -> AfterUp {
    let target_tables = snapshot::tables(target);
    // How the table reads now (forced, partitioned: as the target has it).
    let forced = |schema: &str, name: &str| {
        target_tables
            .iter()
            .find(|t| t.schema == schema && t.name == name)
            .map(Scope::of)
            .unwrap_or_default()
    };
    let mut count_tables = Vec::new();
    let mut value_tables = Vec::new();
    let mut losses = Vec::new();
    let mut compared_rows = 0usize;
    let mut kept_rows = 0usize;
    for info in cx.tables {
        let id = (info.schema.clone(), info.name.clone());
        let before = cx.before.counts.get(&id).copied();
        let Some(t) = cx.table_index(info) else {
            continue;
        };
        let family = cx.up.family_ever(t);
        let walk_table = &cx.up.tables[t];
        let (schema, name) = (walk_table.schema.clone(), walk_table.name.clone());
        let from = (schema != info.schema || name != info.name).then(|| info.label());
        if !walk_table.alive {
            let cause = pick(
                cx.of(Some(Direction::Up))
                    .filter(|l| explains_rows(&l.effect, &family, Shape::RowsGone)),
                Shape::RowsGone,
            );
            let status = status_of(cause.as_ref());
            let message = match &cause {
                Some(c) => format!("Dropped by {}.", describe(c)),
                None => "Dropped, and no statement says so.".to_owned(),
            };
            let sampled = cx.before.samples.get(&id).map_or(0, Vec::len);
            count_tables.push(TableCheck {
                table: info.label(),
                from: None,
                before,
                after: None,
                sampled,
                status,
                notes: vec![Note {
                    kind: NoteKind::Dropped,
                    column: None,
                    from: None,
                    rows: sampled,
                    status,
                    message: message.clone(),
                    statement: cause.clone(),
                    examples: Vec::new(),
                }],
            });
            if before.unwrap_or(0) > 0 {
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: None,
                        kind: LossKind::RowsDeleted,
                        rows: sampled,
                        sampled,
                        count_before: before,
                        count_after: Some(0),
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!(
                            "{}: {} rows are gone. {message}",
                            info.label(),
                            thousands(before.unwrap_or(0))
                        ),
                        cause,
                    },
                    status,
                ));
            }
            continue;
        }
        // Counts: more rows, or fewer, are explained or not.
        let after = snapshot::count(session, &schema, &name, forced(&schema, &name), cx.limits)
            .await
            .ok();
        let mut count_check = TableCheck {
            table: label_of(&schema, &name),
            from: from.clone(),
            before,
            after,
            sampled: 0,
            status: Status::Passed,
            notes: Vec::new(),
        };
        if after != before {
            let shape = if after > before {
                Shape::RowsAdded
            } else {
                Shape::RowsGone
            };
            let cause = pick(
                cx.of(Some(Direction::Up))
                    .filter(|l| explains_rows(&l.effect, &family, shape)),
                shape,
            );
            let status = if after.is_none() {
                Status::Failed
            } else {
                status_of(cause.as_ref())
            };
            let message = match (after, &cause) {
                (None, _) => "Its rows could not be counted.".to_owned(),
                (Some(a), Some(c)) => format!(
                    "{} rows, {} before: {}.",
                    thousands(a),
                    thousands(before.unwrap_or(0)),
                    describe(c)
                ),
                (Some(a), None) => format!(
                    "{} rows, {} before, and no statement explains it.",
                    thousands(a),
                    thousands(before.unwrap_or(0))
                ),
            };
            count_check.status = status;
            count_check.notes.push(Note {
                kind: NoteKind::RowsMissing,
                column: None,
                from: None,
                rows: 0,
                status,
                message: message.clone(),
                statement: cause.clone(),
                examples: Vec::new(),
            });
            // Rows moved out with a detached partition are not lost.
            if let (Some(a), Some(b)) = (after, before)
                && a < b
                && cause.as_ref().is_none_or(|c| c.flag != Flag::Moved)
            {
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: None,
                        kind: LossKind::RowsDeleted,
                        rows: 0,
                        sampled: 0,
                        count_before: Some(b),
                        count_after: Some(a),
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!("{}: {message}", info.label()),
                        cause,
                    },
                    status,
                ));
            }
        }
        count_tables.push(count_check);

        // Values.
        if info.key.is_empty() {
            let now = (schema.as_str(), name.as_str(), forced(&schema, &name));
            if let Some(check) =
                keyless_after_up(session, cx, info, t, now, after == before, &mut losses).await
            {
                value_tables.push(check);
            }
            continue;
        }
        let Some(rows) = cx.before.samples.get(&id).filter(|r| !r.is_empty()) else {
            continue;
        };
        let mut table_check = TableCheck {
            table: label_of(&schema, &name),
            from: from.clone(),
            before,
            after,
            sampled: rows.len(),
            status: Status::Passed,
            notes: Vec::new(),
        };
        let mut planned: Vec<Planned<'_>> = Vec::new();
        for column in &info.columns {
            let c = cx.column_index(t, &column.name);
            let now = c.and_then(|c| {
                let walk = &cx.up.tables[t].columns[c];
                walk.alive.then(|| walk.name.clone())
            });
            let conversions = cx
                .of(Some(Direction::Up))
                .filter_map(|l| match &l.effect {
                    Effect::ColumnConverted {
                        table,
                        column,
                        conversion,
                    } if *table == t && Some(*column) == c => Some((l, conversion)),
                    _ => None,
                })
                .collect();
            let enum_index = cx.enum_index(column.enum_type.as_ref());
            let labels = cx
                .of(Some(Direction::Up))
                .filter_map(|l| match &l.effect {
                    Effect::LabelRenamed {
                        enum_type,
                        from,
                        to,
                    } if Some(*enum_type) == enum_index => Some((from.as_str(), to.as_str())),
                    _ => None,
                })
                .collect();
            planned.push(Planned {
                info: column,
                column: c,
                now,
                conversions,
                labels,
            });
        }
        let walk_columns: Vec<Option<usize>> = planned.iter().map(|p| p.column).collect();
        // The values each row should hold now, column by column.
        let mut expected: Vec<Vec<Option<Cell>>> = vec![Vec::new(); info.columns.len()];
        let mut not_compared: BTreeMap<usize, String> = BTreeMap::new();
        for (ci, plan) in planned.iter().enumerate() {
            if plan.now.is_none() {
                expected[ci] = vec![None; rows.len()];
                continue;
            }
            if plan.conversions.is_empty() {
                expected[ci] = rows
                    .iter()
                    .map(|r| {
                        Some(match &r.values[ci] {
                            Cell::Text(text) if !plan.labels.is_empty() => {
                                Cell::Text(relabel(text, plan.info.is_array, &plan.labels))
                            }
                            cell => cell.clone(),
                        })
                    })
                    .collect();
            } else {
                match convert(
                    session,
                    info,
                    rows,
                    ci,
                    &walk_columns,
                    &plan.conversions,
                    cx.up,
                    cx.limits,
                )
                .await
                {
                    Ok(values) => expected[ci] = values,
                    // The cast cannot be recomputed: the values' text,
                    // then, and a difference must be explained.
                    Err(error) => {
                        expected[ci] = rows.iter().map(|r| Some(r.values[ci].clone())).collect();
                        not_compared.insert(ci, error);
                    }
                }
            }
        }
        // Find the rows again by a key whose columns are all still there
        // (the primary key, else another unique key), as it reads now.
        let found_by: Option<(Vec<usize>, Vec<String>)> = info.keys.iter().find_map(|key| {
            let at: Vec<usize> = key
                .iter()
                .map(|k| info.columns.iter().position(|c| c.name == *k))
                .collect::<Option<_>>()?;
            let now: Vec<String> = at
                .iter()
                .map(|&ci| planned[ci].now.clone())
                .collect::<Option<_>>()?;
            Some((at, now))
        });
        let Some((key_at, key_now)) = found_by else {
            // No key left: what is dropped is still lost; the rest cannot
            // be found again.
            table_check.notes.push(Note {
                kind: NoteKind::NotVerified,
                column: None,
                from: None,
                rows: rows.len(),
                status: Status::Warning,
                message: "Every key of it loses a column: its sampled rows cannot be found again, only its dropped columns are checked.".to_owned(),
                statement: None,
                examples: Vec::new(),
            });
            for (ci, plan) in planned.iter().enumerate() {
                if plan.now.is_none() {
                    let (note, loss) = dropped_column(cx, info, t, plan, ci, rows);
                    table_check.notes.push(note);
                    losses.extend(loss);
                }
            }
            table_check.status = table_check
                .notes
                .iter()
                .fold(Status::Passed, |s, n| s.and(n.status));
            value_tables.push(table_check);
            continue;
        };
        let wanted: Vec<Option<Vec<String>>> = (0..rows.len())
            .map(|ri| {
                key_at
                    .iter()
                    .map(|&at| match &expected[at][ri] {
                        Some(Cell::Text(t)) => Some(t.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        let columns_now: Vec<String> = planned.iter().filter_map(|p| p.now.clone()).collect();
        let reading = snapshot::Reading {
            schema: &schema,
            name: &name,
            key: &key_now,
            columns: &columns_now,
            scope: forced(&schema, &name),
        };
        let found = snapshot::lookup(
            session,
            reading,
            &wanted.iter().flatten().cloned().collect::<Vec<_>>(),
            cx.limits,
        )
        .await;
        let found = match found {
            Ok(found) => found,
            Err(err) => {
                table_check.status = Status::Failed;
                table_check.notes.push(Note {
                    kind: NoteKind::NotCompared,
                    column: None,
                    from: None,
                    rows: rows.len(),
                    status: Status::Failed,
                    message: format!("Its rows could not be read: {err}"),
                    statement: None,
                    examples: Vec::new(),
                });
                value_tables.push(table_check);
                continue;
            }
        };
        // Per row, the row now (values in the order of `columns_now`).
        let now_rows: Vec<Option<&Row>> = wanted
            .iter()
            .map(|k| k.as_ref().and_then(|k| found.get(k)))
            .collect();
        let missing: Vec<usize> = (0..rows.len()).filter(|&i| now_rows[i].is_none()).collect();
        compared_rows += rows.len();
        kept_rows += rows.len() - missing.len();
        if !missing.is_empty() {
            let cause = pick(
                cx.of(Some(Direction::Up))
                    .filter(|l| explains_rows(&l.effect, &family, Shape::RowsGone)),
                Shape::RowsGone,
            );
            let status = status_of(cause.as_ref());
            let message = match &cause {
                Some(c) => format!(
                    "{} of the {} sampled rows are gone: {}.",
                    thousands(i64::try_from(missing.len()).unwrap_or(0)),
                    rows.len(),
                    describe(c)
                ),
                None => format!(
                    "{} of the {} sampled rows are gone, and no statement explains it.",
                    missing.len(),
                    rows.len()
                ),
            };
            table_check.notes.push(Note {
                kind: NoteKind::RowsMissing,
                column: None,
                from: None,
                rows: missing.len(),
                status,
                message,
                statement: cause,
                examples: missing
                    .iter()
                    .take(3)
                    .map(|&i| Example {
                        key: rows[i].key_label(),
                        before: None,
                        after: None,
                    })
                    .collect(),
            });
        }
        let mut position_now = 0usize;
        for (ci, plan) in planned.iter().enumerate() {
            let Some(now_name) = &plan.now else {
                let (note, loss) = dropped_column(cx, info, t, plan, ci, rows);
                table_check.notes.push(note);
                losses.extend(loss);
                continue;
            };
            let at_now = position_now;
            position_now += 1;
            let mut same = 0usize;
            let mut differ: Vec<usize> = Vec::new();
            let mut unknown = 0usize;
            for ri in 0..rows.len() {
                let Some(now) = now_rows[ri] else { continue };
                let actual = &now.values[at_now];
                match &expected[ci][ri] {
                    None => unknown += 1,
                    Some(Cell::Hash(h)) => match actual {
                        Cell::Hash(a) if a == h => same += 1,
                        _ => differ.push(ri),
                    },
                    Some(want) if want == actual => same += 1,
                    Some(_) => differ.push(ri),
                }
            }
            let renamed = (*now_name != plan.info.name).then(|| plan.info.name.clone());
            let examples_of = |list: &[usize]| -> Vec<Example> {
                list.iter()
                    .take(3)
                    .filter_map(|&ri| {
                        Some(example(
                            &rows[ri],
                            &rows[ri].values[ci],
                            &now_rows[ri]?.values[at_now],
                        ))
                    })
                    .collect()
            };
            if let Some(error) = not_compared.get(&ci) {
                if differ.is_empty() {
                    table_check.notes.push(Note {
                        kind: NoteKind::Converted,
                        column: Some(now_name.clone()),
                        from: renamed,
                        rows: same,
                        status: Status::Passed,
                        message: format!("Converted: its {same} sampled values read as before."),
                        statement: plan
                            .conversions
                            .last()
                            .map(|(l, _)| l.reference(Shape::Values)),
                        examples: Vec::new(),
                    });
                    continue;
                }
                // What it should give cannot be recomputed: a difference
                // passes only where a statement says it changes values.
                let cause = pick(
                    cx.of(Some(Direction::Up)).filter(|l| {
                        explains_values(
                            &l.effect,
                            t,
                            plan.column,
                            cx.enum_index(plan.info.enum_type.as_ref()),
                        )
                    }),
                    Shape::Values,
                );
                let status = status_of(cause.as_ref());
                let message = format!(
                    "Converted, and {} of {} sampled values read otherwise; what the conversion gives could not be recomputed ({error}){}",
                    differ.len(),
                    rows.len(),
                    cause.as_ref().map_or_else(
                        || ", and no statement explains it.".to_owned(),
                        |c| format!(": {}.", describe(c))
                    )
                );
                let examples = examples_of(&differ);
                table_check.notes.push(Note {
                    kind: NoteKind::NotCompared,
                    column: Some(now_name.clone()),
                    from: renamed,
                    rows: differ.len(),
                    status,
                    message: message.clone(),
                    statement: cause.clone(),
                    examples: examples.clone(),
                });
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: Some(plan.info.name.clone()),
                        kind: LossKind::ValuesChanged,
                        rows: differ.len(),
                        sampled: rows.len(),
                        count_before: None,
                        count_after: None,
                        examples,
                        expected: false,
                        status,
                        message: format!("{}.{}: {message}", info.label(), plan.info.name),
                        cause,
                    },
                    status,
                ));
                continue;
            }
            if !differ.is_empty() {
                let cause = pick(
                    cx.of(Some(Direction::Up)).filter(|l| {
                        !matches!(l.effect, Effect::ColumnConverted { .. })
                            && explains_values(
                                &l.effect,
                                t,
                                plan.column,
                                cx.enum_index(plan.info.enum_type.as_ref()),
                            )
                    }),
                    Shape::Values,
                );
                let (kind, status, message) = match &cause {
                    Some(c) => (
                        NoteKind::Changed,
                        status_of(Some(c)),
                        format!(
                            "{} of {} sampled values changed: {}.",
                            differ.len(),
                            rows.len(),
                            describe(c)
                        ),
                    ),
                    None if !plan.conversions.is_empty() => (
                        NoteKind::Unexplained,
                        Status::Failed,
                        format!(
                            "{} of {} sampled values are not what the conversion gives.",
                            differ.len(),
                            rows.len()
                        ),
                    ),
                    None => (
                        NoteKind::Unexplained,
                        Status::Failed,
                        format!(
                            "{} of {} sampled values changed, and no statement explains it.",
                            differ.len(),
                            rows.len()
                        ),
                    ),
                };
                table_check.notes.push(Note {
                    kind,
                    column: Some(now_name.clone()),
                    from: renamed.clone(),
                    rows: differ.len(),
                    status,
                    message,
                    statement: cause,
                    examples: examples_of(&differ),
                });
            }
            if !plan.conversions.is_empty() {
                let (located, conversion) = plan.conversions[plan.conversions.len() - 1];
                if differ.is_empty() {
                    table_check.notes.push(Note {
                        kind: NoteKind::Converted,
                        column: Some(now_name.clone()),
                        from: renamed.clone(),
                        rows: same,
                        status: Status::Passed,
                        message: format!(
                            "Converted to {}: {same} sampled values as the migration's cast gives{}.",
                            conversion.resolved(cx.up).0,
                            if unknown > 0 {
                                format!(" ({unknown} too long to recompute)")
                            } else {
                                String::new()
                            }
                        ),
                        statement: Some(located.reference(Shape::Values)),
                        examples: Vec::new(),
                    });
                }
                // What does not convert back was rounded or cut.
                let values: Vec<(usize, String)> = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(ri, _)| {
                        let now = now_rows[ri]?;
                        now.values[at_now].text().map(|t| (ri, t.to_owned()))
                    })
                    .collect();
                match cast_back(session, &values, &plan.info.ty, cx.limits).await {
                    Ok(back) => {
                        let rounded: Vec<usize> = back
                            .iter()
                            .filter(|(ri, text)| {
                                rows[**ri].values[ci].text() != Some(text.as_str())
                            })
                            .map(|(ri, _)| *ri)
                            .collect();
                        if !rounded.is_empty() {
                            let cause = located.reference(Shape::Values);
                            let status = status_of(Some(&cause));
                            let message = format!(
                                "{} of {} sampled values do not convert back to {}: {} rounds or cuts them.",
                                rounded.len(),
                                rows.len(),
                                plan.info.ty,
                                describe(&cause)
                            );
                            let examples = examples_of(&rounded);
                            table_check.notes.push(Note {
                                kind: NoteKind::Rounded,
                                column: Some(now_name.clone()),
                                from: renamed.clone(),
                                rows: rounded.len(),
                                status,
                                message: message.clone(),
                                statement: Some(cause.clone()),
                                examples: examples.clone(),
                            });
                            losses.push(lost(
                                DataLoss {
                                    table: info.label(),
                                    column: Some(plan.info.name.clone()),
                                    kind: LossKind::ValuesChanged,
                                    rows: rounded.len(),
                                    sampled: rows.len(),
                                    count_before: None,
                                    count_after: None,
                                    examples,
                                    expected: false,
                                    status,
                                    message: format!(
                                        "{}.{}: {message}",
                                        info.label(),
                                        plan.info.name
                                    ),
                                    cause: Some(cause),
                                },
                                status,
                            ));
                        }
                    }
                    // Said, not skipped: whether it rounds is unknown.
                    Err(err) if !values.is_empty() => {
                        table_check.notes.push(Note {
                            kind: NoteKind::NotCompared,
                            column: Some(now_name.clone()),
                            from: renamed.clone(),
                            rows: values.len(),
                            status: Status::Warning,
                            message: format!(
                                "Whether the conversion rounds could not be checked: its values do not cast back to {} ({err}).",
                                plan.info.ty
                            ),
                            statement: Some(located.reference(Shape::Values)),
                            examples: Vec::new(),
                        });
                    }
                    Err(_) => {}
                }
            } else if let Some(from) = &renamed
                && differ.is_empty()
            {
                table_check.notes.push(Note {
                    kind: NoteKind::Renamed,
                    column: Some(now_name.clone()),
                    from: Some(from.clone()),
                    rows: same,
                    status: Status::Passed,
                    message: format!("Renamed from {from}: its {same} sampled values followed."),
                    statement: None,
                    examples: Vec::new(),
                });
            }
        }
        table_check.status = table_check
            .notes
            .iter()
            .fold(Status::Passed, |s, n| s.and(n.status));
        if !table_check.notes.is_empty() || from.is_some() {
            value_tables.push(table_check);
        }
    }
    let counts = rollup(
        CheckKind::RowCounts,
        count_tables,
        |tables, status| match status {
            Status::Failed => "Some tables' rows changed that no statement explains.".to_owned(),
            Status::NeedsAcknowledgement => {
                "Some tables' rows changed where the migration does not say so.".to_owned()
            }
            Status::Warning => "Row counts changed where statements say they would.".to_owned(),
            _ => format!(
                "Every table kept its rows ({}).",
                plural(tables, "table", "tables")
            ),
        },
        false,
    );
    let values = rollup(
        CheckKind::Values,
        value_tables,
        |_, status| {
            match status {
            Status::Failed => "Some sampled values changed that no statement explains.".to_owned(),
            Status::NeedsAcknowledgement => {
                "Some values changed where the migration does not say so: acknowledge it, or change the migration.".to_owned()
            }
            Status::Warning => format!(
                "{} of {} sampled rows found again; some values changed where statements say they would, or could not be verified.",
                thousands(i64::try_from(kept_rows).unwrap_or(0)),
                thousands(i64::try_from(compared_rows).unwrap_or(0))
            ),
            _ => format!(
                "{} sampled rows kept their values (renames followed, conversions as cast).",
                thousands(i64::try_from(compared_rows).unwrap_or(0))
            ),
        }
        },
        true,
    );
    AfterUp {
        counts,
        values,
        losses,
    }
}

/// A check from its tables: the worst status, and a summary for it.
fn rollup(
    kind: CheckKind,
    mut tables: Vec<TableCheck>,
    summary: impl Fn(usize, Status) -> String,
    only_noted: bool,
) -> CheckReport {
    let status = tables.iter().fold(Status::Passed, |s, t| s.and(t.status));
    let total = tables.len();
    if only_noted {
        tables.retain(|t| !t.notes.is_empty() || t.from.is_some());
    }
    // The ones with something to say first.
    tables.sort_by_key(|t| match t.status {
        Status::Failed => 0,
        Status::NeedsAcknowledgement => 1,
        Status::Warning => 2,
        _ => 3,
    });
    tables.truncate(MAX_TABLES_LISTED);
    CheckReport {
        kind,
        status,
        summary: summary(total, status),
        details: Vec::new(),
        tables,
    }
}

/// What the down migration gave back, and what it did not.
pub struct AfterDown {
    pub counts: CheckReport,
    pub values: CheckReport,
    pub losses: Vec<DataLoss>,
}

/// A table without a key after the down migration: every column compared
/// as a whole with what it was (when its row count is back).
#[allow(clippy::too_many_lines, reason = "each column, in order")]
async fn keyless_after_down<S: Session>(
    session: &mut S,
    cx: &Context<'_>,
    info: &TableInfo,
    losses: &mut Vec<DataLoss>,
) -> Option<TableCheck> {
    let id = (info.schema.clone(), info.name.clone());
    let before = cx.before.counts.get(&id).copied();
    let t = cx.table_index(info);
    let names: Vec<String> = info.columns.iter().map(|c| c.name.clone()).collect();
    let mut check = TableCheck {
        table: info.label(),
        from: None,
        before,
        after: before,
        sampled: 0,
        status: Status::Passed,
        notes: Vec::new(),
    };
    let Some(wholes) = cx.before.aggregates.get(&id) else {
        if !cx.before.too_large.contains(&id) {
            return None;
        }
        check.notes.push(unread(cx, info, None, false));
        check.status = check.notes[0].status;
        return Some(check);
    };
    match snapshot::aggregates(
        session,
        &info.schema,
        &info.name,
        &names,
        Scope::of(info),
        cx.limits,
    )
    .await
    {
        Ok(after) => {
            for (ci, column) in info.columns.iter().enumerate() {
                if after.get(ci) == Some(&wholes[ci]) {
                    continue;
                }
                let c = t.and_then(|t| cx.column_index(t, &column.name));
                let enum_type = cx.enum_index(column.enum_type.as_ref());
                let candidates: Vec<&Located> = t.map_or_else(Vec::new, |t| {
                    cx.of(None)
                        .filter(|l| explains_values(&l.effect, t, c, enum_type))
                        .collect()
                });
                let dropped = candidates
                    .iter()
                    .any(|l| matches!(l.effect, Effect::ColumnDropped { .. }));
                let shape = if dropped {
                    Shape::ColumnDropped
                } else {
                    Shape::Values
                };
                let cause = pick(candidates, shape);
                let status = status_of(cause.as_ref());
                let message = format!(
                    "Not back as it was (compared as a whole: no key){}",
                    cause.as_ref().map_or_else(
                        || ", and no statement explains it.".to_owned(),
                        |c| format!(": {}.", describe(c))
                    )
                );
                check.notes.push(Note {
                    kind: if cause.is_some() {
                        NoteKind::Changed
                    } else {
                        NoteKind::Unexplained
                    },
                    column: Some(column.name.clone()),
                    from: None,
                    rows: 0,
                    status,
                    message: message.clone(),
                    statement: cause.clone(),
                    examples: Vec::new(),
                });
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: Some(column.name.clone()),
                        kind: if dropped {
                            LossKind::ColumnDropped
                        } else {
                            LossKind::ValuesChanged
                        },
                        rows: count_of(wholes[ci].non_null),
                        sampled: count_of(before.unwrap_or(0)),
                        count_before: before,
                        count_after: before,
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!("{}.{}: {message}", info.label(), column.name),
                        cause,
                    },
                    status,
                ));
            }
            if check.notes.is_empty() {
                check.notes.push(Note {
                    kind: NoteKind::NotCompared,
                    column: None,
                    from: None,
                    rows: 0,
                    status: Status::Passed,
                    message: "No key: every column compared as a whole (count and hash of every value), back as it was.".to_owned(),
                    statement: None,
                    examples: Vec::new(),
                });
            }
        }
        Err(err) => check.notes.push(unread(cx, info, Some(&err), false)),
    }
    check.status = check
        .notes
        .iter()
        .fold(Status::Passed, |s, n| s.and(n.status));
    Some(check)
}

#[allow(clippy::too_many_lines, reason = "each table, each column, in order")]
pub async fn after_down<S: Session>(session: &mut S, cx: &Context<'_>) -> AfterDown {
    let mut count_tables = Vec::new();
    let mut value_tables = Vec::new();
    let mut losses = Vec::new();
    let mut compared = 0usize;
    for info in cx.tables {
        let id = (info.schema.clone(), info.name.clone());
        let before = cx.before.counts.get(&id).copied();
        let t = cx.table_index(info);
        let family = t.map(|t| cx.up.family_ever(t)).unwrap_or_default();
        let after = snapshot::count(
            session,
            &info.schema,
            &info.name,
            Scope::of(info),
            cx.limits,
        )
        .await
        .ok();
        let mut count_check = TableCheck {
            table: info.label(),
            from: None,
            before,
            after,
            sampled: 0,
            status: Status::Passed,
            notes: Vec::new(),
        };
        let mut rows_lost_reported = false;
        if after != before {
            let shape = if after > before {
                Shape::RowsAdded
            } else {
                Shape::RowsGone
            };
            let cause = pick(
                cx.of(None)
                    .filter(|l| explains_rows(&l.effect, &family, shape)),
                shape,
            );
            let status = if after.is_none() {
                Status::Failed
            } else {
                status_of(cause.as_ref())
            };
            let message = match (after, &cause) {
                (None, _) => "Its rows could not be counted (is the table back?).".to_owned(),
                (Some(a), Some(c)) => format!(
                    "{} rows, {} before: {}.",
                    thousands(a),
                    thousands(before.unwrap_or(0)),
                    describe(c)
                ),
                (Some(a), None) => format!(
                    "{} rows, {} before, and no statement explains it.",
                    thousands(a),
                    thousands(before.unwrap_or(0))
                ),
            };
            count_check.status = status;
            count_check.notes.push(Note {
                kind: NoteKind::RowsMissing,
                column: None,
                from: None,
                rows: 0,
                status,
                message: message.clone(),
                statement: cause.clone(),
                examples: Vec::new(),
            });
            if let (Some(a), Some(b)) = (after, before)
                && a < b
                && cause.as_ref().is_none_or(|c| c.flag != Flag::Moved)
            {
                rows_lost_reported = true;
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: None,
                        kind: LossKind::RowsDeleted,
                        rows: cx.before.samples.get(&id).map_or(0, Vec::len),
                        sampled: cx.before.samples.get(&id).map_or(0, Vec::len),
                        count_before: Some(b),
                        count_after: Some(a),
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!(
                            "{}: {} of {} rows are not back. {}",
                            info.label(),
                            thousands(b - a),
                            thousands(b),
                            cause.as_ref().map_or_else(
                                || "No statement explains it.".to_owned(),
                                |c| format!("Explained by {}.", describe(c))
                            )
                        ),
                        cause,
                    },
                    status,
                ));
            }
        }
        count_tables.push(count_check);

        if info.key.is_empty() {
            if after == before
                && let Some(check) = keyless_after_down(session, cx, info, &mut losses).await
            {
                value_tables.push(check);
            }
            continue;
        }
        let Some(rows) = cx.before.samples.get(&id).filter(|r| !r.is_empty()) else {
            continue;
        };
        let mut table_check = TableCheck {
            table: info.label(),
            from: None,
            before,
            after,
            sampled: rows.len(),
            status: Status::Passed,
            notes: Vec::new(),
        };
        // Found again by a key whose columns kept their values both ways
        // (the primary key, else another unique key).
        let kept = |k: &String| {
            let Some(t) = t else { return true };
            let c = cx.column_index(t, k);
            !cx.of(None).any(|l| {
                !matches!(l.effect, Effect::Opaque) && explains_values(&l.effect, t, c, None)
            })
        };
        let Some(key) = info.keys.iter().find(|key| key.iter().all(kept)) else {
            table_check.notes.push(Note {
                kind: NoteKind::NotVerified,
                column: None,
                from: None,
                rows: rows.len(),
                status: Status::Warning,
                message: "Every key of it loses its values on the way: its sampled rows cannot be found again, only its dropped columns are checked.".to_owned(),
                statement: None,
                examples: Vec::new(),
            });
            if let Some(t) = t {
                for (ci, column) in info.columns.iter().enumerate() {
                    let c = cx.column_index(t, &column.name);
                    let dropped: Vec<&Located> = cx
                        .of(None)
                        .filter(|l| {
                            matches!(l.effect, Effect::ColumnDropped { table, column } if table == t && Some(column) == c)
                        })
                        .collect();
                    if dropped.is_empty() {
                        continue;
                    }
                    let held = rows.iter().filter(|r| r.values[ci] != Cell::Null).count();
                    let cause = pick(dropped, Shape::ColumnDropped);
                    let status = status_of(cause.as_ref());
                    if held > 0 {
                        losses.push(lost(
                            DataLoss {
                                table: info.label(),
                                column: Some(column.name.clone()),
                                kind: LossKind::ColumnDropped,
                                rows: held,
                                sampled: rows.len(),
                                count_before: None,
                                count_after: None,
                                examples: Vec::new(),
                                expected: false,
                                status,
                                message: format!(
                                    "{}.{}: its values are gone{}",
                                    info.label(),
                                    column.name,
                                    cause.as_ref().map_or_else(
                                        || ".".to_owned(),
                                        |c| format!(": dropped by {}.", describe(c))
                                    )
                                ),
                                cause,
                            },
                            status,
                        ));
                    }
                }
            }
            table_check.status = Status::Warning;
            value_tables.push(table_check);
            continue;
        };
        let key_at: Vec<usize> = key
            .iter()
            .filter_map(|k| info.columns.iter().position(|c| c.name == *k))
            .collect();
        let key_of = |row: &Row| -> Option<Vec<String>> {
            key_at
                .iter()
                .map(|&ci| row.values[ci].text().map(str::to_owned))
                .collect()
        };
        let columns: Vec<String> = info.columns.iter().map(|c| c.name.clone()).collect();
        let wanted: Vec<Vec<String>> = rows.iter().filter_map(key_of).collect();
        let reading = snapshot::Reading {
            schema: &info.schema,
            name: &info.name,
            key,
            columns: &columns,
            scope: Scope::of(info),
        };
        let found = match snapshot::lookup(session, reading, &wanted, cx.limits).await {
            Ok(found) => found,
            Err(err) => {
                table_check.status = Status::Failed;
                table_check.notes.push(Note {
                    kind: NoteKind::NotCompared,
                    column: None,
                    from: None,
                    rows: rows.len(),
                    status: Status::Failed,
                    message: format!("Its rows could not be read: {err}"),
                    statement: None,
                    examples: Vec::new(),
                });
                value_tables.push(table_check);
                continue;
            }
        };
        let found_row = |row: &Row| key_of(row).and_then(|k| found.get(&k));
        compared += rows.len();
        let missing: Vec<&Row> = rows.iter().filter(|r| found_row(r).is_none()).collect();
        if !missing.is_empty() {
            let cause = pick(
                cx.of(None)
                    .filter(|l| explains_rows(&l.effect, &family, Shape::RowsGone)),
                Shape::RowsGone,
            );
            let status = status_of(cause.as_ref());
            let message = format!(
                "{} of the {} sampled rows are not back{}",
                missing.len(),
                rows.len(),
                cause.as_ref().map_or_else(
                    || ", and no statement explains it.".to_owned(),
                    |c| format!(": {}.", describe(c))
                )
            );
            table_check.notes.push(Note {
                kind: NoteKind::RowsMissing,
                column: None,
                from: None,
                rows: missing.len(),
                status,
                message: message.clone(),
                statement: cause.clone(),
                examples: missing
                    .iter()
                    .take(3)
                    .map(|r| Example {
                        key: r.key_label(),
                        before: None,
                        after: None,
                    })
                    .collect(),
            });
            if !rows_lost_reported {
                losses.push(lost(
                    DataLoss {
                        table: info.label(),
                        column: None,
                        kind: LossKind::RowsDeleted,
                        rows: missing.len(),
                        sampled: rows.len(),
                        count_before: before,
                        count_after: after,
                        examples: Vec::new(),
                        expected: false,
                        status,
                        message: format!("{}: {message}", info.label()),
                        cause,
                    },
                    status,
                ));
            }
        }
        for (ci, column) in info.columns.iter().enumerate() {
            let changed: Vec<(&Row, &Cell)> = rows
                .iter()
                .filter_map(|r| {
                    let now = found_row(r)?;
                    (now.values[ci] != r.values[ci]).then(|| (r, &now.values[ci]))
                })
                .collect();
            if changed.is_empty() {
                continue;
            }
            let c = t.and_then(|t| cx.column_index(t, &column.name));
            let enum_type = cx.enum_index(column.enum_type.as_ref());
            let candidates: Vec<&Located> = t.map_or_else(Vec::new, |t| {
                cx.of(None)
                    .filter(|l| explains_values(&l.effect, t, c, enum_type))
                    .collect()
            });
            let dropped = candidates
                .iter()
                .any(|l| matches!(l.effect, Effect::ColumnDropped { .. }));
            let shape = if dropped {
                Shape::ColumnDropped
            } else {
                Shape::Values
            };
            let cause = pick(candidates.iter().copied(), shape);
            let status = status_of(cause.as_ref());
            let message = format!(
                "{} of {} sampled values are not back as they were{}",
                changed.len(),
                rows.len(),
                match (&cause, dropped) {
                    (Some(c), true) => format!(
                        ": the column is dropped by {}, and the down migration adds it back without them.",
                        describe(c)
                    ),
                    (Some(c), false) => format!(": {}.", describe(c)),
                    (None, _) => ", and no statement explains it.".to_owned(),
                }
            );
            let examples: Vec<Example> = changed
                .iter()
                .take(3)
                .map(|(r, now)| example(r, &r.values[ci], now))
                .collect();
            table_check.notes.push(Note {
                kind: if dropped {
                    NoteKind::Dropped
                } else if cause.is_some() {
                    NoteKind::Changed
                } else {
                    NoteKind::Unexplained
                },
                column: Some(column.name.clone()),
                from: None,
                rows: changed.len(),
                status,
                message: message.clone(),
                statement: cause.clone(),
                examples: examples.clone(),
            });
            losses.push(lost(
                DataLoss {
                    table: info.label(),
                    column: Some(column.name.clone()),
                    kind: if dropped {
                        LossKind::ColumnDropped
                    } else {
                        LossKind::ValuesChanged
                    },
                    rows: changed.len(),
                    sampled: rows.len(),
                    count_before: None,
                    count_after: None,
                    examples,
                    expected: false,
                    status,
                    message: format!("{}.{}: {message}", info.label(), column.name),
                    cause,
                },
                status,
            ));
        }
        table_check.status = table_check
            .notes
            .iter()
            .fold(Status::Passed, |s, n| s.and(n.status));
        if !table_check.notes.is_empty() {
            value_tables.push(table_check);
        }
    }
    let counts = rollup(
        CheckKind::RowCounts,
        count_tables,
        |tables, status| match status {
            Status::Failed => "Some tables are not back with their rows.".to_owned(),
            Status::NeedsAcknowledgement => {
                "Some rows are not back where the migration does not say they go.".to_owned()
            }
            Status::Warning => "Some rows are not back, where statements say they go.".to_owned(),
            _ => format!(
                "Every table is back with its rows ({}).",
                plural(tables, "table", "tables")
            ),
        },
        false,
    );
    let values = rollup(
        CheckKind::Values,
        value_tables,
        |_, status| {
            match status {
            Status::Failed => {
                "Some sampled values are not back, and no flagged statement explains it.".to_owned()
            }
            Status::NeedsAcknowledgement => {
                "Some values are not back where the migration does not say it loses them: acknowledge it, or change the migration.".to_owned()
            }
            Status::Warning => {
                "Some sampled values are not back, where statements say they are lost, or could not be verified.".to_owned()
            }
            _ => format!(
                "{} sampled rows are back with their original values.",
                thousands(i64::try_from(compared).unwrap_or(0))
            ),
        }
        },
        true,
    );
    AfterDown {
        counts,
        values,
        losses,
    }
}

/// No sequence went back after the down migration: one that did (a serial
/// restarted) hands out values its table's rows already hold. Its table
/// empty, nothing collides: a warning.
pub async fn sequences_after_down<S: Session>(
    session: &mut S,
    before: &[SequenceValue],
    limits: Limits,
) -> CheckReport {
    let now = match snapshot::sequences(session, limits).await {
        Ok(now) => now,
        Err(err) => {
            return CheckReport {
                kind: CheckKind::Sequences,
                status: Status::Warning,
                summary: format!("The sequences could not be read: {err}"),
                details: Vec::new(),
                tables: Vec::new(),
            };
        }
    };
    let mut status = Status::Passed;
    let mut details = Vec::new();
    for b in before {
        let Some(a) = now
            .iter()
            .find(|s| s.schema == b.schema && s.name == b.name)
        else {
            continue;
        };
        let back = if b.increment >= 0 {
            a.next < b.next
        } else {
            a.next > b.next
        };
        if !back {
            continue;
        }
        let rows = match &b.owner {
            Some((schema, table)) => {
                snapshot::count(session, schema, table, Scope::default(), limits)
                    .await
                    .ok()
            }
            None => None,
        };
        let empty = rows == Some(0);
        status = status.and(if empty {
            Status::Warning
        } else {
            Status::Failed
        });
        details.push(format!(
            "{}.{} went back: it gives {} next, {} before{}",
            b.schema,
            b.name,
            thousands(a.next),
            thousands(b.next),
            if empty {
                ": its table is empty, nothing collides."
            } else {
                ": new rows would take values rows already hold."
            }
        ));
    }
    CheckReport {
        kind: CheckKind::Sequences,
        status,
        summary: match status {
            Status::Failed => "A sequence went back: new rows would collide.".to_owned(),
            Status::Warning => "A sequence went back, on an empty table.".to_owned(),
            _ => format!(
                "No sequence went back ({}).",
                plural(before.len(), "sequence", "sequences")
            ),
        },
        details,
        tables: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_labels_map_back_to_what_they_were() {
        assert_eq!(
            labels_back(&[("sad", "blue")]),
            [("blue".to_owned(), "sad".to_owned())]
        );
        // A swap through a third name.
        let mut back = labels_back(&[("a", "tmp"), ("b", "a"), ("tmp", "b")]);
        back.sort();
        assert_eq!(
            back,
            [
                ("a".to_owned(), "b".to_owned()),
                ("b".to_owned(), "a".to_owned())
            ]
        );
        // Renamed twice: the last name maps to the first.
        assert_eq!(
            labels_back(&[("x", "y"), ("y", "z")]),
            [("z".to_owned(), "x".to_owned())]
        );
    }

    fn located(effect: Effect, planned: bool, hazard: Option<&str>) -> Located {
        Located {
            direction: Direction::Up,
            index: 3,
            effect,
            planned,
            hazard: hazard.map(str::to_owned),
        }
    }

    #[test]
    fn a_flagged_statement_explains_loss_and_an_unflagged_one_does_not_excuse_it() {
        let dropped = located(
            Effect::ColumnDropped {
                table: 0,
                column: 2,
            },
            true,
            Some("deletes_data"),
        );
        let cause = pick([&dropped], Shape::ColumnDropped).unwrap();
        assert_eq!(cause.flag, Flag::Flagged);
        assert_eq!(status_of(Some(&cause)), Status::Warning);
        assert_eq!(
            describe(&cause),
            "statement 4 of the up migration (flagged: deletes data)"
        );

        let converted = located(
            Effect::ColumnConverted {
                table: 0,
                column: 1,
                conversion: Conversion {
                    to: "integer".into(),
                    using: None,
                    names: Vec::new(),
                    to_type: None,
                    using_types: Vec::new(),
                },
            },
            true,
            Some("can_fail"),
        );
        let cause = pick([&converted], Shape::Values).unwrap();
        assert_eq!(cause.flag, Flag::NotFlagged);
        assert_eq!(status_of(Some(&cause)), Status::Failed);
        assert_eq!(status_of(None), Status::Failed);
    }

    #[test]
    fn written_statements_explain_by_what_they_say_they_do() {
        let update = located(
            Effect::ValuesChanged {
                table: 1,
                columns: Some(vec![4]),
            },
            false,
            None,
        );
        assert!(explains_values(&update.effect, 1, Some(4), None));
        assert!(!explains_values(&update.effect, 1, Some(5), None));
        assert!(!explains_values(&update.effect, 2, Some(4), None));
        assert_eq!(pick([&update], Shape::Values).unwrap().flag, Flag::Written);
        assert!(explains_rows(
            &Effect::RowsDeleted {
                table: 1,
                all: false
            },
            &[1],
            Shape::RowsGone
        ));
        assert!(!explains_rows(
            &Effect::RowsAdded { table: 1 },
            &[1],
            Shape::RowsGone
        ));
        assert!(explains_values(
            &Effect::LabelRenamed {
                enum_type: 2,
                from: "a".into(),
                to: "b".into()
            },
            0,
            Some(0),
            Some(2)
        ));
        // A written conversion that rounds does not say so: acknowledged,
        // never passed as written.
        let converted = located(
            Effect::ColumnConverted {
                table: 1,
                column: 4,
                conversion: Conversion {
                    to: "integer".into(),
                    using: None,
                    names: Vec::new(),
                    to_type: None,
                    using_types: Vec::new(),
                },
            },
            false,
            None,
        );
        let cause = pick([&converted], Shape::Values).unwrap();
        assert_eq!(cause.flag, Flag::WrittenUndeclared);
        assert_eq!(status_of(Some(&cause)), Status::NeedsAcknowledgement);
    }

    #[test]
    fn a_do_block_explains_nothing_it_can_be_trusted_with() {
        let opaque = located(Effect::Opaque, false, None);
        assert!(explains_values(&Effect::Opaque, 7, Some(1), None));
        let cause = pick([&opaque], Shape::Values).unwrap();
        assert_eq!(cause.flag, Flag::Opaque);
        assert_eq!(status_of(Some(&cause)), Status::NeedsAcknowledgement);
        assert!(describe(&cause).contains("not verifiable"));
    }

    #[test]
    fn a_flagged_explanation_is_preferred_to_an_unflagged_one() {
        let unflagged = located(Effect::Opaque, true, None);
        let flagged = located(
            Effect::TableDropped { table: 0 },
            true,
            Some("deletes_data"),
        );
        assert_eq!(
            pick([&unflagged, &flagged], Shape::RowsGone).unwrap().flag,
            Flag::Flagged
        );
    }

    #[test]
    fn renamed_labels_follow_into_arrays() {
        let labels = [("sad", "blue"), ("ok", "fine")];
        assert_eq!(relabel("sad", false, &labels), "blue");
        assert_eq!(
            relabel("{sad,ok,happy}", true, &labels),
            "{blue,fine,happy}"
        );
        assert_eq!(relabel("{NULL,sad}", true, &labels), "{NULL,blue}");
        assert_eq!(
            relabel(r#"{"a b",sad}"#, true, &[("a b", "c d"), ("sad", "blue")]),
            r#"{"c d",blue}"#
        );
        assert_eq!(relabel("{}", true, &labels), "{}");
        assert_eq!(relabel("{{sad}}", true, &labels), "{{sad}}");
    }

    #[test]
    fn numbers_read_with_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(12_400), "12,400");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(thousands(-4_000), "-4,000");
    }
}
