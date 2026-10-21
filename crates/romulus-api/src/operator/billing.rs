//! Billing's events, as the operators hear of them (hosted billing only):
//! a subscription started (the plan, the interval, the price, who bought it
//! and as whom: a business's name and country), upgraded or downgraded (or
//! a change scheduled for the period's end), a cancellation scheduled, a
//! subscription over (with Stripe's reason when it has one), a failed
//! payment, a grace period over, a consumer's withdrawal.
//!
//! The webhook calls [`applied`] in the transaction that stores what Stripe
//! said; [`changes`] decides, from the row before and after, what happened.
//! Every notification links to Stripe's dashboard ([`super::Dashboard`]).

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Dashboard, Event, Notice, QUOTE_MAX, clip, day, monthly, notify, price, who};
use crate::{
    AppState,
    billing::{
        money,
        state::{ENDED_STATUSES, Stored, is_live},
        stripe::{Cancellation, Invoice, Subscription},
    },
    domain::{Buyer, Interval, Plan},
};

/// What a stored change of subscription is, for the operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A subscription began paying (a checkout, or one made in Stripe).
    Started,
    /// The running subscription's plan or interval changed.
    Changed,
    /// A change was scheduled for the end of the period (a downgrade).
    Scheduled,
    /// The running subscription was set to end with its period.
    CancellationScheduled,
    /// The subscription that was running is over (cancelled, or deleted).
    Cancelled,
}

fn paying(stored: &Stored) -> bool {
    stored.withdrawn_at.is_none() && matches!(stored.status.as_deref(), Some("active" | "trialing"))
}

/// What happened between the row `before` and the row `next` a webhook
/// stores.
pub fn changes(before: &Stored, next: &Stored) -> Vec<Change> {
    let same = before.subscription_id.is_some() && before.subscription_id == next.subscription_id;
    // The same subscription, and it was running (paid, or within its grace).
    let was_running = same && before.status.as_deref().is_some_and(is_live);
    let ended = next
        .status
        .as_deref()
        .is_some_and(|status| ENDED_STATUSES.contains(&status));
    let mut changes = Vec::new();
    if paying(next) && !was_running {
        changes.push(Change::Started);
    }
    if was_running && next.is_live() {
        if (before.plan, before.interval) != (next.plan, next.interval) {
            changes.push(Change::Changed);
        }
        if next.scheduled_plan.is_some()
            && (before.scheduled_plan, before.scheduled_interval)
                != (next.scheduled_plan, next.scheduled_interval)
        {
            changes.push(Change::Scheduled);
        }
    }
    if next.is_live() && next.cancel_at_period_end && !(same && before.cancel_at_period_end) {
        changes.push(Change::CancellationScheduled);
    }
    if was_running && ended {
        changes.push(Change::Cancelled);
    }
    changes
}

/// The organization, and who bought its subscription.
struct Context {
    org: String,
    buyer: Option<Bought>,
}

/// A checkout's record: who, and as whom.
struct Bought {
    name: String,
    email: String,
    /// A business's name and country; `None`: a consumer.
    business: Option<(String, Option<String>)>,
}

async fn org_name(conn: &mut PgConnection, org: Uuid) -> Result<String, sqlx::Error> {
    Ok(
        sqlx::query!("SELECT name, slug FROM organizations WHERE id = $1", org)
            .fetch_optional(conn)
            .await?
            .map_or_else(
                || format!("organization {org}"),
                |o| format!("{} ({})", o.name, o.slug),
            ),
    )
}

async fn context(
    conn: &mut PgConnection,
    org: Uuid,
    acceptance: Option<Uuid>,
) -> Result<Context, sqlx::Error> {
    let org = org_name(conn, org).await?;
    let buyer = match acceptance {
        Some(id) => sqlx::query!(
            r#"SELECT user_name, user_email, buyer AS "buyer: Buyer", company_name, company_country
               FROM legal_acceptances WHERE id = $1"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| Bought {
            name: r.user_name,
            email: r.user_email,
            business: (r.buyer != Some(Buyer::Consumer))
                .then(|| r.company_name.map(|name| (name, r.company_country)))
                .flatten(),
        }),
        None => None,
    };
    Ok(Context { org, buyer })
}

fn interval_word(interval: Interval) -> &'static str {
    match interval {
        Interval::Month => "monthly",
        Interval::Year => "yearly",
    }
}

/// `Team, monthly: €49.00 a month`.
fn plan_line(stored: &Stored) -> String {
    plan_of(stored.plan, stored.interval, amount(stored))
}

fn plan_of(plan: Plan, interval: Option<Interval>, amount: Option<(i64, &str)>) -> String {
    match (interval, amount) {
        (Some(interval), Some((cents, currency))) => format!(
            "{}, {}: {}",
            plan.name(),
            interval_word(interval),
            price(cents, currency, interval)
        ),
        (Some(interval), None) => format!("{}, {}", plan.name(), interval_word(interval)),
        (None, _) => plan.name().to_owned(),
    }
}

fn amount(stored: &Stored) -> Option<(i64, &str)> {
    Some((stored.amount_cents?, stored.currency.as_deref()?))
}

/// What a subscription brings in a month (`None`: not known, or not paying).
fn mrr(stored: &Stored) -> Option<(i64, &str)> {
    let (cents, currency) = amount(stored)?;
    Some((monthly(cents, stored.interval?), currency))
}

/// `MRR: +€30.00` (or a loss); `None` when it cannot be said.
fn mrr_line(before: Option<&Stored>, next: Option<&Stored>) -> Option<String> {
    let (delta, currency) = match (before.and_then(mrr), next.and_then(mrr)) {
        (Some((old, old_currency)), Some((new, currency))) if old_currency == currency => {
            (new - old, currency)
        }
        (None, Some((new, currency))) => (new, currency),
        (Some((old, currency)), None) => (-old, currency),
        _ => return None,
    };
    let sign = if delta >= 0 { "+" } else { "" };
    Some(format!("MRR: {sign}{} a month", money(delta, currency)))
}

fn buyer_line(buyer: Option<&Bought>) -> String {
    match buyer {
        Some(Bought {
            name,
            email,
            business: Some((company, country)),
        }) => format!(
            "Bought by {}, as a business: {company}{}.",
            who(name, email),
            country
                .as_deref()
                .map_or_else(String::new, |c| format!(" ({c})"))
        ),
        Some(Bought {
            name,
            email,
            business: None,
        }) => format!("Bought by {}, as a consumer.", who(name, email)),
        None => "No checkout record: made in Stripe's dashboard?".to_owned(),
    }
}

/// Stripe's reason for a cancellation, in words; `None` when it gave none.
pub fn reason_line(cancellation: Option<&Cancellation>) -> Option<String> {
    let c = cancellation?;
    let mut parts = Vec::new();
    if let Some(reason) = c.reason.as_deref() {
        parts.push(
            match reason {
                "cancellation_requested" => "the customer asked",
                "payment_failed" => "payments failed",
                "payment_disputed" => "a payment was disputed",
                other => other,
            }
            .to_owned(),
        );
    }
    if let Some(feedback) = c.feedback.as_deref() {
        let words = match feedback {
            "too_expensive" => "too expensive",
            "missing_features" => "missing features",
            "switched_service" => "switched to another service",
            "unused" => "not used enough",
            "customer_service" => "customer service",
            "too_complex" => "too complex",
            "low_quality" => "quality",
            "other" => "other",
            other => other,
        };
        parts.push(format!("feedback: {words}"));
    }
    if let Some(comment) = c
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        parts.push(format!("comment: “{}”", clip(comment, QUOTE_MAX)));
    }
    (!parts.is_empty()).then(|| format!("Reason: {}.", parts.join("; ")))
}

/// What one webhook stored, for [`applied`].
pub struct Applied<'a> {
    pub org: Uuid,
    /// The Stripe event: a key for what it changed.
    pub event_id: &'a str,
    pub before: &'a Stored,
    pub next: &'a Stored,
    /// The subscription as read from Stripe (`None`: deleted there).
    pub read: Option<&'a Subscription>,
    /// A payment failure that stands (`invoice.payment_failed`).
    pub failed: Option<&'a Invoice>,
}

/// Queues what one webhook's change is worth telling, in its transaction.
pub async fn applied(
    conn: &mut PgConnection,
    state: &AppState,
    event: Applied<'_>,
) -> Result<(), sqlx::Error> {
    let Some(operators) = state.config.operator.as_ref() else {
        return Ok(());
    };
    let changes = changes(event.before, event.next);
    if changes.is_empty() && event.failed.is_none() {
        return Ok(());
    }
    let context = context(conn, event.org, event.next.acceptance_id).await?;
    let dashboard = Dashboard::of(state);
    for change in changes {
        let notice = notice(change, &context, &event, dashboard);
        notify(conn, Some(operators), &notice).await?;
    }
    if let Some(invoice) = event.failed {
        let notice = payment_failed(&context.org, event.next, invoice, dashboard);
        notify(conn, Some(operators), &notice).await?;
    }
    Ok(())
}

fn links(dashboard: Option<Dashboard>, stored: &Stored) -> Vec<String> {
    dashboard.map_or_else(Vec::new, |d| {
        d.lines(
            stored.customer_id.as_deref(),
            stored.subscription_id.as_deref(),
        )
    })
}

fn notice(
    change: Change,
    context: &Context,
    event: &Applied<'_>,
    dashboard: Option<Dashboard>,
) -> Notice {
    let mut notice = match change {
        Change::Started => started(context, event),
        Change::Changed => changed(&context.org, event),
        Change::Scheduled => scheduled(&context.org, event),
        Change::CancellationScheduled => cancelling(&context.org, event),
        Change::Cancelled => cancelled(&context.org, event),
    };
    notice.lines.extend(links(dashboard, event.next));
    notice
}

fn started(context: &Context, event: &Applied<'_>) -> Notice {
    let (org, next) = (&context.org, event.next);
    let mut lines = vec![
        format!("{org} subscribed to {}.", plan_line(next)),
        buyer_line(context.buyer.as_ref()),
    ];
    lines.extend(mrr_line(None, Some(next)));
    Notice {
        event: Event::SubscriptionStarted,
        about: subscription_or(next, event.event_id),
        subject: format!("New subscription: {org}, {}", plan_line(next)),
        lines,
    }
}

/// What a subscription's start or end is about: the subscription (once
/// each), else the event.
fn subscription_or(next: &Stored, event_id: &str) -> String {
    next.subscription_id
        .clone()
        .unwrap_or_else(|| event_id.to_owned())
}

fn changed(org: &str, event: &Applied<'_>) -> Notice {
    let (before, next) = (event.before, event.next);
    let word = match next.plan.cmp(&before.plan) {
        Ordering::Greater => "Upgraded",
        Ordering::Less => "Downgraded",
        Ordering::Equal => "Billing changed",
    };
    let mut lines = vec![format!(
        "{org} moved from {} to {}.",
        plan_line(before),
        plan_line(next)
    )];
    lines.extend(mrr_line(Some(before), Some(next)));
    let named = |s: &Stored| match s.interval {
        Some(interval) => format!("{} {}", s.plan.name(), interval_word(interval)),
        None => s.plan.name().to_owned(),
    };
    Notice {
        event: Event::PlanChanged,
        about: format!("{}:changed", event.event_id),
        subject: format!("{word}: {org}, {} → {}", named(before), named(next)),
        lines,
    }
}

fn scheduled(org: &str, event: &Applied<'_>) -> Notice {
    let next = event.next;
    let to = next.scheduled_plan.unwrap_or(next.plan);
    let word = match to.cmp(&next.plan) {
        Ordering::Less => "Downgrade scheduled",
        Ordering::Greater => "Upgrade scheduled",
        Ordering::Equal => "Change scheduled",
    };
    let when = next.scheduled_at.map_or_else(
        || "at the end of the period".to_owned(),
        |at| format!("on {}", day(at)),
    );
    let interval = next.scheduled_interval.or(next.interval);
    let after = plan_of(
        to,
        interval,
        interval
            .and_then(|i| to.price_cents(i))
            .map(|cents| (i64::from(cents), "eur")),
    );
    Notice {
        event: Event::PlanChanged,
        about: format!("{}:scheduled", event.event_id),
        subject: format!("{word}: {org}, {} → {} {when}", next.plan.name(), to.name()),
        lines: vec![format!(
            "{org} is on {}: it becomes {after} {when}.",
            plan_line(next)
        )],
    }
}

fn cancelling(org: &str, event: &Applied<'_>) -> Notice {
    let next = event.next;
    let ends = next.current_period_end.map_or_else(
        || "with its period".to_owned(),
        |at| format!("on {}", day(at)),
    );
    let mut lines = vec![format!(
        "{org} cancelled its subscription, {}: it ends {ends}, then the organization is on Free.",
        plan_line(next)
    )];
    lines.extend(reason_line(
        event.read.and_then(|r| r.cancellation.as_ref()),
    ));
    lines.extend(mrr_line(Some(next), None).map(|line| format!("{line} from then")));
    Notice {
        event: Event::CancellationScheduled,
        about: format!("{}:cancellation", event.event_id),
        subject: format!(
            "Cancellation scheduled: {org}, {} (ends {ends})",
            next.plan.name()
        ),
        lines,
    }
}

fn cancelled(org: &str, event: &Applied<'_>) -> Notice {
    let (before, next) = (event.before, event.next);
    let mut lines = vec![format!(
        "{org}'s subscription is over (Stripe status {}): {}. The organization is on Free.",
        next.status.as_deref().unwrap_or("canceled"),
        plan_line(before)
    )];
    if let Some(at) = before.withdrawn_at {
        lines.push(format!("After the consumer's withdrawal of {}.", day(at)));
    }
    lines.extend(reason_line(
        event.read.and_then(|r| r.cancellation.as_ref()),
    ));
    lines.extend(mrr_line(Some(before), None));
    Notice {
        event: Event::SubscriptionCancelled,
        about: subscription_or(next, event.event_id),
        subject: format!("Subscription cancelled: {org}, {}", before.plan.name()),
        lines,
    }
}

fn payment_failed(
    org: &str,
    next: &Stored,
    invoice: &Invoice,
    dashboard: Option<Dashboard>,
) -> Notice {
    let total = money(invoice.total, &invoice.currency);
    let mut lines = vec![format!(
        "A payment of {total} for {org}'s {} failed (invoice {}). Stripe tries again.",
        plan_line(next),
        invoice.number.as_deref().unwrap_or(&invoice.id),
    )];
    lines.push(next.grace_until().map_or_else(
        || "The plan holds while Stripe retries.".to_owned(),
        |until| {
            format!(
                "The plan holds until {}, then the organization is on Free unless it is paid.",
                day(until)
            )
        },
    ));
    if let Some(d) = dashboard {
        lines.push(format!("Stripe invoice: {}", d.invoice(&invoice.id)));
        lines.extend(d.lines(next.customer_id.as_deref(), None));
    }
    Notice {
        event: Event::PaymentFailed,
        about: invoice.id.clone(),
        subject: format!("Payment failed: {org}, {total}"),
        lines,
    }
}

/// The grace period after a failed payment ended (`billing::notices`): the
/// organization is on Free. In the transaction that records it.
pub async fn grace_ended(
    conn: &mut PgConnection,
    state: &AppState,
    org: Uuid,
    since: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(operators) = state.config.operator.as_ref() else {
        return Ok(());
    };
    let stored = crate::billing::stored(&mut *conn, org).await?;
    let name = org_name(conn, org).await?;
    let mut lines = vec![
        format!(
            "{name}'s payment for {} was not made within {} days: the organization is on the Free plan now.",
            plan_line(&stored),
            crate::billing::state::GRACE_DAYS
        ),
        format!("Unpaid since {}.", day(since)),
    ];
    lines.extend(links(Dashboard::of(state), &stored));
    notify(
        conn,
        Some(operators),
        &Notice {
            event: Event::GraceEnded,
            about: format!("{org}:{}", since.timestamp()),
            subject: format!("Moved to Free: {name}, {} unpaid", stored.plan.name()),
            lines,
        },
    )
    .await
}

/// A consumer's withdrawal, as recorded (`billing::withdrawal`).
pub struct Withdrawal<'a> {
    pub org: Uuid,
    pub name: &'a str,
    pub email: &'a str,
    pub plan: Plan,
    pub interval: Interval,
    pub started: DateTime<Utc>,
    pub requested_at: DateTime<Utc>,
    pub paid: i64,
    pub refund: i64,
    pub currency: &'a str,
    pub customer: Option<&'a str>,
    pub subscription: &'a str,
}

/// Queues a consumer's withdrawal, in the transaction that records it.
pub async fn withdrawal(
    conn: &mut PgConnection,
    state: &AppState,
    w: &Withdrawal<'_>,
) -> Result<(), sqlx::Error> {
    let Some(operators) = state.config.operator.as_ref() else {
        return Ok(());
    };
    let org = org_name(conn, w.org).await?;
    let days = (w.requested_at - w.started).num_days().max(0);
    let mut lines = vec![
        format!(
            "{} withdrew from {org}'s {} {} subscription, {days} day{} after it began on {}. The organization is on Free.",
            who(w.name, w.email),
            w.plan.name(),
            interval_word(w.interval),
            if days == 1 { "" } else { "s" },
            day(w.started),
        ),
        format!(
            "Paid {}; refunded {} for the days not used. Stripe cancels the subscription and refunds now (retried by the billing task if Stripe fails).",
            money(w.paid, w.currency),
            money(w.refund, w.currency),
        ),
    ];
    if let Some(d) = Dashboard::of(state) {
        lines.extend(d.lines(w.customer, Some(w.subscription)));
    }
    notify(
        conn,
        Some(operators),
        &Notice {
            event: Event::Withdrawal,
            about: w.subscription.to_owned(),
            subject: format!(
                "Withdrawal: {org}, {}, refund {}",
                w.plan.name(),
                money(w.refund, w.currency)
            ),
            lines,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(plan: Plan, status: &str) -> Stored {
        Stored {
            subscription_id: Some("sub_1".into()),
            customer_id: Some("cus_1".into()),
            plan,
            interval: Some(Interval::Month),
            status: Some(status.into()),
            amount_cents: plan.price_cents(Interval::Month).map(i64::from),
            currency: Some("eur".into()),
            ..Stored::default()
        }
    }

    #[test]
    fn a_subscription_starts_once_it_pays_and_not_again_after_a_late_payment() {
        assert_eq!(
            changes(&Stored::default(), &running(Plan::Team, "active")),
            [Change::Started]
        );
        // Paid after `incomplete`: it starts then.
        assert_eq!(
            changes(
                &running(Plan::Team, "incomplete"),
                &running(Plan::Team, "active")
            ),
            [Change::Started]
        );
        assert_eq!(
            changes(&Stored::default(), &running(Plan::Team, "incomplete")),
            []
        );
        // Paid again after a failure: nothing new.
        assert_eq!(
            changes(
                &running(Plan::Team, "past_due"),
                &running(Plan::Team, "active")
            ),
            []
        );
        // A new subscription after the last one ended.
        let mut ended = running(Plan::Starter, "canceled");
        ended.subscription_id = Some("sub_0".into());
        assert_eq!(
            changes(&ended, &running(Plan::Team, "active")),
            [Change::Started]
        );
    }

    #[test]
    fn plan_changes_scheduled_changes_and_cancellations_are_told() {
        let team = running(Plan::Team, "active");
        let starter = running(Plan::Starter, "active");
        assert_eq!(changes(&starter, &team), [Change::Changed]);
        let mut scheduled = team.clone();
        scheduled.scheduled_plan = Some(Plan::Starter);
        scheduled.scheduled_interval = Some(Interval::Month);
        assert_eq!(changes(&team, &scheduled), [Change::Scheduled]);
        assert_eq!(changes(&scheduled, &scheduled), [], "told once");
        let mut cancelling = team.clone();
        cancelling.cancel_at_period_end = true;
        assert_eq!(changes(&team, &cancelling), [Change::CancellationScheduled]);
        assert_eq!(changes(&cancelling, &cancelling), []);
        let over = running(Plan::Team, "canceled");
        assert_eq!(changes(&cancelling, &over), [Change::Cancelled]);
        assert_eq!(changes(&over, &over), []);
        // Never paid, then expired: nothing was running.
        assert_eq!(
            changes(
                &running(Plan::Team, "incomplete"),
                &running(Plan::Team, "incomplete_expired")
            ),
            []
        );
        // A withdrawal, then Stripe cancels.
        let mut withdrawn = team.clone();
        withdrawn.withdrawn_at = Some(Utc::now());
        let mut withdrawn_over = over.clone();
        withdrawn_over.withdrawn_at = withdrawn.withdrawn_at;
        assert_eq!(changes(&withdrawn, &withdrawn_over), [Change::Cancelled]);
    }

    #[test]
    fn money_lines_say_the_monthly_difference() {
        let team = running(Plan::Team, "active");
        let starter = running(Plan::Starter, "active");
        assert_eq!(
            mrr_line(Some(&starter), Some(&team)).as_deref(),
            Some("MRR: +€30.00 a month")
        );
        assert_eq!(
            mrr_line(Some(&team), None).as_deref(),
            Some("MRR: -€49.00 a month")
        );
        assert_eq!(plan_line(&team), "Team, monthly: €49.00 a month");
    }

    #[test]
    fn stripes_reasons_read_as_words() {
        assert_eq!(reason_line(None), None);
        assert_eq!(reason_line(Some(&Cancellation::default())), None);
        let said = Cancellation {
            reason: Some("cancellation_requested".into()),
            feedback: Some("too_expensive".into()),
            comment: Some("  We moved to a spreadsheet. ".into()),
        };
        assert_eq!(
            reason_line(Some(&said)).as_deref(),
            Some(
                "Reason: the customer asked; feedback: too expensive; comment: “We moved to a spreadsheet.”."
            )
        );
    }
}
