//! The outbox contract and billing letters go through (`mail_outbox`): a
//! confirmation, a cancellation, a renewal reminder, a withdrawal's
//! acknowledgement, a change notice must reach the reader, and must not
//! reach them twice.
//!
//! - [`queue`] writes the letter in the transaction that decides it (with
//!   the row saying it was decided: `billing_notices`, `legal_notices`, a
//!   withdrawal), under a key naming what it is: queued twice, it is one row.
//! - [`deliver`] claims what is due (an atomic update, so the API's worker
//!   and a command's own pass never take the same letter), sends each one
//!   and waits for the server's answer: marked sent only once the server
//!   accepted it; otherwise tried again later ([`retry_in`]), until
//!   [`MAX_ATTEMPTS`] or [`GIVE_UP_AFTER`], when it is given up and logged
//!   as an error (the operator sends it by hand).
//! - [`run`]: the API's worker, woken by [`Mailer::wake`] and every
//!   [`EVERY`]; it also deletes sent letters after 30 days and given-up
//!   ones after 90 ([`purge`]).
//!
//! A server crashing between the mail server's answer and the row's update
//! sends that letter again: as rare as it gets without the mail server's
//! help.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{Attachment, Email, Mailer};
use crate::error::Chain;

/// How often the worker looks, besides being woken.
pub const EVERY: Duration = Duration::from_secs(30);
/// Attempts before a letter is given up.
pub const MAX_ATTEMPTS: i32 = 10;
/// Age after which a letter not sent yet is given up.
pub const GIVE_UP_AFTER: chrono::TimeDelta = chrono::TimeDelta::days(3);
/// How long a claimed letter is held by whoever claimed it (a send times out
/// well before).
const HOLD: chrono::TimeDelta = chrono::TimeDelta::minutes(10);
/// Claimed at a time.
const BATCH: i64 = 20;

/// A file a queued letter carries, by name: embedded in the binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Carried {
    filename: String,
    content_type: String,
}

/// Queues `email` under `key` in the caller's transaction: true when queued
/// now, false when a letter with that key was already (sent or not). Call
/// [`Mailer::wake`] once the transaction committed.
pub async fn queue(conn: &mut PgConnection, key: &str, email: &Email) -> Result<bool, sqlx::Error> {
    let carried: Vec<Carried> = email
        .attachments
        .iter()
        .map(|a| Carried {
            filename: a.filename.clone(),
            content_type: a.content_type.to_owned(),
        })
        .collect();
    Ok(sqlx::query_scalar!(
        "INSERT INTO mail_outbox (key, kind, recipient, subject, text_body, html_body, attachments)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (key) DO NOTHING RETURNING id",
        key,
        &*email.kind,
        email.to,
        email.subject,
        email.text,
        email.html,
        serde_json::to_value(carried).unwrap_or_default(),
    )
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

/// What one [`deliver`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    pub sent: usize,
    /// Refused or failed: tried again later.
    pub retrying: usize,
    /// Given up now (logged as errors).
    pub given_up: usize,
}

/// When a letter that failed `attempts` times is tried again: 1 minute, then
/// 4 times longer each time, at most 6 hours.
pub fn retry_in(attempts: i32) -> chrono::TimeDelta {
    let minutes = 4_i64.saturating_pow(u32::try_from(attempts.max(1) - 1).unwrap_or(0));
    chrono::TimeDelta::minutes(minutes.min(6 * 60))
}

struct Claimed {
    id: Uuid,
    kind: String,
    recipient: String,
    subject: String,
    text_body: String,
    html_body: String,
    attachments: serde_json::Value,
    attempts: i32,
    created_at: DateTime<Utc>,
}

/// Sends every letter due, until none is: each marked sent once the server
/// accepted it.
pub async fn deliver(db: &PgPool, mailer: &Mailer) -> Result<Delivered, sqlx::Error> {
    let mut done = Delivered::default();
    if !mailer.is_on() {
        return Ok(done);
    }
    loop {
        let claimed = sqlx::query_as!(
            Claimed,
            "UPDATE mail_outbox SET attempts = attempts + 1, next_attempt_at = $1
             WHERE id IN (SELECT id FROM mail_outbox
                          WHERE sent_at IS NULL AND failed_at IS NULL AND next_attempt_at <= now()
                          ORDER BY next_attempt_at, id LIMIT $2 FOR UPDATE SKIP LOCKED)
             RETURNING id, kind, recipient, subject, text_body, html_body, attachments, attempts, created_at",
            Utc::now() + HOLD,
            BATCH,
        )
        .fetch_all(db)
        .await?;
        if claimed.is_empty() {
            return Ok(done);
        }
        for letter in claimed {
            match send(mailer, &letter).await {
                Ok(()) => {
                    sqlx::query!(
                        "UPDATE mail_outbox SET sent_at = now(), last_error = NULL WHERE id = $1",
                        letter.id
                    )
                    .execute(db)
                    .await?;
                    tracing::info!(kind = %letter.kind, "mail: sent");
                    done.sent += 1;
                }
                Err(error) => {
                    let give_up = letter.attempts >= MAX_ATTEMPTS
                        || Utc::now() - letter.created_at >= GIVE_UP_AFTER;
                    let error: String = error.chars().take(2000).collect();
                    sqlx::query!(
                        "UPDATE mail_outbox SET last_error = $2,
                                next_attempt_at = $3,
                                failed_at = CASE WHEN $4 THEN now() END
                         WHERE id = $1",
                        letter.id,
                        error,
                        Utc::now() + retry_in(letter.attempts),
                        give_up,
                    )
                    .execute(db)
                    .await?;
                    if give_up {
                        tracing::error!(
                            kind = %letter.kind,
                            letter = %letter.id,
                            attempts = letter.attempts,
                            error = %error,
                            "mail: a letter could not be sent and was given up: send it by hand (mail_outbox)"
                        );
                        done.given_up += 1;
                    } else {
                        tracing::warn!(kind = %letter.kind, attempts = letter.attempts, error = %error, "mail: not sent, tried again later");
                        done.retrying += 1;
                    }
                }
            }
        }
    }
}

async fn send(mailer: &Mailer, letter: &Claimed) -> Result<(), String> {
    let carried: Vec<Carried> =
        serde_json::from_value(letter.attachments.clone()).map_err(|err| err.to_string())?;
    let attachments = carried
        .iter()
        .map(|c| {
            crate::legal::pdf::by_filename(&c.filename)
                .ok_or_else(|| format!("{} is not in this build", c.filename))
        })
        .collect::<Result<Vec<Attachment>, String>>()?;
    let email = Email {
        to: letter.recipient.clone(),
        kind: letter.kind.clone().into(),
        subject: letter.subject.clone(),
        text: letter.text_body.clone(),
        html: letter.html_body.clone(),
        attachments,
    };
    mailer
        .send_now(&email)
        .await
        .map_err(|err| Chain(&err).to_string())
}

/// Deletes what no one needs any more: sent letters after 30 days, given-up
/// ones after 90.
pub async fn purge(db: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM mail_outbox
         WHERE sent_at < now() - interval '30 days' OR failed_at < now() - interval '90 days'"
    )
    .execute(db)
    .await?
    .rows_affected())
}

/// The API's worker ([`run`]), when mail is on.
pub fn tasks(db: &PgPool, mailer: &Mailer) -> Vec<tokio::task::JoinHandle<()>> {
    if mailer.is_on() {
        vec![run(db.clone(), mailer.clone())]
    } else {
        Vec::new()
    }
}

/// The API's worker: delivers when woken, and every [`EVERY`]; purges hourly.
pub fn run(db: PgPool, mailer: Mailer) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut purged_at: Option<std::time::Instant> = None;
        loop {
            if let Err(err) = deliver(&db, &mailer).await {
                tracing::warn!(error = %Chain(&err), "mail: the outbox could not be read");
            }
            if purged_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(3600)) {
                match purge(&db).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(letters = n, "mail: old letters deleted from the outbox");
                    }
                    Err(err) => {
                        tracing::warn!(error = %Chain(&err), "mail: the outbox purge failed");
                    }
                }
                purged_at = Some(std::time::Instant::now());
            }
            tokio::select! {
                () = mailer.woken() => {}
                () = tokio::time::sleep(EVERY) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_tried_again_later_each_time_up_to_six_hours() {
        assert_eq!(retry_in(1), chrono::TimeDelta::minutes(1));
        assert_eq!(retry_in(2), chrono::TimeDelta::minutes(4));
        assert_eq!(retry_in(3), chrono::TimeDelta::minutes(16));
        assert_eq!(retry_in(5), chrono::TimeDelta::minutes(256));
        assert_eq!(retry_in(6), chrono::TimeDelta::hours(6));
        assert_eq!(retry_in(40), chrono::TimeDelta::hours(6));
    }
}
