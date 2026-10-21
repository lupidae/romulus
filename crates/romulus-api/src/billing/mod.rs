//! Hosted billing (docs/MVP.md "Billing"): plans and their caps, Stripe
//! subscriptions, the emails the hosted terms promise. Off unless
//! `ROMULUS_BILLING` is set: then nothing is capped and every billing route
//! answers 404, so a self-hosted install is exactly as it was.
//!
//! - [`plans`]: the plan table, the one place the numbers live.
//! - [`state`]: an organization's subscription as stored, and the plan in
//!   force (grace period included).
//! - [`caps`]: each cap's check, 402 `plan_limit` with what lifts it. Callers:
//!   `databases` (create), `versions::store` (versions and schema size, under
//!   the database's lock), `imports`, `orgs` (invitations, approvals),
//!   `drafts::approvals` (override), `playgrounds` and `migration_tests`
//!   (size, hours), and every read of versions (the history window).
//! - [`stripe`]: the calls romulus makes, behind a trait (`Http`, `Fake`).
//! - [`api`]: the endpoints below; [`webhook`]: Stripe's events;
//!   [`events`]: the events kept, 90 days (billing on or off).
//! - [`hours`]: playground time metered per organization and month, the
//!   80 % warning, the stop at 100 %.
//! - [`notices`]: the emails, in English or French: the subscription's
//!   and a cancellation's confirmations, renewal reminders, payment and
//!   grace emails, price changes.
//! - [`withdrawal`]: a consumer's withdrawal within 14 days (cancelled at
//!   once, the unused part refunded, Free).
//! - [`copy`]: the checkout boxes' words (shared with the web app).
//! - [`setup`]: `romulus-api stripe-setup`, products, prices and the portal.
//!
//! Endpoints (who may call them in brackets; see `auth::member`):
//! - `GET /orgs/{org}/billing` [member]: the plan, the subscription, usage
//!   against every cap, the plans on offer.
//! - `POST /orgs/{org}/billing/checkout` [admin]: as a business or a
//!   consumer, the Terms and the boxes of each accepted (recorded in
//!   `legal_acceptances`, audited), a Stripe Checkout Session's URL.
//! - `POST /orgs/{org}/billing/withdrawal` [admin]: a consumer withdraws
//!   (within 14 days of subscribing).
//! - `POST /orgs/{org}/billing/portal` [admin]: a Customer Portal session's
//!   URL, at its home page or straight in a flow (cancel, change plan).
//! - `GET /orgs/{org}/billing/invoices` [admin].
//! - `POST /billing/webhook` [Stripe, signed]: subscription and invoice
//!   events, idempotent by event id, each re-reading the subscription.
//!
//! Tables: `org_billing`, `billing_events`, `playground_usage`,
//! `billing_notices`, `withdrawals`; the checkout's records in
//! `legal_acceptances` (`legal`); events in `audit_events` (org-level, not
//! shown in a database's activity).

pub mod api;
pub mod caps;
pub mod copy;
pub mod countries;
pub mod events;
pub mod hours;
pub mod notices;
pub mod plans;
pub mod setup;
pub mod state;
pub mod stripe;
pub mod webhook;
pub mod withdrawal;

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgExecutor;
use uuid::Uuid;

pub use self::{
    api::router,
    caps::{Cap, HistoryWindow, PlanLimitDetails, VersionLimits},
};
use self::{
    state::Stored,
    stripe::{Fake, Http, StripeApi},
};
use crate::{
    ApiError, ApiResult, Config,
    config::{BillingConfig, LegalConfig, StripeKeys},
    domain::{Interval, Plan},
};

/// Billing as this API process runs it: off, or Stripe (real or fake) with
/// its settings. Cheap to clone; in `AppState`.
#[derive(Clone)]
pub struct Billing(Option<Arc<Inner>>);

pub struct Inner {
    pub config: BillingConfig,
    /// The Terms checkout has people accept (billing needs them).
    pub legal: LegalConfig,
    pub stripe: Arc<dyn StripeApi>,
    /// The fake, when it is one: its checkout page completes sessions.
    pub fake: Option<Fake>,
    /// Where links in emails and redirects point.
    pub public_url: url::Url,
    pub db: sqlx::PgPool,
}

impl Billing {
    pub fn off() -> Self {
        Self(None)
    }

    /// From `Config::billing`: off, Stripe, or the fake.
    pub fn from_config(config: &Config, db: sqlx::PgPool) -> Self {
        let Some(billing) = &config.billing else {
            return Self::off();
        };
        let Some(legal) = config.legal.clone() else {
            // Config refuses billing without Terms.
            tracing::error!("billing: no Terms configured, billing is off");
            return Self::off();
        };
        let (stripe, fake): (Arc<dyn StripeApi>, _) = match &billing.stripe {
            StripeKeys::Stripe { secret_key, .. } => {
                (Arc::new(Http::new(secret_key.clone())), None)
            }
            StripeKeys::Fake { .. } => {
                let fake = Fake::new(&config.public_origin());
                (Arc::new(fake.clone()), Some(fake))
            }
        };
        Self(Some(Arc::new(Inner {
            config: billing.clone(),
            legal,
            stripe,
            fake,
            public_url: config.public_url.clone(),
            db,
        })))
    }

    /// On, with `stripe` (tests: a [`Fake`]).
    pub fn with_stripe(
        config: BillingConfig,
        legal: LegalConfig,
        public_url: url::Url,
        db: sqlx::PgPool,
        stripe: Fake,
    ) -> Self {
        Self(Some(Arc::new(Inner {
            config,
            legal,
            stripe: Arc::new(stripe.clone()),
            fake: Some(stripe),
            public_url,
            db,
        })))
    }

    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// The running billing, or 404: with billing off its routes do not exist.
    pub fn require(&self) -> ApiResult<&Inner> {
        self.0
            .as_deref()
            .ok_or_else(|| ApiError::not_found("No such endpoint."))
    }

    pub fn inner(&self) -> Option<&Inner> {
        self.0.as_deref()
    }

    /// The plan in force for `org` now; `None` when billing is off (no caps).
    pub async fn plan(&self, db: impl PgExecutor<'_>, org: Uuid) -> ApiResult<Option<Plan>> {
        if !self.is_on() {
            return Ok(None);
        }
        Ok(Some(stored(db, org).await?.effective(Utc::now())))
    }

    /// The plan in force, read on the pool (for callers inside a
    /// transaction that must not use it for this read); `None` when
    /// billing is off.
    pub async fn plan_now(&self, org: Uuid) -> ApiResult<Option<Plan>> {
        let Some(inner) = self.inner() else {
            return Ok(None);
        };
        Ok(Some(stored(&inner.db, org).await?.effective(Utc::now())))
    }
}

/// Billing's background tasks: Stripe's webhook events past 90 days swept
/// every hour, billing on or off (those of a past configuration go too);
/// when it is on, playground hours metered every minute, the emails it
/// owes (renewal reminders, payment notices) every hour.
pub fn tasks(state: &crate::AppState) -> Vec<tokio::task::JoinHandle<()>> {
    let events = events::sweeper(state.db.clone(), events::SWEEP_EVERY);
    let Some(inner) = state.billing.inner() else {
        return vec![events];
    };
    if inner.fake.is_some() {
        tracing::warn!("billing: ROMULUS_BILLING=fake, Stripe is simulated (tests only)");
    }
    vec![
        events,
        hours::meter(state.clone(), hours::METER_EVERY),
        notices::run(state.clone(), notices::NOTICES_EVERY),
    ]
}

/// `org`'s `org_billing` row, or what an organization without one has
/// (Free).
pub async fn stored(db: impl PgExecutor<'_>, org: Uuid) -> Result<Stored, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT customer_id, subscription_id, plan AS "plan: Plan",
                  interval AS "interval: Interval", status, current_period_end,
                  cancel_at_period_end, amount_cents, currency, past_due_since,
                  scheduled_plan AS "scheduled_plan: Plan",
                  scheduled_interval AS "scheduled_interval: Interval", scheduled_at, updated_at,
                  acceptance_id, started_at, withdrawn_at
           FROM org_billing WHERE org_id = $1"#,
        org,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map_or_else(
        || Stored {
            org_id: org,
            ..Stored::default()
        },
        |r| Stored {
            org_id: org,
            customer_id: r.customer_id,
            subscription_id: r.subscription_id,
            plan: r.plan,
            interval: r.interval,
            status: r.status,
            current_period_end: r.current_period_end,
            cancel_at_period_end: r.cancel_at_period_end,
            amount_cents: r.amount_cents,
            currency: r.currency,
            past_due_since: r.past_due_since,
            scheduled_plan: r.scheduled_plan,
            scheduled_interval: r.scheduled_interval,
            scheduled_at: r.scheduled_at,
            acceptance_id: r.acceptance_id,
            started_at: r.started_at,
            withdrawn_at: r.withdrawn_at,
            updated_at: Some(r.updated_at),
        },
    ))
}

/// One organization's billing changes at a time (a checkout starting, a
/// webhook applying, a database created against the cap): an advisory lock
/// for the transaction, keyed by the organization.
pub async fn lock_org(tx: &mut sqlx::PgConnection, org: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"SELECT 1 AS "locked!" FROM pg_advisory_xact_lock(hashtextextended('billing:' || $1::text, 0))"#,
        org.to_string(),
    )
    .fetch_one(tx)
    .await?;
    Ok(())
}

/// Money as people read it: `€19.00`, `€490.00`.
pub fn money(cents: i64, currency: &str) -> String {
    let symbol = match currency.to_ascii_lowercase().as_str() {
        "eur" => "€".to_owned(),
        "usd" => "$".to_owned(),
        other => format!("{} ", other.to_ascii_uppercase()),
    };
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}{symbol}{}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_reads_as_euros() {
        assert_eq!(money(1_900, "eur"), "€19.00");
        assert_eq!(money(49_000, "EUR"), "€490.00");
        assert_eq!(money(5, "usd"), "$0.05");
        assert_eq!(money(-250, "gbp"), "-GBP 2.50");
    }
}
