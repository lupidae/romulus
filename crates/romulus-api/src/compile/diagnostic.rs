//! What a compile tells the person who wrote the SQL: errors that block it,
//! and notes, each located in their text.
//!
//! These serialise exactly like `romulus_sandbox::Diagnostic` (same names,
//! same fields), so the web app's generated types do not change when the
//! sandbox's own types replace them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One message about the SQL, located in the text the user wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable: the web app branches on it.
    pub code: Code,
    /// One sentence, Postgres's own message for [`Code::Sql`].
    pub message: String,
    /// Postgres's `DETAIL`, or ours.
    pub detail: Option<String>,
    /// Postgres's `HINT`, or ours: what to do about it.
    pub hint: Option<String>,
    /// The SQLSTATE, for [`Code::Sql`] and [`Code::Timeout`].
    pub sqlstate: Option<String>,
    /// Where in the text. `None` only for problems with the whole text (too
    /// large, sandbox unavailable).
    pub span: Option<Span>,
    /// Index of the statement it is about.
    pub statement: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Blocks the compile: there is no model.
    Error,
    /// Something the compile did on the user's behalf, worth knowing.
    Info,
}

/// Why a [`Diagnostic`] was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The text does not parse (reported per statement, several at once).
    SyntaxError,
    /// A statement or clause outside the DDL allowlist.
    NotAllowed,
    /// A function the sandbox refuses to call.
    FunctionNotAllowed,
    /// `CREATE EXTENSION` of an extension the sandbox does not offer.
    ExtensionNotAvailable,
    /// The text is over the size limit.
    TooLarge,
    /// The text has more statements than the limit.
    TooManyStatements,
    /// Postgres rejected a statement; `sqlstate` says why.
    Sql,
    /// A statement, or the whole compile, ran out of time.
    Timeout,
    /// The sandbox itself failed; retrying helps.
    Unavailable,
    /// Info: a statement was not run because an extension already provides
    /// that object.
    ProvidedByExtension,
}

/// A 1-based line and column (columns count characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A range of the SQL text, in bytes, UTF-16 code units (JavaScript string
/// indexes, what `CodeMirror` counts in) and line/column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
    pub from: Position,
    pub to: Position,
}

impl Span {
    /// The span of `text[start..end]` (byte offsets on character boundaries).
    pub fn of(text: &str, start: usize, end: usize) -> Self {
        let position = |offset: usize| {
            let before = &text[..offset];
            let line_start = before.rfind('\n').map_or(0, |i| i + 1);
            Position {
                line: u32::try_from(before.matches('\n').count() + 1).unwrap_or(u32::MAX),
                column: u32::try_from(before[line_start..].chars().count() + 1)
                    .unwrap_or(u32::MAX),
            }
        };
        let utf16 = |offset: usize| text[..offset].encode_utf16().count();
        Self {
            start,
            end,
            utf16_start: utf16(start),
            utf16_end: utf16(end),
            from: position(start),
            to: position(end),
        }
    }
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

    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_count_lines_columns_and_utf16() {
        let text = "-- é\nCREATE TABLE 🦀 ();";
        let start = text.find('🦀').unwrap();
        let span = Span::of(text, start, start + '🦀'.len_utf8());
        assert_eq!(span.from, Position { line: 2, column: 14 });
        assert_eq!(span.to, Position { line: 2, column: 15 });
        // "-- é\n" is 5 UTF-16 units, "CREATE TABLE " 13 more; the crab is 2.
        assert_eq!((span.utf16_start, span.utf16_end), (18, 20));
    }

    #[test]
    fn diagnostics_serialize_like_the_sandbox_spells_them() {
        let json = serde_json::to_value(Diagnostic::error(Code::SyntaxError, "nope")).unwrap();
        assert_eq!(json["severity"], "error");
        assert_eq!(json["code"], "syntax_error");
        assert!(json["span"].is_null());
    }
}
