//! One pool slot: its connection, and running a prepared script on it.

use remus_core::{INTROSPECT_SQL, Schema};
use serde::Deserialize;
use sqlx::{
    AssertSqlSafe, Connection, PgConnection, Row,
    postgres::{PgConnectOptions, PgDatabaseError, PgErrorPosition},
    raw_sql,
};

use super::bootstrap::SlotSpec;
use crate::{
    catalog::Catalog,
    diagnostic::{Code, Diagnostic},
    parse::ParsedSql,
    prepare::{Injection, PROGRESS, Prepared, statement_of_progress},
    refine::Failure,
    source::char_to_byte,
};

/// Session objects each slot connection creates once: the progress
/// sequence and the policy roles table (see [`crate::prepare`]). Temporary,
/// so they are invisible to introspection and gone with the connection.
const SESSION_SETUP: &str = "
CREATE TEMPORARY SEQUENCE IF NOT EXISTS romulus_sandbox_progress;
CREATE TEMPORARY TABLE IF NOT EXISTS romulus_sandbox_policy_roles (
    policy oid PRIMARY KEY,
    roles text[] NOT NULL
);";

/// The policies whose roles a compile recorded, by their current name.
const POLICY_ROLES_SQL: &str = "
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'schema', n.nspname, 'table', c.relname, 'policy', p.polname, 'roles', r.roles)), '[]')::text
FROM pg_temp.romulus_sandbox_policy_roles r
JOIN pg_catalog.pg_policy p ON p.oid = r.policy
JOIN pg_catalog.pg_class c ON c.oid = p.polrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace";

pub(crate) struct Slot {
    pub spec: SlotSpec,
    pub conn: Option<PgConnection>,
    /// The backend serving `conn`, to terminate a runaway compile.
    pub pid: Option<i32>,
}

/// How an attempt ended, short of producing a model.
pub(crate) enum AttemptError {
    /// Postgres rejected the script (or introspection): a user error.
    Sql(Box<SqlFailure>),
    /// The connection failed: the sandbox is unavailable, not the SQL.
    Connection(sqlx::Error),
}

pub(crate) struct SqlFailure {
    pub failure: Failure,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// The statement the progress markers say was running.
    pub statement: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PolicyRoles {
    schema: String,
    table: String,
    policy: String,
    roles: Vec<String>,
}

pub(crate) struct Settings {
    pub statement_timeout_ms: u128,
    pub lock_timeout_ms: u128,
}

impl Slot {
    pub fn new(spec: SlotSpec) -> Self {
        Self {
            spec,
            conn: None,
            pid: None,
        }
    }

    /// The slot's connection, opened (as its own role) if needed.
    pub async fn connection(
        &mut self,
        base: &PgConnectOptions,
    ) -> Result<&mut PgConnection, sqlx::Error> {
        if self.conn.is_none() {
            let options = base
                .clone()
                .username(&self.spec.role)
                .password(&self.spec.password)
                .database(&self.spec.database)
                .application_name("romulus-sandbox")
                .statement_cache_capacity(0);
            let mut conn = PgConnection::connect_with(&options).await?;
            raw_sql(SESSION_SETUP).execute(&mut conn).await?;
            let pid: i32 = raw_sql("SELECT pg_backend_pid()")
                .fetch_one(&mut conn)
                .await?
                .try_get(0)?;
            self.pid = Some(pid);
            self.conn = Some(conn);
        }
        self.conn.as_mut().ok_or(sqlx::Error::PoolClosed)
    }

    /// Forget a connection in an unknown state (it is closed on drop).
    pub fn discard(&mut self) {
        self.conn = None;
        self.pid = None;
    }

    pub async fn close(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.close().await;
        }
        self.pid = None;
    }

    /// Run a prepared script in a transaction, introspect, roll back.
    pub async fn attempt(
        &mut self,
        base: &PgConnectOptions,
        settings: &Settings,
        prepared: &Prepared,
    ) -> Result<(String, Vec<PolicyRoles>), AttemptError> {
        let conn = self
            .connection(base)
            .await
            .map_err(AttemptError::Connection)?;
        let begin = format!(
            "BEGIN; SET LOCAL statement_timeout = '{}ms'; SET LOCAL lock_timeout = '{}ms'; \
             SET LOCAL search_path TO \"$user\", public",
            settings.statement_timeout_ms, settings.lock_timeout_ms
        );
        raw_sql(AssertSqlSafe(begin))
            .execute(&mut *conn)
            .await
            .map_err(AttemptError::Connection)?;

        let ran = raw_sql(AssertSqlSafe(prepared.script.as_str()))
            .execute(&mut *conn)
            .await;
        if let Err(err) = ran {
            return Err(failed(conn, err, &prepared.script).await);
        }
        let model: Result<String, sqlx::Error> = async {
            raw_sql(INTROSPECT_SQL)
                .fetch_one(&mut *conn)
                .await?
                .try_get(0)
        }
        .await;
        let model = match model {
            Ok(model) => model,
            Err(err) => return Err(failed(conn, err, INTROSPECT_SQL).await),
        };
        let roles = if prepared.records_policy_roles {
            let json: String = raw_sql(POLICY_ROLES_SQL)
                .fetch_one(&mut *conn)
                .await
                .and_then(|row| row.try_get(0))
                .map_err(AttemptError::Connection)?;
            serde_json::from_str(&json).unwrap_or_default()
        } else {
            Vec::new()
        };
        raw_sql("ROLLBACK")
            .execute(&mut *conn)
            .await
            .map_err(AttemptError::Connection)?;
        Ok((model, roles))
    }
}

/// Turn a failed statement into a user error (rolling back and reading the
/// progress marker), or a connection error.
async fn failed(conn: &mut PgConnection, err: sqlx::Error, sent: &str) -> AttemptError {
    let Some(db) = err.as_database_error() else {
        return AttemptError::Connection(err);
    };
    let Some(pg) = db.try_downcast_ref::<PgDatabaseError>() else {
        return AttemptError::Connection(err);
    };
    let position = match pg.position() {
        Some(PgErrorPosition::Original(chars)) => Some(char_to_byte(sent, chars.saturating_sub(1))),
        _ => None,
    };
    let failure = SqlFailure {
        failure: Failure {
            sqlstate: pg.code().to_owned(),
            message: pg.message().to_owned(),
            position,
        },
        detail: pg.detail().map(str::to_owned),
        hint: pg.hint().map(str::to_owned),
        statement: None,
    };
    if let Err(err) = raw_sql("ROLLBACK").execute(&mut *conn).await {
        return AttemptError::Connection(err);
    }
    let progress: Result<String, sqlx::Error> = async {
        raw_sql(AssertSqlSafe(format!(
            "SELECT last_value::text FROM {PROGRESS}"
        )))
        .fetch_one(&mut *conn)
        .await?
        .try_get(0)
    }
    .await;
    match progress {
        Ok(value) => AttemptError::Sql(Box::new(SqlFailure {
            statement: value.parse().ok().and_then(statement_of_progress),
            ..failure
        })),
        Err(err) => AttemptError::Connection(err),
    }
}

/// The diagnostic for a Postgres error, located in the user's text.
pub(crate) fn diagnostic(
    failure: &SqlFailure,
    prepared: Option<&Prepared>,
    parsed: &ParsedSql,
    sql: &str,
    catalog: &Catalog,
) -> Diagnostic {
    let located = prepared.and_then(|p| {
        failure
            .failure
            .position
            .map(|position| p.locate(position, parsed))
    });
    let statement = located
        .as_ref()
        .and_then(|l| l.statement)
        .or(failure.statement);
    let range = located
        .as_ref()
        .and_then(|l| l.source.clone())
        .map(|range| {
            if range.is_empty() {
                parsed.token_at(range.start).unwrap_or(range)
            } else {
                range
            }
        })
        .or_else(|| {
            // Postgres gives no position for some errors (column types of
            // CREATE TABLE, index columns, policy expressions): underline
            // the name the message quotes, when the statement has it once.
            let statement = parsed.statements.get(statement?)?;
            let name = quoted_name(&failure.failure.message)?;
            named_token(parsed, statement, sql, name)
        })
        .or_else(|| {
            statement
                .and_then(|i| parsed.statements.get(i))
                .map(|s| s.span.range())
        });

    let sqlstate = failure.failure.sqlstate.as_str();
    let timeout = matches!(sqlstate, "57014" | "55P03");
    let mut message = failure.failure.message.clone();
    let mut hint = failure.hint.clone();
    if let (Some(Injection::Stub(k)), Some(prepared)) =
        (located.as_ref().and_then(|l| l.injection), prepared)
        && let Some(stub) = prepared.stubs.get(k)
    {
        message = format!("could not stand in for {}: {message}", stub.signature());
        hint = Some(
            "romulus creates a placeholder for functions the SQL calls but does not define; this one could not be created."
                .to_owned(),
        );
    } else if timeout {
        hint = Some(if sqlstate == "55P03" {
            "The statement waited too long for a lock.".to_owned()
        } else {
            "The sandbox stops statements that run too long; nothing in a schema should take this long."
                .to_owned()
        });
    } else if hint.is_none() {
        hint = extension_hint(&message, catalog);
    }

    let mut diagnostic =
        Diagnostic::error(if timeout { Code::Timeout } else { Code::Sql }, message);
    diagnostic.sqlstate = Some(failure.failure.sqlstate.clone());
    diagnostic.detail.clone_from(&failure.detail);
    diagnostic.hint = hint;
    if let Some(range) = range {
        diagnostic = diagnostic.with_span(parsed.span(sql, range));
    }
    if let Some(statement) = statement {
        diagnostic = diagnostic.with_statement(statement);
    }
    diagnostic
}

/// The first double-quoted name in a message, its last dotted part
/// (`relation "app.users" does not exist` → `users`).
fn quoted_name(message: &str) -> Option<&str> {
    let start = message.find('"')? + 1;
    let end = start + message[start..].find('"')?;
    let name = &message[start..end];
    Some(name.rsplit('.').next().unwrap_or(name))
}

/// The only identifier token of a statement that spells `name`.
fn named_token(
    parsed: &ParsedSql,
    statement: &crate::parse::Statement,
    sql: &str,
    name: &str,
) -> Option<std::ops::Range<usize>> {
    let mut found = parsed.tokens_of(statement).iter().filter(|t| {
        let raw = &sql[t.start..t.end];
        let spelled = raw
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .map_or_else(|| raw.to_lowercase(), |q| q.replace("\"\"", "\""));
        !t.is_comment() && spelled == name
    });
    let first = found.next()?;
    found.next().is_none().then_some(first.start..first.end)
}

/// "type "vector" does not exist" → "vector comes with extension vector:
/// add CREATE EXTENSION vector".
fn extension_hint(message: &str, catalog: &Catalog) -> Option<String> {
    let quoted = |prefix: &str| {
        message
            .strip_prefix(prefix)?
            .split('"')
            .next()
            .map(str::to_owned)
    };
    let (thing, extension) = if let Some(name) = quoted("type \"") {
        let bare = name.rsplit('.').next().unwrap_or(&name).to_owned();
        (name, catalog.extension_with_type(&bare)?)
    } else {
        let name = message
            .strip_prefix("function ")?
            .split('(')
            .next()?
            .to_owned();
        let bare = name.rsplit('.').next().unwrap_or(&name).to_owned();
        (name, catalog.extension_with_function(&bare)?)
    };
    Some(format!(
        "{thing} comes with extension {extension}: add CREATE EXTENSION IF NOT EXISTS {} first.",
        if extension.contains('-') {
            format!("\"{extension}\"")
        } else {
            extension.to_owned()
        }
    ))
}

/// The model as the user would see it: policy roles put back, the sandbox
/// database name replaced, canonical.
pub(crate) fn postprocess(
    json: &str,
    roles: &[PolicyRoles],
    database: &str,
) -> Result<Schema, remus_core::Error> {
    let mut schema = Schema::from_json(json)?;
    for recorded in roles {
        if let Some(policy) = schema
            .entities
            .iter_mut()
            .filter(|e| e.schema == recorded.schema && e.name == recorded.table)
            .flat_map(|e| e.policies.iter_mut())
            .find(|p| p.name == recorded.policy)
        {
            policy.roles.clone_from(&recorded.roles);
        }
    }
    database.clone_into(&mut schema.database);
    Ok(schema.canonical())
}

#[cfg(test)]
mod tests {
    use super::{PolicyRoles, extension_hint, postprocess};
    use crate::catalog::{Catalog, Extension};

    #[test]
    fn missing_extension_types_and_functions_get_a_hint() {
        let catalog = Catalog::new(
            [],
            [
                (
                    "vector".to_owned(),
                    Extension {
                        types: ["vector".to_owned()].into_iter().collect(),
                        ..Extension::default()
                    },
                ),
                (
                    "uuid-ossp".to_owned(),
                    Extension {
                        functions: ["uuid_generate_v4".to_owned()].into_iter().collect(),
                        ..Extension::default()
                    },
                ),
            ],
        );
        assert_eq!(
            extension_hint("type \"vector\" does not exist", &catalog).unwrap(),
            "vector comes with extension vector: add CREATE EXTENSION IF NOT EXISTS vector first."
        );
        assert_eq!(
            extension_hint("function uuid_generate_v4() does not exist", &catalog).unwrap(),
            "uuid_generate_v4 comes with extension uuid-ossp: add CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\" first."
        );
        assert_eq!(
            extension_hint("relation \"x\" does not exist", &catalog),
            None
        );
    }

    #[test]
    fn postprocess_restores_roles_and_names_the_database() {
        let json = r#"{
            "remus_version": "0.1", "generated_at": "2026-10-15T00:00:00Z",
            "database": "romulus_sandbox_3", "server_version": "17.11",
            "entities": [{
                "schema": "public", "name": "docs", "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null,
                "policies": [{"name": "own", "command": "*", "permissive": true,
                              "roles": [], "using": "true", "check": null}]
            }]
        }"#;
        let roles = [PolicyRoles {
            schema: "public".to_owned(),
            table: "docs".to_owned(),
            policy: "own".to_owned(),
            roles: vec!["app_user".to_owned()],
        }];
        let schema = postprocess(json, &roles, "sandbox").unwrap();
        assert_eq!(schema.database, "sandbox");
        assert_eq!(schema.generated_at, None);
        assert_eq!(schema.entities[0].policies[0].roles, ["app_user"]);
    }
}
