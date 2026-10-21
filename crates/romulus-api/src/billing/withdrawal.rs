//! A consumer's withdrawal from a paid plan (Terms 9.2 to 9.4; Articles
//! L221-18, L221-21, L221-24 and L221-25 of the French Consumer Code).
//!
//! - **Who**: an admin of an organization whose subscription was bought as
//!   a consumer (its checkout record, `org_billing.acceptance_id`), within
//!   [`deadline`]: the 14th day after the day it began, to midnight (UTC,
//!   which is later than Paris), pushed past a weekend. A business never
//!   sees the button and is refused (403 `not_a_consumer`).
//! - **Recorded first** (`withdrawals`, audited `contract_withdrawn`): from
//!   that moment the organization is on Free (`org_billing.withdrawn_at`,
//!   whatever Stripe still says), and the acknowledgement goes out with the
//!   time it was received (a durable medium).
//! - **Then Stripe**: the subscription cancelled at once (no proration, no
//!   final invoice) and each paid invoice refunded for the days not used
//!   ([`refund_of`]: the price × the days left / the days of its period;
//!   whole days elapsed are charged, a day begun is not), with idempotency
//!   keys by invoice. When Stripe fails midway, the notices task finishes
//!   it ([`complete_pending`]), well within the 14 days the law allows.
//!
//! Nothing is deleted: the organization keeps everything, on Free's caps.

use axum::extract::State;
use chrono::{DateTime, Datelike, Days, Utc, Weekday};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    lock_org,
    notices::{self, Draft, Recipient},
    state::Stored,
    stored,
    stripe::{PaidInvoice, PlannedRefund, StripeError},
};
use crate::{
    ApiError, ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    auth::{Member, min},
    domain::{AuditAction, Buyer, Interval, Locale, Plan},
    error::ErrorBody,
    extract::Json,
    legal::{LegalLinks, words},
};

/// The days a consumer has to withdraw (Article L221-18).
pub const WITHDRAWAL_DAYS: u64 = 14;

/// When the withdrawal period of a contract concluded at `started` ends:
/// the day of conclusion does not count, the period ends at the end of its
/// 14th day, and one ending on a Saturday or a Sunday runs to the end of
/// the Monday (Article L221-19, Articles 641 and 642 of the Code of Civil
/// Procedure). Midnight UTC: an hour or two after midnight in Paris.
pub fn deadline(started: DateTime<Utc>) -> DateTime<Utc> {
    let mut last = started.date_naive() + Days::new(WITHDRAWAL_DAYS);
    last = match last.weekday() {
        Weekday::Sat => last + Days::new(2),
        Weekday::Sun => last + Days::new(1),
        _ => last,
    };
    (last + Days::new(1))
        .and_hms_opt(0, 0, 0)
        .map_or(started, |midnight| midnight.and_utc())
}

/// Whole days between `from` and `to` (never negative).
fn days_between(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    (to - from).num_days().max(0)
}

/// What a withdrawal at `now` gives back of `invoice` (Terms 9.3): the
/// amount paid for the days of its period not used. Whole days elapsed are
/// charged, rounded down in the consumer's favour; a day begun is not.
pub fn refund_of(invoice: &PaidInvoice, now: DateTime<Utc>) -> i64 {
    let seconds = (invoice.period_end - invoice.period_start).num_seconds();
    let days = ((seconds + 43_200) / 86_400).max(1);
    let used = days_between(invoice.period_start, now).min(days);
    invoice.amount_paid - invoice.amount_paid * used / days
}

/// A withdrawal, as the billing page shows it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Withdrawn {
    /// When romulus received it.
    pub requested_at: DateTime<Utc>,
    pub paid_cents: i64,
    /// What is refunded: the days not used.
    pub refund_cents: i64,
    pub currency: String,
    /// Stripe cancelled the subscription and made the refund.
    pub completed: bool,
}

/// The withdrawal right of the subscription, for a consumer's: until when
/// it can be used, or the withdrawal made. `null` for a business.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WithdrawalInfo {
    /// The end of the withdrawal period while it runs.
    pub until: Option<DateTime<Utc>>,
    pub withdrawn: Option<Withdrawn>,
}

/// As whom the stored subscription was bought.
pub async fn buyer_of(db: &sqlx::PgPool, stored: &Stored) -> Result<Option<Buyer>, sqlx::Error> {
    let Some(acceptance) = stored.acceptance_id else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT buyer AS "buyer: Buyer" FROM legal_acceptances WHERE id = $1"#,
        acceptance
    )
    .fetch_optional(db)
    .await?
    .flatten())
}

async fn withdrawn(
    db: &sqlx::PgPool,
    subscription: &str,
) -> Result<Option<Withdrawn>, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT requested_at, paid_cents, refund_cents, currency, completed_at
         FROM withdrawals WHERE subscription_id = $1",
        subscription
    )
    .fetch_optional(db)
    .await?
    .map(|r| Withdrawn {
        requested_at: r.requested_at,
        paid_cents: r.paid_cents,
        refund_cents: r.refund_cents,
        currency: r.currency,
        completed: r.completed_at.is_some(),
    }))
}

/// The billing page's withdrawal info: only for a consumer's subscription.
pub async fn info(
    state: &AppState,
    stored: &Stored,
    now: DateTime<Utc>,
) -> ApiResult<Option<WithdrawalInfo>> {
    let Some(subscription) = stored.subscription_id.as_deref() else {
        return Ok(None);
    };
    if buyer_of(&state.db, stored).await? != Some(Buyer::Consumer) {
        return Ok(None);
    }
    if stored.withdrawn_at.is_some() {
        return Ok(Some(WithdrawalInfo {
            until: None,
            withdrawn: withdrawn(&state.db, subscription).await?,
        }));
    }
    let until = stored
        .started_at
        .map(deadline)
        .filter(|until| stored.has_live_subscription() && now < *until);
    Ok(until.map(|until| WithdrawalInfo {
        until: Some(until),
        withdrawn: None,
    }))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct Withdraw {
    /// The language of the page, which the acknowledgement is written in.
    #[serde(default)]
    pub locale: Option<Locale>,
}

/// The subscription a withdrawal is from, checked.
struct Eligible {
    subscription: String,
    plan: Plan,
    interval: Interval,
    started: DateTime<Utc>,
    acceptance: Uuid,
}

async fn eligible(state: &AppState, stored: &Stored, now: DateTime<Utc>) -> ApiResult<Eligible> {
    let no_subscription = || ApiError::Conflict {
        code: "no_subscription",
        message: "This organization has no running subscription to withdraw from.".into(),
    };
    if stored.withdrawn_at.is_some() {
        return Err(ApiError::Conflict {
            code: "already_withdrawn",
            message: "You already withdrew from this subscription.".into(),
        });
    }
    if !stored.has_live_subscription() {
        return Err(no_subscription());
    }
    let (Some(subscription), Some(interval), Some(acceptance)) = (
        stored.subscription_id.clone(),
        stored.interval,
        stored.acceptance_id,
    ) else {
        return Err(not_a_consumer());
    };
    if buyer_of(&state.db, stored).await? != Some(Buyer::Consumer) {
        return Err(not_a_consumer());
    }
    let started = stored.started_at.ok_or_else(no_subscription)?;
    if now >= deadline(started) {
        return Err(ApiError::Conflict {
            code: "withdrawal_period_over",
            message: "The 14 days to withdraw are over. You can still cancel: the plan then ends with the period paid for.".into(),
        });
    }
    Ok(Eligible {
        subscription,
        plan: stored.plan,
        interval,
        started,
        acceptance,
    })
}

fn not_a_consumer() -> ApiError {
    ApiError::Denied {
        code: "not_a_consumer",
        message: "Withdrawal is a consumer's right, and this plan was bought as a business. Cancel it instead: it then ends with the period paid for.".into(),
    }
}

fn unavailable(err: &StripeError) -> ApiError {
    tracing::warn!(error = %err, "billing: Stripe call failed (withdrawal)");
    ApiError::Down {
        code: "billing_unavailable",
        message: "Billing is not answering right now. Try again in a minute, or write to us: your withdrawal counts from the moment you ask.".into(),
    }
}

/// Withdraws from the organization's plan (a consumer, within 14 days of
/// subscribing): recorded and acknowledged by email at once, the
/// organization on Free, the subscription cancelled and the days not used
/// refunded through Stripe. Admins only, in person (not with an access
/// token).
#[utoipa::path(
    post,
    path = "/orgs/{org}/billing/withdrawal",
    operation_id = "withdraw_from_plan",
    tag = "billing",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = Withdraw,
    responses(
        (status = 200, body = Withdrawn),
        (status = 403, description = "`not_a_consumer`: a business's plan is cancelled instead; `not_for_agents`", body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`no_subscription`, `already_withdrawn`, `withdrawal_period_over`", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`: Stripe could not be read; nothing was recorded", body = ErrorBody),
    )
)]
#[allow(
    clippy::too_many_lines,
    reason = "checked, read, recorded, acknowledged, done: in order"
)]
pub(super) async fn withdraw(
    member: Member<min::Admin>,
    State(state): State<AppState>,
    Json(body): Json<Withdraw>,
) -> ApiResult<Json<Withdrawn>> {
    let inner = state.billing.require()?;
    if member.user.via.is_some() {
        return Err(ApiError::Denied {
            code: "not_for_agents",
            message: "Only the person, signed in, can withdraw; an access token cannot.".into(),
        });
    }
    let org = member.org.id;
    let now = Utc::now();
    let seen = stored(&state.db, org).await?;
    let found = eligible(&state, &seen, now).await?;
    // What was paid and what is due back, read before anything is
    // recorded: a Stripe failure here changes nothing.
    let invoices = inner
        .stripe
        .paid_invoices(found.subscription.clone())
        .await
        .map_err(|err| unavailable(&err))?;
    let paid: i64 = invoices.iter().map(|i| i.amount_paid).sum();
    let currency = invoices
        .first()
        .map_or_else(|| "eur".to_owned(), |i| i.currency.clone());
    let refunds: Vec<PlannedRefund> = invoices
        .iter()
        .filter_map(|invoice| {
            Some(PlannedRefund {
                invoice: invoice.id.clone(),
                payment_intent: invoice.payment_intent.clone()?,
                amount: refund_of(invoice, now),
            })
        })
        .collect();
    let refund: i64 = refunds.iter().map(|r| r.amount).sum();
    let locale = match body.locale {
        Some(locale) => locale,
        None => crate::legal::locale_of(&state.db, member.user.id).await?,
    };

    let mut tx = state.db.begin().await?;
    lock_org(&mut tx, org).await?;
    let now_stored = stored(&mut *tx, org).await?;
    if now_stored.subscription_id.as_deref() != Some(found.subscription.as_str()) {
        return Err(ApiError::Conflict {
            code: "concurrent_change",
            message: "This organization's subscription just changed: look again.".into(),
        });
    }
    eligible(&state, &now_stored, now).await?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO withdrawals (org_id, org_name, user_id, user_email, user_name, locale,
                                    acceptance_id, subscription_id, plan, interval, subscribed_at,
                                    paid_cents, refund_cents, currency, refunds)
           VALUES ($1, $2, $3, $4, $5, $6, (SELECT id FROM legal_acceptances WHERE id = $7),
                   $8, $9, $10, $11, $12, $13, $14, $15)
           ON CONFLICT (subscription_id) DO NOTHING
           RETURNING id"#,
        org,
        member.org.name,
        member.user.id,
        member.user.email,
        member.user.name,
        locale as Locale,
        found.acceptance,
        found.subscription,
        found.plan as Plan,
        found.interval as Interval,
        found.started,
        paid,
        refund.min(paid),
        currency,
        serde_json::to_value(&refunds).unwrap_or_default(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Conflict {
        code: "already_withdrawn",
        message: "You already withdrew from this subscription.".into(),
    })?;
    let requested_at = sqlx::query_scalar!(
        "UPDATE org_billing SET withdrawn_at = now(), updated_at = clock_timestamp()
         WHERE org_id = $1 RETURNING withdrawn_at AS \"at!\"",
        org
    )
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: org,
            database_id: None,
            action: AuditAction::ContractWithdrawn,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: None,
            reason: None,
            details: AuditDetails {
                billing: Some(BillingAudit {
                    plan: Some(found.plan),
                    interval: Some(found.interval),
                    from_plan: Some(found.plan),
                    subscription: Some(found.subscription.clone()),
                    amount_cents: Some(refund),
                    currency: Some(currency.clone()),
                    ..BillingAudit::default()
                }),
                ..AuditDetails::default()
            },
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(org = %member.org.slug, plan = ?found.plan, "billing: a consumer withdrew");

    let ack = Acknowledged {
        org: member.org.name.clone(),
        slug: member.org.slug.clone(),
        name: member.user.name.clone(),
        email: member.user.email.clone(),
        plan: found.plan,
        started: found.started,
        requested_at,
        paid,
        refund,
        currency: currency.clone(),
    };
    acknowledge(&state, &inner.legal, locale, &ack);
    let completed = complete(&state, id, &found.subscription, refunds).await;
    Ok(Json(Withdrawn {
        requested_at,
        paid_cents: paid,
        refund_cents: refund,
        currency,
        completed,
    }))
}

/// Cancels and refunds in Stripe, then marks the withdrawal completed;
/// false when Stripe failed (the notices task tries again).
async fn complete(
    state: &AppState,
    id: Uuid,
    subscription: &str,
    refunds: Vec<PlannedRefund>,
) -> bool {
    let Some(inner) = state.billing.inner() else {
        return false;
    };
    match inner
        .stripe
        .withdraw(subscription.to_owned(), id, refunds)
        .await
    {
        Ok(()) => {
            let marked = sqlx::query!(
                "UPDATE withdrawals SET completed_at = now() WHERE id = $1 AND completed_at IS NULL",
                id
            )
            .execute(&state.db)
            .await;
            if let Err(err) = marked {
                tracing::warn!(error = %err, "billing: a withdrawal done in Stripe, not marked yet");
                return false;
            }
            true
        }
        Err(err) => {
            tracing::warn!(error = %err, withdrawal = %id, "billing: a withdrawal waits for Stripe (retried by the notices task)");
            false
        }
    }
}

/// Withdrawals Stripe had not finished, a few minutes old at least (the
/// request itself may still be on it): tried again.
pub async fn complete_pending(state: &AppState) -> ApiResult<()> {
    let pending = sqlx::query!(
        "SELECT id, subscription_id, refunds FROM withdrawals
         WHERE completed_at IS NULL AND requested_at < now() - interval '2 minutes'
         ORDER BY requested_at LIMIT 20"
    )
    .fetch_all(&state.db)
    .await?;
    for w in pending {
        let refunds: Vec<PlannedRefund> = serde_json::from_value(w.refunds).unwrap_or_default();
        complete(state, w.id, &w.subscription_id, refunds).await;
    }
    Ok(())
}

/// What the acknowledgement says.
struct Acknowledged {
    org: String,
    slug: String,
    name: String,
    email: String,
    plan: Plan,
    started: DateTime<Utc>,
    requested_at: DateTime<Utc>,
    paid: i64,
    refund: i64,
    currency: String,
}

/// The acknowledgement of receipt (Article L221-21), timestamped, to whoever
/// withdrew, in the page's language.
fn acknowledge(
    state: &AppState,
    legal: &crate::config::LegalConfig,
    locale: Locale,
    ack: &Acknowledged,
) {
    let contact = LegalLinks::of(legal).contact;
    let money = |cents| words::money(locale, cents, &ack.currency);
    let days = days_between(ack.started, ack.requested_at);
    let kept = ack.paid - ack.refund;
    let plan = ack.plan.name();
    let (subject, paragraphs, footer) = match locale {
        Locale::En => (
            format!("We received your withdrawal from {}'s {plan} plan", ack.org),
            vec![
                format!("We acknowledge receipt of your withdrawal from the contract for the romulus {plan} plan of {}.", ack.org),
                format!(
                    "Received on {}. Ordered on {}. Withdrawn by {} ({}).",
                    words::timestamp(locale, ack.requested_at),
                    words::timestamp(locale, ack.started),
                    ack.name,
                    ack.email,
                ),
                format!("The subscription is cancelled at once, and {} is on the Free plan. Nothing is deleted.", ack.org),
                if kept > 0 {
                    format!(
                        "You paid {}. For the {days} day{} of service until your withdrawal you pay {}, and we refund {} to the payment method you used, within 14 days (usually within a few days).",
                        money(ack.paid),
                        if days == 1 { "" } else { "s" },
                        money(kept),
                        money(ack.refund),
                    )
                } else {
                    format!(
                        "You paid {}: we refund all of it to the payment method you used, within 14 days (usually within a few days).",
                        money(ack.paid)
                    )
                },
                "Keep this email: it is the acknowledgement of your withdrawal on a durable medium.".to_owned(),
            ],
            format!("You get this because you withdrew from a romulus plan. Questions: {contact}."),
        ),
        Locale::Fr => (
            format!("Nous avons bien reçu votre rétractation de l'offre {plan} de {}", ack.org),
            vec![
                format!("Nous accusons réception de votre rétractation du contrat de l'offre romulus {plan} de {}.", ack.org),
                format!(
                    "Reçue le {}. Commande du {}. Rétractation faite par {} ({}).",
                    words::timestamp(locale, ack.requested_at),
                    words::timestamp(locale, ack.started),
                    ack.name,
                    ack.email,
                ),
                format!("L'abonnement est résilié immédiatement, et {} est sur l'offre gratuite. Rien n'est supprimé.", ack.org),
                if kept > 0 {
                    format!(
                        "Vous avez payé {}. Pour {days} jour{} de service jusqu'à votre rétractation, vous payez {} ; nous vous remboursons {} sur le moyen de paiement utilisé, sous 14 jours (en général sous quelques jours).",
                        money(ack.paid),
                        if days > 1 { "s" } else { "" },
                        money(kept),
                        money(ack.refund),
                    )
                } else {
                    format!(
                        "Vous avez payé {} : nous vous remboursons l'intégralité sur le moyen de paiement utilisé, sous 14 jours (en général sous quelques jours).",
                        money(ack.paid)
                    )
                },
                "Conservez ce message : c'est l'accusé de réception de votre rétractation sur un support durable.".to_owned(),
            ],
            format!("Vous recevez ce message parce que vous vous êtes rétracté d'une offre romulus. Questions : {contact}."),
        ),
    };
    let label = match locale {
        Locale::En => "Billing settings",
        Locale::Fr => "Paramètres de facturation",
    };
    notices::send(
        state,
        "billing_withdrawal_acknowledged",
        &Recipient {
            email: ack.email.clone(),
            locale,
        },
        Draft {
            subject,
            paragraphs,
            action: Some((label, notices::billing_link(state, &ack.slug, ""))),
            footer,
            framed: None,
            attachments: Vec::new(),
        },
    );
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn invoice(paid: i64, start: DateTime<Utc>, end: DateTime<Utc>) -> PaidInvoice {
        PaidInvoice {
            id: "in_1".into(),
            payment_intent: Some("pi_1".into()),
            amount_paid: paid,
            currency: "eur".into(),
            period_start: start,
            period_end: end,
        }
    }

    #[test]
    fn a_withdrawal_refunds_the_days_not_used_and_a_day_begun_is_free() {
        let start = Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap();
        let month = invoice(
            1_900,
            start,
            Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap(),
        );
        // The same day: everything back.
        assert_eq!(refund_of(&month, start + chrono::Duration::hours(5)), 1_900);
        // Three whole days used of 31: 19 × 3 / 31 = 1.83 kept (rounded down).
        assert_eq!(
            refund_of(&month, start + chrono::Duration::days(3)),
            1_900 - 183
        );
        // A year: 13 days of 365.
        let year = invoice(19_000, start, start + chrono::Duration::days(365));
        assert_eq!(
            refund_of(&year, start + chrono::Duration::days(13)),
            19_000 - 676
        );
        // Never more than paid, never negative.
        assert_eq!(refund_of(&month, start - chrono::Duration::days(1)), 1_900);
        assert_eq!(refund_of(&month, start + chrono::Duration::days(60)), 0);
    }

    #[test]
    fn the_period_ends_with_the_fourteenth_day_after_and_skips_a_weekend() {
        // Wednesday 21 October 2026: the 14th day after is Wednesday 4
        // November, open until its end.
        let wednesday = Utc.with_ymd_and_hms(2026, 10, 21, 18, 30, 0).unwrap();
        assert_eq!(
            deadline(wednesday),
            Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap()
        );
        // Saturday 24 October: the 14th day after is Saturday 7 November,
        // pushed to Monday the 9th.
        let saturday = Utc.with_ymd_and_hms(2026, 10, 24, 9, 0, 0).unwrap();
        assert_eq!(
            deadline(saturday),
            Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap()
        );
        let sunday = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
        assert_eq!(
            deadline(sunday),
            Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap()
        );
    }
}
