//! The emails billing sends to an organization's admins, each once
//! (`billing_notices`, keyed by what it is about):
//!
//! - **renewal reminders** (Terms 7.5; Article L215-1 of the French Consumer
//!   Code): [`REMIND_AHEAD`] before a yearly subscription renews, so one to
//!   three months before the end of the period: the date, the amount, how
//!   not to renew, a link to the Cancel button. Once per period;
//! - **a failed payment** (once per invoice), **the grace period ending**
//!   ([`GRACE_WARNING`] before) and **the downgrade** to Free at its end
//!   (audited);
//! - **playground hours**: 80 % and all used, once a month each.
//!
//! [`run`] looks for what is due every [`NOTICES_EVERY`]. A price change
//! has its letter ([`price_change`]) for whoever changes a price: Stripe
//! keeps subscriptions on their price, so moving them is a separate act.

use std::time::Duration;

use chrono::{DateTime, Duration as Span, NaiveDate, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{admins, money, state::GRACE_DAYS, stripe::Invoice};
use crate::{
    ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    domain::{AuditAction, Interval, Plan},
    mail::Letter,
};

pub const NOTICES_EVERY: Duration = Duration::from_hours(1);
/// How long before a yearly renewal its reminder goes: more than a month,
/// less than three.
pub const REMIND_AHEAD: i64 = 40;
/// How long before the grace period ends the last warning goes.
pub const GRACE_WARNING: i64 = 3;

/// Whether a yearly renewal at `period_end` is due its reminder at `now`.
pub fn renewal_due(period_end: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now < period_end && now >= period_end - Span::days(REMIND_AHEAD)
}

/// Records notice `kind` about `about` for `org`: true the first time.
pub async fn once(
    db: impl PgExecutor<'_>,
    org: Uuid,
    kind: &str,
    about: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO billing_notices (org_id, kind, about) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING RETURNING org_id",
        org,
        kind,
        about,
    )
    .fetch_optional(db)
    .await?
    .is_some())
}

fn date(at: DateTime<Utc>) -> String {
    at.format("%B %-d, %Y").to_string()
}

struct Org {
    name: String,
    slug: String,
}

async fn org(state: &AppState, id: Uuid) -> Result<Option<Org>, sqlx::Error> {
    sqlx::query_as!(
        Org,
        "SELECT name, slug FROM organizations WHERE id = $1",
        id
    )
    .fetch_optional(&state.db)
    .await
}

fn billing_link(state: &AppState, slug: &str, query: &str) -> String {
    let mut url = state.config.public_url.clone();
    url.set_path(&format!("/app/{slug}/settings/billing"));
    url.set_query((!query.is_empty()).then_some(query));
    url.to_string()
}

/// Sends one letter to every admin of `org`.
async fn to_admins(
    state: &AppState,
    org_id: Uuid,
    kind: &'static str,
    letter: impl Fn(&str, &Org) -> (String, Vec<String>, Option<(&'static str, String)>, String),
) -> Result<(), sqlx::Error> {
    let Some(org) = org(state, org_id).await? else {
        return Ok(());
    };
    for (name, email) in admins(&state.db, org_id).await? {
        let (subject, paragraphs, action, footer) = letter(&name, &org);
        state.mailer.send(
            Letter {
                kind,
                to: &email,
                subject,
                paragraphs,
                action: action.as_ref().map(|(label, url)| (*label, url.as_str())),
                footer,
            }
            .email(),
        );
    }
    Ok(())
}

fn footer(org: &Org) -> String {
    format!(
        "You get this because you are an admin of {} on romulus. Billing emails go to every admin of an organization.",
        org.name
    )
}

/// What renews at the end of a yearly period: the plan as it is, or the
/// change scheduled for then (a downgrade, a switch to monthly) at the plan
/// table's price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renewing {
    pub plan: Plan,
    pub interval: Interval,
    /// Cents a period; `None` when unknown.
    pub amount: Option<(i64, String)>,
    /// A change scheduled for the renewal.
    pub changed: bool,
}

impl Renewing {
    /// From the stored subscription: `scheduled` applies when it is due at
    /// the period's end (or before).
    pub fn of(
        plan: Plan,
        amount: Option<(i64, String)>,
        period_end: DateTime<Utc>,
        scheduled: Option<(Plan, Option<Interval>, DateTime<Utc>)>,
    ) -> Self {
        match scheduled {
            Some((next, interval, at))
                if at <= period_end && (next, interval) != (plan, Some(Interval::Year)) =>
            {
                let interval = interval.unwrap_or(Interval::Year);
                Self {
                    plan: next,
                    interval,
                    amount: next
                        .price_cents(interval)
                        .map(|cents| (i64::from(cents), "eur".to_owned())),
                    changed: true,
                }
            }
            _ => Self {
                plan,
                interval: Interval::Year,
                amount,
                changed: false,
            },
        }
    }

    /// The reminder's first sentence.
    pub fn sentence(&self, current: Plan, org: &str, period_end: DateTime<Utc>) -> String {
        let period = match self.interval {
            Interval::Month => "month",
            Interval::Year => "year",
        };
        let price = self
            .amount
            .as_ref()
            .map_or_else(String::new, |(cents, currency)| {
                format!(", for {} a {period}", money(*cents, currency))
            });
        if self.changed {
            format!(
                "The yearly {} plan of {org} ends its period on {}. As scheduled, it then renews automatically as the {}ly {} plan{price}.",
                current.name(),
                date(period_end),
                period,
                self.plan.name(),
            )
        } else {
            format!(
                "The yearly {} plan of {org} renews automatically on {}{price}, for another year.",
                self.plan.name(),
                date(period_end),
            )
        }
    }
}

/// The renewal reminder of a yearly subscription (Article L215-1): what
/// renews, as scheduled, for how much, and how not to renew.
pub async fn renewal(
    state: &AppState,
    org_id: Uuid,
    current: Plan,
    period_end: DateTime<Utc>,
    renewing: &Renewing,
) -> Result<(), sqlx::Error> {
    let cancel = |org: &Org| billing_link(state, &org.slug, "action=cancel");
    to_admins(state, org_id, "billing_renewal", |_, org| {
        (
            format!("{}'s {} plan renews on {}", org.name, renewing.plan.name(), date(period_end)),
            vec![
                renewing.sentence(current, &org.name, period_end),
                "If you want it to renew, there is nothing to do.".to_owned(),
                format!(
                    "If you do not, cancel before {}: in the organization's billing settings, Cancel subscription. The plan stays until then, and {} moves to the Free plan afterwards. Nothing is deleted.",
                    date(period_end),
                    org.name,
                ),
            ],
            Some(("Cancel subscription", cancel(org))),
            format!(
                "{} romulus reminds every admin of a yearly renewal more than a month before it (Article L215-1 of the French Consumer Code).",
                footer(org)
            ),
        )
    })
    .await
}

/// A second subscription was cancelled and refunded (two checkouts paid at
/// once): the admins are told what happened to their money.
pub async fn duplicate_cancelled(
    state: &AppState,
    org_id: Uuid,
    subscription: &str,
    refund: Option<&super::stripe::Refund>,
) -> ApiResult<()> {
    if !once(&state.db, org_id, "duplicate_cancelled", subscription).await? {
        return Ok(());
    }
    let refunded = refund.map_or_else(
        || "Nothing had been charged for it.".to_owned(),
        |r| {
            format!(
                "Its payment of {} is refunded in full: it shows on your statement within days.",
                money(r.amount, &r.currency)
            )
        },
    );
    to_admins(state, org_id, "billing_duplicate_cancelled", |_, org| {
        (
            format!("A second subscription for {} was cancelled", org.name),
            vec![
                format!(
                    "Two checkouts for {} were paid at nearly the same moment, which started a second subscription. The first one stays; romulus cancelled the second at once.",
                    org.name
                ),
                refunded.clone(),
            ],
            Some(("Billing settings", billing_link(state, &org.slug, ""))),
            footer(org),
        )
    })
    .await?;
    Ok(())
}

/// A payment failed: once per invoice.
pub async fn payment_failed(
    state: &AppState,
    org_id: Uuid,
    invoice: &Invoice,
    grace_until: Option<DateTime<Utc>>,
) -> ApiResult<()> {
    if !once(&state.db, org_id, "payment_failed", &invoice.id).await? {
        return Ok(());
    }
    let amount = money(invoice.total, &invoice.currency);
    to_admins(state, org_id, "billing_payment_failed", |_, org| {
        let until = grace_until.map_or_else(
            || "for now".to_owned(),
            |until| format!("until {}", date(until)),
        );
        (
            format!("A payment for {} failed", org.name),
            vec![
                format!("The payment of {amount} for {} did not go through. Stripe will try again.", org.name),
                format!(
                    "The plan stays {until}. To keep it after that, update the payment method; otherwise {} moves to the Free plan. Nothing is deleted.",
                    org.name
                ),
            ],
            Some(("Update the payment method", billing_link(state, &org.slug, ""))),
            footer(org),
        )
    })
    .await?;
    Ok(())
}

/// Playground hours at 80 % (once a month).
pub async fn hours_warning(
    state: &AppState,
    org_id: Uuid,
    plan: Plan,
    used_seconds: i64,
    month: NaiveDate,
    next: NaiveDate,
) -> ApiResult<()> {
    let key = month.format("%Y-%m").to_string();
    if !once(&state.db, org_id, "hours_80", &key).await? {
        return Ok(());
    }
    let hours = plan.limits().playground_hours;
    let used = used_seconds / 3600;
    to_admins(state, org_id, "billing_hours_warning", |_, org| {
        (
            format!("{} used 80 % of its playground hours", org.name),
            vec![
                format!(
                    "{} ran playgrounds for about {used} of its {hours} hours this month ({} plan).",
                    org.name,
                    plan.name()
                ),
                format!(
                    "Once all are used, running playgrounds stop and new ones wait until {}. Their data stays.",
                    next.format("%B %-d")
                ),
            ],
            Some(("See usage", billing_link(state, &org.slug, ""))),
            footer(org),
        )
    })
    .await?;
    Ok(())
}

/// Playground hours all used (the caller records it once a month).
pub async fn hours_used_up(
    state: &AppState,
    org_id: Uuid,
    plan: Plan,
    month: NaiveDate,
    next: NaiveDate,
) {
    let sent = to_admins(state, org_id, "billing_hours_used_up", |_, org| {
        (
            format!("{} used its playground hours for {}", org.name, month.format("%B")),
            vec![
                format!(
                    "{} used the {} playground hours of its {} plan this month. Its running playgrounds were stopped; their data stays until they expire.",
                    org.name,
                    plan.limits().playground_hours,
                    plan.name()
                ),
                format!(
                    "Playgrounds and migration tests start again on {}, or now with a larger plan.",
                    next.format("%B %-d")
                ),
            ],
            Some(("See plans", billing_link(state, &org.slug, ""))),
            footer(org),
        )
    })
    .await;
    if let Err(err) = sent {
        tracing::warn!(error = %err, "billing: cannot email about playground hours");
    }
}

/// A plan's price changes at the next renewal (Terms 7.6: 30 days ahead at
/// least). Whoever moves subscriptions to a new price sends it, once per
/// organization and change (`change`: e.g. `team_yearly:2027-01`).
pub async fn price_change(
    state: &AppState,
    org_id: Uuid,
    change: &str,
    plan: Plan,
    interval: Interval,
    new_cents: i64,
    from: DateTime<Utc>,
) -> ApiResult<bool> {
    if !once(&state.db, org_id, "price_change", change).await? {
        return Ok(false);
    }
    let period = match interval {
        Interval::Month => "month",
        Interval::Year => "year",
    };
    to_admins(state, org_id, "billing_price_change", |_, org| {
        (
            format!("The price of {}'s {} plan changes on {}", org.name, plan.name(), date(from)),
            vec![
                format!(
                    "From your renewal on {}, the {} plan costs {} a {period}.",
                    date(from),
                    plan.name(),
                    money(new_cents, "eur")
                ),
                "If you do not want to continue at this price, cancel before then: the plan stays until the end of the period you paid for.".to_owned(),
            ],
            Some(("Cancel subscription", billing_link(state, &org.slug, "action=cancel"))),
            footer(org),
        )
    })
    .await?;
    Ok(true)
}

/// Looks for due notices every [`NOTICES_EVERY`] until aborted.
pub fn run(state: AppState, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            if let Err(err) = due(&state, Utc::now()).await {
                tracing::warn!(error = %err, "billing: notices failed");
            }
        }
    })
}

/// What is due at `now`: renewal reminders, grace warnings, downgrades.
pub async fn due(state: &AppState, now: DateTime<Utc>) -> ApiResult<()> {
    if !state.billing.is_on() {
        return Ok(());
    }
    // Stripe retries a delivery for three days: event ids older than 90 are
    // of no use.
    sqlx::query!("DELETE FROM billing_events WHERE received_at < now() - interval '90 days'")
        .execute(&state.db)
        .await?;
    let renewals = sqlx::query!(
        r#"SELECT org_id, plan AS "plan: Plan", current_period_end AS "period_end!", amount_cents, currency,
                  scheduled_plan AS "scheduled_plan: Plan",
                  scheduled_interval AS "scheduled_interval: Interval", scheduled_at
           FROM org_billing
           WHERE interval = 'year' AND status IN ('active', 'trialing') AND NOT cancel_at_period_end
             AND current_period_end > $1 AND current_period_end <= $1 + make_interval(days => $2)"#,
        now,
        i32::try_from(REMIND_AHEAD).unwrap_or(40),
    )
    .fetch_all(&state.db)
    .await?;
    for r in renewals {
        if !renewal_due(r.period_end, now) {
            continue;
        }
        if once(&state.db, r.org_id, "renewal", &r.period_end.to_rfc3339()).await? {
            if r.period_end - now < Span::days(30) {
                tracing::warn!(org = %r.org_id, "billing: a renewal reminder sent less than 30 days ahead");
            }
            let renewing = Renewing::of(
                r.plan,
                r.amount_cents.zip(r.currency),
                r.period_end,
                r.scheduled_plan
                    .zip(r.scheduled_at)
                    .map(|(plan, at)| (plan, r.scheduled_interval, at)),
            );
            renewal(state, r.org_id, r.plan, r.period_end, &renewing).await?;
        }
    }

    let overdue = sqlx::query!(
        r#"SELECT org_id, plan AS "plan: Plan", past_due_since AS "since!" FROM org_billing
           WHERE status IN ('past_due', 'unpaid') AND past_due_since IS NOT NULL"#
    )
    .fetch_all(&state.db)
    .await?;
    for r in overdue {
        let until = r.since + Span::days(GRACE_DAYS);
        let key = r.since.to_rfc3339();
        if now >= until {
            if once(&state.db, r.org_id, "downgraded", &key).await? {
                downgraded(state, r.org_id, r.plan).await?;
            }
        } else if now >= until - Span::days(GRACE_WARNING)
            && once(&state.db, r.org_id, "grace_ending", &key).await?
        {
            to_admins(state, r.org_id, "billing_grace_ending", |_, org| {
                (
                    format!("{} moves to the Free plan on {}", org.name, date(until)),
                    vec![
                        format!(
                            "A payment for {}'s {} plan is still outstanding. Unless it is paid by {}, the organization moves to the Free plan.",
                            org.name,
                            r.plan.name(),
                            date(until)
                        ),
                        "Nothing is deleted: what is over the Free plan's caps stays readable, and creating more waits until the plan is back.".to_owned(),
                    ],
                    Some(("Update the payment method", billing_link(state, &org.slug, ""))),
                    footer(org),
                )
            })
            .await?;
        }
    }
    Ok(())
}

/// The grace period ended: audited, and the admins told.
async fn downgraded(state: &AppState, org_id: Uuid, plan: Plan) -> ApiResult<()> {
    audit::record(
        &state.db,
        &audit::Entry {
            org_id,
            database_id: None,
            action: AuditAction::PlanDowngraded,
            actor_id: None,
            actor_name: None,
            version_number: None,
            reason: None,
            details: AuditDetails {
                billing: Some(BillingAudit {
                    from_plan: Some(plan),
                    plan: Some(Plan::Free),
                    ..BillingAudit::default()
                }),
                ..AuditDetails::default()
            },
        },
    )
    .await?;
    to_admins(state, org_id, "billing_downgraded", |_, org| {
        (
            format!("{} is on the Free plan", org.name),
            vec![
                format!(
                    "The payment for {}'s {} plan was not made within {GRACE_DAYS} days, so the organization is on the Free plan now.",
                    org.name,
                    plan.name()
                ),
                "Nothing was deleted. What is over the Free plan's caps stays readable; creating more waits until the plan is back. Paying the outstanding invoice brings the plan back at once.".to_owned(),
            ],
            Some(("Billing settings", billing_link(state, &org.slug, ""))),
            footer(org),
        )
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn the_reminder_says_what_renews_a_scheduled_change_included() {
        let end = Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap();
        let same = Renewing::of(Plan::Team, Some((49_000, "eur".into())), end, None);
        assert_eq!(
            same.sentence(Plan::Team, "Acme", end),
            "The yearly Team plan of Acme renews automatically on March 1, 2027, for €490.00 a year, for another year."
        );
        // A downgrade to monthly Starter at the period's end.
        let down = Renewing::of(
            Plan::Team,
            Some((49_000, "eur".into())),
            end,
            Some((Plan::Starter, Some(Interval::Month), end)),
        );
        assert_eq!(
            (down.plan, down.interval, down.changed),
            (Plan::Starter, Interval::Month, true)
        );
        assert_eq!(
            down.sentence(Plan::Team, "Acme", end),
            "The yearly Team plan of Acme ends its period on March 1, 2027. As scheduled, it then renews automatically as the monthly Starter plan, for €19.00 a month."
        );
        // A yearly downgrade keeps "a year".
        let yearly = Renewing::of(
            Plan::Business,
            None,
            end,
            Some((Plan::Team, Some(Interval::Year), end)),
        );
        assert_eq!(yearly.amount, Some((49_000, "eur".into())));
        assert!(
            yearly
                .sentence(Plan::Business, "Acme", end)
                .contains("the yearly Team plan, for €490.00 a year")
        );
        // A change scheduled after this renewal is not this renewal's.
        let later = Renewing::of(
            Plan::Team,
            Some((49_000, "eur".into())),
            end,
            Some((Plan::Starter, Some(Interval::Month), end + Span::days(365))),
        );
        assert!(!later.changed);
    }

    #[test]
    fn a_yearly_renewal_is_reminded_between_forty_days_and_its_date() {
        let end = Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap();
        assert!(!renewal_due(end, end - Span::days(41)));
        assert!(renewal_due(end, end - Span::days(40)));
        assert!(renewal_due(end, end - Span::days(31)));
        // Late (the job was down): still before the renewal, still sent.
        assert!(renewal_due(end, end - Span::days(2)));
        assert!(!renewal_due(end, end));
        // Within the window the law sets: no earlier than three months, no
        // later than one month ahead.
        const { assert!(REMIND_AHEAD > 30 && REMIND_AHEAD < 90) };
    }
}
