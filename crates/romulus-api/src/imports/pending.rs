//! The migrations of a folder a database has not imported yet, as one
//! migration to test: what CI runs on rows before they merge (`romulus
//! test`, the MCP's `test_migration` with an upload).
//!
//! The import's own plan says which migrations are new (matched by id and
//! checksum with the ones imported, refused when one changed or sorts
//! before them); their up SQL, as the import reads it (data statements
//! kept: a test runs them), is laid one after the other, each under a
//! comment naming its file. Each migration says where it sits in that
//! text: its first line (the file's lines follow from it) and its
//! statements (a migration test numbers the text's statements from 0), so
//! a compile error or a failed statement is told against its file.

use romulus_sandbox::import::Migration;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{IgnoredFile, plan::Planned};
use crate::{collab::comments::is_comment_line, domain::MigrationFormat};

/// A folder's new migrations, as one migration.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NewMigrations {
    /// The format the files were read as.
    pub format: MigrationFormat,
    /// Migrations of the files the database imported already (removed ones
    /// included): the history the new ones apply to.
    pub imported: usize,
    /// The new ones, in order.
    pub migrations: Vec<NewMigration>,
    /// Files that are not migrations (down migrations, READMEs, the tool's
    /// own files).
    pub ignored: Vec<IgnoredFile>,
    /// Their up SQL one after the other, each under a `-- <path>` line;
    /// empty when there is no new migration.
    pub sql: String,
}

/// A new migration, and where it sits in [`NewMigrations::sql`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewMigration {
    /// The tool's version, or the file name for plain folders.
    pub id: String,
    pub name: String,
    pub path: String,
    /// The line of `path` its SQL starts at.
    pub line: u32,
    /// The line of `sql` its SQL starts at (1-based): line `sql_line + k`
    /// of `sql` is line `line + k` of `path`.
    pub sql_line: u32,
    /// How many lines of `sql` it takes, from `sql_line`.
    pub lines: u32,
    /// Its statements in `sql`, numbered from 0 as a migration test numbers
    /// them: `statements` of them from `first_statement`.
    pub first_statement: usize,
    pub statements: usize,
}

/// The new migrations of `planned` (the plan must have no problem: see
/// `plan::problem_of`).
pub fn compose(planned: &Planned) -> NewMigrations {
    let (sql, migrations) = lay_out(planned.new.iter().map(|&k| &planned.migrations[k]));
    NewMigrations {
        format: planned.format,
        imported: planned.preview.migrations.len() - planned.new.len(),
        migrations,
        ignored: planned.preview.ignored.clone(),
        sql,
    }
}

/// `migrations` one after the other, each under a comment naming its file.
fn lay_out<'a>(migrations: impl Iterator<Item = &'a Migration>) -> (String, Vec<NewMigration>) {
    let mut sql = String::new();
    let mut out = Vec::new();
    for migration in migrations {
        if !sql.is_empty() {
            sql.push('\n');
        }
        sql.push_str("-- ");
        sql.push_str(&one_line(&migration.path));
        sql.push('\n');
        let sql_line = line_count(&sql) + 1;
        let body = body(migration);
        sql.push_str(&body);
        out.push(NewMigration {
            id: migration.id.clone(),
            name: migration.name.clone(),
            path: migration.path.clone(),
            line: migration.line,
            sql_line,
            lines: line_count(&body),
            first_statement: 0,
            statements: 0,
        });
    }
    count_statements(&sql, &mut out);
    (sql, out)
}

/// A migration's SQL as it goes into the text: its lines (so line `k` is
/// still the file's line `line + k - 1`), trailing blank lines dropped,
/// ended by a `;` when its last statement has none (the next file's first
/// statement would run on into it), and with a line feed.
///
/// A line starting with exactly `---` and words is how a draft's text
/// holds a comment never posted, which does not compile: such lines get a
/// space (`-- -`), still comments, for the text to run as the files do.
fn body(migration: &Migration) -> String {
    let mut out = String::with_capacity(migration.sql.len() + 2);
    for line in migration.sql.trim_end().split('\n') {
        if is_comment_line(line) {
            out.push_str(&line.replacen("---", "-- -", 1));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    let parsed = romulus_sandbox::parse(&out);
    if let Some(last) = parsed.statements.last()
        && !out[last.span.end..].trim_start().starts_with(';')
    {
        out.push_str(";\n");
    }
    out
}

/// Each migration's statements, from the whole text's parse: a migration
/// test splits the text the same way.
fn count_statements(sql: &str, migrations: &mut [NewMigration]) {
    let parsed = romulus_sandbox::parse(sql);
    let mut index = 0;
    for migration in migrations.iter_mut() {
        let end = migration.sql_line + migration.lines;
        migration.first_statement = index;
        while parsed
            .statements
            .get(index)
            .is_some_and(|s| s.span.from.line < end)
        {
            index += 1;
        }
        migration.statements = index - migration.first_statement;
    }
}

fn line_count(text: &str) -> u32 {
    u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX)
}

/// A path on one line: it heads a migration as a comment.
fn one_line(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration(path: &str, line: u32, sql: &str) -> Migration {
        Migration {
            id: path.trim_end_matches(".sql").to_owned(),
            name: path.to_owned(),
            path: path.to_owned(),
            line,
            sql: sql.to_owned(),
        }
    }

    fn composed(migrations: &[Migration]) -> (String, Vec<NewMigration>) {
        lay_out(migrations.iter())
    }

    #[test]
    fn migrations_follow_each_other_under_their_file_names() {
        let (sql, migrations) = composed(&[
            migration(
                "2_orders.sql",
                1,
                "CREATE TABLE orders (id int);\nCREATE INDEX ON orders (id);\n\n\n",
            ),
            migration("3_refunds.sql", 1, "\nCREATE TABLE refunds (id int);\n"),
        ]);
        assert_eq!(
            sql,
            "-- 2_orders.sql\nCREATE TABLE orders (id int);\nCREATE INDEX ON orders (id);\n\n\
             -- 3_refunds.sql\n\nCREATE TABLE refunds (id int);\n"
        );
        assert_eq!(
            (migrations[0].sql_line, migrations[0].lines),
            (2, 2),
            "{migrations:?}"
        );
        assert_eq!((migrations[1].sql_line, migrations[1].lines), (6, 2));
        // Line 7 of the text is line 2 of 3_refunds.sql.
        assert_eq!(sql.lines().nth(6), Some("CREATE TABLE refunds (id int);"));
        assert_eq!(
            (migrations[0].first_statement, migrations[0].statements),
            (0, 2)
        );
        assert_eq!(
            (migrations[1].first_statement, migrations[1].statements),
            (2, 1)
        );
    }

    #[test]
    fn a_file_without_its_last_semicolon_does_not_run_into_the_next() {
        let (sql, migrations) = composed(&[
            migration("1_a.sql", 1, "CREATE TABLE a (id int)\n-- done\n"),
            migration("2_b.sql", 1, "CREATE TABLE b (id int);"),
        ]);
        assert!(sql.contains("-- done\n;\n"), "{sql}");
        assert_eq!(romulus_sandbox::parse(&sql).statements.len(), 2);
        assert_eq!(migrations[0].statements, 1);
        assert_eq!(migrations[1].statements, 1);
        assert_eq!(migrations[1].first_statement, 1);
    }

    #[test]
    fn unposted_comment_lines_become_plain_comments() {
        let (sql, _) = composed(&[migration(
            "1_a.sql",
            1,
            "--- the users\nCREATE TABLE users (id int);\n---------\n",
        )]);
        assert!(sql.contains("\n-- - the users\n"), "{sql}");
        // A ruler of dashes is not one: kept as is.
        assert!(sql.contains("\n---------\n"), "{sql}");
        assert!(sql.lines().all(|l| !is_comment_line(l)), "{sql}");
    }

    #[test]
    fn a_migration_of_comments_only_has_no_statement() {
        let (_, migrations) = composed(&[
            migration("1_a.sql", 1, "-- nothing yet\n"),
            migration("2_b.sql", 1, "CREATE TABLE b (id int);\n"),
        ]);
        assert_eq!(migrations[0].statements, 0);
        assert_eq!(
            (migrations[1].first_statement, migrations[1].statements),
            (0, 1)
        );
    }

    #[test]
    fn exported_files_keep_their_line_numbers() {
        // An Alembic export: the migration starts on line 12 of its file.
        let (sql, migrations) = composed(&[migration(
            "alembic.sql",
            12,
            "ALTER TABLE a ADD COLUMN b int;\n",
        )]);
        assert_eq!(migrations[0].line, 12);
        assert_eq!(migrations[0].sql_line, 2);
        assert!(sql.starts_with("-- alembic.sql\n"));
    }
}
