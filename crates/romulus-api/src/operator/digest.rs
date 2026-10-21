//! The daily digest: once a day from `ROMULUS_OPERATOR_DIGEST_HOUR` (UTC),
//! about the day before and the 7 days to it.
//!
//! - accounts and organizations: new yesterday and in 7 days, in all, and
//!   active (an account used in 7 days, `users.last_seen_at`; an
//!   organization one of whose members is);
//! - with billing on: paying organizations by plan (the plan in force,
//!   `billing::state`), the monthly recurring revenue (each paying
//!   subscription's amount, a yearly one divided by 12) and its change since
//!   the last digest, cancellations scheduled, failed payments in their
//!   grace period, playground hours of the month;
//! - migration imports (one per replay: an import's versions share their
//!   transaction's time), migration tests and their outcomes, deletions
//!   (the sign-in log's `account_deleted`, `organization_deleted`).
//!
//! The API counts no errors anywhere cheap to read, so the digest has none.
//! [`due`] takes the day's row (`operator_digests`) and queues the digest in
//! one transaction: one digest a day, whatever the processes; a day the API
//! did not run past the hour has none.

use std::{collections::BTreeMap, fmt::Write as _};

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Timelike as _, Utc};
use sqlx::PgConnection;

use super::{Event, Notice, day as long_day, notify};
use crate::{
    AppState,
    billing::{money, state::Stored},
    domain::{Interval, Plan},
};

/// What happened in a window: the day before the digest, the 7 days to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub yesterday: i64,
    pub week: i64,
}

/// One digest's numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    /// The day it goes (UTC): "yesterday" is the day before.
    pub day: NaiveDate,
    pub accounts: Counts,
    pub accounts_total: i64,
    pub orgs: Counts,
    pub orgs_total: i64,
    /// Accounts used in the last 7 days, and the organizations they are in.
    pub active_accounts: i64,
    pub active_orgs: i64,
    /// `None` with billing off.
    pub billing: Option<Billing>,
    /// Imports run (each a replay), and the migrations they made versions of.
    pub imports: Counts,
    pub imported_migrations: i64,
    pub tests: Counts,
    /// Yesterday's tests by outcome.
    pub tests_passed: i64,
    pub tests_failed: i64,
    pub tests_errors: i64,
    pub accounts_deleted: Counts,
    pub orgs_deleted: Counts,
}

/// Hosted billing's numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Billing {
    /// Organizations on each paid plan in force.
    pub paying: BTreeMap<Plan, i64>,
    /// Cents a month, per currency.
    pub mrr: BTreeMap<String, i64>,
    /// Subscriptions ending with their period: the organization, when.
    pub cancelling: Vec<(String, Option<DateTime<Utc>>)>,
    /// Failed payments within their grace: the organization, when it ends.
    pub in_grace: Vec<(String, DateTime<Utc>)>,
    /// The month of yesterday, and its playground time.
    pub playground_month: NaiveDate,
    pub playground_seconds: i64,
}

/// What the previous digest recorded, to compare with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub day: NaiveDate,
    pub mrr: BTreeMap<String, i64>,
    pub playground: Option<(NaiveDate, i64)>,
}

fn midnight(day: NaiveDate) -> DateTime<Utc> {
    day.and_time(chrono::NaiveTime::MIN).and_utc()
}

/// The digest of `day` as the database has it at `now`.
#[allow(
    clippy::too_many_lines,
    reason = "one query per line of the digest, in its order"
)]
pub async fn compute(
    conn: &mut PgConnection,
    state: &AppState,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Digest, sqlx::Error> {
    let end = midnight(day);
    let yesterday = end - Duration::days(1);
    let week = end - Duration::days(7);

    let accounts = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE created_at >= $1 AND created_at < $2) AS "yesterday!",
                  count(*) FILTER (WHERE created_at >= $3 AND created_at < $2) AS "week!",
                  count(*) AS "total!"
           FROM users"#,
        yesterday,
        end,
        week,
    )
    .fetch_one(&mut *conn)
    .await?;
    let orgs = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE created_at >= $1 AND created_at < $2) AS "yesterday!",
                  count(*) FILTER (WHERE created_at >= $3 AND created_at < $2) AS "week!",
                  count(*) AS "total!"
           FROM organizations"#,
        yesterday,
        end,
        week,
    )
    .fetch_one(&mut *conn)
    .await?;
    let active = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM users WHERE last_seen_at >= $1) AS "accounts!",
                  (SELECT count(DISTINCT m.org_id) FROM memberships m JOIN users u ON u.id = m.user_id
                   WHERE u.last_seen_at >= $1) AS "orgs!""#,
        now - Duration::days(7),
    )
    .fetch_one(&mut *conn)
    .await?;
    let imports = sqlx::query!(
        r#"SELECT count(DISTINCT (database_id, created_at)) FILTER (WHERE created_at >= $1) AS "yesterday!",
                  count(*) FILTER (WHERE created_at >= $1) AS "migrations!",
                  count(DISTINCT (database_id, created_at)) AS "week!"
           FROM versions WHERE origin = 'import' AND created_at >= $2 AND created_at < $3"#,
        yesterday,
        week,
        end,
    )
    .fetch_one(&mut *conn)
    .await?;
    let tests = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE created_at >= $1) AS "yesterday!",
                  count(*) FILTER (WHERE created_at >= $1 AND status = 'passed') AS "passed!",
                  count(*) FILTER (WHERE created_at >= $1 AND status = 'failed') AS "failed!",
                  count(*) FILTER (WHERE created_at >= $1 AND status = 'error') AS "errors!",
                  count(*) AS "week!"
           FROM migration_tests WHERE created_at >= $2 AND created_at < $3"#,
        yesterday,
        week,
        end,
    )
    .fetch_one(&mut *conn)
    .await?;
    let deletions = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE kind = 'account_deleted' AND created_at >= $1) AS "accounts_yesterday!",
                  count(*) FILTER (WHERE kind = 'account_deleted') AS "accounts_week!",
                  count(*) FILTER (WHERE kind = 'organization_deleted' AND created_at >= $1) AS "orgs_yesterday!",
                  count(*) FILTER (WHERE kind = 'organization_deleted') AS "orgs_week!"
           FROM auth_events
           WHERE kind IN ('account_deleted', 'organization_deleted') AND created_at >= $2 AND created_at < $3"#,
        yesterday,
        week,
        end,
    )
    .fetch_one(&mut *conn)
    .await?;
    let billing = if state.billing.is_on() {
        Some(billing(conn, day, now).await?)
    } else {
        None
    };
    Ok(Digest {
        day,
        accounts: Counts {
            yesterday: accounts.yesterday,
            week: accounts.week,
        },
        accounts_total: accounts.total,
        orgs: Counts {
            yesterday: orgs.yesterday,
            week: orgs.week,
        },
        orgs_total: orgs.total,
        active_accounts: active.accounts,
        active_orgs: active.orgs,
        billing,
        imports: Counts {
            yesterday: imports.yesterday,
            week: imports.week,
        },
        imported_migrations: imports.migrations,
        tests: Counts {
            yesterday: tests.yesterday,
            week: tests.week,
        },
        tests_passed: tests.passed,
        tests_failed: tests.failed,
        tests_errors: tests.errors,
        accounts_deleted: Counts {
            yesterday: deletions.accounts_yesterday,
            week: deletions.accounts_week,
        },
        orgs_deleted: Counts {
            yesterday: deletions.orgs_yesterday,
            week: deletions.orgs_week,
        },
    })
}

async fn billing(
    conn: &mut PgConnection,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Billing, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT b.org_id, o.name, o.slug, b.plan AS "plan: Plan", b.interval AS "interval: Interval",
                  b.status, b.current_period_end, b.cancel_at_period_end, b.amount_cents, b.currency,
                  b.past_due_since, b.withdrawn_at, b.subscription_id
           FROM org_billing b JOIN organizations o ON o.id = b.org_id
           WHERE b.subscription_id IS NOT NULL
           ORDER BY lower(o.name), o.slug"#
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut billing = Billing {
        paying: BTreeMap::new(),
        mrr: BTreeMap::new(),
        cancelling: Vec::new(),
        in_grace: Vec::new(),
        playground_month: first_of_month(day.pred_opt().unwrap_or(day)),
        playground_seconds: 0,
    };
    for r in rows {
        let stored = Stored {
            org_id: r.org_id,
            subscription_id: r.subscription_id,
            plan: r.plan,
            interval: r.interval,
            status: r.status,
            current_period_end: r.current_period_end,
            cancel_at_period_end: r.cancel_at_period_end,
            amount_cents: r.amount_cents,
            currency: r.currency,
            past_due_since: r.past_due_since,
            withdrawn_at: r.withdrawn_at,
            ..Stored::default()
        };
        let org = format!("{} ({})", r.name, r.slug);
        let plan = stored.effective(now);
        if plan == Plan::Free {
            continue;
        }
        *billing.paying.entry(plan).or_default() += 1;
        if let (Some(cents), Some(currency), Some(interval)) = (
            stored.amount_cents,
            stored.currency.as_deref(),
            stored.interval,
        ) {
            *billing
                .mrr
                .entry(currency.to_ascii_lowercase())
                .or_default() += super::monthly(cents, interval);
        }
        if stored.cancel_at_period_end {
            billing
                .cancelling
                .push((org.clone(), stored.current_period_end));
        }
        if let Some(until) = stored.grace_until() {
            billing.in_grace.push((org, until));
        }
    }
    billing.playground_seconds = sqlx::query_scalar!(
        r#"SELECT coalesce(sum(seconds), 0)::bigint AS "seconds!" FROM playground_usage WHERE month = $1"#,
        billing.playground_month,
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(billing)
}

fn first_of_month(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

/// The last digest before `day`.
pub async fn previous(
    conn: &mut PgConnection,
    day: NaiveDate,
) -> Result<Option<Recorded>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT day, mrr, playground_month, playground_seconds FROM operator_digests
         WHERE day < $1 ORDER BY day DESC LIMIT 1",
        day
    )
    .fetch_optional(conn)
    .await?
    .map(|r| Recorded {
        day: r.day,
        mrr: serde_json::from_value(r.mrr).unwrap_or_default(),
        playground: r.playground_month.zip(r.playground_seconds),
    }))
}

/// Queues the day's digest if it is due at `now` and not queued yet: true
/// when this call queued it.
pub async fn due(state: &AppState, now: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let Some(operator) = &state.config.operator else {
        return Ok(false);
    };
    if now.hour() < operator.digest_hour {
        return Ok(false);
    }
    let day = now.date_naive();
    let mut tx = state.db.begin().await?;
    let taken = sqlx::query_scalar!(
        "INSERT INTO operator_digests (day) VALUES ($1) ON CONFLICT (day) DO NOTHING RETURNING day",
        day
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if !taken {
        return Ok(false);
    }
    let digest = compute(&mut tx, state, day, now).await?;
    let previous = previous(&mut tx, day).await?;
    let billing = digest.billing.as_ref();
    sqlx::query!(
        "UPDATE operator_digests SET mrr = $2, playground_month = $3, playground_seconds = $4
         WHERE day = $1",
        day,
        billing.map_or_else(
            || serde_json::json!({}),
            |b| serde_json::to_value(&b.mrr).unwrap_or_default()
        ),
        billing.map(|b| b.playground_month),
        billing.map(|b| b.playground_seconds),
    )
    .execute(&mut *tx)
    .await?;
    let notice = notice(&digest, previous.as_ref(), &state.config.public_origin());
    notify(&mut tx, state.config.operator.as_ref(), &notice).await?;
    tx.commit().await?;
    tracing::info!(%day, "operator: the digest is queued");
    Ok(true)
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn signed(n: i64) -> String {
    if n >= 0 {
        format!("+{n}")
    } else {
        n.to_string()
    }
}

/// `€612.50` (and `+ $20.00` in another currency); `€0.00` for none.
fn amounts(mrr: &BTreeMap<String, i64>) -> String {
    if mrr.is_empty() {
        return money(0, "eur");
    }
    mrr.iter()
        .map(|(currency, cents)| money(*cents, currency))
        .collect::<Vec<_>>()
        .join(" + ")
}

/// The change from `before` to `now`, per currency.
fn mrr_change(before: &BTreeMap<String, i64>, now: &BTreeMap<String, i64>) -> String {
    let changes: Vec<String> = before
        .keys()
        .chain(now.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|currency| {
            let delta = now.get(currency).copied().unwrap_or(0)
                - before.get(currency).copied().unwrap_or(0);
            (delta != 0).then(|| {
                let sign = if delta > 0 { "+" } else { "" };
                format!("{sign}{}", money(delta, currency))
            })
        })
        .collect();
    if changes.is_empty() {
        "no change".to_owned()
    } else {
        changes.join(", ")
    }
}

/// Seconds as hours, one decimal: `312.4`.
fn hours(seconds: i64) -> String {
    let tenths = (seconds + 180) / 360;
    format!("{}.{}", tenths / 10, (tenths % 10).abs())
}

/// At most 5 of `items`, then how many more.
fn some_of(items: &[String]) -> String {
    let mut shown = items.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
    if items.len() > 5 {
        let _ = write!(shown, "; and {} more", items.len() - 5);
    }
    shown
}

/// The digest as it reads, compared with the `previous` one.
pub fn notice(digest: &Digest, previous: Option<&Recorded>, origin: &str) -> Notice {
    let yesterday = digest.day.pred_opt().unwrap_or(digest.day);
    let mut lines = vec![
        format!(
            "romulus at {origin}, {} (UTC), and the 7 days to it.",
            yesterday.format("%A, %B %-d, %Y")
        ),
        String::new(),
        format!(
            "Accounts: {} ({} in 7 days), {} in all; {} active in 7 days",
            signed(digest.accounts.yesterday),
            signed(digest.accounts.week),
            digest.accounts_total,
            digest.active_accounts,
        ),
        format!(
            "Organizations: {} ({} in 7 days), {} in all; {} active in 7 days",
            signed(digest.orgs.yesterday),
            signed(digest.orgs.week),
            digest.orgs_total,
            digest.active_orgs,
        ),
    ];
    let since = previous.map(|p| p.day.format("%B %-d").to_string());
    match &digest.billing {
        Some(billing) => lines.extend(billing_lines(billing, previous, since.as_deref())),
        None => {
            lines
                .push("Billing is off: no plans, revenue or playground hours to count.".to_owned());
        }
    }
    lines.push(format!(
        "Imports: {} ({}); {} in 7 days",
        digest.imports.yesterday,
        plural(digest.imported_migrations, "migration", "migrations"),
        digest.imports.week,
    ));
    lines.push(format!(
        "Migration tests: {} ({} passed, {} failed, {}); {} in 7 days",
        digest.tests.yesterday,
        digest.tests_passed,
        digest.tests_failed,
        plural(digest.tests_errors, "error", "errors"),
        digest.tests.week,
    ));
    lines.push(format!(
        "Deletions: {} and {} ({} and {} in 7 days)",
        plural(digest.accounts_deleted.yesterday, "account", "accounts"),
        plural(
            digest.orgs_deleted.yesterday,
            "organization",
            "organizations"
        ),
        digest.accounts_deleted.week,
        digest.orgs_deleted.week,
    ));

    let mut subject = format!(
        "Digest for {}: {} accounts, {} organizations",
        yesterday.format("%B %-d"),
        signed(digest.accounts.yesterday),
        signed(digest.orgs.yesterday),
    );
    if let Some(billing) = &digest.billing {
        let _ = write!(subject, ", MRR {}", amounts(&billing.mrr));
    }
    Notice {
        event: Event::Digest,
        about: digest.day.to_string(),
        subject,
        lines,
    }
}

fn billing_lines(
    billing: &Billing,
    previous: Option<&Recorded>,
    since: Option<&str>,
) -> Vec<String> {
    let paying: i64 = billing.paying.values().sum();
    let by_plan = billing
        .paying
        .iter()
        .map(|(plan, n)| format!("{} {n}", plan.name()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![if paying == 0 {
        "Paying organizations: 0".to_owned()
    } else {
        format!("Paying organizations: {paying} ({by_plan})")
    }];
    let mut mrr = format!("MRR: {}", amounts(&billing.mrr));
    if let (Some(previous), Some(since)) = (previous, since) {
        let _ = write!(
            mrr,
            " ({} since {since})",
            mrr_change(&previous.mrr, &billing.mrr)
        );
    }
    lines.push(mrr);
    let cancelling: Vec<String> = billing
        .cancelling
        .iter()
        .map(|(org, ends)| match ends {
            Some(at) => format!("{org}, ends {}", long_day(*at)),
            None => org.clone(),
        })
        .collect();
    lines.push(if cancelling.is_empty() {
        "Cancellations scheduled: 0".to_owned()
    } else {
        format!(
            "Cancellations scheduled: {}: {}",
            cancelling.len(),
            some_of(&cancelling)
        )
    });
    let in_grace: Vec<String> = billing
        .in_grace
        .iter()
        .map(|(org, until)| format!("{org}, Free on {}", long_day(*until)))
        .collect();
    lines.push(if in_grace.is_empty() {
        "Failed payments in grace: 0".to_owned()
    } else {
        format!(
            "Failed payments in grace: {}: {}",
            in_grace.len(),
            some_of(&in_grace)
        )
    });
    let mut playground = format!(
        "Playground hours: {} in {}",
        hours(billing.playground_seconds),
        billing.playground_month.format("%B %Y")
    );
    if let (Some((month, seconds)), Some(since)) = (previous.and_then(|p| p.playground), since)
        && month == billing.playground_month
    {
        let delta = billing.playground_seconds - seconds;
        let sign = if delta >= 0 { "+" } else { "-" };
        let _ = write!(playground, " ({sign}{} since {since})", hours(delta.abs()));
    }
    lines.push(playground);
    lines
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn digest(billing: Option<Billing>) -> Digest {
        Digest {
            day: NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
            accounts: Counts {
                yesterday: 3,
                week: 12,
            },
            accounts_total: 128,
            orgs: Counts {
                yesterday: 1,
                week: 4,
            },
            orgs_total: 40,
            active_accounts: 41,
            active_orgs: 17,
            billing,
            imports: Counts {
                yesterday: 2,
                week: 5,
            },
            imported_migrations: 35,
            tests: Counts {
                yesterday: 14,
                week: 60,
            },
            tests_passed: 11,
            tests_failed: 2,
            tests_errors: 1,
            accounts_deleted: Counts {
                yesterday: 1,
                week: 2,
            },
            orgs_deleted: Counts {
                yesterday: 0,
                week: 1,
            },
        }
    }

    #[test]
    fn the_digest_reads_compactly_and_compares_with_the_last_one() {
        let at = |d| Utc.with_ymd_and_hms(2026, 11, d, 0, 0, 0).unwrap();
        let billing = Billing {
            paying: BTreeMap::from([(Plan::Starter, 4), (Plan::Team, 4), (Plan::Business, 1)]),
            mrr: BTreeMap::from([("eur".to_owned(), 61_250)]),
            cancelling: vec![("Acme (acme)".into(), Some(at(3)))],
            in_grace: vec![("Gamma (gamma)".into(), at(1))],
            playground_month: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            playground_seconds: 1_124_640,
        };
        let previous = Recorded {
            day: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
            mrr: BTreeMap::from([("eur".to_owned(), 56_350)]),
            playground: Some((NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(), 1_081_080)),
        };
        let read = notice(
            &digest(Some(billing)),
            Some(&previous),
            "https://romulus.example",
        );
        assert_eq!(
            read.subject,
            "Digest for October 20: +3 accounts, +1 organizations, MRR €612.50"
        );
        let text = read.lines.join("\n");
        for line in [
            "romulus at https://romulus.example, Tuesday, October 20, 2026 (UTC), and the 7 days to it.",
            "Accounts: +3 (+12 in 7 days), 128 in all; 41 active in 7 days",
            "Organizations: +1 (+4 in 7 days), 40 in all; 17 active in 7 days",
            "Paying organizations: 9 (Starter 4, Team 4, Business 1)",
            "MRR: €612.50 (+€49.00 since October 20)",
            "Cancellations scheduled: 1: Acme (acme), ends November 3, 2026",
            "Failed payments in grace: 1: Gamma (gamma), Free on November 1, 2026",
            "Playground hours: 312.4 in October 2026 (+12.1 since October 20)",
            "Imports: 2 (35 migrations); 5 in 7 days",
            "Migration tests: 14 (11 passed, 2 failed, 1 error); 60 in 7 days",
            "Deletions: 1 account and 0 organizations (2 and 1 in 7 days)",
        ] {
            assert!(text.contains(line), "{line}\n---\n{text}");
        }
        let off = notice(&digest(None), None, "https://romulus.example");
        assert!(off.lines.contains(
            &"Billing is off: no plans, revenue or playground hours to count.".to_owned()
        ));
        assert!(!off.subject.contains("MRR"));
    }

    #[test]
    fn money_and_hours_changes_read_with_their_sign() {
        let eur = |cents| BTreeMap::from([("eur".to_owned(), cents)]);
        assert_eq!(mrr_change(&eur(1_000), &eur(1_000)), "no change");
        assert_eq!(mrr_change(&eur(4_900), &eur(1_900)), "-€30.00");
        assert_eq!(mrr_change(&BTreeMap::new(), &eur(1_900)), "+€19.00");
        assert_eq!(hours(0), "0.0");
        assert_eq!(hours(5_400), "1.5");
        assert_eq!(amounts(&BTreeMap::new()), "€0.00");
        let items: Vec<String> = (1..=7).map(|i| i.to_string()).collect();
        assert_eq!(some_of(&items), "1; 2; 3; 4; 5; and 2 more");
    }
}
