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
//! `CONCURRENTLY` refuses). Enum values the *down* migration uses get a
//! migration of their own right after the main one: going down, it runs
//! (and commits) before the main one's down. Versions follow each other
//! (a second apart, the next number, or `V<n>.1`, `V<n>.2`…).
//!
//! The down migration is the migration from the new schema back to the old
//! one, in the main file: best effort, since what the up migration deleted
//! does not come back.

use serde::{Deserialize, Serialize};

use crate::{Hazard, Migration, Section, Stage, Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationTool))]
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

    /// The wire name, as serialized (`golang_migrate`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Sqlx => "sqlx",
            Self::Flyway => "flyway",
            Self::Prisma => "prisma",
            Self::Dbmate => "dbmate",
            Self::GolangMigrate => "golang_migrate",
        }
    }

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
    /// The header's date, `YYYY-MM-DD`.
    pub date: String,
    /// The first file's version: Flyway's `V<version>__…` (digits and
    /// dots), the number other tools put first (a `YYYYMMDDHHMMSS`
    /// timestamp, or a sequence like golang-migrate's `000042`). The files
    /// after it get the next ones ([`next_version`]).
    pub version: String,
    /// The description in file names, as [`slug`] makes it.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationFile))]
pub struct File {
    /// Relative to the tool's migrations folder: `20261016093000_add_invoices.sql`,
    /// `20261016093000_add_invoices/migration.sql`.
    pub path: String,
    pub content: String,
    pub role: FileRole,
    /// Where the statements with a hazard are (with their comments), for
    /// highlighting.
    pub marks: Vec<Mark>,
}

/// Lines of a file holding a statement with a hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationMark))]
pub struct Mark {
    /// 1-based, both ends included.
    pub from: u32,
    pub to: u32,
    pub hazard: Hazard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = MigrationFileRole))]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    /// The migration (or a part of it), in the order to apply.
    Up,
    /// A best-effort down migration.
    Down,
    /// The tool's settings for the file before it (Flyway's `.sql.conf`).
    Config,
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

/// One file's (or migration's) worth of statements: what runs up, and
/// what runs down.
struct Unit<'m> {
    kind: UnitKind,
    up: Vec<&'m Statement>,
    down: Vec<&'m Statement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    /// Enum values the up migration uses, committed before it.
    UpCommit,
    Main,
    /// Enum values the down migration uses: its own migration after the
    /// main one, so that its down runs (and commits) first.
    DownCommit,
    /// One `CONCURRENTLY` statement.
    Concurrent,
}

/// What the files of a migration download as together: `<version>_<name>.zip`
/// (`V<version>__<name>.zip` for Flyway).
pub fn archive_name(format: Format, meta: &Meta) -> String {
    if format == Format::Flyway {
        format!("V{}__{}.zip", meta.version, meta.name)
    } else {
        format!("{}_{}.zip", meta.version, meta.name)
    }
}

/// The files of `up` (and of `down`, the migration back, where the tool has
/// down migrations) for `format`.
pub fn files(up: &Migration, down: &Migration, format: Format, meta: &Meta) -> Vec<File> {
    if format == Format::Plain {
        let mut out = Writer::new();
        header(&mut out, up, meta, None);
        body(&mut out, &up.statements.iter().collect::<Vec<_>>(), true);
        return vec![out.file(format!("{}_{}.sql", meta.version, meta.name), FileRole::Up)];
    }
    let units = split(up, down, format.has_down());
    let parts = units.len();
    let mut files = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let name = match unit.kind {
            UnitKind::Main => meta.name.clone(),
            UnitKind::UpCommit => format!("{}_enum_values", meta.name),
            UnitKind::DownCommit => format!("{}_down_enum_values", meta.name),
            UnitKind::Concurrent => format!("{}_{}", meta.name, concurrent_name(unit)),
        };
        let part = (parts > 1).then_some((i + 1, parts));
        files.extend(unit_files(format, unit, up, down, meta, i, &name, part));
    }
    files
}

/// The migration as one text to edit by hand: what a draft switching to
/// migration mode starts from. A short header, the warnings, then the
/// statements with their headings and comments, without the transaction
/// the tools add (the draft's compile runs it in one of its own).
pub fn editable(up: &Migration, meta: &Meta) -> String {
    // Edited in a pane beside the graph: short lines, headings without
    // rules.
    let mut out = Writer {
        width: 56,
        compact: true,
        ..Writer::new()
    };
    out.line(&format!(
        "-- {}: migration from {}, written by hand.",
        meta.database, meta.from
    ));
    if up.statements.is_empty() {
        out.line(&format!(
            "-- Write what changes {} here (ALTER TABLE …):",
            meta.from
        ));
        out.line("-- the schema and the graph follow from it.");
    } else {
        out.line(&format!(
            "-- Started from romulus's generated migration ({}):",
            meta.date
        ));
        out.line("-- edit it freely, the schema and the graph follow.");
    }
    warnings(&mut out, up);
    out.blank();
    body(&mut out, &up.statements.iter().collect::<Vec<_>>(), false);
    out.file(String::new(), FileRole::Up).content
}

/// The statements split into the migrations a per-file-transaction tool
/// needs, in the order they run up (down runs them backwards).
fn split<'m>(up: &'m Migration, down: &'m Migration, with_down: bool) -> Vec<Unit<'m>> {
    let stage = |m: &'m Migration, stage: Stage| -> Vec<&'m Statement> {
        m.statements.iter().filter(|s| s.stage == stage).collect()
    };
    let mut units = Vec::new();
    let up_commit = stage(up, Stage::Commit);
    if !up_commit.is_empty() {
        units.push(Unit {
            kind: UnitKind::UpCommit,
            up: up_commit,
            down: Vec::new(),
        });
    }
    // The down migration has no concurrent statements (it is planned
    // without the option); its enum values get a migration of their own.
    let mut main_down = stage(down, Stage::Transaction);
    main_down.extend(stage(down, Stage::Concurrent));
    units.push(Unit {
        kind: UnitKind::Main,
        up: stage(up, Stage::Transaction),
        down: if with_down { main_down } else { Vec::new() },
    });
    let down_commit = stage(down, Stage::Commit);
    if with_down && !down_commit.is_empty() {
        units.push(Unit {
            kind: UnitKind::DownCommit,
            up: Vec::new(),
            down: down_commit,
        });
    }
    for s in up
        .statements
        .iter()
        .filter(|s| s.stage == Stage::Concurrent)
    {
        units.push(Unit {
            kind: UnitKind::Concurrent,
            up: vec![s],
            down: Vec::new(),
        });
    }
    units
}

/// `create_index_users_email_idx`, from the statement itself.
fn concurrent_name(unit: &Unit) -> String {
    let sql = unit.up.first().map_or("", |s| s.sql.as_str());
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
    let version = if format == Format::Flyway {
        if index == 0 {
            meta.version.clone()
        } else {
            format!("{}.{index}", meta.version)
        }
    } else {
        next_version(&meta.version, index)
    };
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
            up_body(&mut out, unit, false);
            vec![out.file(format!("{version}_{name}.sql"), FileRole::Up)]
        }
        Format::Flyway => {
            let path = format!("V{version}__{name}.sql");
            header(&mut out, up, meta, part);
            up_body(&mut out, unit, false);
            let mut files = vec![out.file(path.clone(), FileRole::Up)];
            if concurrent {
                files.push(File {
                    path: format!("{path}.conf"),
                    content: "executeInTransaction=false\n".to_owned(),
                    role: FileRole::Config,
                    marks: Vec::new(),
                });
            }
            files
        }
        Format::Prisma => {
            header(&mut out, up, meta, part);
            if concurrent {
                out.comment("Alone in its migration: Prisma sends a migration.sql as one query, which Postgres runs as one transaction when it holds several statements, and CONCURRENTLY refuses that.");
                out.blank();
            }
            up_body(&mut out, unit, false);
            vec![out.file(format!("{version}_{name}/migration.sql"), FileRole::Up)]
        }
        Format::Dbmate => {
            header(&mut out, up, meta, part);
            out.line(if concurrent {
                "-- migrate:up transaction:false"
            } else {
                "-- migrate:up"
            });
            up_body(&mut out, unit, false);
            out.blank();
            out.line("-- migrate:down");
            down_body(&mut out, unit, down, false);
            vec![out.file(format!("{version}_{name}.sql"), FileRole::Up)]
        }
        Format::GolangMigrate => {
            header(&mut out, up, meta, part);
            up_body(&mut out, unit, main);
            let up_file = out.file(format!("{version}_{name}.up.sql"), FileRole::Up);
            let mut back = Writer::new();
            down_header(&mut back, down, meta);
            down_body(&mut back, unit, down, main);
            vec![
                up_file,
                back.file(format!("{version}_{name}.down.sql"), FileRole::Down),
            ]
        }
    }
}

fn up_body(out: &mut Writer, unit: &Unit, transaction: bool) {
    match unit.kind {
        UnitKind::DownCommit => out.comment(
            "Nothing to do going up: this migration holds the enum values its down migration commits before the main one's down uses them.",
        ),
        _ if unit.up.is_empty() => out.comment("Nothing to do."),
        _ => body(out, &unit.up, transaction),
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

fn down_body(out: &mut Writer, unit: &Unit, down: &Migration, transaction: bool) {
    match unit.kind {
        UnitKind::Main if down.is_empty() => out.comment("Nothing to undo."),
        UnitKind::Main | UnitKind::DownCommit => {
            if unit.kind == UnitKind::DownCommit {
                out.comment("Enum values the main migration's down uses, committed first (it runs before that one goes down).");
            }
            body(out, &unit.down, transaction);
        }
        UnitKind::UpCommit => {
            out.comment("Postgres cannot remove an enum value: the main migration's down recreates the type if it must.");
        }
        UnitKind::Concurrent => out.comment("Undone by the main migration's down."),
    }
}

fn header(out: &mut Writer, m: &Migration, meta: &Meta, part: Option<(usize, usize)>) {
    let target = m
        .major
        .map_or_else(String::new, |major| format!(" for PostgreSQL {major}"));
    out.comment(&format!(
        "Migration of {} from {} to {}{target}, generated by romulus on {}.",
        meta.database, meta.from, meta.to, meta.date
    ));
    out.comment("Review it before applying: romulus compares the schemas, not your data.");
    if let Some((n, of)) = part {
        out.comment(&format!(
            "Part {n} of {of}: the migration is split where it cannot run in one transaction."
        ));
    }
    let count = m.statements.len();
    if count == 0 {
        out.comment("Nothing to migrate: the schemas are the same.");
    } else {
        let flagged: Vec<String> = [
            (
                Hazard::DeletesData,
                "delete data (DESTRUCTIVE)",
                "deletes data (DESTRUCTIVE)",
            ),
            (
                Hazard::CanFail,
                "can fail on existing rows (CAN FAIL)",
                "can fail on existing rows (CAN FAIL)",
            ),
            (
                Hazard::WidensAccess,
                "widen access (WIDENS ACCESS)",
                "widens access (WIDENS ACCESS)",
            ),
        ]
        .iter()
        .filter_map(|(hazard, many, one)| match m.count(*hazard) {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        })
        .collect();
        let line = if flagged.is_empty() {
            format!(
                "{count} statement{}, none flagged.",
                if count == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{count} statement{}: {}, each marked.",
                if count == 1 { "" } else { "s" },
                flagged.join(", ")
            )
        };
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
        .any(|s| s.section == Section::Removals && s.hazard == Some(Hazard::DeletesData));
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
    if out.compact {
        if section == Section::Removals && destroys {
            out.comment("Removals. DESTRUCTIVE: what follows deletes data. Back up first.");
        } else {
            out.line(&format!("-- {}", section.title()));
        }
        out.blank();
        return;
    }
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

/// The word a statement's comment starts with, for its hazard.
fn label(hazard: Hazard) -> &'static str {
    match hazard {
        Hazard::DeletesData => "DESTRUCTIVE",
        Hazard::CanFail => "CAN FAIL",
        Hazard::WidensAccess => "WIDENS ACCESS",
    }
}

fn statement(out: &mut Writer, s: &Statement) {
    let start = out.line_number();
    match (&s.note, s.hazard) {
        (Some(note), Some(hazard)) => out.comment(&format!("{}: {note}", label(hazard))),
        (None, Some(hazard)) => out.comment(&format!("{}.", label(hazard))),
        (Some(note), None) => out.comment(note),
        (None, None) => {}
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
    if let Some(hazard) = s.hazard {
        out.marks.push(Mark {
            from: start,
            to: out.line_number() - 1,
            hazard,
        });
    }
    out.blank();
}

/// Text with line numbers kept as it grows.
struct Writer {
    text: String,
    lines: u32,
    marks: Vec<Mark>,
    /// Comments wrap before this many columns (after `-- `).
    width: usize,
    /// Headings without their rules.
    compact: bool,
}

impl Writer {
    fn new() -> Self {
        Self {
            text: String::new(),
            lines: 0,
            marks: Vec::new(),
            width: 75,
            compact: false,
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
        for line in wrap(text, self.width) {
            self.line(&format!("-- {line}"));
        }
    }

    /// A bulleted comment: `--   - text`, continued under the text.
    fn comment_item(&mut self, text: &str) {
        for (i, line) in wrap(text, self.width - 4).into_iter().enumerate() {
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
            marks: self.marks,
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

/// The version `steps` after `version`: a `YYYYMMDDHHMMSS` timestamp a
/// second per step later, another number plus `steps` (its zero padding
/// kept), anything else with `.<steps>` appended.
pub(crate) fn next_version(version: &str, steps: usize) -> String {
    if steps == 0 {
        return version.to_owned();
    }
    if version.len() == 14 && version.bytes().all(|b| b.is_ascii_digit()) {
        return add_seconds(version, steps as u64);
    }
    match version.parse::<u128>() {
        Ok(n) if version.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{:0width$}", n + steps as u128, width = version.len())
        }
        _ => format!("{version}.{steps}"),
    }
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
    use super::{Format, add_seconds, slug, wrap};

    #[test]
    fn wire_names_are_the_serialized_ones() {
        for format in Format::ALL {
            assert_eq!(
                serde_json::to_value(format).unwrap(),
                format.as_str(),
                "{format:?}"
            );
        }
    }

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
