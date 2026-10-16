//! Migration files → an ordered list of migrations: the format detected
//! from the file set (or chosen), each tool's naming and ordering, up
//! sections only, and what was left out and why.

use std::{cmp::Ordering, collections::HashMap};

/// The tools romulus also writes migrations for (`romulus_migration`):
/// their one list, with their wire and display names.
pub use romulus_migration::Format as Tool;
use serde::{Deserialize, Serialize};

use super::{export, lexer};

/// A migration tool's layout: one romulus also generates migrations for,
/// or one it only reads. Serialized as the tool's wire name
/// (`golang_migrate`, `alembic`).
///
/// | format | files |
/// |---|---|
/// | `plain` | ordered `.sql` files (any names, natural order) |
/// | `sqlx` | `<version>_<name>.sql`, or `.up.sql`/`.down.sql` pairs |
/// | `flyway` | `V<version>__<name>.sql`; repeatable (`R__`), undo (`U`) and baseline (`B`) files left out |
/// | `prisma` | `<timestamp>_<name>/migration.sql` |
/// | `dbmate` | `<version>_<name>.sql` with `-- migrate:up` / `-- migrate:down` |
/// | `golang_migrate` | `<version>_<name>.up.sql` / `.down.sql` |
/// | `node_pg_migrate` | `<timestamp>_<name>.sql` with `-- Up Migration` / `-- Down Migration` |
/// | `alembic` | `alembic upgrade head --sql`, split on `-- Running upgrade a -> b` |
/// | `django` | `manage.py sqlmigrate` per migration, each under `-- <app>.<name>` (scripts/export-migrations/django.sh) |
/// | `rails` | the SQL each migration ran, each under `-- <version>_<name>.rb` (scripts/export-migrations/rails.sh) |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Format {
    Tool(Tool),
    Read(ReadOnly),
}

/// Tools romulus reads migrations of but does not write them for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnly {
    NodePgMigrate,
    Alembic,
    Django,
    Rails,
}

impl ReadOnly {
    pub const ALL: [Self; 4] = [
        Self::NodePgMigrate,
        Self::Alembic,
        Self::Django,
        Self::Rails,
    ];
}

impl Format {
    /// Every format: the tools first, in their order.
    pub fn all() -> impl Iterator<Item = Self> {
        Tool::ALL
            .into_iter()
            .map(Self::Tool)
            .chain(ReadOnly::ALL.into_iter().map(Self::Read))
    }

    /// The wire name (`golang_migrate`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool(tool) => tool.as_str(),
            Self::Read(ReadOnly::NodePgMigrate) => "node_pg_migrate",
            Self::Read(ReadOnly::Alembic) => "alembic",
            Self::Read(ReadOnly::Django) => "django",
            Self::Read(ReadOnly::Rails) => "rails",
        }
    }

    /// How people write it (`golang-migrate`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Tool(tool) => tool.label(),
            Self::Read(ReadOnly::NodePgMigrate) => "node-pg-migrate",
            Self::Read(ReadOnly::Alembic) => "Alembic",
            Self::Read(ReadOnly::Django) => "Django",
            Self::Read(ReadOnly::Rails) => "Rails",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::all().find(|f| f.as_str() == name)
    }

    /// Formats whose files are the SQL a tool printed (one file holding
    /// every migration), not migrations as written.
    pub fn is_exported(self) -> bool {
        matches!(
            self,
            Self::Read(ReadOnly::Alembic | ReadOnly::Django | ReadOnly::Rails)
        )
    }
}

/// A file as uploaded: its path inside the folder or zip, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
        }
    }
}

/// One migration, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Migration {
    /// What identifies it across imports: the tool's version
    /// (`20240101120000`, `1.2`, `blog.0002_post_slug`), or the file name
    /// without `.sql` for plain folders (whatever folder was picked).
    pub id: String,
    /// The version's message: the file (or folder) name.
    pub name: String,
    /// The file it comes from.
    pub path: String,
    /// The line of `path` where `sql` starts (1 for a whole file).
    pub line: u32,
    /// What is replayed: the up section, line breaks normalised.
    pub sql: String,
}

/// Why a file is not a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum IgnoreReason {
    /// Not SQL (README, lock file, a Ruby or Python migration…).
    NotSql,
    /// A down (or undo) migration: only up migrations are replayed.
    Down,
    /// Flyway's repeatable (`R__`) migration: re-run whenever it changes,
    /// outside the versioned history.
    Repeatable,
    /// Flyway's baseline (`B`) migration: a snapshot that replaces earlier
    /// ones on a new database.
    Baseline,
    /// The name does not follow the format's pattern.
    Unrecognized,
    /// The tool's own bookkeeping (Prisma's `migration_lock.toml`, Alembic's
    /// version table before the first migration).
    Tool,
}

/// A file (or part of one) left out of the import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Ignored {
    pub path: String,
    pub reason: IgnoreReason,
}

/// The migrations of a file set, ordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The format used.
    pub format: Format,
    /// The format the file set looks like (`format` unless overridden).
    pub detected: Format,
    pub migrations: Vec<Migration>,
    pub ignored: Vec<Ignored>,
}

/// Why a file set cannot be imported as given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("no migration found: {0}")]
    Empty(String),
    #[error("{}", duplicate_message(*format, id, first, second))]
    Duplicate {
        format: Format,
        id: String,
        first: String,
        second: String,
    },
    #[error("{path}: {message}")]
    Malformed { path: String, message: String },
}

/// Two migrations with one id, and how to resolve it.
fn duplicate_message(format: Format, id: &str, first: &str, second: &str) -> String {
    match format {
        Format::Tool(Tool::Plain) => format!(
            "two files are named {id}: {first} and {second}. Plain migrations are known by their \
             file name, so it must be unique: rename one, or import only the folder you mean"
        ),
        Format::Tool(Tool::Prisma) => format!(
            "two migration folders are named {id}: {first} and {second}. Import only the \
             prisma/migrations folder you mean"
        ),
        _ => format!(
            "two migrations have the version {id}: {first} and {second}. Each version must be \
             unique: renumber one, or import only the folder you mean"
        ),
    }
}

/// Paths that are never migrations, whatever the format: macOS and editor
/// litter, hidden files.
fn is_litter(path: &str) -> bool {
    path.split('/')
        .any(|part| part.starts_with('.') && part != "." && part != ".." || part == "__MACOSX")
        || path.ends_with('~')
}

/// `migrations/2024/x.sql` → `2024/x.sql` when every file shares
/// `migrations/`: paths are shown from the folder picked.
fn strip_common_dir(files: &mut [SourceFile]) {
    loop {
        let Some(first) = files.first().and_then(|f| f.path.split_once('/')) else {
            return;
        };
        let prefix = format!("{}/", first.0);
        if !files.iter().all(|f| f.path.starts_with(&prefix)) {
            return;
        }
        for file in files.iter_mut() {
            file.path = file.path[prefix.len()..].to_owned();
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `001_x.sql` → (`001`, `x`) when `001` is digits and `sep` follows them.
fn version_prefix<'a>(name: &'a str, sep: &[char]) -> Option<(&'a str, &'a str)> {
    let digits = name.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &name[digits..];
    let rest = rest.strip_prefix(|c| sep.contains(&c))?;
    Some((&name[..digits], rest))
}

/// Digits without leading zeros (`0042` and `42` are one version for sqlx
/// and golang-migrate, which read it as a number).
fn numeric_id(digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }.to_owned()
}

fn compare_numeric(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Natural order: runs of digits compare as numbers (`2_x` before `10_x`).
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let da = a.bytes().take_while(u8::is_ascii_digit).count();
        let db = b.bytes().take_while(u8::is_ascii_digit).count();
        let ordering = if da > 0 && db > 0 {
            compare_numeric(&a[..da], &b[..db]).then_with(|| da.cmp(&db))
        } else {
            let ca = a.chars().next().unwrap_or_default();
            let cb = b.chars().next().unwrap_or_default();
            ca.cmp(&cb)
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
        let (sa, sb) = if da > 0 && db > 0 {
            (da, db)
        } else {
            (
                a.chars().next().map_or(0, char::len_utf8),
                b.chars().next().map_or(0, char::len_utf8),
            )
        };
        a = &a[sa..];
        b = &b[sb..];
    }
}

/// A Flyway version: `1.2.3`, `1_2`, compared part by part as numbers,
/// trailing zeros ignored (`1.0` is `1`).
fn flyway_version(raw: &str) -> Option<Vec<u64>> {
    let mut parts: Vec<u64> = raw
        .split(['.', '_'])
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<_>>()?;
    while parts.len() > 1 && parts.last() == Some(&0) {
        parts.pop();
    }
    Some(parts)
}

fn is_sql(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

fn is_flyway_versioned(name: &str) -> bool {
    name.strip_prefix('V')
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(version, _)| flyway_version(version).is_some())
        && is_sql(name)
}

/// The format a file set looks like.
pub fn detect(files: &[SourceFile]) -> Format {
    let files: Vec<&SourceFile> = files.iter().filter(|f| !is_litter(&f.path)).collect();
    let names: Vec<&str> = files.iter().map(|f| file_name(&f.path)).collect();
    if names
        .iter()
        .any(|n| *n == "migration.sql" || *n == "migration_lock.toml")
        && files
            .iter()
            .any(|f| f.path.ends_with("/migration.sql") && f.path.contains('_'))
    {
        return Format::Tool(Tool::Prisma);
    }
    if names.iter().any(|n| is_flyway_versioned(n)) {
        return Format::Tool(Tool::Flyway);
    }
    let sql: Vec<&SourceFile> = files.iter().copied().filter(|f| is_sql(&f.path)).collect();
    if let Some(format) = sql.iter().find_map(|f| export::sniff(&f.text)) {
        return format;
    }
    if sql.iter().any(|f| {
        export::marker_lines(&f.text, is_dbmate_marker)
            .next()
            .is_some()
    }) {
        return Format::Tool(Tool::Dbmate);
    }
    if sql.iter().any(|f| {
        export::marker_lines(&f.text, is_node_pg_marker)
            .next()
            .is_some()
    }) {
        return Format::Read(ReadOnly::NodePgMigrate);
    }
    let ups: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| has_suffix(n, ".up.sql"))
        .collect();
    if !ups.is_empty() && ups.iter().all(|n| version_prefix(n, &['_']).is_some()) {
        let timestamps = ups
            .iter()
            .all(|n| version_prefix(n, &['_']).is_some_and(|(v, _)| v.len() == 14));
        return if timestamps {
            Format::Tool(Tool::Sqlx)
        } else {
            Format::Tool(Tool::GolangMigrate)
        };
    }
    let versioned = sql
        .iter()
        .filter(|f| version_prefix(file_name(&f.path), &['_']).is_some())
        .count();
    if versioned > 0 && versioned == sql.len() {
        return Format::Tool(Tool::Sqlx);
    }
    Format::Tool(Tool::Plain)
}

fn has_suffix(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

pub(crate) fn is_dbmate_marker(line: &str) -> Option<Section> {
    let rest = line.trim().strip_prefix("--")?.trim_start();
    let rest = rest.strip_prefix("migrate:")?;
    if rest.starts_with("up") {
        Some(Section::Up)
    } else if rest.starts_with("down") {
        Some(Section::Down)
    } else {
        None
    }
}

/// node-pg-migrate's `-- Up Migration` / `-- Down Migration` (its loader
/// is case-insensitive about them).
pub(crate) fn is_node_pg_marker(line: &str) -> Option<Section> {
    let rest = line.trim().strip_prefix("--")?;
    let words: Vec<String> = rest
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    match words.as_slice() {
        [up, migration] if up == "up" && migration == "migration" => Some(Section::Up),
        [down, migration] if down == "down" && migration == "migration" => Some(Section::Down),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Up,
    Down,
}

/// The up section of a file with section markers: the text after the up
/// marker (the whole file when there is none and `up_required` is false),
/// up to the down marker. The returned text keeps everything before the
/// section as blank lines, so line numbers stay the file's.
fn up_section(
    text: &str,
    marker: fn(&str) -> Option<Section>,
    up_required: bool,
) -> Result<String, String> {
    let mut up = None;
    let mut down = None;
    for (range, section) in export::marker_lines(text, marker) {
        match section {
            Section::Up if up.is_none() && down.is_none() => up = Some(range),
            Section::Down if down.is_none() => down = Some(range),
            Section::Up => return Err("it has a second up section".to_owned()),
            Section::Down => return Err("it has a second down section".to_owned()),
        }
    }
    if up.is_none() && up_required {
        return Err("it has no `-- migrate:up` section".to_owned());
    }
    let start = up.as_ref().map_or(0, |r| r.end);
    let end = down.as_ref().map_or(text.len(), |r| r.start);
    if end < start {
        return Err("its down section comes before its up section".to_owned());
    }
    let mut kept = lexer::blank(text, std::iter::once(0..start));
    kept.truncate(end);
    Ok(kept)
}

/// Line breaks as `\n`, no byte order mark: one file has one checksum
/// whichever system checked it out.
pub fn normalize(text: &str) -> String {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    text.replace("\r\n", "\n")
}

struct Candidate {
    id: String,
    name: String,
    path: String,
    line: u32,
    sql: String,
    order: OrderKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OrderKey {
    Number(String),
    Dotted(Vec<u64>),
    Natural(String),
    Position(usize),
}

impl OrderKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => compare_numeric(a, b),
            (Self::Dotted(a), Self::Dotted(b)) => a.cmp(b),
            (Self::Natural(a), Self::Natural(b)) => natural_cmp(a, b),
            (Self::Position(a), Self::Position(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

/// The ordered migrations of a file set, in `format` (detected when
/// `None`). Files are normalised ([`normalize`]); their paths keep only
/// what follows the folder they all share.
#[allow(clippy::too_many_lines)]
pub fn plan(files: Vec<SourceFile>, format: Option<Format>) -> Result<Plan, PlanError> {
    let mut files: Vec<SourceFile> = files
        .into_iter()
        .map(|f| SourceFile {
            path: f
                .path
                .replace('\\', "/")
                .trim_start_matches("./")
                .to_owned(),
            text: normalize(&f.text),
        })
        .filter(|f| !f.path.is_empty() && !f.path.ends_with('/'))
        .collect();
    let mut ignored: Vec<Ignored> = Vec::new();
    files.retain(|f| !is_litter(&f.path));
    strip_common_dir(&mut files);
    files.sort_by(|a, b| natural_cmp(&a.path, &b.path));
    let detected = detect(&files);
    let format = format.unwrap_or(detected);

    let mut candidates = Vec::new();
    for (position, file) in files.into_iter().enumerate() {
        let name = file_name(&file.path).to_owned();
        let ignore = |reason| Ignored {
            path: file.path.clone(),
            reason,
        };
        if format == Format::Tool(Tool::Prisma) {
            if name == "migration_lock.toml" {
                ignored.push(ignore(IgnoreReason::Tool));
                continue;
            }
            let folder = file.path.rsplit('/').nth(1);
            match folder {
                Some(folder) if name == "migration.sql" => candidates.push(Candidate {
                    id: folder.to_owned(),
                    name: folder.to_owned(),
                    order: OrderKey::Natural(folder.to_owned()),
                    path: file.path,
                    line: 1,
                    sql: file.text,
                }),
                _ => ignored.push(ignore(if is_sql(&name) {
                    IgnoreReason::Unrecognized
                } else {
                    IgnoreReason::NotSql
                })),
            }
            continue;
        }
        if !is_sql(&name) {
            ignored.push(ignore(IgnoreReason::NotSql));
            continue;
        }
        match format {
            Format::Tool(Tool::Prisma) => unreachable!("handled above"),
            Format::Read(ReadOnly::Alembic)
            | Format::Read(ReadOnly::Django)
            | Format::Read(ReadOnly::Rails) => {
                let split =
                    export::split(format, &file.text).map_err(|message| PlanError::Malformed {
                        path: file.path.clone(),
                        message,
                    })?;
                if split.preamble_is_tool {
                    ignored.push(ignore(IgnoreReason::Tool));
                }
                for chunk in split.chunks {
                    candidates.push(Candidate {
                        id: chunk.id,
                        name: chunk.name,
                        path: file.path.clone(),
                        line: chunk.line,
                        sql: chunk.sql,
                        order: OrderKey::Position(position * 1_000_000 + candidates.len()),
                    });
                }
            }
            Format::Tool(Tool::Plain) => {
                if has_suffix(&name, ".down.sql") {
                    ignored.push(ignore(IgnoreReason::Down));
                    continue;
                }
                // The name, not the path: the same file has the same id
                // whichever folder (or files) an import picks.
                let id = strip_sql_suffix(&name);
                candidates.push(Candidate {
                    order: OrderKey::Natural(file.path.clone()),
                    id,
                    name,
                    path: file.path,
                    line: 1,
                    sql: file.text,
                });
            }
            Format::Tool(Tool::Sqlx) | Format::Tool(Tool::GolangMigrate) => {
                if has_suffix(&name, ".down.sql") {
                    ignored.push(ignore(IgnoreReason::Down));
                    continue;
                }
                let Some((version, _)) = version_prefix(&name, &['_']) else {
                    ignored.push(ignore(IgnoreReason::Unrecognized));
                    continue;
                };
                if format == Format::Tool(Tool::GolangMigrate) && !has_suffix(&name, ".up.sql") {
                    ignored.push(ignore(IgnoreReason::Unrecognized));
                    continue;
                }
                candidates.push(Candidate {
                    id: numeric_id(version),
                    order: OrderKey::Number(version.to_owned()),
                    name,
                    path: file.path,
                    line: 1,
                    sql: file.text,
                });
            }
            Format::Tool(Tool::Dbmate) | Format::Read(ReadOnly::NodePgMigrate) => {
                let separators: &[char] = if format == Format::Tool(Tool::Dbmate) {
                    &['_']
                } else {
                    &['_', '-']
                };
                let Some((version, _)) = version_prefix(&name, separators) else {
                    ignored.push(ignore(IgnoreReason::Unrecognized));
                    continue;
                };
                let sql = if format == Format::Tool(Tool::Dbmate) {
                    up_section(&file.text, is_dbmate_marker, true)
                } else {
                    up_section(&file.text, is_node_pg_marker, false)
                }
                .map_err(|message| PlanError::Malformed {
                    path: file.path.clone(),
                    message,
                })?;
                candidates.push(Candidate {
                    id: version.to_owned(),
                    order: OrderKey::Number(version.to_owned()),
                    name,
                    path: file.path,
                    line: 1,
                    sql,
                });
            }
            Format::Tool(Tool::Flyway) => {
                let reason = match name.chars().next() {
                    Some('R') if name.starts_with("R__") => Some(IgnoreReason::Repeatable),
                    Some('U') if flyway_prefixed(&name, 'U') => Some(IgnoreReason::Down),
                    Some('B') if flyway_prefixed(&name, 'B') => Some(IgnoreReason::Baseline),
                    _ if !is_flyway_versioned(&name) => Some(IgnoreReason::Unrecognized),
                    _ => None,
                };
                if let Some(reason) = reason {
                    ignored.push(ignore(reason));
                    continue;
                }
                let (version, _) = name[1..].split_once("__").unwrap_or_default();
                let parts = flyway_version(version).unwrap_or_default();
                let id = parts
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                candidates.push(Candidate {
                    id,
                    order: OrderKey::Dotted(parts),
                    name,
                    path: file.path,
                    line: 1,
                    sql: file.text,
                });
            }
        }
    }

    candidates.sort_by(|a, b| a.order.cmp(&b.order));
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for candidate in &candidates {
        if let Some(first) = seen.insert(&candidate.id, &candidate.path) {
            return Err(PlanError::Duplicate {
                format,
                id: candidate.id.clone(),
                first: first.to_owned(),
                second: candidate.path.clone(),
            });
        }
    }
    if candidates.is_empty() {
        return Err(PlanError::Empty(match format {
            Format::Tool(Tool::Prisma) => {
                "Prisma keeps each migration in <timestamp>_<name>/migration.sql.".to_owned()
            }
            Format::Tool(Tool::Flyway) => {
                "Flyway migrations are named V<version>__<name>.sql.".to_owned()
            }
            f if f.is_exported() => format!(
                "this is not {} output: run scripts/export-migrations/{}.sh and import the file it writes.",
                f.label(),
                f.as_str()
            ),
            _ => "pick the folder that holds the .sql migration files.".to_owned(),
        }));
    }
    Ok(Plan {
        format,
        detected,
        migrations: candidates
            .into_iter()
            .map(|c| Migration {
                id: c.id,
                name: c.name,
                path: c.path,
                line: c.line,
                sql: c.sql,
            })
            .collect(),
        ignored,
    })
}

fn flyway_prefixed(name: &str, prefix: char) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(version, _)| flyway_version(version).is_some())
}

/// `x.up.sql` → `x`.
fn strip_sql_suffix(name: &str) -> String {
    let stem = &name[..name.len() - 4];
    if has_suffix(stem, ".up") {
        stem[..stem.len() - 3].to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<SourceFile> {
        list.iter().map(|(p, t)| SourceFile::new(*p, *t)).collect()
    }

    fn ids(plan: &Plan) -> Vec<&str> {
        plan.migrations.iter().map(|m| m.id.as_str()).collect()
    }

    fn ignored(plan: &Plan) -> Vec<(&str, IgnoreReason)> {
        plan.ignored
            .iter()
            .map(|i| (i.path.as_str(), i.reason))
            .collect()
    }

    #[test]
    fn formats_go_by_their_wire_names_tools_first() {
        let names: Vec<&str> = Format::all().map(Format::as_str).collect();
        assert_eq!(
            names,
            [
                "plain",
                "sqlx",
                "flyway",
                "prisma",
                "dbmate",
                "golang_migrate",
                "node_pg_migrate",
                "alembic",
                "django",
                "rails"
            ]
        );
        for format in Format::all() {
            let json = serde_json::to_value(format).unwrap();
            assert_eq!(json, format.as_str());
            assert_eq!(serde_json::from_value::<Format>(json).unwrap(), format);
            assert_eq!(Format::parse(format.as_str()), Some(format));
        }
    }

    #[test]
    fn natural_order_compares_numbers_as_numbers() {
        let mut names = vec!["10_c.sql", "2_b.sql", "1_a.sql", "002_a.sql", "a.sql"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            ["1_a.sql", "2_b.sql", "002_a.sql", "10_c.sql", "a.sql"]
        );
    }

    #[test]
    fn sqlx_orders_by_version_and_leaves_down_files_out() {
        let set = files(&[
            (
                "migrations/20240102000000_posts.up.sql",
                "CREATE TABLE posts ();",
            ),
            (
                "migrations/20240102000000_posts.down.sql",
                "DROP TABLE posts;",
            ),
            (
                "migrations/20240101000000_users.up.sql",
                "CREATE TABLE users ();",
            ),
            ("migrations/README.md", "# notes"),
            ("migrations/.DS_Store", "\0"),
        ]);
        assert_eq!(detect(&set), Format::Tool(Tool::Sqlx));
        let plan = plan(set, None).unwrap();
        assert_eq!(plan.format, Format::Tool(Tool::Sqlx));
        assert_eq!(ids(&plan), ["20240101000000", "20240102000000"]);
        assert_eq!(plan.migrations[0].name, "20240101000000_users.up.sql");
        assert_eq!(plan.migrations[0].path, "20240101000000_users.up.sql");
        assert_eq!(
            ignored(&plan),
            [
                ("20240102000000_posts.down.sql", IgnoreReason::Down),
                ("README.md", IgnoreReason::NotSql),
            ]
        );
    }

    #[test]
    fn golang_migrate_uses_sequence_numbers() {
        let set = files(&[
            ("000010_tags.up.sql", "x"),
            ("000002_posts.up.sql", "x"),
            ("000002_posts.down.sql", "x"),
            ("000001_users.up.sql", "x"),
        ]);
        assert_eq!(detect(&set), Format::Tool(Tool::GolangMigrate));
        assert_eq!(ids(&plan(set, None).unwrap()), ["1", "2", "10"]);
    }

    #[test]
    fn flyway_compares_dotted_versions_and_skips_repeatable_undo_and_baseline() {
        let set = files(&[
            ("sql/V1__init.sql", "x"),
            ("sql/V1.10__ten.sql", "x"),
            ("sql/V1.2__two.sql", "x"),
            ("sql/V2_0_1__patch.sql", "x"),
            ("sql/R__views.sql", "x"),
            ("sql/U1.2__undo_two.sql", "x"),
            ("sql/B2__baseline.sql", "x"),
            ("sql/beforeMigrate.sql", "x"),
        ]);
        assert_eq!(detect(&set), Format::Tool(Tool::Flyway));
        let plan = plan(set, None).unwrap();
        assert_eq!(ids(&plan), ["1", "1.2", "1.10", "2.0.1"]);
        assert_eq!(
            ignored(&plan),
            [
                ("B2__baseline.sql", IgnoreReason::Baseline),
                ("R__views.sql", IgnoreReason::Repeatable),
                ("U1.2__undo_two.sql", IgnoreReason::Down),
                ("beforeMigrate.sql", IgnoreReason::Unrecognized),
            ]
        );
        // V1.0 and V1 are the same version.
        let clash = files(&[("V1__a.sql", "x"), ("V1.0__b.sql", "x")]);
        assert!(matches!(
            plan_of(clash),
            Err(PlanError::Duplicate { id, .. }) if id == "1"
        ));
    }

    fn plan_of(set: Vec<SourceFile>) -> Result<Plan, PlanError> {
        plan(set, None)
    }

    #[test]
    fn prisma_takes_each_folder_s_migration_sql() {
        let set = files(&[
            (
                "prisma/migrations/migration_lock.toml",
                "provider = \"postgresql\"",
            ),
            ("prisma/migrations/20240102000000_posts/migration.sql", "x"),
            ("prisma/migrations/20240101000000_init/migration.sql", "x"),
        ]);
        assert_eq!(detect(&set), Format::Tool(Tool::Prisma));
        let plan = plan(set, None).unwrap();
        assert_eq!(ids(&plan), ["20240101000000_init", "20240102000000_posts"]);
        assert_eq!(
            plan.migrations[1].path,
            "20240102000000_posts/migration.sql"
        );
        assert_eq!(
            ignored(&plan),
            [("migration_lock.toml", IgnoreReason::Tool)]
        );
    }

    #[test]
    fn dbmate_replays_the_up_section_on_the_file_s_lines() {
        let set = files(&[(
            "db/migrations/20240101000000_users.sql",
            "-- migrate:up\r\nCREATE TABLE users ();\r\n\r\n-- migrate:down\r\nDROP TABLE users;\r\n",
        )]);
        assert_eq!(detect(&set), Format::Tool(Tool::Dbmate));
        let dbmate = plan(set, None).unwrap();
        let sql = &dbmate.migrations[0].sql;
        assert_eq!(sql.trim(), "CREATE TABLE users ();");
        // The marker line is blanked, not removed: line 2 is still line 2.
        assert_eq!(sql.lines().nth(1), Some("CREATE TABLE users ();"));
        assert!(!sql.contains("DROP"));

        let missing = files(&[("20240101000000_x.sql", "CREATE TABLE x ();")]);
        assert!(matches!(
            plan(missing, Some(Format::Tool(Tool::Dbmate))),
            Err(PlanError::Malformed { .. })
        ));
    }

    #[test]
    fn node_pg_migrate_markers_are_optional_for_up() {
        let set = files(&[
            (
                "migrations/1700000000000_create-users.sql",
                "-- Up Migration\nCREATE TABLE users ();\n-- Down Migration\nDROP TABLE users;",
            ),
            (
                "migrations/1700000000001_posts.sql",
                "CREATE TABLE posts ();",
            ),
        ]);
        assert_eq!(detect(&set), Format::Read(ReadOnly::NodePgMigrate));
        let plan = plan(set, None).unwrap();
        assert_eq!(ids(&plan), ["1700000000000", "1700000000001"]);
        assert!(!plan.migrations[0].sql.contains("DROP"));
        assert_eq!(plan.migrations[1].sql, "CREATE TABLE posts ();");
    }

    #[test]
    fn a_marker_inside_a_function_body_is_not_a_section() {
        let text = "-- migrate:up\nCREATE FUNCTION f() RETURNS text LANGUAGE sql AS $$\nSELECT '\n-- migrate:down\n'\n$$;\n-- migrate:down\nDROP FUNCTION f();\n";
        let up = up_section(text, is_dbmate_marker, true).unwrap();
        assert!(up.contains("SELECT '\n-- migrate:down\n'"));
        assert!(!up.contains("DROP FUNCTION"));
    }

    #[test]
    fn plain_folders_keep_natural_order_and_ids_from_file_names() {
        let set = files(&[
            ("schema/10_indexes.sql", "x"),
            ("schema/2_tables.sql", "x"),
            ("schema/1_types.sql", "x"),
            ("schema/1_types.down.sql", "x"),
        ]);
        assert_eq!(detect(&set), Format::Tool(Tool::Sqlx));
        let plan = plan(set, Some(Format::Tool(Tool::Plain))).unwrap();
        assert_eq!(plan.detected, Format::Tool(Tool::Sqlx));
        assert_eq!(ids(&plan), ["1_types", "2_tables", "10_indexes"]);
        let named = files(&[("b.sql", "x"), ("a.sql", "x")]);
        assert_eq!(detect(&named), Format::Tool(Tool::Plain));
        assert_eq!(ids(&plan_of(named).unwrap()), ["a", "b"]);
    }

    /// Appending works whether the next import picks the whole folder, a
    /// subfolder or a few files: the ids are the same.
    #[test]
    fn plain_ids_do_not_depend_on_what_was_picked() {
        let whole = files(&[
            ("db/2024/01_users.sql", "x"),
            ("db/2025/02_orders.up.sql", "x"),
        ]);
        let one = files(&[("2025/02_orders.up.sql", "x")]);
        let alone = files(&[("02_orders.up.sql", "x")]);
        assert_eq!(
            ids(&plan(whole, Some(Format::Tool(Tool::Plain))).unwrap()),
            ["01_users", "02_orders"]
        );
        assert_eq!(
            ids(&plan(one, Some(Format::Tool(Tool::Plain))).unwrap()),
            ["02_orders"]
        );
        assert_eq!(
            ids(&plan(alone, Some(Format::Tool(Tool::Plain))).unwrap()),
            ["02_orders"]
        );
        let twice = files(&[("2024/01_init.sql", "x"), ("2025/01_init.sql", "y")]);
        assert_eq!(
            plan(twice, Some(Format::Tool(Tool::Plain))),
            Err(PlanError::Duplicate {
                format: Format::Tool(Tool::Plain),
                id: "01_init".to_owned(),
                first: "2024/01_init.sql".to_owned(),
                second: "2025/01_init.sql".to_owned(),
            })
        );
        let message = plan(
            files(&[("2024/01_init.sql", "x"), ("2025/01_init.sql", "y")]),
            Some(Format::Tool(Tool::Plain)),
        )
        .unwrap_err()
        .to_string();
        assert!(
            message.contains("rename one, or import only the folder you mean"),
            "{message}"
        );
    }

    #[test]
    fn nothing_to_import_says_what_was_expected() {
        let err = plan(files(&[("notes.txt", "x")]), None).unwrap_err();
        assert!(matches!(err, PlanError::Empty(_)));
        let err = plan(
            files(&[("V1__x.txt", "x")]),
            Some(Format::Tool(Tool::Flyway)),
        )
        .unwrap_err();
        assert!(err.to_string().contains("V<version>__<name>.sql"));
    }
}
