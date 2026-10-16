//! A draft written as a migration (docs/MVP.md "Review round 1", item 2):
//! what its compile runs, and where its errors are.
//!
//! The draft's schema is what its migration makes of its base version, so
//! the text compiled is the base version's SQL, then the migration
//! ([`script`]), through the same compile as any draft: the same allowlist
//! (not the import mode's), the same stubs and limits. Between the two,
//! the default `search_path` again (as an import's replay does between
//! migrations): a migration runs in a session of its own, whatever the
//! base's SQL set.
//!
//! A migration holds what a schema does not, and the compile skips it,
//! blanked in place (every byte offset stays the migration's), with a note:
//!
//! - **data statements** (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, `SELECT`,
//!   `TRUNCATE`, `REFRESH MATERIALIZED VIEW`): a backfill between adding a
//!   column and making it `NOT NULL`, the `setval` romulus's own generated
//!   migrations hold. They cannot change the schema; skipped, they never
//!   run either;
//! - **`BEGIN` / `COMMIT`** around the migration: the compile runs it in a
//!   transaction of its own;
//! - **`CONCURRENTLY`** of `CREATE INDEX`, `DROP INDEX` and `DETACH
//!   PARTITION`, the keyword alone: it cannot run inside the compile's
//!   transaction, and the schema is the same without it.
//!
//! Everything else goes to the allowlist as written: `DO` blocks, functions,
//! `COPY` and the rest are refused there, located in the migration.
//!
//! [`Script::relocate`] maps the compile's diagnostics back onto the
//! migration's text. An error in the base's part (a base that no longer
//! compiles, or a stub guessed differently for the two) is put at the
//! migration's start, saying so; notes about the base are dropped; a text
//! over the size limit says that the base counts.
//!
//! [`faithful_sql`] is SQL that compiles to exactly what the script did: the
//! base's SQL, then what of the migration runs. A version published from a
//! migration (or a draft going back to schema mode) keeps its schema as
//! canonical SQL when that compiles back to the same model, and as this
//! otherwise: canonical SQL has gaps (foreign tables, `NOT VALID` checks…),
//! and a source that says less than its model would lose objects at the next
//! draft.

use pg_query::{
    NodeEnum,
    protobuf::{Token as TokenKind, TransactionStmtKind},
};

use crate::{
    diagnostic::{Code, Diagnostic, Severity},
    parse::parse,
    source::LineIndex,
};

/// Between the base's SQL and the migration: ends the base's last
/// statement, and resets what it set.
const SEPARATOR: &str = "\n;\nSET search_path TO \"$user\", public;\n";

/// The text a migration draft compiles, and how to read its diagnostics.
#[derive(Debug, Clone)]
pub struct Script {
    /// The base's SQL, then the migration with what is not run blanked.
    pub text: String,
    /// Where the migration starts in `text`.
    pub offset: usize,
    /// What was not run, located in the migration.
    pub notes: Vec<Diagnostic>,
}

/// What compiling a migration on top of `base` (the base version's SQL,
/// empty for a database without versions) runs.
pub fn script(base: &str, migration: &str) -> Script {
    let (runnable, notes) = runnable(migration);
    let mut text = String::with_capacity(base.len() + SEPARATOR.len() + runnable.len());
    if !base.trim().is_empty() {
        text.push_str(base);
        text.push_str(SEPARATOR);
    }
    let offset = text.len();
    text.push_str(&runnable);
    Script {
        text,
        offset,
        notes,
    }
}

impl Script {
    /// The compile's diagnostics of [`Script::text`], located in
    /// `migration` (the text [`script`] was given), with the notes about
    /// what was not run after them.
    pub fn relocate(&self, migration: &str, diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
        let lines = LineIndex::new(migration);
        let mut out: Vec<Diagnostic> = Vec::with_capacity(diagnostics.len() + self.notes.len());
        for mut diagnostic in diagnostics {
            // Statement indices count the base's statements: not the
            // migration's to show.
            diagnostic.statement = None;
            if diagnostic.code == Code::TooLarge && self.offset > 0 {
                let detail = diagnostic.message.clone();
                diagnostic.message = format!(
                    "The base version's SQL ({} KiB) and this migration together are larger than the compiler takes.",
                    self.offset.div_ceil(1024)
                );
                diagnostic.detail = Some(detail);
                diagnostic.hint = Some(
                    "The base counts: start the next version from a smaller schema, or shorten the migration.".into(),
                );
            }
            match diagnostic.span {
                Some(span) if span.start >= self.offset => {
                    let range = span.start - self.offset..span.end.saturating_sub(self.offset);
                    diagnostic.span = Some(lines.span(migration, range));
                    out.push(diagnostic);
                }
                Some(_) if diagnostic.severity == Severity::Info => {}
                Some(_) => {
                    diagnostic.span = Some(lines.span(migration, 0..0));
                    diagnostic.detail = Some(match diagnostic.detail.take() {
                        Some(detail) => {
                            format!("In the base version's SQL, before the migration. {detail}")
                        }
                        None => "In the base version's SQL, before the migration.".to_owned(),
                    });
                    out.push(diagnostic);
                }
                None => out.push(diagnostic),
            }
        }
        out.extend(self.notes.iter().cloned());
        out
    }
}

/// What of a migration the compile does not run: whole statements (with a
/// note each) and `CONCURRENTLY` keywords.
struct Skipped {
    statements: Vec<std::ops::Range<usize>>,
    keywords: Vec<std::ops::Range<usize>>,
    notes: Vec<Diagnostic>,
}

fn skipped(migration: &str) -> Skipped {
    let parsed = parse(migration);
    let mut out = Skipped {
        statements: Vec::new(),
        keywords: Vec::new(),
        notes: Vec::new(),
    };
    for statement in &parsed.statements {
        let skipped = match statement.node() {
            // A setting changed at run time: the statements after it are
            // checked without it.
            NodeEnum::SelectStmt(_)
                if parsed
                    .tokens_of(statement)
                    .iter()
                    .any(|t| migration[t.start..t.end].eq_ignore_ascii_case("set_config")) =>
            {
                Some((
                    "Not run here: set_config changes a setting when the migration runs.",
                    "The statements after it are checked without that setting (search_path, say): qualify their names if they need it.",
                ))
            }
            NodeEnum::InsertStmt(_)
            | NodeEnum::UpdateStmt(_)
            | NodeEnum::DeleteStmt(_)
            | NodeEnum::MergeStmt(_)
            | NodeEnum::SelectStmt(_)
            | NodeEnum::TruncateStmt(_)
            | NodeEnum::RefreshMatViewStmt(_) => Some((
                "Not run here: data statements cannot change the schema.",
                "It stays in the migration, and runs when the migration does.",
            )),
            NodeEnum::TransactionStmt(s)
                if matches!(
                    s.kind(),
                    TransactionStmtKind::TransStmtBegin
                        | TransactionStmtKind::TransStmtStart
                        | TransactionStmtKind::TransStmtCommit
                ) =>
            {
                Some((
                    "Not run here: the migration is checked in a transaction of its own.",
                    "It stays in the migration.",
                ))
            }
            NodeEnum::IndexStmt(_) | NodeEnum::DropStmt(_) | NodeEnum::AlterTableStmt(_) => {
                out.keywords.extend(
                    parsed
                        .tokens_of(statement)
                        .iter()
                        .filter(|t| t.kind == TokenKind::Concurrently)
                        .map(|t| t.start..t.end),
                );
                None
            }
            _ => None,
        };
        if let Some((message, hint)) = skipped {
            out.statements.push(statement.span.range());
            out.notes.push(
                Diagnostic::info(Code::NotRun, message)
                    .with_hint(hint)
                    .with_span(statement.span),
            );
        }
    }
    out
}

/// The migration with what the compile does not run blanked (every byte
/// but line breaks becomes a space), and a note for each statement skipped.
fn runnable(migration: &str) -> (String, Vec<Diagnostic>) {
    let skipped = skipped(migration);
    let mut bytes = migration.as_bytes().to_vec();
    for range in skipped.statements.iter().chain(&skipped.keywords).cloned() {
        for byte in &mut bytes[range] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    // Whole characters become spaces (a range never splits one): still UTF-8.
    let text = String::from_utf8(bytes).unwrap_or_else(|_| migration.to_owned());
    (text, skipped.notes)
}

/// SQL that compiles to what [`script`] compiled (see the module
/// documentation): the base's SQL, a line saying what follows, then the
/// migration without what the compile does not run (its statements gone
/// with their `;`, `CONCURRENTLY` dropped).
pub fn faithful_sql(base: &str, migration: &str) -> String {
    let skipped = skipped(migration);
    let mut cut: Vec<std::ops::Range<usize>> = skipped
        .statements
        .iter()
        .map(|range| {
            // The statement's `;` goes with it.
            let rest = &migration[range.end..];
            let semicolon = rest
                .find(|c: char| !c.is_whitespace())
                .filter(|&at| rest[at..].starts_with(';'))
                .map_or(0, |at| at + 1);
            range.start..range.end + semicolon
        })
        .chain(skipped.keywords.iter().map(|range| {
            let space = usize::from(migration[range.end..].starts_with(' '));
            range.start..range.end + space
        }))
        .collect();
    cut.sort_by_key(|range| range.start);
    let mut kept = String::with_capacity(migration.len());
    let mut at = 0;
    for range in cut {
        if range.start >= at {
            kept.push_str(&migration[at..range.start]);
            at = range.end;
        }
    }
    kept.push_str(&migration[at..]);
    // Lines a statement left empty, and runs of blank lines, go.
    let mut tidy = String::with_capacity(kept.len());
    let mut blank = 0;
    for line in kept.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(line);
        tidy.push('\n');
    }
    let migration = tidy.trim_matches('\n');
    if base.trim().is_empty() {
        return format!("{migration}\n");
    }
    format!(
        "{}\n;\n\n-- Then the migration that made this schema, as written in its draft \
         (what does not change the schema left out).\nSET search_path TO \"$user\", public;\n{migration}\n",
        base.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{allowlist, catalog::Catalog};

    fn errors_of(sql: &str) -> Vec<String> {
        let parsed = parse(sql);
        let mut out: Vec<String> = parsed.errors.iter().map(|d| d.message.clone()).collect();
        out.extend(
            allowlist::check(
                &parsed,
                sql,
                &Catalog::new(
                    [
                        "lower".to_owned(),
                        "repeat".to_owned(),
                        "pg_sleep".to_owned(),
                    ],
                    [],
                ),
            )
            .iter()
            .map(|d| d.message.clone()),
        );
        out
    }

    #[test]
    fn the_migration_runs_after_the_base_in_a_fresh_search_path() {
        let s = script(
            "SET search_path = app;\nCREATE TABLE a (id int)",
            "ALTER TABLE a ADD b int;",
        );
        assert_eq!(
            s.text,
            "SET search_path = app;\nCREATE TABLE a (id int)\n;\nSET search_path TO \"$user\", public;\nALTER TABLE a ADD b int;"
        );
        assert_eq!(&s.text[s.offset..], "ALTER TABLE a ADD b int;");
        // No base: the migration alone.
        let s = script("  \n", "CREATE TABLE a ();");
        assert_eq!((s.text.as_str(), s.offset), ("CREATE TABLE a ();", 0));
    }

    #[test]
    fn data_and_transaction_statements_are_blanked_in_place_with_a_note() {
        let migration = "BEGIN;\nALTER TABLE users ADD slug text;\nUPDATE users SET slug = lower(name) -- é\nWHERE true;\nSELECT setval('s', 1);\nALTER TABLE users ALTER slug SET NOT NULL;\nCOMMIT;\n";
        let s = script("", migration);
        assert_eq!(s.text.len(), migration.len());
        assert_eq!(s.text.lines().count(), migration.lines().count());
        assert!(!s.text.contains("UPDATE") && !s.text.contains("setval"));
        assert!(!s.text.contains("BEGIN") && !s.text.contains("COMMIT"));
        assert!(s.text.contains("ALTER TABLE users ADD slug text;"));
        assert_eq!(errors_of(&s.text), Vec::<String>::new());
        let noted: Vec<&str> = s
            .notes
            .iter()
            .map(|n| &migration[n.span.unwrap().range()])
            .collect();
        assert_eq!(
            noted,
            [
                "BEGIN",
                "UPDATE users SET slug = lower(name) -- é\nWHERE true",
                "SELECT setval('s', 1)",
                "COMMIT"
            ]
        );
        assert!(
            s.notes
                .iter()
                .all(|n| n.code == Code::NotRun && !n.is_error())
        );
    }

    #[test]
    fn concurrently_is_dropped_and_nothing_else() {
        let migration = "CREATE INDEX CONCURRENTLY users_email ON users (email);\nDROP INDEX CONCURRENTLY old;\n-- CONCURRENTLY in a comment\n";
        let s = script("", migration);
        assert_eq!(
            s.text,
            "CREATE INDEX              users_email ON users (email);\nDROP INDEX              old;\n-- CONCURRENTLY in a comment\n"
        );
        assert_eq!(s.notes, Vec::<Diagnostic>::new());
        assert_eq!(errors_of(&s.text), Vec::<String>::new());
    }

    #[test]
    fn what_the_allowlist_refuses_stays_refused() {
        for hostile in [
            "DO $$ BEGIN PERFORM pg_sleep(10); END $$;",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1';",
            "COPY users FROM PROGRAM 'id';",
            "GRANT ALL ON users TO PUBLIC;",
            "CREATE TABLE t AS SELECT pg_sleep(10);",
            "ALTER SYSTEM SET work_mem = '1TB';",
            "SET statement_timeout = 0;",
            "ROLLBACK;",
            "CREATE INDEX ON users ((repeat('x', 100000000)));",
        ] {
            let s = script("CREATE TABLE users (id int);", hostile);
            assert!(
                !errors_of(&s.text).is_empty(),
                "{hostile} passed the allowlist"
            );
            assert!(
                s.text.ends_with(hostile.trim_end()),
                "{hostile} was changed"
            );
        }
    }

    #[test]
    fn set_config_is_noted_for_what_it_changes() {
        let migration = "SELECT pg_catalog.set_config('search_path', 'app', false);\nCREATE TABLE t (id int);\n";
        let s = script("", migration);
        assert_eq!(s.notes.len(), 1);
        assert!(
            s.notes[0].message.contains("set_config"),
            "{:?}",
            s.notes[0]
        );
        assert!(s.notes[0].hint.as_deref().unwrap().contains("search_path"));
        assert!(!s.text.contains("set_config"));
    }

    #[test]
    fn a_base_too_large_says_it_counts() {
        let base = "CREATE TABLE a (id int);\n".repeat(100);
        let s = script(&base, "CREATE TABLE b (id int);");
        let located = s.relocate(
            "CREATE TABLE b (id int);",
            vec![Diagnostic::error(
                Code::TooLarge,
                "the SQL is 1025 KiB; the limit is 1024 KiB",
            )],
        );
        assert!(
            located[0]
                .message
                .starts_with("The base version's SQL (3 KiB)"),
            "{:?}",
            located[0]
        );
        assert_eq!(
            located[0].detail.as_deref(),
            Some("the SQL is 1025 KiB; the limit is 1024 KiB")
        );
        // A migration alone keeps the compiler's words.
        let alone = script("", "CREATE TABLE b (id int);").relocate(
            "x",
            vec![Diagnostic::error(Code::TooLarge, "the SQL is big")],
        );
        assert_eq!(alone[0].message, "the SQL is big");
    }

    #[test]
    fn the_faithful_sql_is_the_base_then_what_runs() {
        let base = "CREATE TABLE users (id int, email text);";
        let migration = "BEGIN;\nALTER TABLE users ADD slug text;\n\nUPDATE users SET slug = email;\n\n\nCREATE INDEX CONCURRENTLY users_slug ON users (slug);\nCOMMIT;\n";
        let sql = faithful_sql(base, migration);
        assert_eq!(
            sql,
            "CREATE TABLE users (id int, email text);\n;\n\n-- Then the migration that made this schema, as written in its draft (what does not change the schema left out).\nSET search_path TO \"$user\", public;\nALTER TABLE users ADD slug text;\n\nCREATE INDEX users_slug ON users (slug);\n"
        );
        assert_eq!(errors_of(&sql), Vec::<String>::new());
        assert_eq!(
            faithful_sql("", "CREATE TABLE a ();\nSELECT 1;"),
            "CREATE TABLE a ();\n"
        );
    }

    #[test]
    fn errors_are_located_in_the_migration() {
        let base = "CREATE TABLE a (id int);\nCREATE TABLE b (id int);";
        let migration = "-- é\nALTER TABLE a ADD x int;\nALTER TABLE nope ADD y int;";
        let s = script(base, migration);
        let at = s.text.find("nope").unwrap();
        let whole = LineIndex::new(&s.text);
        let diagnostics = vec![
            Diagnostic::error(Code::Sql, "relation \"nope\" does not exist")
                .with_span(whole.span(&s.text, at..at + 4))
                .with_statement(3),
            Diagnostic::info(Code::ProvidedByExtension, "skipped in the base")
                .with_span(whole.span(&s.text, 0..6)),
            Diagnostic::error(Code::Sql, "the base broke").with_span(whole.span(&s.text, 7..12)),
            Diagnostic::error(Code::TooLarge, "too large"),
        ];
        let located = s.relocate(migration, diagnostics);
        assert_eq!(located.len(), 3);
        let span = located[0].span.unwrap();
        assert_eq!(&migration[span.range()], "nope");
        assert_eq!((span.from.line, span.from.column), (3, 13));
        // UTF-16 offsets count the migration's own characters.
        assert_eq!(
            span.utf16_start,
            migration[..span.start].encode_utf16().count()
        );
        assert_eq!(located[0].statement, None);
        assert_eq!(located[1].message, "the base broke");
        assert_eq!(located[1].span.unwrap().start, 0);
        assert!(
            located[1]
                .detail
                .as_deref()
                .unwrap()
                .starts_with("In the base version")
        );
        assert_eq!(located[2].span, None);
    }
}
