//! An organization's subscription as stored (`org_billing`), and the plan in
//! force from it: the rules, pure, so they are tested without Stripe.
//!
//! - `active` or `trialing`: the subscribed plan;
//! - `past_due` or `unpaid`: the subscribed plan for [`GRACE_DAYS`] from the
//!   unpaid invoice's due date, then Free;
//! - anything else (`canceled`, `incomplete`, `paused`, none): Free;
//! - withdrawn from (a consumer, within 14 days): Free at once, whatever
//!   Stripe still says while it cancels.
//!
//! Nothing is ever deleted when the plan drops: caps only block creating
//! the next resource.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::domain::{Interval, Plan};

/// Days a past-due subscription keeps its plan, from the invoice's due date.
pub const GRACE_DAYS: i64 = 14;

/// The Stripe subscription statuses that pay for their plan (or did,
/// recently): [`is_live`], for SQL.
pub const LIVE_STATUSES: [&str; 4] = ["active", "trialing", "past_due", "unpaid"];

/// A Stripe subscription status that pays for its plan (or did, recently).
pub fn is_live(status: &str) -> bool {
    LIVE_STATUSES.contains(&status)
}

/// The statuses of a subscription that is over for good: neither charging
/// nor able to resume. Anything else (`incomplete` may still be paid,
/// `paused` resumed) keeps an organization from being deleted
/// (`retention::deletion`).
pub const ENDED_STATUSES: [&str; 2] = ["canceled", "incomplete_expired"];

fn is_past_due(status: &str) -> bool {
    matches!(status, "past_due" | "unpaid")
}

/// One `org_billing` row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stored {
    pub org_id: Uuid,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub plan: Plan,
    pub interval: Option<Interval>,
    pub status: Option<String>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    /// The price's amount a period (the plan table's, unless the price
    /// moved since).
    pub amount_cents: Option<i64>,
    /// Off every period for as long as it runs (the founding-team offer):
    /// what a period costs is [`Stored::paid_cents`].
    pub discount_cents: Option<i64>,
    pub currency: Option<String>,
    pub past_due_since: Option<DateTime<Utc>>,
    pub scheduled_plan: Option<Plan>,
    pub scheduled_interval: Option<Interval>,
    pub scheduled_at: Option<DateTime<Utc>>,
    /// The checkout record of this subscription (who bought it, as whom).
    pub acceptance_id: Option<Uuid>,
    /// When it started: a consumer may withdraw for 14 days from then.
    pub started_at: Option<DateTime<Utc>>,
    /// A consumer withdrew from it: Free from then on.
    pub withdrawn_at: Option<DateTime<Utc>>,
    /// When the row last changed (`None`: no row): a webhook that read
    /// Stripe before another one wrote starts again.
    pub updated_at: Option<DateTime<Utc>>,
}

impl Stored {
    /// What a period costs: the price's amount less a discount for as long as it runs.
    pub fn paid_cents(&self) -> Option<i64> {
        self.amount_cents
            .map(|amount| (amount - self.discount_cents.unwrap_or(0)).max(0))
    }

    /// The plan in force at `now`.
    pub fn effective(&self, now: DateTime<Utc>) -> Plan {
        let Some(status) = self.status.as_deref() else {
            return Plan::Free;
        };
        if self.withdrawn_at.is_some() {
            return Plan::Free;
        }
        match status {
            "active" | "trialing" => self.plan,
            _ if is_past_due(status) => match self.grace_until() {
                Some(until) if now < until => self.plan,
                _ => Plan::Free,
            },
            _ => Plan::Free,
        }
    }

    /// While past due: when the plan stops holding.
    pub fn grace_until(&self) -> Option<DateTime<Utc>> {
        let status = self.status.as_deref()?;
        if !is_past_due(status) {
            return None;
        }
        // Past due without a known due date: the grace starts when romulus
        // learns of it, so it is never shorter.
        Some(self.past_due_since? + Duration::days(GRACE_DAYS))
    }

    /// A subscription is running (whatever its payments): changing plans
    /// goes through the portal, not a new checkout.
    pub fn has_live_subscription(&self) -> bool {
        self.subscription_id.is_some() && self.is_live()
    }

    /// The stored subscription pays for its plan (or did, recently), and
    /// nobody withdrew from it.
    pub fn is_live(&self) -> bool {
        self.withdrawn_at.is_none() && self.status.as_deref().is_some_and(is_live)
    }
}

/// Whether a subscription read from Stripe may overwrite what is stored: the
/// one stored, or a live one replacing one that ended. A late event about an
/// old subscription never undoes the current one, and a second live
/// subscription never replaces the first ([`is_duplicate`]).
pub fn should_apply(stored: &Stored, incoming_id: &str, incoming_status: &str) -> bool {
    match stored.subscription_id.as_deref() {
        None => true,
        Some(id) if id == incoming_id => true,
        Some(_) => is_live(incoming_status) && !stored.is_live(),
    }
}

/// A second live subscription while another live one is stored (two
/// checkouts paid at once, or one made in the Dashboard): the stored one is
/// kept, this one cancelled and refunded.
pub fn is_duplicate(stored: &Stored, incoming_id: &str, incoming_status: &str) -> bool {
    stored
        .subscription_id
        .as_deref()
        .is_some_and(|id| id != incoming_id)
        && stored.is_live()
        && is_live(incoming_status)
}

/// When to set `past_due_since`: kept once set, the due date otherwise,
/// cleared when the subscription pays again.
pub fn past_due_since(
    stored: Option<DateTime<Utc>>,
    status: &str,
    due: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !is_past_due(status) {
        return None;
    }
    stored.or(due).or(Some(now)).map(|since| since.min(now))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap()
    }

    fn subscribed(status: &str) -> Stored {
        Stored {
            subscription_id: Some("sub_1".into()),
            plan: Plan::Team,
            interval: Some(Interval::Month),
            status: Some(status.into()),
            ..Stored::default()
        }
    }

    #[test]
    fn a_period_costs_the_price_less_its_discount_for_life() {
        let mut stored = subscribed("active");
        assert_eq!(stored.paid_cents(), None);
        stored.amount_cents = Some(4_900);
        assert_eq!(stored.paid_cents(), Some(4_900));
        stored.discount_cents = Some(2_000);
        assert_eq!(stored.paid_cents(), Some(2_900));
        stored.discount_cents = Some(9_000);
        assert_eq!(stored.paid_cents(), Some(0));
    }

    #[test]
    fn an_active_subscription_gives_its_plan_and_none_gives_free() {
        assert_eq!(subscribed("active").effective(at(1)), Plan::Team);
        assert_eq!(subscribed("trialing").effective(at(1)), Plan::Team);
        assert_eq!(Stored::default().effective(at(1)), Plan::Free);
        for status in ["canceled", "incomplete", "incomplete_expired", "paused"] {
            assert_eq!(subscribed(status).effective(at(1)), Plan::Free, "{status}");
        }
    }

    #[test]
    fn past_due_keeps_the_plan_for_fourteen_days_from_the_due_date_then_free() {
        let mut stored = subscribed("past_due");
        stored.past_due_since = Some(at(1));
        assert_eq!(stored.effective(at(1)), Plan::Team);
        assert_eq!(stored.effective(at(14)), Plan::Team);
        assert_eq!(stored.grace_until(), Some(at(15)));
        assert_eq!(stored.effective(at(15)), Plan::Free);
        assert_eq!(stored.effective(at(28)), Plan::Free);
        stored.status = Some("unpaid".into());
        assert_eq!(stored.effective(at(10)), Plan::Team);
        // Paid again: no grace to speak of.
        stored.status = Some("active".into());
        assert_eq!(stored.grace_until(), None);
        assert_eq!(stored.effective(at(28)), Plan::Team);
    }

    #[test]
    fn the_due_date_is_kept_once_known_and_cleared_when_paid() {
        let now = at(10);
        assert_eq!(
            past_due_since(None, "past_due", Some(at(3)), now),
            Some(at(3))
        );
        assert_eq!(
            past_due_since(Some(at(2)), "past_due", Some(at(9)), now),
            Some(at(2))
        );
        assert_eq!(past_due_since(None, "unpaid", None, now), Some(now));
        assert_eq!(past_due_since(Some(at(2)), "active", None, now), None);
        // A due date in the future (an invoice not due yet) starts today.
        assert_eq!(
            past_due_since(None, "past_due", Some(at(20)), now),
            Some(now)
        );
    }

    #[test]
    fn a_late_event_about_an_old_subscription_never_undoes_the_live_one() {
        let live = subscribed("active");
        assert!(should_apply(&live, "sub_1", "canceled"));
        assert!(!should_apply(&live, "sub_0", "canceled"));
        let ended = subscribed("canceled");
        assert!(should_apply(&ended, "sub_2", "active"));
        assert!(!should_apply(&ended, "sub_2", "incomplete"));
        assert!(!should_apply(&ended, "sub_0", "canceled"));
        assert!(should_apply(&Stored::default(), "sub_9", "active"));
    }

    #[test]
    fn a_second_live_subscription_is_a_duplicate_and_never_replaces_the_first() {
        let live = subscribed("active");
        assert!(is_duplicate(&live, "sub_2", "active"));
        assert!(!should_apply(&live, "sub_2", "active"));
        assert!(is_duplicate(&subscribed("past_due"), "sub_2", "active"));
        assert!(!is_duplicate(&live, "sub_1", "active"), "the same one");
        assert!(!is_duplicate(&live, "sub_2", "canceled"), "an ended one");
        assert!(
            !is_duplicate(&subscribed("canceled"), "sub_2", "active"),
            "a replacement"
        );
        assert!(!is_duplicate(&Stored::default(), "sub_2", "active"));
    }

    #[test]
    fn a_withdrawn_subscription_is_free_at_once_and_a_new_one_may_replace_it() {
        let mut withdrawn = subscribed("active");
        withdrawn.withdrawn_at = Some(at(2));
        assert_eq!(withdrawn.effective(at(3)), Plan::Free);
        assert!(!withdrawn.has_live_subscription());
        // Stripe still says active while it cancels: a new checkout's
        // subscription is not a duplicate of it.
        assert!(!is_duplicate(&withdrawn, "sub_2", "active"));
        assert!(should_apply(&withdrawn, "sub_2", "active"));
        assert!(should_apply(&withdrawn, "sub_1", "canceled"));
    }

    #[test]
    fn only_a_running_subscription_sends_plan_changes_to_the_portal() {
        assert!(subscribed("active").has_live_subscription());
        assert!(subscribed("past_due").has_live_subscription());
        assert!(!subscribed("canceled").has_live_subscription());
        assert!(!Stored::default().has_live_subscription());
    }
}
