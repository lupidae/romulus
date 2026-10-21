//! A consumer's withdrawal from a paid plan (Terms 9.2 to 9.4; Articles
//! L221-18, L221-21, L221-24 and L221-25 of the French Consumer Code).
//!
//! - **Who**: an admin of an organization whose subscription was bought as
//!   a consumer (its checkout record, `org_billing.acceptance_id`), until
//!   [`deadline`]: the end of the 14th day after the day it began, in Paris,
//!   pushed past a weekend or a public holiday ([`last_day`]). A business
//!   never sees the button and is refused (403 `not_a_consumer`).
//! - **Recorded first** (`withdrawals`, audited `contract_withdrawn`): from
//!   that moment the organization is on Free (`org_billing.withdrawn_at`,
//!   whatever Stripe still says), its contract ended (the checkout record's
//!   `contract_ended_at`), and the acknowledgement is queued with it, with
//!   the time it was received (a durable medium): to whoever withdrew, and
//!   to the buyer when another admin did.
//! - **Then Stripe**: the subscription cancelled at once (no proration, no
//!   final invoice) and each paid invoice refunded for the days not used
//!   ([`refund_of`]: the price × the days left / the days of its period;
//!   whole days elapsed are charged, a day begun is not). Each refund is made
//!   once: one already made for this withdrawal is skipped (its
//!   `metadata[withdrawal]`), and none gives back more than was paid less
//!   what was refunded already. When Stripe fails midway, the notices task
//!   finishes it ([`complete_pending`]), well within the 14 days the law
//!   allows; after [`MAX_ATTEMPTS`] or [`GIVE_UP_AFTER`] it stops and logs an
//!   error for the operator (`withdrawals.failed_at`).
//! - **A paid invoice without a payment** (paid from the customer's
//!   balance, or out of band) cannot be refunded here: the withdrawal is
//!   refused (503 `withdrawal_by_email`, logged as an error), so no
//!   acknowledgement ever states an amount romulus cannot refund. The
//!   consumer withdraws by email, which counts as well.
//!
//! Nothing is deleted: the organization keeps everything, on Free's caps.

use axum::extract::State;
use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Europe::Paris;
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

/// Stripe failures a withdrawal survives before romulus stops asking.
pub const MAX_ATTEMPTS: i32 = 8;
/// How long romulus keeps asking Stripe, at most.
pub const GIVE_UP_AFTER: chrono::TimeDelta = chrono::TimeDelta::days(7);

/// Easter Sunday of `year` (the Gregorian computus).
#[allow(
    clippy::many_single_char_names,
    reason = "the computus's own letters, as it is always written"
)]
fn easter(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month.unsigned_abs(), day.unsigned_abs())
        .unwrap_or(NaiveDate::MIN)
}

/// A public holiday in France (Article L3133-1 of the Labour Code): New
/// Year's Day, Easter Monday, 1 and 8 May, Ascension Day, Whit Monday, 14
/// July, 15 August, 1 and 11 November, Christmas Day.
pub fn is_public_holiday(day: NaiveDate) -> bool {
    let fixed = [
        (1, 1),
        (5, 1),
        (5, 8),
        (7, 14),
        (8, 15),
        (11, 1),
        (11, 11),
        (12, 25),
    ];
    if fixed.contains(&(day.month(), day.day())) {
        return true;
    }
    let easter = easter(day.year());
    [1, 39, 50]
        .iter()
        .any(|after| easter + Days::new(*after) == day)
}

/// The last day to withdraw from a contract concluded at `started`: the day
/// of conclusion (in Paris) does not count, the 14th day after it is the
/// last, and one that is a Saturday, a Sunday or a public holiday gives way
/// to the next working day (Article L221-19 of the Consumer Code, Articles
/// 641 and 642 of the Code of Civil Procedure, Regulation 1182/71).
pub fn last_day(started: DateTime<Utc>) -> NaiveDate {
    let mut last = started.with_timezone(&Paris).date_naive() + Days::new(WITHDRAWAL_DAYS);
    while matches!(last.weekday(), Weekday::Sat | Weekday::Sun) || is_public_holiday(last) {
        last = last + Days::new(1);
    }
    last
}

/// When the withdrawal period of a contract concluded at `started` ends:
/// midnight in Paris at the end of its [`last_day`].
pub fn deadline(started: DateTime<Utc>) -> DateTime<Utc> {
    let end = (last_day(started) + Days::new(1)).and_time(chrono::NaiveTime::MIN);
    // Paris never changes its clocks at midnight: the hour always exists.
    Paris
        .from_local_datetime(&end)
        .earliest()
        .map_or(started, |midnight| midnight.with_timezone(&Utc))
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
    /// The end of the withdrawal period while it runs (midnight in Paris
    /// after `last_day`).
    pub until: Option<DateTime<Utc>>,
    /// The last day it can be used, in Paris.
    pub last_day: Option<NaiveDate>,
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
            last_day: None,
            withdrawn: withdrawn(&state.db, subscription).await?,
        }));
    }
    let Some(started) = stored.started_at else {
        return Ok(None);
    };
    let until = deadline(started);
    Ok(
        (stored.has_live_subscription() && now < until).then(|| WithdrawalInfo {
            until: Some(until),
            last_day: Some(last_day(started)),
            withdrawn: None,
        }),
    )
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
        (status = 503, description = "`billing_unavailable`: Stripe could not be read; `withdrawal_by_email`: a payment romulus cannot refund itself (withdraw by email). Nothing was recorded", body = ErrorBody),
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
    // A payment Stripe cannot refund (no payment intent: the customer's
    // balance, out of band): no amount romulus states could be honoured.
    if let Some(unrefundable) = invoices
        .iter()
        .find(|i| i.payment_intent.is_none() && i.amount_paid > 0)
    {
        tracing::error!(
            org = %member.org.slug,
            invoice = %unrefundable.id,
            "billing: a consumer's withdrawal was refused: a paid invoice has no payment to refund; handle it by hand"
        );
        return Err(ApiError::Down {
            code: "withdrawal_by_email",
            message: format!(
                "A payment of this subscription cannot be refunded from here. Withdraw by email to {}: your withdrawal counts from the moment you write.",
                inner.legal.contact
            ),
        });
    }
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
                paid: invoice.amount_paid,
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
    // The contract it concluded ends now (the record is kept from then).
    sqlx::query!(
        "UPDATE legal_acceptances SET contract_ended_at = now()
         WHERE id = $1 AND contract_ended_at IS NULL",
        found.acceptance
    )
    .execute(&mut *tx)
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
    crate::operator::billing::withdrawal(
        &mut tx,
        &state,
        &crate::operator::billing::Withdrawal {
            org,
            name: &member.user.name,
            email: &member.user.email,
            plan: found.plan,
            interval: found.interval,
            started: found.started,
            requested_at,
            paid,
            refund: refund.min(paid),
            currency: &currency,
            customer: now_stored.customer_id.as_deref(),
            subscription: &found.subscription,
        },
    )
    .await?;
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
    // To whoever withdrew, and to the buyer when another admin did.
    let mut readers = vec![Recipient {
        email: member.user.email.clone(),
        locale,
    }];
    if let Some(buyer) = buyer(&mut tx, found.acceptance).await?
        && crate::email::key(&buyer.email) != crate::email::key(&member.user.email)
    {
        readers.push(buyer);
    }
    for reader in &readers {
        notices::queue(
            &mut tx,
            &format!("withdrawal:{id}"),
            "billing_withdrawal_acknowledged",
            reader,
            acknowledgement(&state, &inner.legal, reader.locale, &ack),
        )
        .await?;
    }
    tx.commit().await?;
    state.mailer.wake();
    tracing::info!(org = %member.org.slug, plan = ?found.plan, "billing: a consumer withdrew");

    let completed = complete(&state, id, &found.subscription, refunds).await;
    Ok(Json(Withdrawn {
        requested_at,
        paid_cents: paid,
        refund_cents: refund,
        currency,
        completed,
    }))
}

/// The buyer of a checkout record, where they are now, in the record's
/// language.
async fn buyer(
    conn: &mut sqlx::PgConnection,
    acceptance: Uuid,
) -> Result<Option<Recipient>, sqlx::Error> {
    Ok(sqlx::query!(
        r#"SELECT coalesce(u.email, a.user_email) AS "email!", a.locale AS "locale: Locale"
           FROM legal_acceptances a LEFT JOIN users u ON u.id = a.user_id
           WHERE a.id = $1"#,
        acceptance
    )
    .fetch_optional(conn)
    .await?
    .map(|r| Recipient {
        email: r.email,
        locale: r.locale,
    }))
}

/// Cancels and refunds in Stripe, then marks the withdrawal completed;
/// false when Stripe failed (the notices task tries again, until it gives
/// up: `failed_at`, an error for the operator).
async fn complete(
    state: &AppState,
    id: Uuid,
    subscription: &str,
    refunds: Vec<PlannedRefund>,
) -> bool {
    let Some(inner) = state.billing.inner() else {
        return false;
    };
    let attempt = sqlx::query!(
        "UPDATE withdrawals SET attempts = attempts + 1 WHERE id = $1 RETURNING attempts, requested_at",
        id
    )
    .fetch_one(&state.db)
    .await;
    let attempt = match attempt {
        Ok(attempt) => attempt,
        Err(err) => {
            tracing::warn!(error = %err, withdrawal = %id, "billing: a withdrawal could not be counted");
            return false;
        }
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
            let give_up = attempt.attempts >= MAX_ATTEMPTS
                || Utc::now() - attempt.requested_at >= GIVE_UP_AFTER;
            let error: String = err.to_string().chars().take(2000).collect();
            let noted = sqlx::query!(
                "UPDATE withdrawals SET last_error = $2, failed_at = CASE WHEN $3 THEN now() END
                 WHERE id = $1",
                id,
                error,
                give_up,
            )
            .execute(&state.db)
            .await;
            if let Err(err) = noted {
                tracing::warn!(error = %err, withdrawal = %id, "billing: a withdrawal's failure could not be noted");
            }
            if give_up {
                tracing::error!(
                    error = %err,
                    withdrawal = %id,
                    subscription,
                    attempts = attempt.attempts,
                    "billing: a withdrawal could not be completed in Stripe and is no longer retried: cancel the subscription and make the refunds by hand, then set withdrawals.completed_at"
                );
            } else {
                tracing::warn!(error = %err, withdrawal = %id, attempts = attempt.attempts, "billing: a withdrawal waits for Stripe (retried by the notices task)");
            }
            false
        }
    }
}

/// Withdrawals Stripe had not finished, a few minutes old at least (the
/// request itself may still be on it): tried again.
pub async fn complete_pending(state: &AppState) -> ApiResult<()> {
    let pending = sqlx::query!(
        "SELECT id, subscription_id, refunds FROM withdrawals
         WHERE completed_at IS NULL AND failed_at IS NULL
           AND requested_at < now() - interval '2 minutes'
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

/// The acknowledgement of receipt (Article L221-21), timestamped, in
/// `locale`.
fn acknowledgement(
    state: &AppState,
    legal: &crate::config::LegalConfig,
    locale: Locale,
    ack: &Acknowledged,
) -> Draft {
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
                format!("L'abonnement est résilié immédiatement, et {} est sur l'offre gratuite (Free). Rien n'est supprimé.", ack.org),
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
    Draft {
        subject,
        paragraphs,
        action: Some((label, notices::billing_link(state, &ack.slug, ""))),
        footer,
        framed: None,
        attachments: Vec::new(),
    }
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

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn easter_and_the_holidays_that_follow_it_are_known() {
        assert_eq!(easter(2024), day(2024, 3, 31));
        assert_eq!(easter(2025), day(2025, 4, 20));
        assert_eq!(easter(2026), day(2026, 4, 5));
        assert_eq!(easter(2027), day(2027, 3, 28));
        for holiday in [
            day(2027, 1, 1),
            day(2027, 3, 29),
            day(2027, 5, 1),
            day(2027, 5, 6),
            day(2027, 5, 8),
            day(2027, 5, 17),
            day(2027, 7, 14),
            day(2027, 8, 15),
            day(2027, 11, 1),
            day(2027, 11, 11),
            day(2027, 12, 25),
        ] {
            assert!(is_public_holiday(holiday), "{holiday}");
        }
        assert!(
            !is_public_holiday(day(2027, 3, 28)),
            "Easter Sunday is a Sunday"
        );
        assert!(!is_public_holiday(day(2027, 12, 26)));
        assert!(
            !is_public_holiday(day(2027, 3, 26)),
            "Good Friday: Alsace-Moselle only"
        );
    }

    #[test]
    fn the_period_ends_with_the_fourteenth_day_after_in_paris() {
        // Wednesday 21 October 2026, 20:30 in Paris: the 14th day after is
        // Wednesday 4 November, open until midnight in Paris (23:00 UTC,
        // winter time by then).
        let wednesday = Utc.with_ymd_and_hms(2026, 10, 21, 18, 30, 0).unwrap();
        assert_eq!(last_day(wednesday), day(2026, 11, 4));
        assert_eq!(
            deadline(wednesday),
            Utc.with_ymd_and_hms(2026, 11, 4, 23, 0, 0).unwrap()
        );
        // 22:30 UTC is already the next day in Paris: concluded on the 22nd.
        let late = Utc.with_ymd_and_hms(2026, 10, 21, 22, 30, 0).unwrap();
        assert_eq!(last_day(late), day(2026, 11, 5));
        assert_eq!(
            deadline(late),
            Utc.with_ymd_and_hms(2026, 11, 5, 23, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_period_ending_on_a_weekend_or_a_holiday_runs_to_the_next_working_day() {
        let at = |m, d| Utc.with_ymd_and_hms(2026, m, d, 9, 0, 0).unwrap();
        // Saturday 7 and Sunday 8 November: Monday the 9th.
        assert_eq!(last_day(at(10, 24)), day(2026, 11, 9));
        assert_eq!(last_day(at(10, 25)), day(2026, 11, 9));
        // Wednesday 11 November (Armistice Day): Thursday the 12th.
        assert_eq!(last_day(at(10, 28)), day(2026, 11, 12));
        // Sunday 1 November, All Saints' Day too: Monday the 2nd.
        assert_eq!(last_day(at(10, 18)), day(2026, 11, 2));
        // Friday 25 December, then a weekend: Monday the 28th.
        assert_eq!(last_day(at(12, 11)), day(2026, 12, 28));
        // Easter Monday 29 March 2027: Tuesday the 30th, at midnight in
        // Paris on summer time (22:00 UTC).
        let march = Utc.with_ymd_and_hms(2027, 3, 15, 9, 0, 0).unwrap();
        assert_eq!(last_day(march), day(2027, 3, 30));
        assert_eq!(
            deadline(march),
            Utc.with_ymd_and_hms(2027, 3, 30, 22, 0, 0).unwrap()
        );
        // Ascension Day, Thursday 6 May 2027: Friday the 7th.
        let april = Utc.with_ymd_and_hms(2027, 4, 22, 9, 0, 0).unwrap();
        assert_eq!(last_day(april), day(2027, 5, 7));
    }
}
