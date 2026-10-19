//! The security log of password sign-in (`auth_events`): who signed in,
//! failed, got locked, reset or changed a password, and what admins did from
//! the CLI. Append only (a trigger refuses updates), kept [`RETENTION`]
//! (then [`sweep`]). Read with `romulus-api auth-events`.

use std::{net::IpAddr, time::Duration};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::domain::AuthEventKind;

/// How long events are kept.
pub const RETENTION: Duration = Duration::from_hours(90 * 24);

/// One event to write.
pub struct Event<'a> {
    pub kind: AuthEventKind,
    pub user_id: Option<Uuid>,
    pub email: Option<&'a str>,
    pub ip: Option<IpAddr>,
    pub details: serde_json::Value,
}

impl<'a> Event<'a> {
    pub fn new(kind: AuthEventKind) -> Self {
        Self {
            kind,
            user_id: None,
            email: None,
            ip: None,
            details: serde_json::json!({}),
        }
    }

    #[must_use]
    pub fn user(mut self, user_id: Option<Uuid>) -> Self {
        self.user_id = user_id;
        self
    }

    #[must_use]
    pub fn email(mut self, email: &'a str) -> Self {
        self.email = Some(email);
        self
    }

    #[must_use]
    pub fn ip(mut self, ip: Option<IpAddr>) -> Self {
        self.ip = ip;
        self
    }

    #[must_use]
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

/// Writes `event` (in the caller's transaction, when it has one).
pub async fn record(db: impl PgExecutor<'_>, event: Event<'_>) -> Result<(), sqlx::Error> {
    // An address longer than the column allows is cut, never refused: the
    // event matters more than the exact text of a bogus input.
    let email = event
        .email
        .map(|email| email.chars().take(254).collect::<String>());
    sqlx::query!(
        "INSERT INTO auth_events (kind, user_id, email, ip, details) VALUES ($1, $2, $3, $4, $5)",
        event.kind as AuthEventKind,
        event.user_id,
        email,
        event.ip.map(ipnet::IpNet::from),
        event.details,
    )
    .execute(db)
    .await
    .map(drop)
}

/// An event as `romulus-api auth-events` prints it (one JSON line each).
#[derive(Debug, Serialize)]
pub struct Listed {
    pub at: DateTime<Utc>,
    pub kind: AuthEventKind,
    pub email: Option<String>,
    pub user_id: Option<Uuid>,
    pub ip: Option<String>,
    pub details: serde_json::Value,
}

/// The latest `limit` events, newest first, of `email` if given.
pub async fn list(
    db: impl PgExecutor<'_>,
    email: Option<&str>,
    limit: i64,
) -> Result<Vec<Listed>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT created_at, kind AS "kind: AuthEventKind", email, user_id, ip, details
           FROM auth_events
           WHERE $1::text IS NULL
              OR lower(email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)
           ORDER BY created_at DESC, id DESC
           LIMIT $2"#,
        email,
        limit,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Listed {
            at: row.created_at,
            kind: row.kind,
            email: row.email,
            user_id: row.user_id,
            ip: row.ip.map(|ip| ip.addr().to_string()),
            details: row.details,
        })
        .collect())
}

/// Deletes events older than [`RETENTION`].
pub async fn sweep(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM auth_events WHERE created_at < now() - make_interval(secs => $1)",
        RETENTION.as_secs_f64(),
    )
    .execute(db)
    .await?
    .rows_affected())
}
