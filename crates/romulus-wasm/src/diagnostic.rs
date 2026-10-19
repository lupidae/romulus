//! What a compile tells the person typing, in the shape the app's editor
//! reads (romulus-sandbox's `Diagnostic` and `Span`, serialised alike), so
//! the editor's lint gutter and compile status take them as they are.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    /// One sentence: Postgres's own wording where Postgres would say it.
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// The SQLSTATE Postgres would raise, for [`Code::Sql`].
    pub sqlstate: Option<String>,
    pub span: Option<Span>,
    /// Index of the statement it is about.
    pub statement: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Blocks the compile: there is no model.
    Error,
    /// Worth knowing, the model is still there.
    Info,
}

/// The sandbox's codes, those this engine raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The text does not parse.
    SyntaxError,
    /// A statement outside the drafts' DDL allowlist (the app refuses it too).
    NotAllowed,
    /// `CREATE EXTENSION` of an extension the app does not offer.
    ExtensionNotAvailable,
    /// The text is over the size limit.
    TooLarge,
    /// What Postgres would refuse, with its message and SQLSTATE.
    Sql,
    /// Info: a statement this engine does not model (the app's Postgres
    /// sandbox runs it): it is left out of the model.
    NotRun,
    /// A `--- words` line that was never posted as a comment.
    CommentLine,
    /// The preview could not check it: an invariant broke; never expected
    /// (the app's compile is the judge).
    Unavailable,
}

impl Diagnostic {
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            detail: None,
            hint: None,
            sqlstate: None,
            span: None,
            statement: None,
        }
    }

    /// An error Postgres raises, with its SQLSTATE.
    pub fn sql(sqlstate: &str, message: impl Into<String>) -> Self {
        Self {
            sqlstate: Some(sqlstate.to_owned()),
            ..Self::error(Code::Sql, message)
        }
    }

    pub fn info(code: Code, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            ..Self::error(code, message)
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// A 1-based line and column (columns count characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A range of the SQL in bytes, UTF-16 code units (CodeMirror, Yjs) and
/// line/column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
    pub from: Position,
    pub to: Position,
}

/// Byte offsets to spans, over one text.
#[derive(Debug, Clone)]
pub struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of each line's start.
    starts: Vec<usize>,
    /// UTF-16 offset of each line's start.
    utf16_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        let mut utf16_starts = vec![0];
        let mut utf16 = 0;
        for (offset, c) in text.char_indices() {
            utf16 += c.len_utf16();
            if c == '\n' {
                starts.push(offset + 1);
                utf16_starts.push(utf16);
            }
        }
        Self {
            text,
            starts,
            utf16_starts,
        }
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        }
    }

    fn floor(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset = offset.saturating_sub(1);
        }
        offset
    }

    fn position(&self, offset: usize) -> (Position, usize) {
        let line = self.line_of(offset);
        let start = self.starts.get(line).copied().unwrap_or(0);
        let before = self.text.get(start..offset).unwrap_or("");
        let utf16 = self.utf16_starts.get(line).copied().unwrap_or(0)
            + before.chars().map(char::len_utf16).sum::<usize>();
        let column = before.chars().count() + 1;
        (
            Position {
                line: u32::try_from(line + 1).unwrap_or(u32::MAX),
                column: u32::try_from(column).unwrap_or(u32::MAX),
            },
            utf16,
        )
    }

    /// The span of the byte range `start..end`, clamped to the text.
    pub fn span(&self, start: usize, end: usize) -> Span {
        let start = self.floor(start);
        let end = self.floor(end.max(start));
        let (from, utf16_start) = self.position(start);
        let (to, utf16_end) = self.position(end);
        Span {
            start,
            end,
            utf16_start,
            utf16_end,
            from,
            to,
        }
    }

    /// The byte offset of the `n`th character (0-based), clamped.
    pub fn char_offset(&self, n: usize) -> usize {
        self.text
            .char_indices()
            .nth(n)
            .map_or(self.text.len(), |(offset, _)| offset)
    }

    /// The end of the token starting at `start`: an identifier, a number, a
    /// quoted literal or identifier, or one character.
    pub fn token_end(&self, start: usize) -> usize {
        let start = self.floor(start);
        let rest = self.text.get(start..).unwrap_or("");
        let mut chars = rest.char_indices();
        let Some((_, first)) = chars.next() else {
            return start;
        };
        if first == '\'' || first == '"' {
            let mut escaped = false;
            for (i, c) in chars {
                if c == first && !escaped {
                    return start + i + 1;
                }
                escaped = c == '\\' && !escaped && first == '\'';
            }
            return self.text.len();
        }
        if first.is_alphanumeric() || first == '_' {
            let len = rest
                .char_indices()
                .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.'))
                .map_or(rest.len(), |(i, _)| i);
            return start + len;
        }
        start + first.len_utf8()
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn spans_count_bytes_utf16_and_lines() {
        let text = "a\né😀 x";
        let index = LineIndex::new(text);
        let x = text.find('x').unwrap();
        let span = index.span(x, x + 1);
        assert_eq!(span.from.line, 2);
        assert_eq!(span.from.column, 4);
        assert_eq!(span.utf16_start, 2 + 1 + 2 + 1);
        assert_eq!(index.char_offset(4), x - 1);
    }

    #[test]
    fn a_token_ends_where_the_word_or_quote_does() {
        let text = "select 'it''s' from \"T x\" where";
        let index = LineIndex::new(text);
        assert_eq!(&text[..index.token_end(0)], "select");
        let q = text.find('\'').unwrap();
        assert_eq!(&text[q..index.token_end(q)], "'it'");
        let d = text.find('"').unwrap();
        assert_eq!(&text[d..index.token_end(d)], "\"T x\"");
    }
}
