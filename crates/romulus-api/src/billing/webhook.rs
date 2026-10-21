//! `POST /api/billing/webhook`: Stripe's events.
//!
//! - **Signed**: the raw body and `Stripe-Signature` are checked against the
//!   endpoint's secret (HMAC-SHA256 of `t.body`, five minutes' tolerance)
//!   before anything is parsed. The CSRF guard lets this one path through:
//!   the signature is its authentication (`csrf::WEBHOOK`).
//! - **Idempotent**: each event id is stored (`billing_events`) in the
//!   transaction that applies it; a delivery seen again changes nothing.
//! - **Order-proof**: an event only says *which* subscription changed. It is
//!   read from Stripe (to find its organization), then again under the
//!   organization's lock, and stored as it is now, so events arriving late or out of order cannot undo a newer
//!   state; one about an old subscription never overwrites the live one
//!   ([`super::state::should_apply`]).
//! - Handled: `checkout.session.completed`, `customer.subscription.created`,
//!   `.updated`, `.deleted` (and Stripe's other subscription events),
//!   `invoice.paid`, `invoice.payment_failed`. Every change is audited; a
//!   failed payment emails the organization's admins (once per invoice); a
//!   subscription starting confirms it to its buyer, a cancellation to the
//!   admins (`notices`, once each). A completed checkout notes the business
//!   Stripe's page named on its record (`legal_acceptances.stripe_business_name`);
//!   a subscription over for good ends the contract its record concluded
//!   (`contract_ended_at`).
//!
//! An event for an organization this server does not know (another
//! environment sharing the Stripe account) is acknowledged and ignored.

use std::fmt::Write as _;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    lock_org, notices,
    state::{self, Stored},
    stored,
    stripe::{Invoice, StripeError, Subscription, parse_invoice},
};
use crate::{
    ApiError, ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    domain::{AuditAction, Interval, Plan},
    error::ErrorBody,
    extract::{Json, read_body},
};

/// How old a signature's timestamp may be (Stripe's own default).
pub const TOLERANCE_SECONDS: i64 = 300;
const BODY_LIMIT: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("no Stripe-Signature header")]
    Missing,
    #[error("the Stripe-Signature header is malformed")]
    Malformed,
    #[error("the signature's timestamp is too old (a replay?)")]
    Stale,
    #[error("no signature matches")]
    Mismatch,
}

/// Checks `header` (`t=…,v1=…[,v1=…]`) against `body` and `secret` at `now`
/// (Unix seconds).
pub fn verify(
    secret: &str,
    header: Option<&str>,
    body: &[u8],
    now: i64,
) -> Result<(), SignatureError> {
    let header = header.ok_or(SignatureError::Missing)?;
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", hex)) => signatures.push(hex),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    if signatures.is_empty() {
        return Err(SignatureError::Malformed);
    }
    if (now - timestamp).abs() > TOLERANCE_SECONDS {
        return Err(SignatureError::Stale);
    }
    for signature in signatures {
        let Some(expected) = unhex(signature) else {
            continue;
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|_| SignatureError::Malformed)?;
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        if mac.verify_slice(&expected).is_ok() {
            return Ok(());
        }
    }
    Err(SignatureError::Mismatch)
}

/// The `Stripe-Signature` header for `body` at `timestamp` (tests, and the
/// fake's own deliveries).
pub fn sign(secret: &str, body: &[u8], timestamp: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let hex = mac
        .finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
    format!("t={timestamp},v1={hex}")
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[derive(Debug, Deserialize)]
struct Event {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    data: EventData,
}

#[derive(Debug, Deserialize)]
struct EventData {
    object: Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Received {
    pub received: bool,
}

/// What an event is about: the subscription to read again, and the invoice
/// it carries (invoice events).
fn subject(kind: &str, object: &Value) -> Option<(String, Option<Invoice>)> {
    let id = |value: &Value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value["id"].as_str().map(str::to_owned))
    };
    match kind {
        "checkout.session.completed" | "checkout.session.async_payment_succeeded" => {
            Some((id(&object["subscription"])?, None))
        }
        kind if kind.starts_with("customer.subscription.") => Some((id(&object["id"])?, None)),
        "invoice.paid" | "invoice.payment_succeeded" | "invoice.payment_failed" => {
            let invoice = parse_invoice(object).ok()?;
            Some((invoice.subscription.clone()?, Some(invoice)))
        }
        _ => None,
    }
}

/// Stripe's events (see the module docs). Answers 400 `bad_signature` to
/// anything not signed with the endpoint's secret.
#[utoipa::path(
    post,
    path = "/billing/webhook",
    operation_id = "billing_webhook",
    tag = "billing",
    request_body(content = String, content_type = "application/json", description = "A Stripe event, as Stripe signed it"),
    responses(
        (status = 200, body = Received),
        (status = 400, description = "`bad_signature`", body = ErrorBody),
        (status = 404, description = "Billing is off on this server", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`: Stripe could not be read; it retries", body = ErrorBody),
    )
)]
pub(super) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<Received>)> {
    let inner = state.billing.require()?;
    let secret = inner
        .config
        .stripe
        .webhook_secret()
        .ok_or_else(|| ApiError::not_found("No such endpoint."))?;
    let body = read_body(&headers, body, BODY_LIMIT).await?;
    let header = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok());
    if let Err(err) = verify(secret.expose(), header, &body, Utc::now().timestamp()) {
        tracing::warn!(error = %err, "billing: webhook refused");
        return Err(ApiError::Rejected {
            code: "bad_signature",
            message: format!("Not a signed Stripe event: {err}."),
        });
    }
    let event: Event = serde_json::from_slice(&body)
        .map_err(|err| ApiError::BadRequest(format!("Not a Stripe event: {err}")))?;
    if event.kind == "checkout.session.completed" {
        note_business(&state, &event.data.object).await?;
    }
    match subject(&event.kind, &event.data.object) {
        Some((subscription, invoice)) => {
            apply_subscription(&state, &event.id, &event.kind, &subscription, invoice).await?;
        }
        None => record_only(&state, &event.id, &event.kind).await?,
    }
    Ok((StatusCode::OK, Json(Received { received: true })))
}

/// The business a completed checkout's payment page named (Terms 3.5 c:
/// the Customer), kept with the checkout's record, once.
async fn note_business(state: &AppState, session: &Value) -> ApiResult<()> {
    let name = [
        &session["collected_information"]["business_name"],
        &session["customer_details"]["business_name"],
    ]
    .into_iter()
    .find_map(|v| v.as_str().map(str::trim).filter(|n| !n.is_empty()))
    .map(|n| n.chars().take(500).collect::<String>());
    let (Some(name), Some(id)) = (name, session["id"].as_str()) else {
        return Ok(());
    };
    sqlx::query!(
        "UPDATE legal_acceptances SET stripe_business_name = $2
         WHERE checkout_session_id = $1 AND kind = 'checkout' AND stripe_business_name IS NULL",
        id,
        name,
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

/// An event romulus does not act on: remembered, so a redelivery is cheap.
async fn record_only(state: &AppState, id: &str, kind: &str) -> ApiResult<()> {
    sqlx::query!(
        "INSERT INTO billing_events (id, type) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
        id,
        kind,
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

fn unavailable(err: &StripeError) -> ApiError {
    tracing::warn!(error = %err, "billing: cannot read a subscription for a webhook");
    ApiError::Down {
        code: "billing_unavailable",
        message: "Stripe could not be read; deliver the event again later.".into(),
    }
}

/// The organization `subscription` belongs to: its metadata, else its
/// customer's row. `None`: not this server's.
async fn org_of(state: &AppState, subscription: &Subscription) -> ApiResult<Option<Uuid>> {
    if let Some(org) = subscription.org_id {
        let known = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM organizations WHERE id = $1) AS "known!""#,
            org
        )
        .fetch_one(&state.db)
        .await?;
        if known {
            return Ok(Some(org));
        }
    }
    Ok(sqlx::query_scalar!(
        "SELECT org_id FROM org_billing WHERE customer_id = $1",
        subscription.customer
    )
    .fetch_optional(&state.db)
    .await?)
}

/// Most times a webhook reads Stripe again because another one wrote between
/// its read and its lock.
const ATTEMPTS: usize = 5;

/// A duplicate subscription cancelled (and refunded) while handling an event.
struct Cancelled {
    subscription: String,
    refund: Option<super::stripe::Refund>,
}

/// What an event needs from Stripe besides the subscription, read before the
/// lock: the unpaid invoice's due date, and whether a payment failure still
/// stands (a late `invoice.payment_failed` for an invoice paid since says
/// nothing any more).
struct Looked {
    due: Option<DateTime<Utc>>,
    failure_stands: bool,
}

async fn look(
    inner: &super::Inner,
    seen: &Stored,
    read: Option<&Subscription>,
    invoice: Option<&Invoice>,
    kind: &str,
) -> ApiResult<Looked> {
    let past_due = read.is_some_and(|r| matches!(r.status.as_str(), "past_due" | "unpaid"));
    let failure_stands = if kind == "invoice.payment_failed"
        && let Some(invoice) = invoice
    {
        if past_due {
            true
        } else {
            let again = inner
                .stripe
                .invoice(invoice.id.clone())
                .await
                .map_err(|err| unavailable(&err))?;
            matches!(again.status.as_deref(), Some("open" | "uncollectible"))
        }
    } else {
        false
    };
    let due = match (invoice, read) {
        (Some(invoice), _) => invoice.due,
        (None, Some(read)) if past_due && seen.past_due_since.is_none() => {
            match &read.latest_invoice {
                Some(latest) => inner
                    .stripe
                    .invoice(latest.clone())
                    .await
                    .ok()
                    .and_then(|i| i.due),
                None => None,
            }
        }
        _ => None,
    };
    Ok(Looked {
        due,
        failure_stands,
    })
}

/// The subscription as Stripe has it now; `None` once deleted for good
/// (test data cleared): ended, as far as romulus is concerned.
async fn read_subscription(inner: &super::Inner, id: &str) -> ApiResult<Option<Subscription>> {
    match inner.stripe.subscription(id.to_owned()).await {
        Ok(read) => Ok(Some(read)),
        Err(err) if err.is_missing() => Ok(None),
        Err(err) => Err(unavailable(&err)),
    }
}

/// Reads subscription `id` from Stripe and stores it as it is now, for event
/// `event_id` (stored with it: a second delivery does nothing). `invoice`:
/// the invoice an invoice event carried.
///
/// Stripe is read first, with no lock and no connection held; then, under
/// the organization's lock, the row must be as it was when read (else
/// another event wrote meanwhile: read again). A second live subscription
/// (two checkouts paid at once) is cancelled at once and refunded in full,
/// the stored one kept: it never took effect, so nothing of it was used.
#[allow(
    clippy::too_many_lines,
    reason = "read, then the lock, then write: in order"
)]
pub async fn apply_subscription(
    state: &AppState,
    event_id: &str,
    kind: &str,
    id: &str,
    invoice: Option<Invoice>,
) -> ApiResult<()> {
    let inner = state.billing.require()?;
    let handled = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM billing_events WHERE id = $1) AS "handled!""#,
        event_id
    )
    .fetch_one(&state.db)
    .await?;
    if handled {
        tracing::debug!(event = event_id, "billing: event already handled");
        return Ok(());
    }
    let mut cancelled: Option<Cancelled> = None;
    // The organization, when the subscription is known already: its row is
    // looked at *before* Stripe is read, so a write made while Stripe
    // answers is seen at the lock.
    let mut known = sqlx::query_scalar!(
        "SELECT org_id FROM org_billing WHERE subscription_id = $1",
        id
    )
    .fetch_optional(&state.db)
    .await?;
    for _ in 0..ATTEMPTS {
        let seen = match known {
            Some(org) => Some(stored(&state.db, org).await?),
            None => None,
        };
        let read = read_subscription(inner, id).await?;
        let org = match &read {
            Some(read) => org_of(state, read).await?,
            None => known,
        };
        let Some(org) = org else {
            tracing::info!(
                event = event_id,
                kind,
                "billing: an event for no organization of this server"
            );
            return record_only(state, event_id, kind).await;
        };
        // Its row was not looked at before this read: again, in order.
        let Some(seen) = seen.filter(|_| known == Some(org)) else {
            known = Some(org);
            continue;
        };
        if let Some(read) = &read
            && cancelled.is_none()
            && state::is_duplicate(&seen, &read.id, &read.status)
        {
            let refund = inner
                .stripe
                .cancel_duplicate(read.id.clone())
                .await
                .map_err(|err| unavailable(&err))?;
            tracing::warn!(org = %org, subscription = %read.id, "billing: a second subscription, cancelled and refunded");
            cancelled = Some(Cancelled {
                subscription: read.id.clone(),
                refund,
            });
            // Read again: cancelled now.
            continue;
        }
        let looked = look(inner, &seen, read.as_ref(), invoice.as_ref(), kind).await?;

        let mut tx = state.db.begin().await?;
        lock_org(&mut tx, org).await?;
        let fresh = sqlx::query_scalar!(
            "INSERT INTO billing_events (id, type, org_id) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO NOTHING RETURNING id",
            event_id,
            kind,
            org,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if fresh.is_none() {
            tx.commit().await?;
            return Ok(());
        }
        let before = stored(&mut *tx, org).await?;
        if before.updated_at != seen.updated_at {
            tx.rollback().await?;
            continue;
        }
        let event = Applied {
            org,
            event_id,
            kind,
        };
        if let Some(duplicate) = &cancelled {
            event.duplicate(&mut tx, &before, duplicate).await?;
        }
        let mut failure = None;
        let mut letters = Letters::default();
        if let Some(next) = next_state(&before, read.as_ref(), id, looked.due) {
            letters = Letters::of(&before, &next);
            event.store(&mut tx, &before, &next).await?;
            event
                .payments(&mut tx, &next, invoice.as_ref(), looked.failure_stands)
                .await?;
            failure = looked
                .failure_stands
                .then(|| invoice.as_ref().map(|i| (i.clone(), next.grace_until())))
                .flatten();
        } else {
            tracing::info!(
                event = event_id,
                subscription = id,
                "billing: not the organization's subscription, ignored"
            );
        }
        tx.commit().await?;
        if let Some(duplicate) = cancelled {
            notices::duplicate_cancelled(
                state,
                org,
                &duplicate.subscription,
                duplicate.refund.as_ref(),
            )
            .await?;
        }
        if let Some((invoice, grace)) = failure {
            notices::payment_failed(state, org, &invoice, grace).await?;
        }
        if let Some(started) = &letters.started {
            notices::subscription_confirmed(state, org, started).await?;
        }
        if let Some(cancelled) = &letters.cancelled {
            notices::cancellation_confirmed(state, org, cancelled).await?;
        }
        return Ok(());
    }
    Err(ApiError::Down {
        code: "billing_unavailable",
        message: "This organization's billing kept changing: deliver the event again later.".into(),
    })
}

/// The letters a stored change owes (each sent once, after the commit).
#[derive(Default)]
struct Letters {
    /// A subscription began paying: its confirmation.
    started: Option<Stored>,
    /// A running one is set to end at its period's end: the cancellation's.
    cancelled: Option<Stored>,
}

impl Letters {
    fn of(before: &Stored, next: &Stored) -> Self {
        let paying = |s: &Stored| {
            s.withdrawn_at.is_none() && matches!(s.status.as_deref(), Some("active" | "trialing"))
        };
        let same = before.subscription_id == next.subscription_id;
        Self {
            started: (paying(next) && !(same && paying(before))).then(|| next.clone()),
            cancelled: (next.cancel_at_period_end
                && next.is_live()
                && !(same && before.cancel_at_period_end))
                .then(|| next.clone()),
        }
    }
}

/// The row to store after reading subscription `id` (`None` from Stripe: it
/// is gone), or `None` when it is not the organization's to store.
fn next_state(
    before: &Stored,
    read: Option<&Subscription>,
    id: &str,
    due: Option<DateTime<Utc>>,
) -> Option<Stored> {
    match read {
        Some(read) => state::should_apply(before, &read.id, &read.status)
            .then(|| from_subscription(before, read, due, Utc::now())),
        None => (before.subscription_id.as_deref() == Some(id)).then(|| Stored {
            status: Some("canceled".into()),
            cancel_at_period_end: false,
            past_due_since: None,
            scheduled_plan: None,
            scheduled_interval: None,
            scheduled_at: None,
            ..before.clone()
        }),
    }
}

/// Writing what one event applies, in its transaction.
struct Applied<'a> {
    org: Uuid,
    event_id: &'a str,
    kind: &'a str,
}

impl Applied<'_> {
    fn entry(&self, action: AuditAction, billing: BillingAudit) -> audit::Entry {
        audit::Entry {
            org_id: self.org,
            database_id: None,
            action,
            actor_id: None,
            actor_name: None,
            version_number: None,
            reason: None,
            details: AuditDetails {
                billing: Some(BillingAudit {
                    stripe_event: Some(self.event_id.to_owned()),
                    event_type: Some(self.kind.to_owned()),
                    ..billing
                }),
                ..AuditDetails::default()
            },
        }
    }

    async fn duplicate(
        &self,
        tx: &mut sqlx::PgConnection,
        kept: &Stored,
        duplicate: &Cancelled,
    ) -> ApiResult<()> {
        audit::record(
            &mut *tx,
            &self.entry(
                AuditAction::SubscriptionDuplicateCancelled,
                BillingAudit {
                    plan: Some(kept.plan),
                    interval: kept.interval,
                    status: kept.status.clone(),
                    subscription: Some(duplicate.subscription.clone()),
                    amount_cents: duplicate.refund.as_ref().map(|r| r.amount),
                    currency: duplicate.refund.as_ref().map(|r| r.currency.clone()),
                    ..BillingAudit::default()
                },
            ),
        )
        .await?;
        Ok(())
    }

    /// Stores `next` and audits what changed from `before`.
    async fn store(
        &self,
        tx: &mut sqlx::PgConnection,
        before: &Stored,
        next: &Stored,
    ) -> ApiResult<()> {
        sqlx::query!(
            "INSERT INTO org_billing (org_id, customer_id, subscription_id, plan, interval, status,
                                      current_period_end, cancel_at_period_end, amount_cents, currency,
                                      past_due_since, scheduled_plan, scheduled_interval, scheduled_at,
                                      acceptance_id, started_at, withdrawn_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                     (SELECT id FROM legal_acceptances WHERE id = $15), $16, $17)
             ON CONFLICT (org_id) DO UPDATE SET
                 customer_id = coalesce(EXCLUDED.customer_id, org_billing.customer_id),
                 subscription_id = EXCLUDED.subscription_id, plan = EXCLUDED.plan,
                 interval = EXCLUDED.interval, status = EXCLUDED.status,
                 current_period_end = EXCLUDED.current_period_end,
                 cancel_at_period_end = EXCLUDED.cancel_at_period_end,
                 amount_cents = EXCLUDED.amount_cents, currency = EXCLUDED.currency,
                 past_due_since = EXCLUDED.past_due_since, scheduled_plan = EXCLUDED.scheduled_plan,
                 scheduled_interval = EXCLUDED.scheduled_interval, scheduled_at = EXCLUDED.scheduled_at,
                 acceptance_id = EXCLUDED.acceptance_id, started_at = EXCLUDED.started_at,
                 withdrawn_at = EXCLUDED.withdrawn_at,
                 updated_at = clock_timestamp()",
            self.org,
            next.customer_id,
            next.subscription_id,
            next.plan as Plan,
            next.interval as Option<Interval>,
            next.status,
            next.current_period_end,
            next.cancel_at_period_end,
            next.amount_cents,
            next.currency,
            next.past_due_since,
            next.scheduled_plan as Option<Plan>,
            next.scheduled_interval as Option<Interval>,
            next.scheduled_at,
            next.acceptance_id,
            next.started_at,
            next.withdrawn_at,
        )
        .execute(&mut *tx)
        .await?;
        // The contract a checkout concluded ends with its subscription (over
        // for good, or replaced by another): its record is kept from then.
        let mut over = Vec::new();
        if next
            .status
            .as_deref()
            .is_some_and(|s| super::state::ENDED_STATUSES.contains(&s))
        {
            over.extend(next.acceptance_id);
        }
        if before.acceptance_id != next.acceptance_id {
            over.extend(before.acceptance_id);
        }
        if !over.is_empty() {
            sqlx::query!(
                "UPDATE legal_acceptances SET contract_ended_at = now()
                 WHERE id = ANY($1) AND kind = 'checkout' AND contract_ended_at IS NULL",
                &over,
            )
            .execute(&mut *tx)
            .await?;
        }
        let changed = (
            before.plan,
            before.interval,
            &before.status,
            before.cancel_at_period_end,
            before.scheduled_plan,
        ) != (
            next.plan,
            next.interval,
            &next.status,
            next.cancel_at_period_end,
            next.scheduled_plan,
        ) || before.subscription_id != next.subscription_id;
        if changed {
            audit::record(
                &mut *tx,
                &self.entry(
                    AuditAction::SubscriptionChanged,
                    BillingAudit {
                        plan: Some(next.plan),
                        interval: next.interval,
                        status: next.status.clone(),
                        from_plan: Some(before.plan),
                        from_status: before.status.clone(),
                        cancel_at_period_end: Some(next.cancel_at_period_end),
                        ..BillingAudit::default()
                    },
                ),
            )
            .await?;
        }
        Ok(())
    }

    /// A payment that went through, or a failure that still stands.
    async fn payments(
        &self,
        tx: &mut sqlx::PgConnection,
        next: &Stored,
        invoice: Option<&Invoice>,
        failure_stands: bool,
    ) -> ApiResult<()> {
        let Some(invoice) = invoice else {
            return Ok(());
        };
        let action = match self.kind {
            "invoice.payment_failed" if failure_stands => AuditAction::PaymentFailed,
            "invoice.payment_failed" => return Ok(()),
            _ => AuditAction::PaymentSucceeded,
        };
        audit::record(
            &mut *tx,
            &self.entry(
                action,
                BillingAudit {
                    plan: Some(next.plan),
                    interval: next.interval,
                    status: next.status.clone(),
                    invoice: Some(invoice.id.clone()),
                    amount_cents: Some(invoice.total),
                    currency: Some(invoice.currency.clone()),
                    ..BillingAudit::default()
                },
            ),
        )
        .await?;
        Ok(())
    }
}

/// The row a subscription read from Stripe makes.
fn from_subscription(
    before: &Stored,
    read: &Subscription,
    due: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Stored {
    let named = read
        .item
        .as_ref()
        .and_then(|item| item.lookup_key.as_deref())
        .and_then(Plan::from_lookup_key);
    // A price romulus did not make (no lookup key of its own): the same
    // subscription keeps the plan it had; any other gets nothing for it.
    let same = before.subscription_id.as_deref() == Some(read.id.as_str());
    if named.is_none() {
        tracing::warn!(subscription = %read.id, same, "billing: a subscription to a price without a romulus lookup key");
    }
    let (plan, interval) = match named {
        Some((plan, interval)) => (plan, Some(interval)),
        None if same => (before.plan, before.interval),
        None => (Plan::Free, None),
    };
    let scheduled = read
        .scheduled
        .as_ref()
        .and_then(|s| Plan::from_lookup_key(&s.lookup_key).map(|(p, i)| (p, i, s.at)));
    Stored {
        org_id: before.org_id,
        customer_id: Some(read.customer.clone()).filter(|c| !c.is_empty()),
        subscription_id: Some(read.id.clone()),
        plan,
        interval,
        status: Some(read.status.clone()),
        current_period_end: read.current_period_end,
        cancel_at_period_end: read.cancel_at_period_end,
        amount_cents: read
            .item
            .as_ref()
            .and_then(|i| i.unit_amount.map(|a| a * i.quantity.max(1))),
        currency: read.item.as_ref().and_then(|i| i.currency.clone()),
        past_due_since: state::past_due_since(before.past_due_since, &read.status, due, now),
        scheduled_plan: scheduled.map(|s| s.0),
        scheduled_interval: scheduled.map(|s| s.1),
        scheduled_at: scheduled.map(|s| s.2),
        // Who bought it (its metadata), when it began; a withdrawal belongs
        // to the subscription it was made from.
        acceptance_id: read
            .acceptance_id
            .or_else(|| before.acceptance_id.filter(|_| same)),
        started_at: read
            .started
            .or_else(|| before.started_at.filter(|_| same))
            .or(Some(now)),
        withdrawn_at: before.withdrawn_at.filter(|_| same),
        updated_at: before.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_secret";

    #[test]
    fn a_signature_made_with_the_secret_verifies_and_anything_else_does_not() {
        let body = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000;
        let header = sign(SECRET, body, now);
        assert_eq!(verify(SECRET, Some(&header), body, now), Ok(()));
        assert_eq!(verify(SECRET, Some(&header), body, now + 299), Ok(()));
        assert_eq!(
            verify("whsec_other", Some(&header), body, now),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(SECRET, Some(&header), br#"{"id":"evt_2"}"#, now),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(SECRET, Some(&header), body, now + 301),
            Err(SignatureError::Stale)
        );
        assert_eq!(
            verify(SECRET, None, body, now),
            Err(SignatureError::Missing)
        );
        assert_eq!(
            verify(SECRET, Some("v1=abc"), body, now),
            Err(SignatureError::Malformed)
        );
        assert_eq!(
            verify(SECRET, Some("t=1"), body, 1),
            Err(SignatureError::Malformed)
        );
        // Stripe sends several v1 while rolling the secret: any one matches.
        let rolled = format!("{header},v1=00ff");
        assert_eq!(verify(SECRET, Some(&rolled), body, now), Ok(()));
        let other_first = format!("t={now},v1=00ff,{}", &header[header.find("v1=").unwrap()..]);
        assert_eq!(verify(SECRET, Some(&other_first), body, now), Ok(()));
    }

    #[test]
    fn events_name_the_subscription_to_read_again() {
        let checkout = serde_json::json!({"subscription": "sub_1", "client_reference_id": "x"});
        assert_eq!(
            subject("checkout.session.completed", &checkout).unwrap().0,
            "sub_1"
        );
        let updated = serde_json::json!({"id": "sub_2", "object": "subscription"});
        assert_eq!(
            subject("customer.subscription.updated", &updated)
                .unwrap()
                .0,
            "sub_2"
        );
        assert_eq!(
            subject("customer.subscription.deleted", &updated)
                .unwrap()
                .0,
            "sub_2"
        );
        let invoice = serde_json::json!({"id": "in_1", "parent": {"subscription_details": {"subscription": "sub_3"}}, "total": 1900, "created": 1});
        let (sub, inv) = subject("invoice.payment_failed", &invoice).unwrap();
        assert_eq!((sub.as_str(), inv.unwrap().id.as_str()), ("sub_3", "in_1"));
        assert!(subject("customer.created", &updated).is_none());
        // A one-off invoice (no subscription) is not billing's.
        assert!(
            subject(
                "invoice.paid",
                &serde_json::json!({"id": "in_2", "created": 1})
            )
            .is_none()
        );
    }
}
