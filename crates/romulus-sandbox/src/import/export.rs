//! The SQL code-based tools print, split back into migrations.
//!
//! romulus never runs a project's code: the scripts in
//! `scripts/export-migrations/` run the tool in the project (against a
//! scratch database where it needs one) and write one file:
//!
//! - **Alembic**: `alembic upgrade head --sql`, one section per revision
//!   under `-- Running upgrade <from> -> <to>` (the exporter appends the
//!   revision's message: `-- Running upgrade <from> -> <to>, <message>`).
//!   What comes before the first revision is Alembic's version table; the
//!   `alembic_version` bookkeeping inside each section is left out too.
//! - **Django**: `manage.py sqlmigrate <app> <migration>` for each migration
//!   of `showmigrations --plan`, each under `-- <app>.<migration>`.
//! - **Rails**: the SQL each migration ran on a scratch database, each under
//!   `-- <version>_<name>.rb`.
//!
//! Headers count only at the top level of the text (not inside a function
//! body or a string).

use std::ops::Range;

use super::{Format, lexer};

/// One migration out of an exported file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub id: String,
    pub name: String,
    /// The file's line where `sql` starts.
    pub line: u32,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Split {
    pub chunks: Vec<Chunk>,
    /// The text before the first migration was the tool's own (Alembic's
    /// version table).
    pub preamble_is_tool: bool,
}

/// The top-level lines `classify` recognises, with what it said.
pub(crate) fn marker_lines<T>(
    text: &str,
    classify: impl Fn(&str) -> Option<T>,
) -> impl Iterator<Item = (Range<usize>, T)> {
    lexer::scan(text)
        .top_level_lines
        .into_iter()
        .filter_map(move |range| classify(&text[range.clone()]).map(|t| (range, t)))
}

/// `-- Running upgrade a -> b[, message]`: (`b`, message).
fn alembic_header(line: &str) -> Option<Header> {
    let rest = line.trim_end().strip_prefix("-- Running ")?;
    let (direction, rest) = rest.split_once(' ')?;
    let (_, to) = rest.split_once("-> ")?;
    let (to, message) = match to.split_once(", ") {
        // A merge's target is one revision; a comma after it starts the
        // message.
        Some((to, message)) => (to.trim(), Some(message.trim())),
        None => (to.trim(), None),
    };
    if to.is_empty() {
        return None;
    }
    Some(Header {
        downgrade: direction == "downgrade",
        id: to.to_owned(),
        name: match message.filter(|m| !m.is_empty()) {
            Some(message) => format!("{to} {message}"),
            None => to.to_owned(),
        },
    })
}

/// `-- blog.0002_post_slug`.
fn django_header(line: &str) -> Option<Header> {
    let rest = line.trim_end().strip_prefix("-- ")?;
    let (app, migration) = rest.split_once('.')?;
    let label = |s: &str, first_digit: bool| {
        !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && (first_digit || !s.as_bytes()[0].is_ascii_digit())
    };
    (label(app, false) && label(migration, true)).then(|| Header {
        downgrade: false,
        id: rest.to_owned(),
        name: rest.to_owned(),
    })
}

/// `-- 20240101000000_create_users.rb`.
fn rails_header(line: &str) -> Option<Header> {
    let rest = line.trim_end().strip_prefix("-- ")?;
    let stem = rest.strip_suffix(".rb")?;
    let (version, name) = stem.split_once('_')?;
    let ok = !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    ok.then(|| Header {
        downgrade: false,
        id: version.to_owned(),
        name: rest.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    downgrade: bool,
    id: String,
    name: String,
}

/// The exported format a text is, from its headers.
pub(crate) fn sniff(text: &str) -> Option<Format> {
    let lines = lexer::scan(text).top_level_lines;
    if lines
        .iter()
        .any(|r| alembic_header(&text[r.clone()]).is_some())
    {
        return Some(Format::Alembic);
    }
    let first = lines
        .iter()
        .map(|r| text[r.clone()].trim())
        .find(|l| !l.is_empty())?;
    if rails_header(first).is_some() {
        Some(Format::Rails)
    } else if django_header(first).is_some() {
        Some(Format::Django)
    } else {
        None
    }
}

/// Split an exported text into migrations, in the order printed.
pub(crate) fn split(format: Format, text: &str) -> Result<Split, String> {
    let header: fn(&str) -> Option<Header> = match format {
        Format::Alembic => alembic_header,
        Format::Django => django_header,
        Format::Rails => rails_header,
        _ => return Err(format!("{} files are not split", format.label())),
    };
    let headers: Vec<(Range<usize>, Header)> = marker_lines(text, header).collect();
    if headers.is_empty() {
        let example = match format {
            Format::Alembic => "-- Running upgrade  -> 1975ea83b712",
            Format::Django => "-- blog.0001_initial",
            _ => "-- 20240101000000_create_users.rb",
        };
        return Err(format!(
            "no migration header found (a line like `{example}`): is this {} output?",
            format.label()
        ));
    }
    if let Some((_, h)) = headers.iter().find(|(_, h)| h.downgrade) {
        return Err(format!(
            "it runs a downgrade to {}: export with `alembic upgrade head --sql`",
            h.id
        ));
    }
    let preamble = &text[..headers[0].0.start];
    let preamble_is_tool = format == Format::Alembic && preamble.contains("alembic_version");
    let mut chunks = Vec::with_capacity(headers.len());
    for (k, (range, header)) in headers.iter().enumerate() {
        let start = (range.end + 1).min(text.len());
        let end = headers
            .get(k + 1)
            .map_or(text.len(), |(next, _)| next.start);
        let mut sql = text[start..end.max(start)].to_owned();
        if format == Format::Alembic {
            sql = without_alembic_bookkeeping(&sql);
        }
        chunks.push(Chunk {
            id: header.id.clone(),
            name: header.name.clone(),
            line: line_of(text, start),
            sql,
        });
    }
    Ok(Split {
        chunks,
        preamble_is_tool,
    })
}

/// The 1-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> u32 {
    u32::try_from(text[..offset].bytes().filter(|b| *b == b'\n').count() + 1).unwrap_or(u32::MAX)
}

/// Alembic stamps every revision into `alembic_version`
/// (`INSERT INTO alembic_version …`, `UPDATE alembic_version SET …`): the
/// tool's bookkeeping, not the project's data. Blanked, so the report only
/// lists the project's own data statements.
fn without_alembic_bookkeeping(sql: &str) -> String {
    let mut ranges = Vec::new();
    let lines = lexer::scan(sql).top_level_lines;
    let mut k = 0;
    while k < lines.len() {
        let line = sql[lines[k].clone()].trim_start().to_ascii_lowercase();
        let bookkeeping = [
            "insert into alembic_version",
            "update alembic_version",
            "delete from alembic_version",
        ]
        .iter()
        .any(|p| line.starts_with(p));
        if bookkeeping {
            // Through the line that ends the statement.
            let start = lines[k].start;
            let mut end = lines[k].end;
            while !sql[..end].trim_end().ends_with(';') && k + 1 < lines.len() {
                k += 1;
                end = lines[k].end;
            }
            ranges.push(start..end);
        }
        k += 1;
    }
    lexer::blank(sql, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alembic_sections_by_revision_without_its_bookkeeping() {
        let text = "BEGIN;\n\nCREATE TABLE alembic_version (\n    version_num VARCHAR(32) NOT NULL, \n    CONSTRAINT alembic_version_pkc PRIMARY KEY (version_num)\n);\n\n-- Running upgrade  -> 1975ea83b712\n\nCREATE TABLE account (id SERIAL NOT NULL);\n\nINSERT INTO alembic_version (version_num) VALUES ('1975ea83b712') RETURNING alembic_version.version_num;\n\n-- Running upgrade 1975ea83b712 -> ae1027a6acf, add a column\n\nALTER TABLE account ADD COLUMN x TEXT;\n\nUPDATE alembic_version SET version_num='ae1027a6acf' WHERE alembic_version.version_num = '1975ea83b712';\n\nCOMMIT;\n\n";
        assert_eq!(sniff(text), Some(Format::Alembic));
        let split = split(Format::Alembic, text).unwrap();
        assert!(split.preamble_is_tool);
        let ids: Vec<_> = split
            .chunks
            .iter()
            .map(|c| (c.id.as_str(), c.name.as_str(), c.line))
            .collect();
        assert_eq!(
            ids,
            [
                ("1975ea83b712", "1975ea83b712", 9),
                ("ae1027a6acf", "ae1027a6acf add a column", 15)
            ]
        );
        assert!(split.chunks[0].sql.contains("CREATE TABLE account"));
        assert!(!split.chunks[0].sql.contains("alembic_version"));
        assert!(!split.chunks[1].sql.contains("alembic_version"));
        assert!(split.chunks[1].sql.contains("COMMIT;"));
        // Offsets survive: the chunk's lines are the file's, shifted.
        assert_eq!(
            split.chunks[0].sql.lines().nth(1),
            Some("CREATE TABLE account (id SERIAL NOT NULL);")
        );
    }

    #[test]
    fn alembic_downgrades_are_refused() {
        let text = "-- Running downgrade ae1027a6acf -> 1975ea83b712\nALTER TABLE account DROP COLUMN x;\n";
        assert!(
            split(Format::Alembic, text)
                .unwrap_err()
                .contains("downgrade")
        );
    }

    #[test]
    fn django_headers_name_app_and_migration() {
        let text = "-- blog.0001_initial\nBEGIN;\n--\n-- Create model Post\n--\nCREATE TABLE \"blog_post\" (\"id\" bigint NOT NULL);\nCOMMIT;\n-- accounts.0002_profile_bio\nBEGIN;\n--\n-- Add field bio to profile\n--\nALTER TABLE \"accounts_profile\" ADD COLUMN \"bio\" text;\nCOMMIT;\n";
        assert_eq!(sniff(text), Some(Format::Django));
        let split = split(Format::Django, text).unwrap();
        let ids: Vec<_> = split
            .chunks
            .iter()
            .map(|c| (c.id.as_str(), c.line))
            .collect();
        assert_eq!(
            ids,
            [("blog.0001_initial", 2), ("accounts.0002_profile_bio", 9)]
        );
        // Comments of sqlmigrate's own (`-- Create model Post`) are not headers.
        assert!(split.chunks[0].sql.contains("-- Create model Post"));
    }

    #[test]
    fn rails_headers_are_migration_file_names() {
        let text = "-- 20240101000000_create_users.rb\nCREATE TABLE \"users\" (\"id\" bigserial primary key);\n-- 20240102000000_add_name_to_users.rb\nALTER TABLE \"users\" ADD \"name\" character varying;\n";
        assert_eq!(sniff(text), Some(Format::Rails));
        let split = split(Format::Rails, text).unwrap();
        assert_eq!(split.chunks[1].id, "20240102000000");
        assert_eq!(split.chunks[1].name, "20240102000000_add_name_to_users.rb");
    }

    #[test]
    fn plain_sql_is_not_mistaken_for_an_export() {
        assert_eq!(sniff("-- just a comment\nCREATE TABLE t ();"), None);
        assert_eq!(sniff("CREATE TABLE t ();\n-- blog.0001_initial\n"), None);
        // A header inside a function body does not count.
        assert_eq!(
            sniff(
                "CREATE FUNCTION f() RETURNS text AS $$\n-- Running upgrade  -> abc\n$$ LANGUAGE sql;"
            ),
            None
        );
        assert!(split(Format::Django, "CREATE TABLE t ();").is_err());
    }
}
