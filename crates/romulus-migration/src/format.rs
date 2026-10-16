//! A migration as files, the way each tool expects them.
//!
//! Every tool gets the same SQL; what differs is the file names, where the
//! transaction comes from, and whether there is a down migration:
//!
//! | format | files | transaction | down |
//! |---|---|---|---|
//! | `plain` | `<ts>_<name>.sql` | `BEGIN`/`COMMIT` in the file | no |
//! | `sqlx` | `<ts>_<name>.sql` | sqlx's own (`-- no-transaction` opts out) | no |
//! | `flyway` | `V<n>__<name>.sql` | Flyway's own (a `.sql.conf` opts out) | no |
//! | `prisma` | `<ts>_<name>/migration.sql` | Prisma's own | no |
//! | `dbmate` | `<ts>_<name>.sql`, `-- migrate:up` / `-- migrate:down` | dbmate's own (`transaction:false` opts out) | yes |
//! | `golang_migrate` | `<ts>_<name>.up.sql` / `.down.sql` | `BEGIN`/`COMMIT` in the file | yes |
//!
//! A migration with [`Stage::Commit`] or [`Stage::Concurrent`] statements
//! is split where the tool runs each file in its own transaction: the
//! enum values first, the main file, then one file per concurrent index
//! (several statements in one file run as one implicit transaction, which
//! `CONCURRENTLY` refuses). Their versions follow each other (a second
//! apart, or `V<n>.1`, `V<n>.2`…).
//!
//! The down migration is the migration from the new schema back to the old
//! one, in the main file: best effort, since what the up migration deleted
//! does not come back.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::{Migration, Section, Stage, Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Plain,
    Sqlx,
    Flyway,
    Prisma,
    Dbmate,
    GolangMigrate,
}

impl Format {
    pub const ALL: [Self; 6] = [
        Self::Plain,
        Self::Sqlx,
        Self::Flyway,
        Self::Prisma,
        Self::Dbmate,
        Self::GolangMigrate,
    ];

    /// The tool's name as people write it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain SQL",
            Self::Sqlx => "sqlx",
            Self::Flyway => "Flyway",
            Self::Prisma => "Prisma",
            Self::Dbmate => "dbmate",
            Self::GolangMigrate => "golang-migrate",
        }
    }

    /// The tool has down migrations.
    pub fn has_down(self) -> bool {
        matches!(self, Self::Dbmate | Self::GolangMigrate)
    }
}

/// What the files are named after and say in their header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// The database, as `org/db`.
    pub database: String,
    /// The two sides: `v4` and `v5`, or `v12` and `draft "Billing"`.
    pub from: String,
    pub to: String,
    /// UTC, `YYYYMMDDHHMMSS`: the version of timestamped tools, and the
    /// header's date.
    pub timestamp: String,
    /// Flyway's version for the main file (`V<version>__…`).
    pub version: u32,
    /// The description in file names, as [`slug`] makes it.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct File {
    /// Relative to the tool's migrations folder: `20261016093000_add_invoices.sql`,
    /// `20261016093000_add_invoices/migration.sql`.
    pub path: String,
    pub content: String,
    pub role: FileRole,
    /// Where the destructive statements are (with their comments), for
    /// highlighting.
    pub destructive: Vec<Lines>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    /// The migration (or a part of it), in the order to apply.
    Up,
    /// A best-effort down migration.
    Down,
    /// The tool's settings for the file before it (Flyway's `.sql.conf`).
    Config,
}

/// A range of lines, 1-based, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Lines {
    pub from: u32,
    pub to: u32,
}

/// `text` as a file name's description: lowercase ASCII letters and digits,
/// words joined by `_`, at most 60 characters; `migration` when nothing is
/// left.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.to_lowercase().chars() {
        let c = fold(c);
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    if out.len() > 60 {
        let cut = out[..=60].rfind('_').unwrap_or(60);
        out.truncate(cut);
    }
    if out.is_empty() {
        "migration".to_owned()
    } else {
        out
    }
}

/// Common accented Latin letters as their base letter.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

/// One file's worth of statements.
struct Unit<'m> {
    kind: UnitKind,
    statements: Vec<&'m Statement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    /// Enum values committed before the rest.
    Commit,
    Main,
    /// One `CONCURRENTLY` statement.
    Concurrent,
}

/// The files of `up` (and of `down`, the migration back, where the tool has
/// down migrations) for `format`.
pub fn files(up: &Migration, down: &Migration, format: Format, meta: &Meta) -> Vec<File> {
    if format == Format::Plain {
        let mut out = Writer::new();
        header(&mut out, up, meta, None);
        body(&mut out, &up.statements.iter().collect::<Vec<_>>(), true);
        return vec![out.file(
            format!("{}_{}.sql", meta.timestamp, meta.name),
            FileRole::Up,
        )];
    }
    let units = split(up);
    let parts = units.len();
    let mut files = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let (version, name) = match unit.kind {
            UnitKind::Main => (i, meta.name.clone()),
            UnitKind::Commit => (i, format!("{}_enum_values", meta.name)),
            UnitKind::Concurrent => (i, format!("{}_{}", meta.name, concurrent_name(unit))),
        };
        let part = (parts > 1).then_some((i + 1, parts));
        files.extend(unit_files(
            format, unit, up, down, meta, version, &name, part,
        ));
    }
    files
}

/// The statements split into the files a per-file-transaction tool needs.
fn split(m: &Migration) -> Vec<Unit<'_>> {
    let mut units = Vec::new();
    let commit: Vec<&Statement> = m
        .statements
        .iter()
        .filter(|s| s.stage == Stage::Commit)
        .collect();
    if !commit.is_empty() {
        units.push(Unit {
            kind: UnitKind::Commit,
            statements: commit,
        });
    }
    units.push(Unit {
        kind: UnitKind::Main,
        statements: m
            .statements
            .iter()
            .filter(|s| s.stage == Stage::Transaction)
            .collect(),
    });
    for s in m.statements.iter().filter(|s| s.stage == Stage::Concurrent) {
        units.push(Unit {
            kind: UnitKind::Concurrent,
            statements: vec![s],
        });
    }
    units
}

/// `create_index_users_email_idx`, from the statement itself.
fn concurrent_name(unit: &Unit) -> String {
    let sql = unit.statements.first().map_or("", |s| s.sql.as_str());
    let verb = if sql.starts_with("DROP") {
        "drop"
    } else {
        "create"
    };
    let name = sql
        .split_whitespace()
        .skip_while(|w| *w != "CONCURRENTLY")
        .nth(1)
        .unwrap_or("index");
    let name = name
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(';');
    slug(&format!("{verb}_index_{name}"))
}

#[allow(clippy::too_many_arguments)]
fn unit_files(
    format: Format,
    unit: &Unit,
    up: &Migration,
    down: &Migration,
    meta: &Meta,
    index: usize,
    name: &str,
    part: Option<(usize, usize)>,
) -> Vec<File> {
    let ts = add_seconds(&meta.timestamp, index as u64);
    let concurrent = unit.kind == UnitKind::Concurrent;
    let main = unit.kind == UnitKind::Main;
    let mut out = Writer::new();
    match format {
        Format::Plain => unreachable!("plain is one file"),
        Format::Sqlx => {
            if concurrent {
                out.line("-- no-transaction");
            }
            header(&mut out, up, meta, part);
            body(&mut out, &unit.statements, false);
            vec![out.file(format!("{ts}_{name}.sql"), FileRole::Up)]
        }
        Format::Flyway => {
            let version = if index == 0 {
                meta.version.to_string()
            } else {
                format!("{}.{index}", meta.version)
            };
            let path = format!("V{version}__{name}.sql");
            header(&mut out, up, meta, part);
            body(&mut out, &unit.statements, false);
            let mut files = vec![out.file(path.clone(), FileRole::Up)];
            if concurrent {
                files.push(File {
                    path: format!("{path}.conf"),
                    content: "executeInTransaction=false\n".to_owned(),
                    role: FileRole::Config,
                    destructive: Vec::new(),
                });
            }
            files
        }
        Format::Prisma => {
            header(&mut out, up, meta, part);
            if concurrent {
                out.comment("Prisma runs a migration in a transaction, which CONCURRENTLY refuses: run this statement yourself, then mark it applied with prisma migrate resolve --applied <this folder>.");
                out.blank();
            }
            body(&mut out, &unit.statements, false);
            vec![out.file(format!("{ts}_{name}/migration.sql"), FileRole::Up)]
        }
        Format::Dbmate => {
            header(&mut out, up, meta, part);
            out.line(if concurrent {
                "-- migrate:up transaction:false"
            } else {
                "-- migrate:up"
            });
            body(&mut out, &unit.statements, false);
            out.blank();
            out.line("-- migrate:down");
            down_body(&mut out, unit.kind, down, false);
            vec![out.file(format!("{ts}_{name}.sql"), FileRole::Up)]
        }
        Format::GolangMigrate => {
            header(&mut out, up, meta, part);
            body(&mut out, &unit.statements, main);
            let up_file = out.file(format!("{ts}_{name}.up.sql"), FileRole::Up);
            let mut back = Writer::new();
            down_header(&mut back, down, meta);
            down_body(&mut back, unit.kind, down, true);
            vec![
                up_file,
                back.file(format!("{ts}_{name}.down.sql"), FileRole::Down),
            ]
        }
    }
}

fn down_header(out: &mut Writer, down: &Migration, meta: &Meta) {
    out.comment(&format!(
        "Down migration of {}, {} → {}: back to {}. Best effort: what the up migration deleted does not come back.",
        meta.database, meta.from, meta.to, meta.from
    ));
    warnings(out, down);
    out.blank();
}

fn down_body(out: &mut Writer, kind: UnitKind, down: &Migration, transaction: bool) {
    match kind {
        UnitKind::Main => {
            if down.is_empty() {
                out.comment("Nothing to undo.");
            } else {
                // Concurrent statements of the down migration run with it.
                body(
                    out,
                    &down.statements.iter().collect::<Vec<_>>(),
                    transaction,
                );
            }
        }
        UnitKind::Commit => {
            out.comment("Postgres cannot remove an enum value: undone, if at all, by the main migration's down.");
        }
        UnitKind::Concurrent => {
            out.comment("Undone by the main migration's down.");
        }
    }
}

fn header(out: &mut Writer, m: &Migration, meta: &Meta, part: Option<(usize, usize)>) {
    let date = format!(
        "{}-{}-{}",
        meta.timestamp.get(0..4).unwrap_or("????"),
        meta.timestamp.get(4..6).unwrap_or("??"),
        meta.timestamp.get(6..8).unwrap_or("??")
    );
    out.comment(&format!(
        "Migration of {} from {} to {}, generated by romulus on {date}.",
        meta.database, meta.from, meta.to
    ));
    out.comment("Review it before applying: romulus compares the schemas, not your data.");
    if let Some((n, of)) = part {
        out.comment(&format!(
            "Part {n} of {of}: the migration is split where it cannot run in one transaction."
        ));
    }
    let count = m.statements.len();
    let destructive = m.destructive();
    if count == 0 {
        out.comment("Nothing to migrate: the schemas are the same.");
    } else {
        let mut line = format!("{count} statement{}", if count == 1 { "" } else { "s" });
        if destructive > 0 {
            let _ = write!(
                line,
                ", {destructive} destructive (marked DESTRUCTIVE: they delete data or can fail on existing rows)."
            );
        } else {
            line.push_str(", none destructive.");
        }
        out.comment(&line);
    }
    warnings(out, m);
    out.blank();
}

fn warnings(out: &mut Writer, m: &Migration) {
    if m.warnings.is_empty() {
        return;
    }
    out.line("--");
    out.line("-- Warnings:");
    for warning in &m.warnings {
        out.comment_item(&warning.message);
    }
}

/// The statements with their section headings and comments; inside
/// `BEGIN`/`COMMIT` when `transaction` is set, with the commit-stage ones
/// before it and the concurrent ones after.
fn body(out: &mut Writer, statements: &[&Statement], transaction: bool) {
    let stage = |s: Stage| statements.iter().copied().filter(move |x| x.stage == s);
    let main: Vec<&Statement> = if transaction {
        stage(Stage::Transaction).collect()
    } else {
        statements.to_vec()
    };
    if transaction {
        let commit: Vec<&Statement> = stage(Stage::Commit).collect();
        if !commit.is_empty() {
            out.comment("Committed before the transaction: the migration uses these enum values, and Postgres refuses a value added in the same transaction.");
            for s in commit {
                statement(out, s);
            }
            out.blank();
        }
    }
    let wrap = transaction && !main.is_empty();
    if wrap {
        out.line("BEGIN;");
        out.blank();
    }
    let mut section = None;
    let removals_destroy = main
        .iter()
        .any(|s| s.section == Section::Removals && s.destructive);
    for s in &main {
        if section != Some(s.section) && s.stage != Stage::Concurrent {
            section = Some(s.section);
            heading(out, s.section, removals_destroy);
        }
        statement(out, s);
    }
    if wrap {
        out.line("COMMIT;");
    }
    if transaction {
        let concurrent: Vec<&Statement> = stage(Stage::Concurrent).collect();
        if !concurrent.is_empty() {
            out.blank();
            out.comment(
                "After the transaction, one at a time: CONCURRENTLY cannot run inside one.",
            );
            out.blank();
            for s in concurrent {
                statement(out, s);
            }
        }
    }
}

fn heading(out: &mut Writer, section: Section, destroys: bool) {
    let rule = format!("-- {}", "-".repeat(74));
    out.line(&rule);
    if section == Section::Removals && destroys {
        out.line("-- Removals. DESTRUCTIVE: what follows deletes data. Back up first.");
    } else {
        out.line(&format!("-- {}", section.title()));
    }
    out.line(&rule);
    out.blank();
}

fn statement(out: &mut Writer, s: &Statement) {
    let start = out.line_number();
    match (&s.note, s.destructive) {
        (Some(note), true) => out.comment(&format!("DESTRUCTIVE: {note}")),
        (None, true) => out.comment("DESTRUCTIVE."),
        (Some(note), false) => out.comment(note),
        (None, false) => {}
    }
    if let Some(alternative) = &s.alternative {
        out.comment("To keep the data instead:");
        for line in alternative.lines() {
            out.line(&format!("--   {line}"));
        }
    }
    for line in s.sql.lines() {
        out.line(line);
    }
    if s.destructive {
        out.destructive.push(Lines {
            from: start,
            to: out.line_number() - 1,
        });
    }
    out.blank();
}

/// Text with line numbers kept as it grows.
struct Writer {
    text: String,
    lines: u32,
    destructive: Vec<Lines>,
}

impl Writer {
    fn new() -> Self {
        Self {
            text: String::new(),
            lines: 0,
            destructive: Vec::new(),
        }
    }

    /// The number the next line gets.
    fn line_number(&self) -> u32 {
        self.lines + 1
    }

    fn line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
        self.lines += 1;
    }

    fn blank(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            self.line("");
        }
    }

    /// `text` as `--` lines of at most 78 columns.
    fn comment(&mut self, text: &str) {
        for line in wrap(text, 75) {
            self.line(&format!("-- {line}"));
        }
    }

    /// A bulleted comment: `--   - text`, continued under the text.
    fn comment_item(&mut self, text: &str) {
        for (i, line) in wrap(text, 71).into_iter().enumerate() {
            let bullet = if i == 0 { "- " } else { "  " };
            self.line(&format!("--   {bullet}{line}"));
        }
    }

    fn file(mut self, path: String, role: FileRole) -> File {
        while self.text.ends_with("\n\n") {
            self.text.pop();
        }
        File {
            path,
            content: self.text,
            role,
            destructive: self.destructive,
        }
    }
}

/// Words in lines of at most `width` characters (a longer word alone).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `YYYYMMDDHHMMSS` plus `seconds`, calendar included; unchanged when it
/// does not read as a timestamp.
fn add_seconds(timestamp: &str, seconds: u64) -> String {
    if seconds == 0 {
        return timestamp.to_owned();
    }
    let field = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<u64>().ok();
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        field(0..4),
        field(4..6),
        field(6..8),
        field(8..10),
        field(10..12),
        field(12..14),
    ) else {
        return timestamp.to_owned();
    };
    if timestamp.len() != 14 || !(1..=12).contains(&month) {
        return timestamp.to_owned();
    }
    let (mut year, mut month, mut day) = (year, month, day);
    let mut total = hour * 3600 + minute * 60 + second + seconds;
    let leap = |y: u64| (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days_in = |y: u64, m: u64| match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    while total >= 86_400 {
        total -= 86_400;
        day += 1;
        if day > days_in(year, month) {
            day = 1;
            month += 1;
            if month > 12 {
                month = 1;
                year += 1;
            }
        }
    }
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        total / 3600,
        total % 3600 / 60,
        total % 60
    )
}

#[cfg(test)]
mod tests {
    use super::{add_seconds, slug, wrap};

    #[test]
    fn slugs_are_file_name_safe() {
        assert_eq!(
            slug("Add invoices & their line items!"),
            "add_invoices_their_line_items"
        );
        assert_eq!(slug("Équipes: créer"), "equipes_creer");
        assert_eq!(slug("   "), "migration");
        assert!(slug(&"word ".repeat(40)).len() <= 60);
    }

    #[test]
    fn timestamps_carry_over_days_months_and_years() {
        assert_eq!(add_seconds("20261016093000", 1), "20261016093001");
        assert_eq!(add_seconds("20261231235959", 2), "20270101000001");
        assert_eq!(add_seconds("20240228235959", 1), "20240229000000");
        assert_eq!(add_seconds("not a timestamp", 1), "not a timestamp");
    }

    #[test]
    fn comments_wrap_on_words() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("a verylongword", 4), ["a", "verylongword"]);
    }
}
