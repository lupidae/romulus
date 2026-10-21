//! Accounts unused for [`UNUSED_YEARS`] years are deleted, after an email
//! [`NOTICE_DAYS`] days before (`ROMULUS_DELETE_INACTIVE_ACCOUNTS`, which
//! needs mail; off by default, so a self-hosted install keeps every
//! account). "Used" is a sign-in, a session's use or an agent's with a
//! personal access token (`users.last_seen_at`: `auth::session`,
//! `auth::access_tokens`).
//!
//! - **The notice** goes once the account is [`NOTICE_DAYS`] days from its
//!   3 years (or at once, for older accounts when this is turned on): the
//!   date, what goes with it, and that signing in keeps it. Recorded in
//!   `users.inactivity_notice_at` only once the mail server took it; using
//!   the account clears it.
//! - **The deletion** comes [`NOTICE_DAYS`] days after the notice at the
//!   earliest, never before the 3 years, if the account was not used since:
//!   `deletion::Deleter::delete_account` with `By::Inactivity` (the
//!   organizations it is the only member of go with it; where it is the only
//!   admin, another member is appointed).
//! - An account that is the only member of an organization still paying (a
//!   live subscription) is left alone: someone pays for it to be there.
//!
//! A hundred of each per sweep (`super::sweeper`, hourly).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::deletion::{By, Deleter, DeletionError, Fate};
use crate::{AppState, billing::state::LIVE_STATUSES, mail::Letter};

/// Years without use before an account is deleted.
pub const UNUSED_YEARS: i32 = 3;
/// Days between the notice and the deletion, at least.
pub const NOTICE_DAYS: i32 = 30;

/// Sends the notices due. How many the mail server took.
pub async fn send_notices(state: &AppState) -> Result<u64, sqlx::Error> {
    let due = sqlx::query!(
        r#"SELECT u.id, u.last_seen_at,
                  greatest(u.last_seen_at + make_interval(years => $1),
                           now() + make_interval(days => $2)) AS "deleted_on!"
           FROM users u
           WHERE u.inactivity_notice_at IS NULL
             AND u.last_seen_at < now() - make_interval(years => $1) + make_interval(days => $2)
             AND NOT EXISTS (
                 SELECT 1 FROM memberships m JOIN org_billing b ON b.org_id = m.org_id
                 WHERE m.user_id = u.id AND b.subscription_id IS NOT NULL AND b.status = ANY($3)
                   AND NOT EXISTS (SELECT 1 FROM memberships o WHERE o.org_id = m.org_id AND o.user_id <> u.id)
             )
           ORDER BY u.last_seen_at LIMIT 100"#,
        UNUSED_YEARS,
        NOTICE_DAYS,
        &LIVE_STATUSES.map(str::to_owned) as &[String],
    )
    .fetch_all(&state.db)
    .await?;
    let deleter = deleter(state);
    let mut sent = 0;
    for row in due {
        match notify(state, &deleter, row.id, row.last_seen_at, row.deleted_on).await {
            Ok(true) => sent += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(account = %row.id, error = %err, "inactive account: notice failed");
            }
        }
    }
    Ok(sent)
}

async fn notify(
    state: &AppState,
    deleter: &Deleter,
    user_id: Uuid,
    last_seen_at: DateTime<Utc>,
    deleted_on: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(plan) = deleter.plan_account(user_id).await? else {
        return Ok(false);
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
        tracing::warn!(account = %user_id, error = %crate::error::Chain(&err), "inactive account: notice not sent, again next sweep");
        return Ok(false);
    }
    // Only if nothing moved since it was read: used meanwhile, no notice.
    let recorded = sqlx::query!(
        "UPDATE users SET inactivity_notice_at = now()
         WHERE id = $1 AND inactivity_notice_at IS NULL AND last_seen_at = $2",
        user_id,
        last_seen_at,
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    tracing::info!(account = %user_id, deleted_on = %deleted_on, "inactive account: notice sent");
    Ok(recorded == 1)
}

/// Deletes the accounts whose notice ran out unused. How many.
pub async fn delete_due(state: &AppState) -> Result<u64, sqlx::Error> {
    let due = sqlx::query_scalar!(
        "SELECT id FROM users
         WHERE inactivity_notice_at IS NOT NULL
           AND inactivity_notice_at <= now() - make_interval(days => $1)
           AND last_seen_at <= now() - make_interval(years => $2)
           AND last_seen_at < inactivity_notice_at
         ORDER BY last_seen_at LIMIT 100",
        NOTICE_DAYS,
        UNUSED_YEARS,
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
                tracing::warn!(account = %id, error = %crate::error::Chain(&err), "inactive account: not deleted, again next sweep");
            }
        }
    }
    Ok(gone)
}

fn deleter(state: &AppState) -> Deleter {
    Deleter::new(state.db.clone(), &state.config, state.mailer.clone())
}
