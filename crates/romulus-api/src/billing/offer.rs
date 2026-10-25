//! The founding-team offer (`plans::FOUNDING`, Terms 7.11): Team at €29 a
//! month or €290 a year for as long as the subscription stays on Team, for
//! the first 20 teams, once per organization. Stripe holds it as two prices
//! of Team's own product (`team_founding_monthly`, `team_founding_yearly`,
//! made by `romulus-api stripe-setup --founding-offer`, found by their
//! lookup keys), so every invoice, proration and credit is at the price
//! paid. On only with `ROMULUS_FOUNDING_OFFER`.
//!
//! - **Places left**: the 20 less the organizations that ever had a
//!   founding subscription (`founding_offer_uses`, stored by the webhook,
//!   kept when the organization is deleted) less the places held by founding
//!   checkouts (`org_billing.checkout_offer_until`: the session expires
//!   after about 30 minutes, the place stays held a day longer, so a payment
//!   at the last minute whose webhook comes late is still counted; released
//!   when the session expires (`checkout.session.expired`), when the buyer
//!   comes back from it cancelled (`POST …/billing/checkout/cancel`), or
//!   when a live subscription is stored).
//! - **Checkout** ([`reserve`], in the checkout's transaction under the
//!   organization's lock, then the offer's own: one founding checkout decides
//!   at a time) uses the founding price when the buyer came with
//!   `?offer=founding` and asks for Team (any other plan ignores it): refused
//!   when the offer is off or no place is left (409 `offer_ended`), for an
//!   organization that had it (409 `offer_used`), or for someone holding a
//!   place for another organization already (409 `offer_held`, one at a
//!   time); else a place held ([`hold`]) before the session is made.
//! - **Afterwards**, the Customer Portal of a founding subscription
//!   (`stripe-setup`'s second configuration) offers Team at the two founding
//!   prices only, so moving between monthly and yearly keeps the offer, and
//!   Starter and Business at theirs. Moving off Team ends it: the portal of a
//!   subscription no longer on a founding price is the usual one, Team at
//!   its own price. Cancelled or withdrawn, it ends with the subscription.
//!
//! Endpoint (no session): `GET /billing/offers/founding`, the offer and how
//! many are left (none while the offer is off, so the homepage asking logs
//! no error); 404 while billing is off, as every billing route.

use std::time::Duration;

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Inner, plans::FOUNDING};
use crate::{
    ApiError, ApiResult, AppState,
    domain::{Interval, Plan},
    error::ErrorBody,
    extract::Json,
};

/// How long the homepage's count waits for the database.
pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// When a founding checkout's session expires, from when it is made
/// (Stripe takes 30 minutes at least).
pub const SESSION: chrono::Duration = chrono::Duration::minutes(31);
/// How long its place stays held past that, unless released before: a
/// payment made at the last minute whose webhook comes late still counts.
pub const HOLD_AFTER: chrono::Duration = chrono::Duration::hours(24);

/// An offer a checkout may ask for (`StartCheckout::offer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferChoice {
    /// The founding-team offer: Team only.
    Founding,
}

/// The founding-team offer, as the homepage and the billing page show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FoundingOffer {
    /// The plan it is for (Team).
    pub plan: Plan,
    /// Euro cents with the offer, for as long as the subscription stays on
    /// the plan.
    pub monthly_cents: u32,
    pub yearly_cents: u32,
    /// The plan's own prices.
    pub list_monthly_cents: u32,
    pub list_yearly_cents: u32,
    /// How many teams get it in all, and how many places are left.
    pub teams: u32,
    pub left: u32,
    /// The organization asking had it already (once per organization):
    /// its checkouts are at the plan's own price. Always false on the
    /// homepage's count.
    #[serde(default)]
    pub used: bool,
}

/// What a founding-team subscription costs, each period (its plan at these
/// prices, whatever the offer's places).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FoundingPrices {
    pub monthly_cents: u32,
    pub yearly_cents: u32,
}

impl FoundingPrices {
    pub fn of_offer() -> Self {
        Self {
            monthly_cents: FOUNDING.price_cents(Interval::Month),
            yearly_cents: FOUNDING.price_cents(Interval::Year),
        }
    }
}

impl FoundingOffer {
    fn with(left: u32, used: bool) -> Self {
        let list = |interval| FOUNDING.plan.price_cents(interval).unwrap_or_default();
        Self {
            plan: FOUNDING.plan,
            monthly_cents: FOUNDING.price_cents(Interval::Month),
            yearly_cents: FOUNDING.price_cents(Interval::Year),
            list_monthly_cents: list(Interval::Month),
            list_yearly_cents: list(Interval::Year),
            teams: FOUNDING.teams,
            left,
            used,
        }
    }
}

/// The places left: the teams less the organizations that had the offer and
/// those holding a place now, but `except`'s hold.
async fn places_left(
    db: impl sqlx::PgExecutor<'_>,
    except: Option<Uuid>,
) -> Result<u32, sqlx::Error> {
    let taken = sqlx::query_scalar!(
        r#"SELECT (SELECT count(*) FROM founding_offer_uses)
                + (SELECT count(*) FROM org_billing b
                   WHERE b.checkout_offer_until > now() AND b.org_id IS DISTINCT FROM $1
                     AND NOT EXISTS (SELECT 1 FROM founding_offer_uses u WHERE u.org_id = b.org_id))
                AS "taken!""#,
        except,
    )
    .fetch_one(db)
    .await?;
    Ok(u32::try_from((i64::from(FOUNDING.teams) - taken).max(0)).unwrap_or(0))
}

/// The count could not be read.
#[derive(Debug, Clone, Copy)]
pub struct Unread;

/// The places left, within [`READ_TIMEOUT`]; `Ok(None)`: the offer is off.
async fn left(inner: &Inner) -> Result<Option<u32>, Unread> {
    if !inner.config.founding {
        return Ok(None);
    }
    match tokio::time::timeout(READ_TIMEOUT, places_left(&inner.db, None)).await {
        Ok(Ok(left)) => Ok(Some(left)),
        Ok(Err(err)) => {
            tracing::error!(error = %err, "billing: the founding offer's places did not read");
            Err(Unread)
        }
        Err(_) => {
            tracing::warn!("billing: the founding offer's places took too long to read");
            Err(Unread)
        }
    }
}

/// Whether `org` had the founding-team offer already.
async fn used(db: impl sqlx::PgExecutor<'_>, org: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM founding_offer_uses WHERE org_id = $1) AS "used!""#,
        org
    )
    .fetch_one(db)
    .await
}

/// Records that `org` had a subscription on a founding price (the
/// webhook, in the transaction storing it): once, kept for good.
pub async fn record_use(tx: &mut sqlx::PgConnection, org: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO founding_offer_uses (org_id) VALUES ($1) ON CONFLICT (org_id) DO NOTHING",
        org
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// The founding-team offer for `org`'s billing page (its prices, the places
/// left, whether the organization had it): `None` while it is off or its
/// count does not read.
pub async fn shown(inner: &Inner, org: Uuid) -> ApiResult<Option<FoundingOffer>> {
    let Ok(Some(left)) = left(inner).await else {
        return Ok(None);
    };
    Ok(Some(FoundingOffer::with(left, used(&inner.db, org).await?)))
}

fn ended(plan: Plan) -> ApiError {
    ApiError::Conflict {
        code: "offer_ended",
        message: format!(
            "The founding-team offer has ended: {} is at its usual price now.",
            plan.name()
        ),
    }
}

/// Whether a checkout of `plan` asked for with `offer` by `user` is at the
/// founding price, in its transaction (`tx`, the organization locked):
/// `false` for another plan or no offer; 409 `offer_ended` when the offer is
/// off or no place is left (the others' holds counted under the offer's
/// lock, kept until the transaction ends), 409 `offer_used` when the
/// organization had it, 409 `offer_held` when `user` holds a place for
/// another organization. The caller then holds the place ([`hold`]).
pub async fn reserve(
    inner: &Inner,
    tx: &mut sqlx::PgConnection,
    org: Uuid,
    user: Uuid,
    offer: Option<OfferChoice>,
    plan: Plan,
) -> ApiResult<bool> {
    if offer != Some(OfferChoice::Founding) || plan != FOUNDING.plan {
        return Ok(false);
    }
    // Asked for and not there: never charged at the usual price unasked.
    if !inner.config.founding {
        return Err(ended(plan));
    }
    if used(&mut *tx, org).await? {
        return Err(ApiError::Conflict {
            code: "offer_used",
            message: "This organization has had the founding-team offer already: it is once per organization.".into(),
        });
    }
    sqlx::query!(
        r#"SELECT 1 AS "locked!" FROM pg_advisory_xact_lock(hashtextextended('billing:founding_offer', 0))"#,
    )
    .fetch_one(&mut *tx)
    .await?;
    let elsewhere = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM org_billing
                          WHERE checkout_offer_by = $1 AND checkout_offer_until > now()
                            AND org_id <> $2) AS "held!""#,
        user,
        org,
    )
    .fetch_one(&mut *tx)
    .await?;
    if elsewhere {
        return Err(ApiError::Conflict {
            code: "offer_held",
            message: "You have a founding-team checkout open for another organization: pay or cancel it first (one at a time).".into(),
        });
    }
    if places_left(&mut *tx, Some(org)).await? == 0 {
        return Err(ended(plan));
    }
    Ok(true)
}

/// Holds `org`'s place for `user` until `until` (or, `None`, releases it),
/// with its checkout.
pub async fn hold(
    tx: &mut sqlx::PgConnection,
    org: Uuid,
    held: Option<(DateTime<Utc>, Uuid)>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE org_billing SET checkout_offer_until = $2, checkout_offer_by = $3 WHERE org_id = $1",
        org,
        held.map(|(until, _)| until),
        held.map(|(_, user)| user),
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// Releases the place a checkout session held, once it can no longer be
/// paid (expired, in Stripe's event).
pub async fn release_session(
    db: impl sqlx::PgExecutor<'_>,
    session: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE org_billing SET checkout_offer_until = NULL, checkout_offer_by = NULL
         WHERE checkout_session_id = $1 AND checkout_offer_until IS NOT NULL",
        session,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// The founding-team offer and how many of its places are left, for the
/// homepage (no session). `left` is 0 while the offer is off.
#[utoipa::path(
    get,
    path = "/billing/offers/founding",
    operation_id = "get_founding_offer",
    tag = "billing",
    responses(
        (status = 200, body = FoundingOffer),
        (status = 404, description = "Billing is off on this server", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`: the count did not read in time", body = ErrorBody),
    )
)]
pub async fn get_founding_offer(State(state): State<AppState>) -> ApiResult<Json<FoundingOffer>> {
    let inner = state.billing.require()?;
    match left(inner).await {
        Ok(left) => Ok(Json(FoundingOffer::with(left.unwrap_or(0), false))),
        Err(Unread) => Err(ApiError::Down {
            code: "billing_unavailable",
            message: "Billing is not answering right now. Try again in a minute.".into(),
        }),
    }
}
