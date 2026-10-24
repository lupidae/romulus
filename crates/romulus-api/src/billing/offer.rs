//! The founding-team offer (`plans::FOUNDING`, Terms 7.11): Team at €29 a
//! month or €290 a year for as long as the subscription stays on Team, for
//! the first 20 teams, once per organization. Stripe holds it: a coupon of
//! duration `forever` per interval (€20 off a month, €200 off a year, Team
//! only, metadata `romulus_offer: founding`) and a promotion code of each,
//! made by `romulus-api stripe-setup`, their ids configured
//! (`ROMULUS_FOUNDING_PROMO_MONTHLY`, `_YEARLY`); unset, the offer is off.
//!
//! - **Places left**: the 20 less both codes' `times_redeemed` (none once a
//!   code is off or at its cap) less the places held by open offered
//!   checkouts (`org_billing.checkout_offer_until`, the session's expiry,
//!   about half an hour). The homepage's count ([`left`]) reads Stripe at
//!   most every [`FRESH_FOR`], within [`READ_TIMEOUT`], a failure remembered
//!   for [`FAILED_FOR`]; the holds are counted live.
//! - **Checkout** ([`reserve`], in the checkout's transaction under the
//!   organization's lock, then the offer's own: one offered checkout decides
//!   at a time) applies the interval's code when the buyer came with
//!   `?offer=founding` and asks for Team (any other plan ignores it):
//!   refused for an organization that used it (409 `offer_used`) or when no
//!   place is left (409 `offer_ended`, Stripe read fresh), else a place held
//!   until the session expires ([`hold`]).
//! - **Webhooks** ([`correct`], [`note`], [`close_when_full`]): a founding
//!   subscription moving between monthly and yearly gets the other
//!   interval's coupon, one leaving Team loses it for good; the
//!   organization's use is recorded with its first such subscription, its
//!   hold released; both codes are switched off once 20 were redeemed.
//!
//! Endpoint (no session): `GET /billing/offers/founding`, the offer and how
//! many are left (none while the offer is off, so the homepage asking logs
//! no error); 404 while billing is off, as every billing route.

use std::time::{Duration, Instant};

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Inner,
    plans::FOUNDING,
    state::is_live,
    stripe::{StripeError, Subscription},
};
use crate::{
    ApiError, ApiResult, AppState,
    config::PromotionCodes,
    domain::{Interval, Plan},
    error::ErrorBody,
    extract::Json,
};

/// How long a count read from Stripe is shown before Stripe is asked again.
pub const FRESH_FOR: Duration = Duration::from_mins(5);
/// How long a failed read is remembered (the homepage hides the offer).
pub const FAILED_FOR: Duration = Duration::from_secs(30);
/// How long the homepage's count waits for Stripe.
pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// How long an offered checkout holds its place: its session's
/// `expires_at`, from when it was asked for (Stripe takes 30 minutes at
/// least, so the same request again a moment later still may).
pub const HOLD: chrono::Duration = chrono::Duration::minutes(32);
/// How long a pending offered checkout may be asked again as the same
/// request (its `expires_at` then still 30 minutes away at least).
pub const RETRY_WITHIN: chrono::Duration = chrono::Duration::minutes(2);

/// An offer a checkout may ask for (`StartCheckout::offer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferChoice {
    /// The founding-team offer: Team only.
    Founding,
}

/// The founding-team offer, as the homepage and the checkout show it.
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
}

impl FoundingOffer {
    fn with(left: u32) -> Self {
        let list = |interval| FOUNDING.plan.price_cents(interval).unwrap_or_default();
        Self {
            plan: FOUNDING.plan,
            monthly_cents: FOUNDING.price_cents(Interval::Month),
            yearly_cents: FOUNDING.price_cents(Interval::Year),
            list_monthly_cents: list(Interval::Month),
            list_yearly_cents: list(Interval::Year),
            teams: FOUNDING.teams,
            left,
        }
    }
}

/// The last count read from Stripe (places not redeemed, before holds),
/// shared by the requests of this process.
#[derive(Default)]
pub struct Counter(tokio::sync::Mutex<Option<(Instant, Option<u32>)>>);

/// The count could not be read.
#[derive(Debug, Clone, Copy)]
pub struct Unread;

/// The teams less both codes' redemptions; none once a code is off
/// (switched off in Stripe, or at its own cap).
async fn unredeemed(inner: &Inner, codes: &PromotionCodes) -> Result<u32, StripeError> {
    let mut redeemed = 0_i64;
    for interval in [Interval::Month, Interval::Year] {
        let code = inner
            .stripe
            .promotion_code(codes.of(interval).to_owned())
            .await?;
        let used_up = code
            .max_redemptions
            .is_some_and(|max| code.times_redeemed >= max);
        if !code.active || used_up {
            return Ok(0);
        }
        redeemed += code.times_redeemed.max(0);
    }
    Ok(u32::try_from((i64::from(FOUNDING.teams) - redeemed).max(0)).unwrap_or(0))
}

/// The places open offered checkouts hold, but `except`'s.
async fn held(db: impl sqlx::PgExecutor<'_>, except: Option<Uuid>) -> Result<u32, sqlx::Error> {
    let n = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM org_billing
           WHERE checkout_offer_until > now() AND org_id IS DISTINCT FROM $1"#,
        except,
    )
    .fetch_one(db)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Stripe's count, read in the last [`FRESH_FOR`] (one read at a time, the
/// others waiting for it, at most [`READ_TIMEOUT`]).
async fn cached(inner: &Inner, codes: &PromotionCodes) -> Option<u32> {
    let mut last = inner.founding.0.lock().await;
    if let Some((at, read)) = *last {
        let fresh_for = if read.is_some() {
            FRESH_FOR
        } else {
            FAILED_FOR
        };
        if at.elapsed() < fresh_for {
            return read;
        }
    }
    let now = match tokio::time::timeout(READ_TIMEOUT, unredeemed(inner, codes)).await {
        Ok(Ok(n)) => Some(n),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "billing: the founding offer's promotion codes did not read");
            None
        }
        Err(_) => {
            tracing::warn!("billing: the founding offer's promotion codes took too long to read");
            None
        }
    };
    *last = Some((Instant::now(), now));
    now
}

/// How many places are left: Stripe's count ([`cached`]) less the places
/// held now. `Ok(None)`: the offer is off.
async fn left(inner: &Inner) -> Result<Option<u32>, Unread> {
    let Some(codes) = &inner.config.founding else {
        return Ok(None);
    };
    let unredeemed = cached(inner, codes).await.ok_or(Unread)?;
    let held = held(&inner.db, None).await.map_err(|err| {
        tracing::error!(error = %err, "billing: the founding offer's holds did not read");
        Unread
    })?;
    Ok(Some(unredeemed.saturating_sub(held)))
}

/// Whether `org` had the founding-team offer already.
async fn used(db: impl sqlx::PgExecutor<'_>, org: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT founding_offer_used_at FROM org_billing WHERE org_id = $1",
        org
    )
    .fetch_optional(db)
    .await?
    .flatten()
    .is_some())
}

/// The founding-team offer for `org`'s billing page: `None` while it is
/// off, its count does not read, or the organization had it.
pub async fn shown(inner: &Inner, org: Uuid) -> ApiResult<Option<FoundingOffer>> {
    if used(&inner.db, org).await? {
        return Ok(None);
    }
    Ok(left(inner).await.ok().flatten().map(FoundingOffer::with))
}

/// The founding-team offer for a checkout of `plan` for `interval` asked for
/// with `offer`, in its transaction (`tx`, the organization locked): `None`
/// for another plan, no offer, or the offer off; 409 `offer_used` when the
/// organization had it, 409 `offer_ended` when no place is left (Stripe read
/// fresh, the others' holds counted under the offer's lock, kept until the
/// transaction ends). Else its promotion code: the caller holds the place
/// with the session ([`hold`]).
pub async fn reserve(
    inner: &Inner,
    tx: &mut sqlx::PgConnection,
    org: Uuid,
    offer: Option<OfferChoice>,
    plan: Plan,
    interval: Interval,
) -> ApiResult<Option<String>> {
    let (Some(OfferChoice::Founding), Some(codes)) = (offer, &inner.config.founding) else {
        return Ok(None);
    };
    if plan != FOUNDING.plan {
        return Ok(None);
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
    let unredeemed = unredeemed(inner, codes)
        .await
        .map_err(|err| super::api::unavailable(&err))?;
    *inner.founding.0.lock().await = Some((Instant::now(), Some(unredeemed)));
    if unredeemed.saturating_sub(held(&mut *tx, Some(org)).await?) == 0 {
        return Err(ApiError::Conflict {
            code: "offer_ended",
            message: format!(
                "The founding-team offer has just ended: {} is at its usual price now.",
                plan.name()
            ),
        });
    }
    Ok(Some(codes.of(interval).to_owned()))
}

/// When a checkout record `id` (a version 7 UUID) was made.
pub fn made_at(id: Uuid) -> Option<DateTime<Utc>> {
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    DateTime::from_timestamp(i64::try_from(secs).ok()?, nanos)
}

/// Holds (or, `None`, releases) `org`'s place until its offered session
/// expires.
pub async fn hold(
    tx: &mut sqlx::PgConnection,
    org: Uuid,
    until: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE org_billing SET checkout_offer_until = $2 WHERE org_id = $1",
        org,
        until,
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// The plan and interval `read` is on, from its price's lookup key.
fn plan_of(read: &Subscription) -> Option<(Plan, Interval)> {
    read.item
        .as_ref()?
        .lookup_key
        .as_deref()
        .and_then(Plan::from_lookup_key)
}

/// Puts a live founding subscription's discounts right in Stripe: moved
/// between monthly and yearly, the other interval's coupon (€200 off a
/// month, or €20 off a year, would be wrong); moved off Team (or to a price
/// romulus did not make), no founding coupon any more: the offer ends for
/// good. `true` when it changed something (read the subscription again).
pub async fn correct(inner: &Inner, read: &Subscription) -> ApiResult<bool> {
    if !is_live(&read.status) || read.offer_discounts(FOUNDING.slug).next().is_none() {
        return Ok(false);
    }
    let others = read
        .discounts
        .iter()
        .filter(|d| d.offer.as_deref() != Some(FOUNDING.slug))
        .map(|d| d.coupon.clone());
    let wanted: Vec<String> = match plan_of(read) {
        Some((plan, interval)) if plan == FOUNDING.plan => {
            let right = read
                .offer_discounts(FOUNDING.slug)
                .all(|d| d.amount_off == Some(i64::from(FOUNDING.amount_off(interval))));
            if right {
                return Ok(false);
            }
            let Some(codes) = &inner.config.founding else {
                tracing::error!(subscription = %read.id, "billing: a founding subscription changed interval, and the offer is not configured: put its coupon right in the Dashboard");
                return Ok(false);
            };
            let code = inner
                .stripe
                .promotion_code(codes.of(interval).to_owned())
                .await
                .map_err(|err| super::api::unavailable(&err))?;
            let Some(coupon) = code.coupon else {
                tracing::error!(subscription = %read.id, "billing: the founding offer's promotion code has no coupon");
                return Ok(false);
            };
            others.chain([coupon]).collect()
        }
        _ => others.collect(),
    };
    inner
        .stripe
        .set_discounts(read.id.clone(), wanted)
        .await
        .map_err(|err| super::api::unavailable(&err))?;
    tracing::info!(subscription = %read.id, "billing: the founding offer's coupon put right");
    Ok(true)
}

/// After storing `read` for `org`, in its transaction: a live subscription
/// releases the organization's hold, and one with the offer records its use
/// (once per organization). `true` when that use is new (a redemption:
/// [`close_when_full`] after the commit).
pub async fn note(
    tx: &mut sqlx::PgConnection,
    org: Uuid,
    read: &Subscription,
) -> Result<bool, sqlx::Error> {
    if !is_live(&read.status) {
        return Ok(false);
    }
    let founding = read.offer_discounts(FOUNDING.slug).next().is_some();
    let before = sqlx::query_scalar!(
        "SELECT founding_offer_used_at FROM org_billing WHERE org_id = $1",
        org
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    sqlx::query!(
        "UPDATE org_billing SET checkout_offer_until = NULL,
                founding_offer_used_at = CASE WHEN $2 THEN coalesce(founding_offer_used_at, now())
                                              ELSE founding_offer_used_at END
         WHERE org_id = $1",
        org,
        founding,
    )
    .execute(&mut *tx)
    .await?;
    Ok(founding && before.is_none())
}

/// After a redemption: the count read again, and both promotion codes
/// switched off once the teams redeemed them all (Stripe then refuses them
/// whatever a count says). Logged, never failing (the count says none left
/// anyway).
pub async fn close_when_full(inner: &Inner) {
    let Some(codes) = &inner.config.founding else {
        return;
    };
    let mut redeemed = 0_i64;
    let mut active = Vec::new();
    for interval in [Interval::Month, Interval::Year] {
        match inner
            .stripe
            .promotion_code(codes.of(interval).to_owned())
            .await
        {
            Ok(code) => {
                redeemed += code.times_redeemed;
                if code.active {
                    active.push(code.id);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "billing: the founding offer's promotion codes did not read");
                return;
            }
        }
    }
    let unredeemed = u32::try_from((i64::from(FOUNDING.teams) - redeemed).max(0)).unwrap_or(0);
    *inner.founding.0.lock().await = Some((Instant::now(), Some(unredeemed)));
    if unredeemed > 0 {
        return;
    }
    for id in active {
        match inner.stripe.deactivate_promotion_code(id.clone()).await {
            Ok(()) => {
                tracing::info!(code = %id, "billing: the founding offer is full, its promotion code switched off");
            }
            Err(err) => {
                tracing::error!(code = %id, error = %err, "billing: the founding offer is full but its promotion code was not switched off: switch it off in the Dashboard");
            }
        }
    }
}

/// The founding-team offer and how many of its places are left, for the
/// homepage (no session; Stripe's count is at most five minutes old, the
/// places held by open checkouts counted now). `left` is 0 while the offer
/// is off.
#[utoipa::path(
    get,
    path = "/billing/offers/founding",
    operation_id = "get_founding_offer",
    tag = "billing",
    responses(
        (status = 200, body = FoundingOffer),
        (status = 404, description = "Billing is off on this server", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`: Stripe did not answer", body = ErrorBody),
    )
)]
pub async fn get_founding_offer(State(state): State<AppState>) -> ApiResult<Json<FoundingOffer>> {
    let inner = state.billing.require()?;
    match left(inner).await {
        Ok(left) => Ok(Json(FoundingOffer::with(left.unwrap_or(0)))),
        Err(Unread) => Err(ApiError::Down {
            code: "billing_unavailable",
            message: "Billing is not answering right now. Try again in a minute.".into(),
        }),
    }
}
