//! What a compile tells the person typing: errors that block it, and notes.

use serde::Serialize;

use crate::source::Span;

/// One message about the SQL, located in the text the user wrote (never in
/// the prepared script the sandbox ran).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
    /// Index of the statement it is about, in [`crate::ParsedSql::statements`]
    /// order when the text parsed, else in source order.
    pub statement: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Blocks the compile: there is no model.
    Error,
    /// Something the compile did on the user's behalf, worth knowing.
    Info,
}

/// Why a [`Diagnostic`] was raised. Serialised in `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The text does not parse (reported per statement, several at once).
    SyntaxError,
    /// A statement or clause outside the DDL allowlist.
    NotAllowed,
    /// A function the sandbox refuses to call (side effects, or a constant
    /// call Postgres would evaluate while compiling).
    FunctionNotAllowed,
    /// `CREATE EXTENSION` of an extension the sandbox does not offer.
    ExtensionNotAvailable,
    /// The text is over the size limit.
    TooLarge,
    /// The text has more statements than the limit.
    TooManyStatements,
    /// Postgres rejected a statement; `sqlstate` says why.
    Sql,
    /// A statement ran past `statement_timeout` / `lock_timeout`, or the
    /// whole compile past its deadline.
    Timeout,
    /// The sandbox itself failed (unreachable, restarting); retrying helps.
    Unavailable,
    /// Info: a statement was not run because an extension created earlier
    /// already provides that object (remus output for PostGIS databases).
    ProvidedByExtension,
    /// The text still has a conflict block from "Update draft" (or a stray
    /// marker line): nothing compiles until it is resolved.
    Conflict,
    /// Info: a statement of a draft's migration that the compile does not
    /// run (data, transaction control): it cannot change the schema, and
    /// stays in the migration ([`crate::draft_migration`]).
    NotRun,
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

    pub fn info(code: Code, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            ..Self::error(code, message)
        }
    }

    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    #[must_use]
    pub fn with_statement(mut self, statement: usize) -> Self {
        self.statement = Some(statement);
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}
