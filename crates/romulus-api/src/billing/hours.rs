//! Playground hours, the one metered resource: running playground time per
//! organization and calendar month (UTC), migration tests' playgrounds
//! included (`playground_usage`).
//!
//! Every [`METER_EVERY`] the meter asks the host which playgrounds run, and
//! adds the time since its last look to each one's organization (split at
//! month boundaries; never more than [`MAX_STEP`] at once, so a paused
//! process does not bill its pause). Then, per organization metered:
//!
//! - at 80 % of the plan's hours, its admins are emailed (once a month);
//! - at 100 %, its running playgrounds are stopped with the reason (their
//!   data stays until their normal expiry; a migration test in progress
//!   finishes), the admins emailed and the event audited, once a month; new
//!   playgrounds, tests, console runs, resets and migration runs are
//!   refused ([`check`]) until the month ends or the plan grows.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use romulus_playground::protocol::Access;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{caps, notices};
use crate::{
    ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    domain::{AuditAction, Plan},
    playgrounds::host::Caller,
};

pub const METER_EVERY: Duration = Duration::from_mins(1);
/// The most one look adds per running playground.
pub const MAX_STEP: Duration = Duration::from_mins(3);

/// The first day of `now`'s month, and of the next one.
pub fn month_bounds(now: DateTime<Utc>) -> (NaiveDate, NaiveDate) {
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap_or_default();
    (first, super::api::next_month(first))
}

fn midnight(day: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap_or_default())
}

/// Seconds of `from..to` in each calendar month it touches.
pub fn split_by_month(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<(NaiveDate, i64)> {
    let mut parts = Vec::new();
    let mut start = from;
    while start < to {
        let (month, next) = month_bounds(start);
        let end = to.min(midnight(next));
        parts.push((month, (end - start).num_seconds()));
        start = end;
    }
    parts.retain(|(_, seconds)| *seconds > 0);
    parts
}

/// Seconds `org` ran playgrounds in `now`'s month.
pub async fn used(
    db: impl PgExecutor<'_>,
    org: Uuid,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let (month, _) = month_bounds(now);
    Ok(sqlx::query_scalar!(
        "SELECT seconds FROM playground_usage WHERE org_id = $1 AND month = $2",
        org,
        month,
    )
    .fetch_optional(db)
    .await?
    .unwrap_or(0))
}

/// Adds `seconds` in `month` to `org`.
pub async fn add(
    db: impl PgExecutor<'_>,
    org: Uuid,
    month: NaiveDate,
    seconds: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO playground_usage (org_id, month, seconds) VALUES ($1, $2, $3)
         ON CONFLICT (org_id, month) DO UPDATE SET seconds = playground_usage.seconds + EXCLUDED.seconds",
        org,
        month,
        seconds,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// 402 when `org` used the month's hours (starting, resetting, running
/// anything on a playground). Passes with billing off.
pub async fn check(state: &AppState, org: Uuid) -> ApiResult<()> {
    let Some(plan) = state.billing.plan(&state.db, org).await? else {
        return Ok(());
    };
    let now = Utc::now();
    let (month, next) = month_bounds(now);
    caps::playground_hours(
        plan,
        used(&state.db, org, now).await?,
        &month.format("%B").to_string(),
        &next.format("%B %-d").to_string(),
    )
}

/// The share of the plan's hours `seconds` is, in percent.
pub fn percent(plan: Plan, seconds: i64) -> i64 {
    let limit = i64::from(plan.limits().playground_hours) * 3600;
    if limit == 0 {
        100
    } else {
        seconds * 100 / limit
    }
}

/// Meters every [`METER_EVERY`] until aborted (billing and playgrounds on).
pub fn meter(state: AppState, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = Utc::now();
        loop {
            tick.tick().await;
            let now = Utc::now();
            match look(&state, last, now).await {
                Ok(()) => last = now,
                Err(err) => tracing::warn!(error = %err, "billing: playground meter failed"),
            }
        }
    })
}

/// One look: what ran between `last` and `now`, counted; thresholds acted on.
pub async fn look(state: &AppState, last: DateTime<Utc>, now: DateTime<Utc>) -> ApiResult<()> {
    if !state.billing.is_on() {
        return Ok(());
    }
    let Ok(backend) = state.playgrounds.host.backend() else {
        return Ok(());
    };
    let Ok(Ok(listing)) = tokio::time::timeout(Duration::from_secs(10), backend.list()).await
    else {
        tracing::debug!("billing: the playground host did not list its playgrounds");
        return Ok(());
    };
    let running: Vec<Uuid> = listing
        .playgrounds
        .iter()
        .filter(|p| p.running)
        .map(|p| p.id)
        .collect();
    if running.is_empty() {
        return Ok(());
    }
    // A playground's organization, or its migration test's.
    let owners = sqlx::query!(
        r#"SELECT id AS "id!", org_id AS "org_id!", false AS "test!" FROM playgrounds WHERE id = ANY($1)
           UNION ALL
           SELECT playground_id AS "id!", org_id AS "org_id!", true AS "test!" FROM migration_tests
           WHERE playground_id = ANY($1)"#,
        &running,
    )
    .fetch_all(&state.db)
    .await?;
    let step = (now - last).min(chrono::Duration::from_std(MAX_STEP).unwrap_or_default());
    let from = now - step;
    let mut per_org: HashMap<Uuid, (i64, Vec<Uuid>)> = HashMap::new();
    for owner in &owners {
        let entry = per_org.entry(owner.org_id).or_default();
        entry.0 += 1;
        if !owner.test {
            entry.1.push(owner.id);
        }
    }
    for (org, (count, playgrounds)) in per_org {
        let before = used(&state.db, org, now).await?;
        for (month, seconds) in split_by_month(from, now) {
            add(&state.db, org, month, seconds * count).await?;
        }
        let after = used(&state.db, org, now).await?;
        thresholds(state, org, before, after, now, &playgrounds).await?;
    }
    Ok(())
}

/// Warns at 80 %, stops at 100 %.
async fn thresholds(
    state: &AppState,
    org: Uuid,
    before: i64,
    after: i64,
    now: DateTime<Utc>,
    running: &[Uuid],
) -> ApiResult<()> {
    let Some(plan) = state.billing.plan(&state.db, org).await? else {
        return Ok(());
    };
    let (month, next) = month_bounds(now);
    let month_key = month.format("%Y-%m").to_string();
    if percent(plan, after) >= 80 && percent(plan, before) < 100 {
        notices::hours_warning(state, org, plan, after, month, next).await?;
    }
    if percent(plan, after) < 100 {
        return Ok(());
    }
    let reason = format!(
        "your organization used its {} playground hours for {} ({} plan); they come back on {}",
        plan.limits().playground_hours,
        month.format("%B"),
        plan.name(),
        next.format("%B %-d"),
    );
    let backend = state.playgrounds.host.backend()?;
    for id in running {
        let caller = Caller {
            org,
            user: None,
            access: Access::Develop,
        };
        if let Err(err) = backend.stop(*id, caller, reason.clone()).await {
            tracing::warn!(playground = %id, error = %err, "billing: cannot stop a playground past its hours");
        }
    }
    if notices::once(&state.db, org, "hours_used_up", &month_key).await? {
        audit::record(
            &state.db,
            &audit::Entry {
                org_id: org,
                database_id: None,
                action: AuditAction::PlaygroundHoursUsedUp,
                actor_id: None,
                actor_name: None,
                version_number: None,
                reason: None,
                details: AuditDetails {
                    billing: Some(BillingAudit {
                        plan: Some(plan),
                        month: Some(month_key),
                        ..BillingAudit::default()
                    }),
                    ..AuditDetails::default()
                },
            },
        )
        .await?;
        notices::hours_used_up(state, org, plan, month, next).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, hour, minute, 0)
            .unwrap()
    }

    fn first(month: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, 1).unwrap()
    }

    #[test]
    fn a_step_inside_a_month_counts_whole() {
        assert_eq!(
            split_by_month(at(10, 20, 12, 0), at(10, 20, 12, 1)),
            vec![(first(10), 60)]
        );
        assert_eq!(split_by_month(at(10, 20, 12, 0), at(10, 20, 12, 0)), vec![]);
    }

    #[test]
    fn a_step_across_midnight_utc_at_the_month_end_is_split() {
        let parts = split_by_month(at(10, 31, 23, 59), at(11, 1, 0, 2));
        assert_eq!(parts, vec![(first(10), 60), (first(11), 120)]);
        let december = split_by_month(
            Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 30).unwrap(),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 30).unwrap(),
        );
        assert_eq!(
            december,
            vec![
                (first(12), 30),
                (NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), 30)
            ]
        );
    }

    #[test]
    fn months_start_on_the_first_and_end_on_the_next_first() {
        assert_eq!(month_bounds(at(2, 28, 23, 0)), (first(2), first(3)));
        assert_eq!(
            month_bounds(Utc.with_ymd_and_hms(2026, 12, 5, 0, 0, 0).unwrap()).1,
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
    }

    #[test]
    fn the_share_of_the_plan_used_is_in_percent() {
        // Free: 2 hours.
        assert_eq!(percent(Plan::Free, 0), 0);
        assert_eq!(percent(Plan::Free, 5760), 80);
        assert_eq!(percent(Plan::Free, 7200), 100);
        assert_eq!(percent(Plan::Starter, 7200), 10);
    }
}
