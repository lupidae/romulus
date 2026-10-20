//! The one thing the engine needs from where it runs: a Postgres session.
//!
//! [`Session`] runs SQL in the simple query protocol (every value arrives
//! as Postgres's own text) within a time limit, and copies rows in. What a
//! limit means is the session's to enforce: a playground's cancels the
//! statement, then terminates its backend after a grace period; a sqlx
//! connection (feature `sqlx`) gives up waiting. Nothing else about where
//! it runs (a playground, a scratch database of the sandbox cluster) is
//! known here.

use std::{future::Future, time::Duration};

use serde::{Deserialize, Serialize};

/// A Postgres error as the server sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbError {
    /// The SQLSTATE (`23502`).
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// 1-based, in characters of the query sent.
    pub position: Option<usize>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub constraint: Option<String>,
}

impl DbError {
    /// The byte offset of [`DbError::position`] in `query`.
    pub fn byte_position(&self, query: &str) -> Option<usize> {
        let chars = self.position?.checked_sub(1)?;
        Some(
            query
                .char_indices()
                .nth(chars)
                .map_or(query.len(), |(i, _)| i),
        )
    }
}

/// Why a [`Session`] call did not give an answer.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SessionError {
    /// Postgres refused it (a cancel past the limit is one too, `57014`).
    #[error("{} ({})", .0.message, .0.code)]
    Db(Box<DbError>),
    /// Past its limit, and the session could not say more (it gave up
    /// waiting): the session may be unusable now.
    #[error("it ran past its time limit")]
    Timeout,
    /// The connection broke, or the server said something unexpected.
    #[error("{0}")]
    Lost(String),
}

impl SessionError {
    pub fn db(&self) -> Option<&DbError> {
        match self {
            Self::Db(error) => Some(error),
            _ => None,
        }
    }

    /// Past its limit: cancelled (`57014`), or given up on.
    pub fn timed_out(&self) -> bool {
        matches!(self, Self::Timeout) || self.db().is_some_and(|e| e.code == "57014")
    }
}

impl From<DbError> for SessionError {
    fn from(error: DbError) -> Self {
        Self::Db(Box::new(error))
    }
}

/// What a query left: the command tag of its last statement (when the
/// session knows it) and, when asked, the rows of the statement that
/// returned rows, every value as text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub tag: Option<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Rows a query should keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rows {
    /// None: rows are read past without being decoded (a migration's own
    /// `SELECT`, which may return millions).
    Skip,
    /// The first `rows`, within `bytes` (the rest read past).
    Keep { rows: usize, bytes: usize },
}

impl Rows {
    /// What the engine's own reads keep: counts, samples, catalogs.
    pub const READ: Self = Self::Keep {
        rows: 100_000,
        bytes: 64 * 1024 * 1024,
    };
}

/// A Postgres session the engine runs on. Its queries name everything
/// qualified and ask for text (`::text`), so a session needs no type
/// knowledge.
pub trait Session: Send {
    /// Runs `sql` to its end (one statement, or several in one simple
    /// query: then the rows are those of the one statement returning rows)
    /// within `limit`.
    fn run(
        &mut self,
        sql: &str,
        limit: Duration,
        rows: Rows,
    ) -> impl Future<Output = Result<Answer, SessionError>> + Send;

    /// `statement` (a `COPY … FROM STDIN`, text format), fed `data`'s
    /// chunks of whole lines, within `limit`: the rows it took.
    fn copy_in(
        &mut self,
        statement: &str,
        data: &mut (dyn Iterator<Item = Vec<u8>> + Send),
        limit: Duration,
    ) -> impl Future<Output = Result<u64, SessionError>> + Send;
}

/// Quotes an identifier for SQL, always.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quotes a string literal as an escape string (`E'…'`): right whatever
/// `standard_conforming_strings` a session set. A NUL, which no Postgres
/// text holds, is left out.
pub fn quote_literal(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 3);
    quoted.push_str("E'");
    for c in text.chars() {
        match c {
            '\'' => quoted.push_str("''"),
            '\\' => quoted.push_str("\\\\"),
            '\0' => {}
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

/// `"schema"."name"`.
pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

#[cfg(feature = "sqlx")]
mod sqlx_session {
    use std::time::Duration;

    use sqlx::{
        AssertSqlSafe, PgConnection, Row, ValueRef,
        postgres::{PgDatabaseError, PgErrorPosition},
        raw_sql,
    };

    use super::{Answer, DbError, Rows, Session, SessionError};

    #[allow(clippy::needless_pass_by_value, reason = "the shape `map_err` takes")]
    fn error(err: sqlx::Error) -> SessionError {
        match &err {
            sqlx::Error::Database(db) => match db.try_downcast_ref::<PgDatabaseError>() {
                Some(pg) => SessionError::Db(Box::new(DbError {
                    code: pg.code().to_owned(),
                    message: pg.message().to_owned(),
                    detail: pg.detail().map(str::to_owned),
                    hint: pg.hint().map(str::to_owned),
                    position: match pg.position() {
                        Some(PgErrorPosition::Original(at)) => Some(at),
                        _ => None,
                    },
                    schema: pg.schema().map(str::to_owned),
                    table: pg.table().map(str::to_owned),
                    column: pg.column().map(str::to_owned),
                    constraint: pg.constraint().map(str::to_owned),
                })),
                None => SessionError::Lost(err.to_string()),
            },
            _ => SessionError::Lost(err.to_string()),
        }
    }

    /// A scratch database's connection (the oracle, tests). A limit passed
    /// is waiting given up: the connection is then left as it is (drop the
    /// database).
    impl Session for PgConnection {
        async fn run(
            &mut self,
            sql: &str,
            limit: Duration,
            rows: Rows,
        ) -> Result<Answer, SessionError> {
            let sql = sql.to_owned();
            let work = async {
                match rows {
                    Rows::Skip => {
                        raw_sql(AssertSqlSafe(sql))
                            .execute(&mut *self)
                            .await
                            .map_err(error)?;
                        Ok(Answer::default())
                    }
                    Rows::Keep { rows: max, bytes } => {
                        let fetched = raw_sql(AssertSqlSafe(sql))
                            .fetch_all(&mut *self)
                            .await
                            .map_err(error)?;
                        let mut kept = Vec::new();
                        let mut size = 0;
                        for row in fetched.iter().take(max) {
                            let mut values = Vec::with_capacity(row.len());
                            for i in 0..row.len() {
                                let raw = row.try_get_raw(i).map_err(error)?;
                                values.push(if raw.is_null() {
                                    None
                                } else {
                                    let text = raw
                                        .as_str()
                                        .map_err(|e| SessionError::Lost(e.to_string()))?;
                                    size += text.len();
                                    Some(text.to_owned())
                                });
                            }
                            if size > bytes {
                                break;
                            }
                            kept.push(values);
                        }
                        Ok(Answer {
                            tag: None,
                            rows: kept,
                        })
                    }
                }
            };
            tokio::time::timeout(limit, work)
                .await
                .map_err(|_| SessionError::Timeout)?
        }

        async fn copy_in(
            &mut self,
            statement: &str,
            data: &mut (dyn Iterator<Item = Vec<u8>> + Send),
            limit: Duration,
        ) -> Result<u64, SessionError> {
            let work = async {
                let mut sink = self.copy_in_raw(statement).await.map_err(error)?;
                for chunk in data {
                    if let Err(err) = sink.send(chunk).await {
                        let _ = sink.abort("the rows could not be sent").await;
                        return Err(error(err));
                    }
                }
                sink.finish().await.map_err(error)
            };
            tokio::time::timeout(limit, work)
                .await
                .map_err(|_| SessionError::Timeout)?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_literals_are_quoted() {
        assert_eq!(quote_ident(r#"we"ird"#), r#""we""ird""#);
        assert_eq!(quote_literal("it's"), "E'it''s'");
        assert_eq!(quote_literal(r"a\'; x"), r"E'a\\''; x'");
        assert_eq!(qualified("public", "Order"), r#""public"."Order""#);
    }

    #[test]
    fn positions_are_characters_turned_into_bytes() {
        let error = DbError {
            position: Some(10),
            ..DbError::default()
        };
        assert_eq!(error.byte_position("SELECT 'é' + x"), Some(10));
    }
}
