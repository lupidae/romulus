//! Splitting SQL into statements, with exact ranges and keys.
//!
//! [`parse`] never fails: a text that does not parse still yields every
//! statement that does, plus one [`Code::SyntaxError`] per statement that
//! does not, each located on the offending token. That is what lets an
//! editor underline several mistakes at once, and the merge and graph edits
//! work on a draft that is half-typed.

use pg_query::{NodeEnum, protobuf, protobuf::Token as TokenKind};

use crate::{
    diagnostic::{Code, Diagnostic},
    key::{Key, key_of},
    names::SearchPath,
    source::{LineIndex, Span, location},
};

/// A SQL text split into statements.
#[derive(Debug, Clone)]
pub struct ParsedSql {
    /// The statements that parse, in source order.
    pub statements: Vec<Statement>,
    /// One syntax error per statement that does not parse (and one for a
    /// NUL byte or an unterminated string, comment or quoted identifier).
    pub errors: Vec<Diagnostic>,
    pub(crate) tokens: Vec<Token>,
    pub(crate) lines: LineIndex,
}

/// One top-level statement.
#[derive(Debug, Clone)]
pub struct Statement {
    /// Position in [`ParsedSql::statements`].
    pub index: usize,
    /// The object it defines or changes (see [`crate::key`]).
    pub key: Key,
    /// How many earlier statements have the same key: `(key, occurrence)`
    /// identifies a statement within a text.
    pub occurrence: usize,
    /// The statement itself, first token to last, without its `;`.
    pub span: Span,
    /// The span plus the comment block directly above it (no blank line in
    /// between), its `;`, and a comment after the `;` on the same line: what
    /// moves with the statement when it is moved or replaced.
    pub extent: Span,
    pub(crate) node: NodeEnum,
    /// The `search_path` in effect when the statement runs.
    pub(crate) search_path: SearchPath,
}

impl Statement {
    /// The parse tree (`pg_query`'s), with byte locations into the whole text.
    pub fn node(&self) -> &NodeEnum {
        &self.node
    }

    /// Its text, sliced out of the text it was parsed from.
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.span.range()]
    }
}

impl ParsedSql {
    /// The span of a byte range of the text this was parsed from.
    pub fn span(&self, sql: &str, range: std::ops::Range<usize>) -> Span {
        self.lines.span(sql, range)
    }

    /// The statement whose extent contains a byte offset.
    pub fn statement_at(&self, offset: usize) -> Option<&Statement> {
        self.statements
            .iter()
            .find(|s| s.extent.start <= offset && offset < s.extent.end.max(s.extent.start + 1))
    }

    /// The token covering a byte offset, as a range (for underlining a point
    /// Postgres reported).
    pub(crate) fn token_at(&self, offset: usize) -> Option<std::ops::Range<usize>> {
        self.tokens
            .iter()
            .find(|t| t.start <= offset && offset < t.end)
            .map(|t| t.start..t.end)
    }

    /// The tokens of a statement, comments included.
    pub(crate) fn tokens_of(&self, statement: &Statement) -> &[Token] {
        let first = self
            .tokens
            .partition_point(|t| t.start < statement.span.start);
        let last = self.tokens.partition_point(|t| t.end <= statement.span.end);
        &self.tokens[first..last.max(first)]
    }
}

/// A scanner token: a byte range and its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
    /// A SQL keyword (reserved or not), as opposed to an identifier,
    /// literal or operator.
    pub keyword: bool,
}

impl Token {
    pub fn is_comment(&self) -> bool {
        matches!(self.kind, TokenKind::SqlComment | TokenKind::CComment)
    }
}

/// Split and parse a SQL text. See the module documentation.
pub fn parse(sql: &str) -> ParsedSql {
    let lines = LineIndex::new(sql);
    let mut errors = Vec::new();

    // libpg_query takes a C string: a NUL byte would end the text early.
    let cleaned;
    let text = if let Some(nul) = sql.find('\0') {
        errors.push(
            Diagnostic::error(Code::SyntaxError, "the text contains a NUL byte")
                .with_span(lines.span(sql, nul..nul + 1)),
        );
        cleaned = sql.replace('\0', " ");
        cleaned.as_str()
    } else {
        sql
    };

    let (tokens, scan_error) = scan(text);
    if let Some((offset, message)) = &scan_error {
        errors.push(
            Diagnostic::error(Code::SyntaxError, message.clone())
                .with_span(lines.span(sql, *offset..sql.len())),
        );
    }

    let whole = if scan_error.is_none() {
        pg_query::parse(text).ok()
    } else {
        None
    };
    let raw = whole.or_else(|| {
        let unterminated = scan_error.as_ref().map(|(offset, _)| *offset);
        parse_around_errors(text, &tokens, unterminated, &mut |message, range| {
            errors.push(
                Diagnostic::error(Code::SyntaxError, message).with_span(lines.span(sql, range)),
            );
        })
    });

    let mut statements: Vec<Statement> = Vec::new();
    if let Some(parsed) = raw {
        let mut path = SearchPath::default();
        for raw in parsed.protobuf.stmts {
            let Some(node) = raw.stmt.and_then(|s| s.node) else {
                continue;
            };
            let begin = location(raw.stmt_location).unwrap_or(0);
            let end = match location(raw.stmt_len) {
                Some(0) | None => text.len(),
                Some(len) => begin + len,
            };
            let Some((first, last)) = code_tokens(&tokens, begin, end) else {
                continue;
            };
            let span = tokens[first].start..tokens[last].end;
            let extent = extent(text, &tokens, first, last);
            let key = key_of(&node, &path);
            let occurrence = statements.iter().filter(|s| s.key == key).count();
            let search_path = path.clone();
            if let NodeEnum::VariableSetStmt(set) = &node {
                path.apply(set);
            }
            statements.push(Statement {
                index: statements.len(),
                key,
                occurrence,
                span: lines.span(sql, span),
                extent: lines.span(sql, extent),
                node,
                search_path,
            });
        }
    }

    errors.sort_by_key(|e| e.span.map_or(0, |s| s.start));
    ParsedSql {
        statements,
        errors,
        tokens,
        lines,
    }
}

/// Parse a text with syntax errors: report the statements that do not
/// parse, blank them out (same length, so every location stays valid) and
/// parse the rest.
fn parse_around_errors(
    text: &str,
    tokens: &[Token],
    unterminated: Option<usize>,
    report: &mut impl FnMut(String, std::ops::Range<usize>),
) -> Option<pg_query::ParseResult> {
    let mut blanked = text.as_bytes().to_vec();
    for chunk in chunks(tokens, unterminated, text.len()) {
        let code: Vec<&Token> = tokens[chunk.tokens.clone()]
            .iter()
            .filter(|t| !t.is_comment())
            .collect();
        let valid = if chunk.unterminated {
            false
        } else if code.is_empty() {
            true
        } else if let Err(err) = pg_query::parse(&text[chunk.start..chunk.end]) {
            let message = parse_message(&err);
            let range = locate(text, &code, chunk.start, &message);
            report(message, range);
            false
        } else {
            true
        };
        if !valid {
            for byte in &mut blanked[chunk.start..chunk.end] {
                *byte = b' ';
            }
        }
    }
    let blanked = String::from_utf8(blanked).ok()?;
    pg_query::parse(&blanked).ok()
}

/// Tokens of the text, and the scanner's error if a string, comment or
/// quoted identifier is unterminated: its offset (it runs to the end of the
/// text) and message. Tokens stop where the error starts.
fn scan(text: &str) -> (Vec<Token>, Option<(usize, String)>) {
    match pg_query::scan(text) {
        Ok(result) => (tokens(&result), None),
        Err(err) => {
            let message = parse_message(&err);
            let offset = near_text(&message)
                .filter(|near| text.ends_with(near))
                .map_or(0, |near| text.len() - near.len());
            let tokens = pg_query::scan(&text[..offset])
                .map(|r| tokens(&r))
                .unwrap_or_default();
            (tokens, Some((offset, message)))
        }
    }
}

fn tokens(result: &protobuf::ScanResult) -> Vec<Token> {
    result
        .tokens
        .iter()
        .filter_map(|t| {
            Some(Token {
                start: location(t.start)?,
                end: location(t.end)?,
                kind: TokenKind::try_from(t.token).ok()?,
                keyword: t.keyword_kind != protobuf::KeywordKind::NoKeyword as i32,
            })
        })
        .collect()
}

/// `libpg_query`'s message without the `pg_query` crate's prefix.
fn parse_message(err: &pg_query::Error) -> String {
    match err {
        // prost refuses trees nested deeper than its recursion limit.
        pg_query::Error::Decode(_) => "the statement is nested too deeply to parse".to_owned(),
        pg_query::Error::Parse(m) | pg_query::Error::Scan(m) | pg_query::Error::Split(m) => {
            m.clone()
        }
        other => other.to_string(),
    }
}

/// The token text of an `… at or near "X"` message.
fn near_text(message: &str) -> Option<&str> {
    const NEAR: &str = " at or near \"";
    let start = message.find(NEAR)? + NEAR.len();
    message.get(start..message.len().checked_sub(1)?)
}

/// A run of tokens between two top-level semicolons.
struct Chunk {
    /// Byte range, from just after the previous `;` to just before the next.
    start: usize,
    end: usize,
    tokens: std::ops::Range<usize>,
    /// Runs into an unterminated string or comment.
    unterminated: bool,
}

fn chunks(tokens: &[Token], unterminated_at: Option<usize>, len: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut first = 0;
    let mut depth = 0_usize;
    for (i, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::Ascii40 => depth += 1,
            TokenKind::Ascii41 => depth = depth.saturating_sub(1),
            TokenKind::Ascii59 if depth == 0 => {
                chunks.push(Chunk {
                    start,
                    end: token.start,
                    tokens: first..i,
                    unterminated: false,
                });
                start = token.end;
                first = i + 1;
            }
            _ => {}
        }
    }
    // An unterminated literal runs to the end of the text.
    let end = if unterminated_at.is_some() {
        len
    } else {
        tokens.last().map_or(0, |t| t.end)
    };
    chunks.push(Chunk {
        start,
        end: end.max(start),
        tokens: first..tokens.len(),
        unterminated: unterminated_at.is_some(),
    });
    chunks
}

/// Where in a statement a syntax error is: the token Postgres names, found
/// by re-parsing prefixes up to each token with that text (the right one
/// fails with the same message; earlier ones end the input early instead).
fn locate(text: &str, code: &[&Token], start: usize, message: &str) -> std::ops::Range<usize> {
    let whole = code.first().map_or(start, |t| t.start)..code.last().map_or(start, |t| t.end);
    if message.ends_with("at end of input") {
        return whole.end..whole.end;
    }
    let Some(near) = near_text(message) else {
        return whole;
    };
    let candidates: Vec<&&Token> = code
        .iter()
        .filter(|t| text.get(t.start..t.end) == Some(near))
        .collect();
    for token in &candidates {
        if let Err(err) = pg_query::parse(&text[start..token.end])
            && parse_message(&err) == message
        {
            return token.start..token.end;
        }
    }
    candidates.first().map_or(whole, |t| t.start..t.end)
}

/// The first and last non-comment tokens inside `begin..end`.
fn code_tokens(tokens: &[Token], begin: usize, end: usize) -> Option<(usize, usize)> {
    let inside = |t: &Token| t.start >= begin && t.end <= end && !t.is_comment();
    let first = tokens.iter().position(inside)?;
    let last = tokens.iter().rposition(inside)?;
    Some((first, last))
}

/// See [`Statement::extent`].
fn extent(text: &str, tokens: &[Token], first: usize, last: usize) -> std::ops::Range<usize> {
    let mut end_token = last;
    if tokens
        .get(last + 1)
        .is_some_and(|t| t.kind == TokenKind::Ascii59)
    {
        end_token = last + 1;
    }
    let mut end = tokens[end_token].end;
    if let Some(next) = tokens.get(end_token + 1)
        && next.is_comment()
        && !text[end..next.start].contains('\n')
    {
        end = next.end;
    }

    let mut start = tokens[first].start;
    let mut i = first;
    while i > 0 {
        let comment = tokens[i - 1];
        if !comment.is_comment() || text[comment.end..start].matches('\n').count() > 1 {
            break;
        }
        // A comment on the same line as the previous token belongs to that
        // statement (`…; -- note`).
        let before = if i >= 2 { tokens[i - 2].end } else { 0 };
        if i >= 2 && !text[before..comment.start].contains('\n') {
            break;
        }
        start = comment.start;
        i -= 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{diagnostic::Code, source::Position};

    fn texts<'a>(sql: &'a str, parsed: &super::ParsedSql) -> Vec<&'a str> {
        parsed.statements.iter().map(|s| s.text(sql)).collect()
    }

    #[test]
    fn statements_span_first_to_last_token_without_semicolon() {
        let sql = "-- head\nCREATE TABLE a (id int);\n\n/* c */ CREATE TABLE b (x int) ; -- trailing\nALTER TABLE a ADD COLUMN y text";
        let parsed = parse(sql);
        assert_eq!(parsed.errors, []);
        assert_eq!(
            texts(sql, &parsed),
            [
                "CREATE TABLE a (id int)",
                "CREATE TABLE b (x int)",
                "ALTER TABLE a ADD COLUMN y text"
            ]
        );
        let b = &parsed.statements[1];
        assert_eq!(b.span.from, Position { line: 4, column: 9 });
        assert_eq!(b.index, 1);
    }

    #[test]
    fn extent_takes_the_comment_block_above_and_the_semicolon() {
        let sql = "CREATE TABLE a (id int); -- about a\n\n-- Orders.\n-- Placed by users.\nCREATE TABLE orders (id int); -- same line\n-- next\n\nCREATE TABLE c (id int)";
        let parsed = parse(sql);
        let extents: Vec<&str> = parsed
            .statements
            .iter()
            .map(|s| &sql[s.extent.range()])
            .collect();
        assert_eq!(
            extents,
            [
                "CREATE TABLE a (id int); -- about a",
                "-- Orders.\n-- Placed by users.\nCREATE TABLE orders (id int); -- same line",
                "CREATE TABLE c (id int)",
            ]
        );
    }

    #[test]
    fn spans_count_utf16_for_the_browser() {
        let sql = "-- ☕ café 🐘\nCREATE TABLE \"Café\" (id int);";
        let parsed = parse(sql);
        let span = parsed.statements[0].span;
        assert_eq!(span.from, Position { line: 2, column: 1 });
        // "-- ☕ café 🐘\n": 3 + 1 + 1 + 4 + 1 + 2 + 1 = 13 UTF-16 units.
        assert_eq!(span.utf16_start, 13);
        assert_eq!(&sql[span.range()], "CREATE TABLE \"Café\" (id int)");
    }

    #[test]
    fn empty_statements_and_comments_only_yield_nothing() {
        let parsed = parse(";;  -- nothing\n/* here */ ;");
        assert!(parsed.statements.is_empty());
        assert_eq!(parsed.errors, []);
        assert!(parse("").statements.is_empty());
    }

    #[test]
    fn every_broken_statement_is_reported_and_the_rest_still_parse() {
        let sql = "CREATE TABLE a (id int);\nCREATE TABL b (x int);\nCREATE TABLE c (id int,);\nCREATE TABLE d (id int);\nCREATE TABLE e (id int";
        let parsed = parse(sql);
        assert_eq!(
            texts(sql, &parsed),
            ["CREATE TABLE a (id int)", "CREATE TABLE d (id int)"]
        );
        let errors: Vec<(&str, Position, &str)> = parsed
            .errors
            .iter()
            .map(|e| {
                let span = e.span.unwrap();
                (e.message.as_str(), span.from, &sql[span.range()])
            })
            .collect();
        assert_eq!(
            errors,
            [
                (
                    "syntax error at or near \"TABL\"",
                    Position { line: 2, column: 8 },
                    "TABL"
                ),
                (
                    "syntax error at or near \")\"",
                    Position {
                        line: 3,
                        column: 24
                    },
                    ")"
                ),
                (
                    "syntax error at end of input",
                    Position {
                        line: 5,
                        column: 23
                    },
                    ""
                ),
            ]
        );
        assert!(parsed.errors.iter().all(|e| e.code == Code::SyntaxError));
        // Locations inside the statements that parsed are still absolute.
        assert_eq!(parsed.statements[1].key.to_string(), "table:public.d");
    }

    #[test]
    fn the_failing_token_is_found_even_when_its_text_repeats() {
        let sql = "CREATE TABLE t (a int, b int, c int int)";
        let parsed = parse(sql);
        let span = parsed.errors[0].span.unwrap();
        assert_eq!(span.start, sql.rfind("int").unwrap());
    }

    #[test]
    fn an_unterminated_string_is_reported_where_it_starts() {
        let sql =
            "CREATE TABLE a (id int);\nCOMMENT ON TABLE a IS 'oops;\nCREATE TABLE b (id int);";
        let parsed = parse(sql);
        assert_eq!(texts(sql, &parsed), ["CREATE TABLE a (id int)"]);
        assert_eq!(parsed.errors.len(), 1);
        let span = parsed.errors[0].span.unwrap();
        assert!(
            parsed.errors[0]
                .message
                .starts_with("unterminated quoted string")
        );
        assert_eq!(
            span.from,
            Position {
                line: 2,
                column: 23
            }
        );
        assert_eq!(span.end, sql.len());
    }

    #[test]
    fn a_nul_byte_is_an_error_but_does_not_hide_the_statements() {
        let sql = "CREATE TABLE a (id int);\0\nCREATE TABLE b (id int);";
        let parsed = parse(sql);
        assert_eq!(parsed.statements.len(), 2);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            parsed.errors[0].span.unwrap().start,
            sql.find('\0').unwrap()
        );
    }

    #[test]
    fn keys_repeat_with_an_occurrence_count() {
        let parsed = parse(
            "ALTER TABLE t ADD COLUMN a int, ADD COLUMN b int; ALTER TABLE t ADD COLUMN c int, ADD COLUMN d int;",
        );
        let occurrences: Vec<(String, usize)> = parsed
            .statements
            .iter()
            .map(|s| (s.key.to_string(), s.occurrence))
            .collect();
        assert_eq!(
            occurrences,
            [
                ("alter:table:public.t".to_owned(), 0),
                ("alter:table:public.t".to_owned(), 1)
            ]
        );
    }

    #[test]
    fn statement_at_finds_the_statement_around_an_offset() {
        let sql = "CREATE TABLE a (id int);\n-- b\nCREATE TABLE b (id int);";
        let parsed = parse(sql);
        assert_eq!(
            parsed
                .statement_at(sql.find("-- b").unwrap())
                .unwrap()
                .index,
            1
        );
        assert_eq!(parsed.statement_at(3).unwrap().index, 0);
    }
}
