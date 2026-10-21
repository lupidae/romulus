//! Accounts unused for [`UNUSED_YEARS`] years are deleted, after an email
//! [`NOTICE_DAYS`] days before (`ROMULUS_DELETE_INACTIVE_ACCOUNTS`, which
//! needs mail, and the Zitadel token when single sign-on is on; off by
//! default, so a self-hosted install keeps every account). What counts as
//! use is `super::activity`'s (`users.last_seen_at`).
//!
//! - **The notice** goes once the account is [`NOTICE_DAYS`] days from its
//!   3 years (or at once, for older accounts when this is turned on): the
//!   date, what goes with it, and that signing in keeps it. Once: the
//!   account is claimed first (`users.inactivity_retry_at`, an hour's lease
//!   an `UPDATE … RETURNING` takes), the email sent, and the notice recorded
//!   (`users.inactivity_notice_at`) only once the mail server took it; a
//!   failed send gives the claim back as a back-off. A process dying
//!   mid-send leaves the lease to run out: never a notice recorded unsent.
//!   Using the account clears it all.
//! - **The deletion** comes [`NOTICE_DAYS`] days after the notice at the
//!   earliest, never before the 3 years, if the account was not used since:
//!   `deletion::Deleter::delete_account` with `By::Inactivity` (the
//!   organizations it is the only member of go with it; where it is the only
//!   admin, another member is appointed).
//! - An account that is the only member of an organization with a
//!   subscription not over for good is left alone, notice and deletion:
//!   someone pays, or may, for it to be there.
//! - **A failure** (mail refused, the identity provider down) puts that
//!   account off: an hour, doubled at each failure in a row, a week at most
//!   (`users.inactivity_failures`). The others go on.
//!
//! A hundred of each per sweep (`super::sweep`, hourly, one process at a
//! time).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::deletion::{By, Deleter, DeletionError, Fate};
use crate::{AppState, billing::state::ENDED_STATUSES, error::Chain, mail::Letter};

/// Years without use before an account is deleted.
pub const UNUSED_YEARS: i32 = 3;
/// Days between the notice and the deletion, at least.
pub const NOTICE_DAYS: i32 = 30;

/// Sends the notices due. How many the mail server took (and were
/// recorded).
pub async fn send_notices(state: &AppState) -> Result<u64, sqlx::Error> {
    let due = sqlx::query!(
        r#"SELECT u.id, u.last_seen_at FROM users u
           WHERE u.inactivity_notice_at IS NULL
             AND u.last_seen_at < now() - make_interval(years => $1) + make_interval(days => $2)
             AND (u.inactivity_retry_at IS NULL OR u.inactivity_retry_at <= now())
             AND NOT EXISTS (
                 SELECT 1 FROM memberships m JOIN org_billing b ON b.org_id = m.org_id
                 WHERE m.user_id = u.id AND b.subscription_id IS NOT NULL
                   AND (b.status IS NULL OR NOT (b.status = ANY($3)))
                   AND NOT EXISTS (SELECT 1 FROM memberships o WHERE o.org_id = m.org_id AND o.user_id <> u.id)
             )
           ORDER BY u.last_seen_at LIMIT 100"#,
        UNUSED_YEARS,
        NOTICE_DAYS,
        &ENDED_STATUSES.map(str::to_owned) as &[String],
    )
    .fetch_all(&state.db)
    .await?;
    let deleter = deleter(state);
    let mut sent = 0;
    for row in due {
        match notify(state, &deleter, row.id, row.last_seen_at).await {
            Ok(Notice::Sent) => sent += 1,
            Ok(Notice::NotDue) => {}
            Ok(Notice::Failed(err)) => failed(state, row.id, "the notice", err).await,
            Err(err) => {
                let error = Chain(&err).to_string();
                failed(state, row.id, "the notice", error).await;
            }
        }
    }
    Ok(sent)
}

/// What became of one notice.
enum Notice {
    Sent,
    /// Claimed elsewhere, or used since it was found: nothing to send.
    NotDue,
    /// The mail server did not take it.
    Failed(String),
}

async fn notify(
    state: &AppState,
    deleter: &Deleter,
    user_id: Uuid,
    last_seen_at: DateTime<Utc>,
) -> Result<Notice, sqlx::Error> {
    let Some(deleted_on) = claim(state, user_id, last_seen_at).await? else {
        return Ok(Notice::NotDue);
    };
    let Some(plan) = deleter.plan_account(user_id).await? else {
        return Ok(Notice::NotDue);
    };
    let day = |at: DateTime<Utc>| at.format("%-d %B %Y").to_string();
    let mut paragraphs = vec![format!(
        "Nobody has signed in to your romulus account ({}) since {}. Accounts unused for {UNUSED_YEARS} years are deleted: yours will be on {}, unless you sign in before then.",
        plan.email,
        day(last_seen_at),
        day(deleted_on),
    )];
    let alone_in: Vec<&str> = plan
        .deletes()
        .iter()
        .map(|org| org.org.name.as_str())
        .collect();
    if !alone_in.is_empty() {
        paragraphs.push(format!(
            "The organizations you are the only member of would go with it: {}. Export what you want to keep first.",
            alone_in.join(", ")
        ));
    }
    let led: Vec<&str> = plan
        .memberships
        .iter()
        .filter(|m| matches!(m.fate, Fate::LastAdmin { .. }))
        .map(|m| m.org.name.as_str())
        .collect();
    if !led.is_empty() {
        paragraphs.push(format!(
            "You are the only admin of {}: its most recently active developer (or reviewer, where it has no developer) would become an admin.",
            led.join(", ")
        ));
    }
    paragraphs.push("Signing in once keeps your account; nothing else is needed.".to_owned());
    let signin = format!("{}/app/signin", state.config.public_origin());
    let email = Letter {
        kind: "inactivity_notice",
        to: &plan.email,
        subject: format!("Your romulus account will be deleted on {}", day(deleted_on)),
        paragraphs,
        action: Some(("Sign in to keep it", &signin)),
        footer: format!(
            "Sent by romulus at {} to the address of an account nobody has used for almost {UNUSED_YEARS} years.",
            state.config.public_origin()
        ),
    }
    .email();
    if let Err(err) = state.mailer.send_now(&email).await {
        return Ok(Notice::Failed(Chain(&err).to_string()));
    }
    // Recorded with the claim released, unless the account was used
    // meanwhile (then no notice holds, sent or not).
    let recorded = sqlx::query!(
        "UPDATE users SET inactivity_notice_at = now(), inactivity_retry_at = NULL, inactivity_failures = 0
         WHERE id = $1 AND inactivity_notice_at IS NULL AND last_seen_at = $2",
        user_id,
        last_seen_at,
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    if recorded == 1 {
        tracing::info!(account = %user_id, deleted_on = %deleted_on, "inactive account: notice sent and recorded");
        Ok(Notice::Sent)
    } else {
        tracing::info!(account = %user_id, "inactive account: notice sent, but the account was used meanwhile: not recorded");
        Ok(Notice::NotDue)
    }
}

/// Claims `user_id`'s notice for an hour, if it is still due as read
/// (`last_seen_at`) and nobody holds it: the day the account would be
/// deleted, or `None`.
pub async fn claim(
    state: &AppState,
    user_id: Uuid,
    last_seen_at: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"UPDATE users SET inactivity_retry_at = now() + interval '1 hour'
           WHERE id = $1 AND inactivity_notice_at IS NULL AND last_seen_at = $2
             AND (inactivity_retry_at IS NULL OR inactivity_retry_at <= now())
           RETURNING greatest(last_seen_at + make_interval(years => $3),
                              now() + make_interval(days => $4)) AS "deleted_on!""#,
        user_id,
        last_seen_at,
        UNUSED_YEARS,
        NOTICE_DAYS,
    )
    .fetch_optional(&state.db)
    .await
}

/// Puts `user_id` off after a failure: an hour, doubled at each failure in
/// a row, a week at most. Never fails the sweep.
async fn failed(state: &AppState, user_id: Uuid, what: &str, error: String) {
    let retry_at = sqlx::query_scalar!(
        r#"UPDATE users SET inactivity_failures = inactivity_failures + 1,
                  inactivity_retry_at = now() + least(interval '1 hour' * power(2::float8, least(inactivity_failures, 8)::float8),
                                                      interval '7 days')
           WHERE id = $1 RETURNING inactivity_retry_at AS "retry_at!""#,
        user_id,
    )
    .fetch_optional(&state.db)
    .await;
    match retry_at {
        Ok(Some(retry_at)) => {
            tracing::warn!(account = %user_id, %error, %retry_at, "inactive account: {what} failed, tried again later");
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(account = %user_id, %error, record_error = %err, "inactive account: {what} failed, and so did putting it off");
        }
    }
}

/// Deletes the accounts whose notice ran out unused. How many.
pub async fn delete_due(state: &AppState) -> Result<u64, sqlx::Error> {
    let due = sqlx::query_scalar!(
        "SELECT u.id FROM users u
         WHERE u.inactivity_notice_at IS NOT NULL
           AND u.inactivity_notice_at <= now() - make_interval(days => $1)
           AND u.last_seen_at <= now() - make_interval(years => $2)
           AND u.last_seen_at < u.inactivity_notice_at
           AND (u.inactivity_retry_at IS NULL OR u.inactivity_retry_at <= now())
           AND NOT EXISTS (
               SELECT 1 FROM memberships m JOIN org_billing b ON b.org_id = m.org_id
               WHERE m.user_id = u.id AND b.subscription_id IS NOT NULL
                 AND (b.status IS NULL OR NOT (b.status = ANY($3)))
                 AND NOT EXISTS (SELECT 1 FROM memberships o WHERE o.org_id = m.org_id AND o.user_id <> u.id)
           )
         ORDER BY u.last_seen_at LIMIT 100",
        NOTICE_DAYS,
        UNUSED_YEARS,
        &ENDED_STATUSES.map(str::to_owned) as &[String],
    )
    .fetch_all(&state.db)
    .await?;
    let deleter = deleter(state);
    let mut gone = 0;
    for id in due {
        match deleter.delete_account(id, By::Inactivity).await {
            Ok(_) => gone += 1,
            Err(DeletionError::NoLongerDue | DeletionError::NotFound) => {}
            Err(err) => {
                let error = Chain(&err).to_string();
                failed(state, id, "the deletion", error).await;
            }
        }
    }
    Ok(gone)
}

fn deleter(state: &AppState) -> Deleter {
    Deleter::new(state.db.clone(), &state.config, state.mailer.clone())
}
