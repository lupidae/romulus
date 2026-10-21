//! The billing endpoints (see the module docs of [`super`]). Each answers 404
//! while billing is off.

use axum::{
    extract::State,
    response::{IntoResponse, Redirect as Found, Response},
    routing::get,
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    Billing, Inner, hours,
    plans::{self, Limits},
    state::Stored,
    stored,
    stripe::{NewCheckout, NewCustomer, NewPortal, PortalFlow, StripeError},
    webhook,
};
use crate::{
    ApiError, ApiResult, AppState,
    audit::{self, AuditDetails, BillingAudit},
    auth::{Member, min},
    domain::{AuditAction, Interval, Plan, PlaygroundSize},
    error::ErrorBody,
    extract::{Json, Path},
    orgs::invalid,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_billing))
        .routes(routes!(checkout))
        .routes(routes!(portal))
        .routes(routes!(invoices))
        .routes(routes!(webhook::receive))
        // Undocumented: only with `ROMULUS_BILLING=fake` (the e2e suite).
        .route("/billing/fake/checkout/{session}", get(fake_checkout))
}

/// What a cap allows and what there is.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Meter {
    pub used: i64,
    /// `null`: unlimited.
    pub limit: Option<i64>,
    /// The database the figure is about (the one with the most versions, or
    /// the largest schema).
    pub database: Option<String>,
}

impl Meter {
    fn of(used: i64, limit: Option<u32>) -> Self {
        Self {
            used,
            limit: limit.map(i64::from),
            database: None,
        }
    }
}

/// Playground time this calendar month (UTC).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HoursMeter {
    pub used_seconds: i64,
    pub limit_seconds: i64,
    /// The month counted (its first day).
    pub month: NaiveDate,
    /// When the count starts again.
    pub resets_on: NaiveDate,
    /// 80 % used or more.
    pub warning: bool,
    /// All used: new playgrounds and tests are refused, running ones stop.
    pub exhausted: bool,
}

/// Usage against every cap of the plan in force.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Usage {
    /// Members and pending invitations.
    pub members: Meter,
    pub databases: Meter,
    /// The database with the most versions.
    pub versions: Meter,
    /// The largest schema (tables and views) among the latest versions.
    pub schema_size: Meter,
    /// Versions older than this are hidden; `null`: all are shown.
    pub history_days: Option<u32>,
    /// Versions kept but hidden by the plan, every database together.
    pub hidden_versions: i64,
    pub playground_hours: HoursMeter,
    pub playground_size: PlaygroundSize,
    /// The approvals publishing needs, and the most the plan allows.
    pub approvals: Meter,
    pub approvals_override: bool,
}

/// A change Stripe will make at the end of the period (a downgrade).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScheduledChange {
    pub plan: Plan,
    pub interval: Option<Interval>,
    pub at: DateTime<Utc>,
}

/// The subscription, as Stripe last said.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionInfo {
    pub plan: Plan,
    pub interval: Option<Interval>,
    /// Stripe's status: `active`, `past_due`, `canceled`…
    pub status: String,
    /// The end of the period paid for: the renewal date (or the end, when
    /// cancelled at the period's end).
    pub current_period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    /// What a period costs, in cents.
    pub amount_cents: Option<i64>,
    pub currency: Option<String>,
    /// While a payment is overdue: when the plan stops holding.
    pub grace_until: Option<DateTime<Utc>>,
    pub scheduled: Option<ScheduledChange>,
}

/// A plan as the picker shows it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlanOffer {
    pub plan: Plan,
    pub name: String,
    /// Euro cents; `null` for Free.
    pub monthly_cents: Option<u32>,
    pub yearly_cents: Option<u32>,
    pub limits: Limits,
}

/// An organization's billing page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrgBilling {
    /// The plan in force now: the caps are its.
    pub plan: Plan,
    /// `null` when the organization never subscribed (or its subscription
    /// ended).
    pub subscription: Option<SubscriptionInfo>,
    pub usage: Usage,
    pub plans: Vec<PlanOffer>,
    /// The caller may change billing (admins).
    pub can_manage: bool,
    /// The Terms of Service checkout has people accept.
    pub terms_url: String,
    pub terms_version: String,
}

/// Who buys: the checkout page asks a consumer for more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Buyer {
    Business,
    Consumer,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartCheckout {
    /// A paid plan.
    pub plan: Plan,
    pub interval: Interval,
    /// The Terms of Service (`terms_url`, `terms_version`) are accepted.
    pub accept_terms: bool,
    pub buyer: Buyer,
    /// A consumer asks for the service to start before the withdrawal period
    /// ends…
    #[serde(default)]
    pub immediate_start: bool,
    /// …and acknowledges losing the right of withdrawal once it is fully
    /// performed.
    #[serde(default)]
    pub withdrawal_acknowledged: bool,
}

/// Where to send the browser.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BillingRedirect {
    pub url: String,
}

/// What the portal opens on.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PortalChoice {
    /// The subscription's cancel page (it ends at the period's end).
    Cancel,
    /// Another plan: straight to confirming it when one is named.
    ChangePlan {
        plan: Option<Plan>,
        interval: Option<Interval>,
    },
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct OpenPortal {
    /// The portal's home page (payment method, invoices) when absent.
    #[serde(default)]
    pub flow: Option<PortalChoice>,
}

/// One invoice, as Stripe keeps it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvoiceSummary {
    pub id: String,
    pub number: Option<String>,
    /// `paid`, `open`, `void`, `uncollectible`, `draft`.
    pub status: Option<String>,
    pub total_cents: i64,
    pub currency: String,
    pub created: DateTime<Utc>,
    /// Stripe's page for it (pay, download).
    pub url: Option<String>,
    pub pdf: Option<String>,
}

fn unavailable(err: &StripeError) -> ApiError {
    tracing::warn!(error = %err, "billing: Stripe call failed");
    ApiError::Down {
        code: "billing_unavailable",
        message: "Billing is not answering right now. Try again in a minute.".into(),
    }
}

fn billing_url(inner: &Inner, org: &str, query: &str) -> String {
    let mut url = inner.public_url.clone();
    url.set_path(&format!("/app/{org}/settings/billing"));
    url.set_query((!query.is_empty()).then_some(query));
    url.to_string()
}

/// This month's playground meter for `plan`.
fn hours_meter(plan: Plan, used_seconds: i64, now: DateTime<Utc>) -> HoursMeter {
    let limit_seconds = i64::from(plan.limits().playground_hours) * 3600;
    let (month, resets_on) = hours::month_bounds(now);
    HoursMeter {
        used_seconds,
        limit_seconds,
        month,
        resets_on,
        warning: used_seconds * 5 >= limit_seconds * 4,
        exhausted: used_seconds >= limit_seconds,
    }
}

async fn usage(
    state: &AppState,
    org: uuid::Uuid,
    plan: Plan,
    approvals_required: i16,
) -> ApiResult<Usage> {
    let limits = plan.limits();
    let now = Utc::now();
    let counts = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM memberships WHERE org_id = $1) AS "members!",
                  (SELECT count(*) FROM invitations
                   WHERE org_id = $1 AND accepted_at IS NULL AND expires_at > now()) AS "invited!",
                  (SELECT count(*) FROM databases WHERE org_id = $1 AND deleted_at IS NULL) AS "databases!""#,
        org,
    )
    .fetch_one(&state.db)
    .await?;
    let most = sqlx::query!(
        r#"SELECT d.name, count(v.id) AS "versions!"
           FROM databases d LEFT JOIN versions v ON v.database_id = d.id AND v.removed_at IS NULL
           WHERE d.org_id = $1 AND d.deleted_at IS NULL
           GROUP BY d.id, d.name ORDER BY count(v.id) DESC, d.name LIMIT 1"#,
        org,
    )
    .fetch_optional(&state.db)
    .await?;
    let largest = sqlx::query!(
        r#"SELECT d.name, coalesce(jsonb_array_length(l.model -> 'entities'), 0) AS "objects!"
           FROM databases d
           CROSS JOIN LATERAL (
               SELECT model FROM versions
               WHERE database_id = d.id AND removed_at IS NULL ORDER BY number DESC LIMIT 1
           ) l
           WHERE d.org_id = $1 AND d.deleted_at IS NULL
           ORDER BY 2 DESC, d.name LIMIT 1"#,
        org,
    )
    .fetch_optional(&state.db)
    .await?;
    let window = super::HistoryWindow::of(Some(plan), now);
    let hidden = match window.cutoff {
        None => 0,
        Some(cutoff) => sqlx::query_scalar!(
            r#"SELECT count(*) AS "hidden!" FROM versions v JOIN databases d ON d.id = v.database_id
               WHERE d.org_id = $1 AND d.deleted_at IS NULL AND v.removed_at IS NULL
                 AND v.created_at < $2 AND NOT version_kept_visible(v.id, v.database_id)"#,
            org,
            cutoff,
        )
        .fetch_one(&state.db)
        .await?,
    };
    let used_seconds = hours::used(&state.db, org, now).await?;
    Ok(Usage {
        members: Meter::of(counts.members + counts.invited, limits.members),
        databases: Meter::of(counts.databases, Some(limits.databases)),
        versions: Meter {
            used: most.as_ref().map_or(0, |m| m.versions),
            limit: limits.versions_per_database.map(i64::from),
            database: most.map(|m| m.name),
        },
        schema_size: Meter {
            used: largest.as_ref().map_or(0, |l| i64::from(l.objects)),
            limit: Some(limits.schema_objects.into()),
            database: largest.map(|l| l.name),
        },
        history_days: limits.history_days,
        hidden_versions: hidden,
        playground_hours: hours_meter(plan, used_seconds, now),
        playground_size: limits.playground_size,
        approvals: Meter::of(approvals_required.into(), Some(limits.approvals.into())),
        approvals_override: limits.approvals_override,
    })
}

fn subscription_info(stored: &Stored) -> Option<SubscriptionInfo> {
    stored.subscription_id.as_ref()?;
    let status = stored.status.clone()?;
    Some(SubscriptionInfo {
        plan: stored.plan,
        interval: stored.interval,
        status,
        current_period_end: stored.current_period_end,
        cancel_at_period_end: stored.cancel_at_period_end,
        amount_cents: stored.amount_cents,
        currency: stored.currency.clone(),
        grace_until: stored.grace_until(),
        scheduled: match (stored.scheduled_plan, stored.scheduled_at) {
            (Some(plan), Some(at)) => Some(ScheduledChange {
                plan,
                interval: stored.scheduled_interval,
                at,
            }),
            _ => None,
        },
    })
}

/// The organization's plan, subscription and usage against every cap. Any
/// member reads it; admins change it.
#[utoipa::path(
    get,
    path = "/orgs/{org}/billing",
    operation_id = "get_billing",
    tag = "billing",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = OrgBilling),
        (status = 404, description = "Billing is off on this server, or not a member", body = ErrorBody),
    )
)]
async fn get_billing(member: Member, State(state): State<AppState>) -> ApiResult<Json<OrgBilling>> {
    let inner = state.billing.require()?;
    let stored = stored(&state.db, member.org.id).await?;
    let plan = stored.effective(Utc::now());
    let usage = usage(&state, member.org.id, plan, member.org.approvals_required).await?;
    Ok(Json(OrgBilling {
        plan,
        subscription: subscription_info(&stored),
        usage,
        plans: plans::ALL
            .into_iter()
            .map(|plan| PlanOffer {
                plan,
                name: plan.name().to_owned(),
                monthly_cents: plan.price_cents(Interval::Month),
                yearly_cents: plan.price_cents(Interval::Year),
                limits: *plan.limits(),
            })
            .collect(),
        can_manage: member.role.can_manage_members(),
        terms_url: inner.config.terms_url.to_string(),
        terms_version: inner.config.terms_version.clone(),
    }))
}

fn customer_for(inner: &Inner, member: &Member<min::Admin>) -> NewCustomer {
    NewCustomer {
        org_id: member.org.id,
        name: member.org.name.clone(),
        email: member.user.email.clone(),
        invoice_footer: inner.config.invoice_footer.clone(),
        invoice_fields: inner.config.invoice_fields.clone(),
    }
}

/// What the checkout's lock guards: the organization's customer and its
/// pending checkout, read under [`super::lock_org`].
struct Pending {
    customer_id: Option<String>,
    session_id: Option<String>,
    nonce: Option<String>,
    price: Option<String>,
}

/// Opens one Checkout Session for the organization, under its billing lock
/// for the whole exchange with Stripe (a second tab or admin waits, then
/// sees the first one's session):
///
/// 1. the customer, created once (its idempotency key is the
///    organization's), its invoice mentions brought up to date;
/// 2. refused when Stripe already has a running subscription for it (a
///    checkout paid whose webhook has not arrived yet);
/// 3. the session, with an idempotency key made of the organization, the
///    price and a nonce kept until the session is known: the same request
///    again after a timeout gets the same session, never a second one;
/// 4. every other open session of the customer expired (after creating
///    this one, so that a replayed request never expires its own answer):
///    one open checkout at a time, so two can never both be paid.
///
/// What it learned (the customer, the pending nonce, the session) is
/// committed even when Stripe fails midway, for the retry.
async fn open_checkout(
    state: &AppState,
    inner: &Inner,
    member: &Member<min::Admin>,
    price: &str,
) -> ApiResult<String> {
    let org = member.org.id;
    let mut tx = state.db.begin().await?;
    super::lock_org(&mut tx, org).await?;
    let current = stored(&mut *tx, org).await?;
    if current.has_live_subscription() {
        return Err(already_subscribed());
    }
    sqlx::query!(
        "INSERT INTO org_billing (org_id) VALUES ($1) ON CONFLICT (org_id) DO NOTHING",
        org
    )
    .execute(&mut *tx)
    .await?;
    let pending = sqlx::query_as!(
        Pending,
        r#"SELECT customer_id, checkout_session_id AS session_id, checkout_nonce AS nonce,
                  checkout_price AS price
           FROM org_billing WHERE org_id = $1"#,
        org
    )
    .fetch_one(&mut *tx)
    .await?;
    let outcome = exchange(inner, member, price, pending, &mut tx).await;
    // Kept whatever happened: the customer and the pending nonce matter to
    // the next attempt.
    tx.commit().await?;
    outcome
}

/// The calls to Stripe of [`open_checkout`], writing what they learn in `tx`.
async fn exchange(
    inner: &Inner,
    member: &Member<min::Admin>,
    price: &str,
    pending: Pending,
    tx: &mut sqlx::PgConnection,
) -> ApiResult<String> {
    let org = member.org.id;
    let customer = if let Some(id) = pending.customer_id {
        inner
            .stripe
            .update_customer(id.clone(), customer_for(inner, member))
            .await
            .map_err(|err| unavailable(&err))?;
        id
    } else {
        let id = inner
            .stripe
            .create_customer(customer_for(inner, member))
            .await
            .map_err(|err| unavailable(&err))?;
        sqlx::query!(
            "UPDATE org_billing SET customer_id = $2, updated_at = now() WHERE org_id = $1",
            org,
            id,
        )
        .execute(&mut *tx)
        .await?;
        id
    };
    let running = inner
        .stripe
        .live_subscriptions(customer.clone())
        .await
        .map_err(|err| unavailable(&err))?;
    if !running.is_empty() {
        return Err(already_subscribed());
    }
    let nonce = match (pending.nonce, pending.session_id) {
        (Some(nonce), None) if pending.price.as_deref() == Some(price) => nonce,
        _ => crate::token::generate(),
    };
    sqlx::query!(
        "UPDATE org_billing SET checkout_nonce = $2, checkout_price = $3, checkout_session_id = NULL
         WHERE org_id = $1",
        org,
        nonce,
        price,
    )
    .execute(&mut *tx)
    .await?;
    let session = inner
        .stripe
        .checkout(NewCheckout {
            org_id: org,
            customer: customer.clone(),
            price: price.to_owned(),
            success_url: billing_url(inner, &member.org.slug, "checkout=success"),
            cancel_url: billing_url(inner, &member.org.slug, "checkout=cancelled"),
            idempotency_key: super::stripe::checkout_key(org, price, &nonce),
        })
        .await
        .map_err(|err| unavailable(&err))?;
    let others = inner
        .stripe
        .open_checkouts(customer)
        .await
        .map_err(|err| unavailable(&err))?;
    for other in others.into_iter().filter(|id| *id != session.id) {
        if let Err(err) = inner.stripe.expire_checkout(other).await {
            // Another session could still be paid: this one must not be too.
            let _ = inner.stripe.expire_checkout(session.id.clone()).await;
            return Err(match err {
                StripeError::Refused {
                    code: Some(code), ..
                } if code == "checkout_complete" => ApiError::Conflict {
                    code: "checkout_completed",
                    message:
                        "A checkout of this organization was just paid: its plan shows in a moment."
                            .into(),
                },
                err => unavailable(&err),
            });
        }
    }
    sqlx::query!(
        "UPDATE org_billing SET checkout_session_id = $2, checkout_nonce = NULL, checkout_price = NULL,
                                updated_at = now()
         WHERE org_id = $1",
        org,
        session.id,
    )
    .execute(&mut *tx)
    .await?;
    Ok(session.url)
}

fn already_subscribed() -> ApiError {
    ApiError::Conflict {
        code: "already_subscribed",
        message:
            "This organization already has a subscription: change its plan with Manage billing."
                .into(),
    }
}

/// Starts paying for a plan: the Terms accepted (kept in the audit log, with
/// a consumer's requests), then a Stripe Checkout Session to send the
/// browser to. Admins only. An organization already subscribed changes plan
/// in the portal.
#[utoipa::path(
    post,
    path = "/orgs/{org}/billing/checkout",
    operation_id = "start_checkout",
    tag = "billing",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = StartCheckout,
    responses(
        (status = 200, body = BillingRedirect),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`already_subscribed`: change plans in the portal; `checkout_completed`: another checkout was just paid", body = ErrorBody),
        (status = 422, description = "`validation`: the Terms (or a consumer's requests) not accepted, or Free", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`: Stripe did not answer", body = ErrorBody),
    )
)]
async fn checkout(
    member: Member<min::Admin>,
    State(state): State<AppState>,
    Json(body): Json<StartCheckout>,
) -> ApiResult<Json<BillingRedirect>> {
    let inner = state.billing.require()?;
    let lookup_key = body
        .plan
        .lookup_key(body.interval)
        .ok_or_else(|| invalid("plan", "Choose a paid plan: Free needs no checkout."))?;
    if !body.accept_terms {
        return Err(invalid(
            "accept_terms",
            "Accept the Terms of Service to continue.",
        ));
    }
    if body.buyer == Buyer::Consumer && !(body.immediate_start && body.withdrawal_acknowledged) {
        return Err(invalid(
            "immediate_start",
            "As a consumer, ask for the service to start now and acknowledge the withdrawal terms to continue.",
        ));
    }
    if stored(&state.db, member.org.id)
        .await?
        .has_live_subscription()
    {
        return Err(already_subscribed());
    }
    let price = inner
        .stripe
        .price(lookup_key)
        .await
        .map_err(|err| unavailable(&err))?;
    let url = open_checkout(&state, inner, &member, &price).await?;
    audit::record(
        &state.db,
        &audit::Entry {
            org_id: member.org.id,
            database_id: None,
            action: AuditAction::TermsAccepted,
            actor_id: Some(member.user.id),
            actor_name: Some(member.user.name.clone()),
            version_number: None,
            reason: None,
            details: AuditDetails {
                billing: Some(BillingAudit {
                    plan: Some(body.plan),
                    interval: Some(body.interval),
                    terms_version: Some(inner.config.terms_version.clone()),
                    buyer: Some(
                        match body.buyer {
                            Buyer::Business => "business",
                            Buyer::Consumer => "consumer",
                        }
                        .to_owned(),
                    ),
                    immediate_start: (body.buyer == Buyer::Consumer)
                        .then_some(body.immediate_start),
                    withdrawal_acknowledged: (body.buyer == Buyer::Consumer)
                        .then_some(body.withdrawal_acknowledged),
                    ..BillingAudit::default()
                }),
                ..AuditDetails::default()
            },
        },
    )
    .await?;
    tracing::info!(org = %member.org.slug, plan = ?body.plan, interval = ?body.interval, "billing: checkout started");
    Ok(Json(BillingRedirect { url }))
}

/// A Customer Portal session: payment method, invoices, plan changes
/// (upgrades now, prorated; downgrades at the period's end) and
/// cancelling, straight on the cancel page with `{"flow": {"type":
/// "cancel"}}`. Admins only.
#[utoipa::path(
    post,
    path = "/orgs/{org}/billing/portal",
    operation_id = "open_billing_portal",
    tag = "billing",
    params(("org" = String, Path, description = "The organization's slug")),
    request_body = OpenPortal,
    responses(
        (status = 200, body = BillingRedirect),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "`no_customer`: never subscribed; `no_subscription`: nothing to cancel or change", body = ErrorBody),
        (status = 503, description = "`billing_unavailable`", body = ErrorBody),
    )
)]
async fn portal(
    member: Member<min::Admin>,
    State(state): State<AppState>,
    Json(body): Json<OpenPortal>,
) -> ApiResult<Json<BillingRedirect>> {
    let inner = state.billing.require()?;
    let current = stored(&state.db, member.org.id).await?;
    let customer = current
        .customer_id
        .clone()
        .ok_or_else(|| ApiError::Conflict {
            code: "no_customer",
            message: "This organization has never subscribed: choose a plan first.".into(),
        })?;
    let live = || {
        current
            .has_live_subscription()
            .then(|| current.subscription_id.clone())
            .flatten()
            .ok_or_else(|| ApiError::Conflict {
                code: "no_subscription",
                message: "This organization has no running subscription.".into(),
            })
    };
    let flow = match body.flow {
        None => None,
        Some(PortalChoice::Cancel) => Some(PortalFlow::Cancel {
            subscription: live()?,
        }),
        Some(PortalChoice::ChangePlan { plan, interval }) => {
            let subscription = live()?;
            match plan.zip(interval).and_then(|(p, i)| p.lookup_key(i)) {
                None => Some(PortalFlow::Update { subscription }),
                Some(key) => {
                    let read = inner
                        .stripe
                        .subscription(subscription.clone())
                        .await
                        .map_err(|err| unavailable(&err))?;
                    let price = inner
                        .stripe
                        .price(key)
                        .await
                        .map_err(|err| unavailable(&err))?;
                    match read.item {
                        Some(item) if item.price != price => Some(PortalFlow::Confirm {
                            subscription,
                            item: item.id,
                            price,
                        }),
                        _ => Some(PortalFlow::Update { subscription }),
                    }
                }
            }
        }
    };
    let url = inner
        .stripe
        .portal(NewPortal {
            customer,
            return_url: billing_url(inner, &member.org.slug, ""),
            flow,
        })
        .await
        .map_err(|err| unavailable(&err))?;
    Ok(Json(BillingRedirect { url }))
}

/// The organization's invoices, newest first. Admins only.
#[utoipa::path(
    get,
    path = "/orgs/{org}/billing/invoices",
    operation_id = "list_invoices",
    tag = "billing",
    params(("org" = String, Path, description = "The organization's slug")),
    responses(
        (status = 200, body = Vec<InvoiceSummary>),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 503, description = "`billing_unavailable`", body = ErrorBody),
    )
)]
async fn invoices(
    member: Member<min::Admin>,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<InvoiceSummary>>> {
    let inner = state.billing.require()?;
    let Some(customer) = stored(&state.db, member.org.id).await?.customer_id else {
        return Ok(Json(Vec::new()));
    };
    let invoices = inner
        .stripe
        .invoices(customer)
        .await
        .map_err(|err| unavailable(&err))?;
    Ok(Json(
        invoices
            .into_iter()
            .filter(|i| i.status.as_deref() != Some("draft"))
            .map(|i| InvoiceSummary {
                id: i.id,
                number: i.number,
                status: i.status,
                total_cents: i.total,
                currency: i.currency,
                created: i.created,
                url: i.hosted_invoice_url,
                pdf: i.invoice_pdf,
            })
            .collect(),
    ))
}

/// The fake checkout page (`ROMULUS_BILLING=fake`): completes the session as
/// a paid subscription, applies it as its webhook would, and goes back to the
/// billing page. 404 otherwise.
async fn fake_checkout(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> ApiResult<Response> {
    let Some(inner) = state.billing.inner() else {
        return Err(ApiError::not_found("No such endpoint."));
    };
    let Some(fake) = &inner.fake else {
        return Err(ApiError::not_found("No such endpoint."));
    };
    let checkout = fake
        .state()
        .checkouts
        .get(&session)
        .cloned()
        .ok_or_else(|| ApiError::not_found("No such checkout."))?;
    let subscription = fake
        .complete_checkout(&session)
        .ok_or_else(|| ApiError::Conflict {
            code: "checkout_expired",
            message:
                "This checkout expired or was paid already: start again from the billing page."
                    .into(),
        })?;
    webhook::apply_subscription(
        &state,
        &format!("evt_fake_{session}"),
        "checkout.session.completed",
        &subscription,
        None,
    )
    .await?;
    Ok(Found::to(&checkout.success_url).into_response())
}

impl Billing {
    /// The month's playground meter of `org` (the console and the pages).
    pub async fn hours(&self, state: &AppState, org: uuid::Uuid) -> ApiResult<Option<HoursMeter>> {
        let Some(plan) = self.plan(&state.db, org).await? else {
            return Ok(None);
        };
        let now = Utc::now();
        Ok(Some(hours_meter(
            plan,
            hours::used(&state.db, org, now).await?,
            now,
        )))
    }
}

/// The first day of the month after `day`'s.
pub fn next_month(day: NaiveDate) -> NaiveDate {
    let (year, month) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(day)
}
