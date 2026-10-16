//! A top-level scan of a migration file: which lines start outside any
//! string, quoted identifier, dollar quote or comment, and what in it is not
//! SQL at all (psql meta-commands, `COPY … FROM stdin` data).
//!
//! Section markers (`-- migrate:up`), the headers of exported files (`--
//! Running upgrade a -> b`) and psql commands only count at the top level:
//! the same text inside a function body is part of the body.

use std::ops::Range;

/// What a migration file holds besides SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotSql {
    /// A psql meta-command line (`\connect`, `\restrict`, `\set`…).
    Psql,
    /// The rows of a `COPY … FROM stdin`, up to and including `\.`.
    CopyData,
}

#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Byte ranges of the lines that start at the top level (without their
    /// line break).
    pub top_level_lines: Vec<Range<usize>>,
    /// Ranges that are not SQL, each a whole number of lines (line break
    /// excluded).
    pub not_sql: Vec<(Range<usize>, NotSql)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Top,
    /// `'…'`; `true` when backslash escapes apply (`E'…'`).
    Single(bool),
    Double,
    Dollar(String),
    Block(usize),
    Line,
}

#[allow(clippy::too_many_lines)]
/// Scan `text`. Never fails: an unterminated string just runs to the end
/// (the parser reports it).
pub(crate) fn scan(text: &str) -> Scan {
    let bytes = text.as_bytes();
    let mut out = Scan::default();
    let mut state = State::Top;
    // Where the current top-level statement started, for COPY.
    let mut statement_start = 0;
    let mut i = 0;
    let mut at_line_start = true;
    while i < bytes.len() {
        if at_line_start {
            at_line_start = false;
            let line_end = text[i..].find('\n').map_or(text.len(), |n| i + n);
            if state == State::Top {
                out.top_level_lines.push(i..line_end);
                if text[i..line_end].trim_start().starts_with('\\') {
                    out.not_sql.push((i..line_end, NotSql::Psql));
                    statement_start = line_end;
                    i = line_end;
                    continue;
                }
            }
        }
        let b = bytes[i];
        if b == b'\n' {
            if state == State::Line {
                state = State::Top;
            }
            i += 1;
            at_line_start = true;
            continue;
        }
        match &state {
            State::Top => match b {
                b'\'' => {
                    let escapes = i > 0
                        && matches!(bytes[i - 1], b'e' | b'E')
                        && (i < 2 || !is_ident_byte(bytes[i - 2]));
                    state = State::Single(escapes);
                }
                b'"' => state = State::Double,
                b'-' if bytes.get(i + 1) == Some(&b'-') => {
                    state = State::Line;
                    i += 1;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    state = State::Block(1);
                    i += 1;
                }
                b'$' if i == 0 || !is_ident_byte(bytes[i - 1]) => {
                    if let Some(tag) = dollar_tag(&text[i..]) {
                        i += tag.len() - 1;
                        state = State::Dollar(tag);
                    }
                }
                b';' => {
                    let statement = &text[statement_start..i];
                    statement_start = i + 1;
                    if is_copy_from_stdin(statement) {
                        // The data starts on the next line.
                        let next = text[i..].find('\n').map_or(text.len(), |n| i + n + 1);
                        let (data, after) = copy_data(text, next);
                        if !data.is_empty() {
                            out.not_sql.push((data, NotSql::CopyData));
                        }
                        statement_start = after;
                        i = after;
                        at_line_start = true;
                        continue;
                    }
                }
                _ => {}
            },
            State::Single(escapes) => match b {
                b'\\' if *escapes => i += 1,
                b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 1,
                b'\'' => state = State::Top,
                _ => {}
            },
            State::Double => {
                if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 1;
                    } else {
                        state = State::Top;
                    }
                }
            }
            State::Dollar(tag) => {
                if b == b'$' && text[i..].starts_with(tag.as_str()) {
                    i += tag.len() - 1;
                    state = State::Top;
                }
            }
            State::Block(depth) => {
                let depth = *depth;
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    i += 1;
                    state = if depth == 1 {
                        State::Top
                    } else {
                        State::Block(depth - 1)
                    };
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    i += 1;
                    state = State::Block(depth + 1);
                }
            }
            State::Line => {}
        }
        i += 1;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// `$tag$` or `$$` at the start of `rest`; `$1` is a parameter, not a quote.
fn dollar_tag(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut end = 1;
    while end < bytes.len() && bytes[end] != b'$' {
        let b = bytes[end];
        let ok = if end == 1 {
            b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
        } else {
            is_ident_byte(b)
        };
        if !ok {
            return None;
        }
        end += 1;
    }
    (end < bytes.len()).then(|| rest[..=end].to_owned())
}

/// A `COPY … FROM stdin` statement (its rows follow it in the file).
fn is_copy_from_stdin(statement: &str) -> bool {
    let words: Vec<String> = statement
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let starts_with_copy = statement
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("--"))
        .is_some_and(|l| l.to_ascii_lowercase().starts_with("copy"));
    starts_with_copy && words.windows(2).any(|w| w[0] == "from" && w[1] == "stdin")
}

/// The data lines from `start` up to and including the `\.` line, and
/// where the text resumes.
fn copy_data(text: &str, start: usize) -> (Range<usize>, usize) {
    let mut at = start;
    while at < text.len() {
        let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
        if text[at..end].trim_end() == "\\." {
            let resume = (end + 1).min(text.len());
            return (start..end, resume);
        }
        at = end + 1;
    }
    // No terminator: everything left is data.
    (start..text.len(), text.len())
}

/// `text` with `ranges` replaced by spaces (line breaks kept), so every
/// offset, line and column of the rest is unchanged.
pub(crate) fn blank(text: &str, ranges: impl IntoIterator<Item = Range<usize>>) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for range in ranges {
        for at in range {
            // Only whole characters are blanked, so the result stays UTF-8:
            // every byte of a multi-byte character becomes a space.
            if bytes[at] != b'\n' && bytes[at] != b'\r' {
                bytes[at] = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        scan(text)
            .top_level_lines
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn lines_inside_strings_comments_and_dollar_quotes_are_not_top_level() {
        let text = "-- migrate:up\nCREATE FUNCTION f() RETURNS int AS $body$\n-- migrate:down\nSELECT 1\n$body$ LANGUAGE sql;\n/* a\n-- migrate:down\n*/\nSELECT 'x\n-- migrate:down\n';\n-- migrate:down\n";
        assert_eq!(
            lines(text),
            [
                "-- migrate:up",
                "CREATE FUNCTION f() RETURNS int AS $body$",
                "/* a",
                "SELECT 'x",
                "-- migrate:down",
            ]
        );
    }

    #[test]
    fn escape_strings_and_doubled_quotes_do_not_end_early() {
        let text = "SELECT E'it\\'s\n-- no', 'a''b\n-- no';\n-- yes\n";
        assert_eq!(lines(text), ["SELECT E'it\\'s", "-- yes"]);
        // `$1` is a parameter, `x$y` part of a name: neither opens a quote.
        let text = "PREPARE p AS SELECT $1;\n-- yes\nSELECT a$b FROM t;\n-- yes too\n";
        assert_eq!(lines(text).len(), 4);
    }

    #[test]
    fn psql_commands_and_copy_data_are_not_sql() {
        let text = "\\restrict abc\nSET x = 1;\nCOPY public.t (a, b) FROM stdin;\n1\tx\n2\t\\N\n\\.\nCREATE TABLE u ();\n  \\connect db\n";
        let found: Vec<(&str, NotSql)> = scan(text)
            .not_sql
            .iter()
            .map(|(r, k)| (&text[r.clone()], *k))
            .collect();
        assert_eq!(
            found,
            [
                ("\\restrict abc", NotSql::Psql),
                ("1\tx\n2\t\\N\n\\.", NotSql::CopyData),
                ("  \\connect db", NotSql::Psql),
            ]
        );
        // A COPY from a file has no inline data.
        assert_eq!(
            scan("COPY t FROM '/tmp/x';\nCREATE TABLE u ();\n")
                .not_sql
                .len(),
            0
        );
    }

    #[test]
    fn blanking_keeps_offsets_and_lines() {
        let text = "INSERT INTO t VALUES ('é');\nCREATE TABLE u ();";
        let blanked = blank(text, std::iter::once(0..text.find('\n').unwrap()));
        assert_eq!(blanked.len(), text.len());
        assert!(blanked.starts_with("   "));
        assert!(blanked.ends_with("\nCREATE TABLE u ();"));
    }
}
