//! Stripe, as billing reaches it: the few calls romulus makes, behind
//! [`StripeApi`]. [`Http`] is the real client (`STRIPE_SECRET_KEY`, form
//! requests to api.stripe.com, one pinned API version); [`Fake`] keeps
//! everything in memory, for the API's tests and the end-to-end suite
//! (`ROMULUS_BILLING=fake`, loopback only).
//!
//! What romulus reads back is parsed leniently: a field Stripe moved between
//! API versions (a subscription's period end, now on its items; an
//! invoice's subscription, now under `parent`) is read from either place.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::{config::Secret, domain::Locale};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The API version every request pins: the shapes parsed here.
pub const API_VERSION: &str = "2025-08-27.basil";
const API: &str = "https://api.stripe.com/v1/";

#[derive(Debug, Clone, thiserror::Error)]
pub enum StripeError {
    /// Stripe answered with an error (`code`, `message` as it said).
    #[error("Stripe refused: {message}")]
    Refused {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("Stripe is unreachable: {0}")]
    Unreachable(String),
    #[error("an answer Stripe should not give: {0}")]
    Unexpected(String),
}

impl StripeError {
    /// 404: no such object (a subscription deleted in the Dashboard).
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Refused { status: 404, .. })
    }
}

/// A customer romulus creates for an organization before its first
/// checkout: the invoices' footer and custom fields (the seller's mentions)
/// are the customer's, so every invoice carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCustomer {
    pub org_id: Uuid,
    pub name: String,
    pub email: String,
    pub invoice_footer: Option<String>,
    pub invoice_fields: Vec<(String, String)>,
    /// The language of Stripe's pages, emails and invoices for it.
    pub locale: Locale,
    /// A business buyer's name, which its invoices carry.
    pub business_name: Option<String>,
}

/// A subscription checkout for one price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCheckout {
    pub org_id: Uuid,
    pub customer: String,
    pub price: String,
    pub success_url: String,
    pub cancel_url: String,
    /// Made of the organization and the checkout's pending nonce: the same
    /// request again (after a timeout) gets the same session.
    pub idempotency_key: String,
    /// The record of what the buyer accepted (`legal_acceptances`), which
    /// the subscription carries in its metadata.
    pub acceptance_id: Uuid,
    /// The payment page's language, and of its words under the button.
    pub locale: Locale,
    /// The Terms in force: the words under the button are theirs.
    pub edition: crate::domain::TermsEdition,
    /// A business buys: the payment page requires its name (the business
    /// named there is the Customer, Terms 3.5).
    pub business: bool,
    /// An offer's promotion code (`promo_…`) applied to the subscription
    /// (the founding-team offer), shown on the payment page.
    pub promotion_code: Option<String>,
    /// When the session expires (Unix seconds): an offered one holds its
    /// place until then, 30 minutes or so; `None`, Stripe's default.
    pub expires_at: Option<i64>,
}

/// A promotion code as romulus reads it: how often it was redeemed (the
/// founding-team offer's count), and whether it still applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionCode {
    pub id: String,
    pub code: String,
    pub active: bool,
    pub times_redeemed: i64,
    pub max_redemptions: Option<i64>,
    /// Its coupon's `amount_off`, in its currency's cents.
    pub amount_off: Option<i64>,
    /// Its coupon's id.
    pub coupon: Option<String>,
}

/// A discount on a subscription, its coupon as Stripe reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discount {
    pub coupon: String,
    /// The coupon's `metadata.romulus_offer` (`founding`): an offer's.
    pub offer: Option<String>,
    pub amount_off: Option<i64>,
    /// Hundredths of a percent (`percent_off` 40.5 is 4050).
    pub percent_off: Option<i64>,
    /// Of duration `forever`: every period, for as long as it runs.
    pub forever: bool,
    /// The products it is restricted to (`applies_to.products`); `None`:
    /// every product.
    pub products: Option<Vec<String>>,
}

impl Discount {
    /// What it takes off a period of `price` cents of `product`: nothing
    /// unless forever and it applies to that product.
    pub fn off(&self, product: Option<&str>, price: Option<i64>) -> i64 {
        let applies = match &self.products {
            None => true,
            Some(products) => product.is_some_and(|p| products.iter().any(|q| q == p)),
        };
        if !(self.forever && applies) {
            return 0;
        }
        self.amount_off
            .or_else(|| Some(price? * self.percent_off? / 10_000))
            .unwrap_or(0)
    }
}

/// A Checkout Session, as romulus keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutSession {
    pub id: String,
    pub url: String,
}

/// What a refund gave back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refund {
    pub amount: i64,
    pub currency: String,
}

/// A paid invoice of a subscription, with the period its lines cover: what
/// a withdrawal refunds a share of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidInvoice {
    pub id: String,
    pub payment_intent: Option<String>,
    pub amount_paid: i64,
    pub currency: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
}

/// One refund a withdrawal makes (kept with it, `withdrawals.refunds`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedRefund {
    pub invoice: String,
    pub payment_intent: String,
    pub amount: i64,
    /// What the invoice was paid: no refund of it ever gives back more,
    /// with the ones made before.
    pub paid: i64,
}

/// A refund made on a payment already, as Stripe lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MadeRefund {
    pub amount: i64,
    /// `succeeded`, `pending`, `requires_action`, `failed`, `canceled`.
    pub status: String,
    /// The withdrawal it was made for (`metadata[withdrawal]`).
    pub withdrawal: Option<String>,
}

/// What `planned` still gives back, given the refunds made on its payment:
/// nothing when this withdrawal made one already (an attempt that ended
/// before romulus heard back); otherwise its amount, at most what was paid
/// less what was refunded (by hand in the Dashboard, say). A failed or
/// canceled refund gave nothing back.
pub fn still_due(planned: &PlannedRefund, withdrawal: Uuid, made: &[MadeRefund]) -> i64 {
    let gave_back = |r: &&MadeRefund| !matches!(r.status.as_str(), "failed" | "canceled");
    let ours = withdrawal.to_string();
    if made
        .iter()
        .filter(gave_back)
        .any(|r| r.withdrawal.as_deref() == Some(ours.as_str()))
    {
        return 0;
    }
    let refunded: i64 = made.iter().filter(gave_back).map(|r| r.amount).sum();
    planned.amount.min(planned.paid - refunded).max(0)
}

fn parse_refunds(list: &Value) -> Vec<MadeRefund> {
    list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| MadeRefund {
            amount: r["amount"].as_i64().unwrap_or(0),
            status: text(&r["status"]).unwrap_or_default(),
            withdrawal: text(&r["metadata"]["withdrawal"]),
        })
        .collect()
}

/// A Customer Portal session, at its home page or straight in one flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPortal {
    pub customer: String,
    pub return_url: String,
    pub flow: Option<PortalFlow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalFlow {
    /// The cancel page of `subscription` (cancels at the period's end).
    Cancel { subscription: String },
    /// Choosing another plan.
    Update { subscription: String },
    /// Confirming one change: `item` to `price`.
    Confirm {
        subscription: String,
        item: String,
        price: String,
    },
}

/// A subscription as romulus keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub customer: String,
    pub status: String,
    pub cancel_at_period_end: bool,
    pub current_period_end: Option<DateTime<Utc>>,
    pub item: Option<Item>,
    /// `metadata.org_id`, set at checkout.
    pub org_id: Option<Uuid>,
    /// `metadata.acceptance_id`, set at checkout: its buyer's record.
    pub acceptance_id: Option<Uuid>,
    /// `start_date`: when the contract began.
    pub started: Option<DateTime<Utc>>,
    /// Its discounts (the founding-team offer's coupon), as Stripe has
    /// them: [`Subscription::discount_cents`] is what they take off.
    pub discounts: Vec<Discount>,
    pub latest_invoice: Option<String>,
    /// A change Stripe will make (a downgrade at the period's end).
    pub scheduled: Option<Scheduled>,
    /// Why it was cancelled (`cancellation_details`), when Stripe says.
    pub cancellation: Option<Cancellation>,
}

/// Why a subscription was cancelled, as Stripe keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cancellation {
    /// `cancellation_requested`, `payment_failed` or `payment_disputed`.
    pub reason: Option<String>,
    /// What the customer picked in the portal (`too_expensive`,
    /// `missing_features`, `switched_service`…).
    pub feedback: Option<String>,
    /// What the customer wrote.
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub price: String,
    pub lookup_key: Option<String>,
    pub unit_amount: Option<i64>,
    pub currency: Option<String>,
    pub quantity: i64,
    /// Its price's product (`romulus_team`).
    pub product: Option<String>,
}

impl Subscription {
    /// What its discounts take off every period for as long as it runs:
    /// coupons of duration `forever` that apply to its item's product.
    pub fn discount_cents(&self) -> Option<i64> {
        let item = self.item.as_ref();
        let off: i64 = self
            .discounts
            .iter()
            .map(|d| {
                d.off(
                    item.and_then(|i| i.product.as_deref()),
                    item.and_then(|i| i.unit_amount),
                )
            })
            .sum();
        (off > 0).then_some(off)
    }

    /// Its discounts of an offer (`founding`).
    pub fn offer_discounts<'a>(&'a self, offer: &'a str) -> impl Iterator<Item = &'a Discount> {
        self.discounts
            .iter()
            .filter(move |d| d.offer.as_deref() == Some(offer))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub lookup_key: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoice {
    pub id: String,
    pub number: Option<String>,
    pub status: Option<String>,
    pub total: i64,
    pub currency: String,
    pub created: DateTime<Utc>,
    pub due: Option<DateTime<Utc>>,
    pub hosted_invoice_url: Option<String>,
    pub invoice_pdf: Option<String>,
    pub subscription: Option<String>,
    pub customer: Option<String>,
}

/// What billing asks of Stripe.
pub trait StripeApi: Send + Sync + 'static {
    fn create_customer(&self, customer: NewCustomer) -> BoxFuture<'_, Result<String, StripeError>>;
    /// The invoices' footer and fields again (the seller's mentions may have
    /// changed since the customer was created).
    fn update_customer(
        &self,
        id: String,
        customer: NewCustomer,
    ) -> BoxFuture<'_, Result<(), StripeError>>;
    /// The active price with this `lookup_key`.
    fn price(&self, lookup_key: String) -> BoxFuture<'_, Result<String, StripeError>>;
    /// A Checkout Session (its id and URL).
    fn checkout(
        &self,
        checkout: NewCheckout,
    ) -> BoxFuture<'_, Result<CheckoutSession, StripeError>>;
    /// The customer's Checkout Sessions still open.
    fn open_checkouts(&self, customer: String) -> BoxFuture<'_, Result<Vec<String>, StripeError>>;
    /// Expires an open Checkout Session; `Err` with code `checkout_complete`
    /// when it was paid meanwhile. An expired one stays expired (`Ok`).
    fn expire_checkout(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>>;
    /// The customer's subscriptions that pay (or did, recently: past due).
    fn live_subscriptions(
        &self,
        customer: String,
    ) -> BoxFuture<'_, Result<Vec<Subscription>, StripeError>>;
    /// Cancels `subscription` now (no proration) and refunds in full what its
    /// latest invoice charged: a duplicate that never took effect.
    /// Idempotent. `None`: nothing had been charged.
    fn cancel_duplicate(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Option<Refund>, StripeError>>;
    /// The subscription's paid invoices, with the periods they cover.
    fn paid_invoices(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Vec<PaidInvoice>, StripeError>>;
    /// A consumer's withdrawal: cancels `subscription` now (no proration,
    /// no final invoice) and makes `refunds`. Idempotent (keys by
    /// subscription and invoice): the notices task calls it again until it
    /// succeeds.
    fn withdraw(
        &self,
        subscription: String,
        withdrawal: Uuid,
        refunds: Vec<PlannedRefund>,
    ) -> BoxFuture<'_, Result<(), StripeError>>;
    /// A Customer Portal session's URL.
    fn portal(&self, portal: NewPortal) -> BoxFuture<'_, Result<String, StripeError>>;
    fn subscription(&self, id: String) -> BoxFuture<'_, Result<Subscription, StripeError>>;
    fn invoice(&self, id: String) -> BoxFuture<'_, Result<Invoice, StripeError>>;
    /// The customer's newest invoices.
    fn invoices(&self, customer: String) -> BoxFuture<'_, Result<Vec<Invoice>, StripeError>>;
    /// A promotion code, its redemptions counted.
    fn promotion_code(&self, id: String) -> BoxFuture<'_, Result<PromotionCode, StripeError>>;
    /// Switches a promotion code off: no checkout can use it any more.
    fn deactivate_promotion_code(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>>;
    /// Replaces the subscription's discounts by these coupons' (none: no
    /// discount left), with no proration.
    fn set_discounts(
        &self,
        subscription: String,
        coupons: Vec<String>,
    ) -> BoxFuture<'_, Result<(), StripeError>>;
}

/// The real thing.
pub struct Http {
    key: Secret,
    http: reqwest::Client,
}

impl Http {
    pub fn new(key: Secret) -> Self {
        Self {
            key,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("an HTTP client"),
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value, StripeError> {
        let response = request
            .bearer_auth(self.key.expose())
            .header("Stripe-Version", API_VERSION)
            .send()
            .await
            .map_err(|err| StripeError::Unreachable(err.without_url().to_string()))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|err| StripeError::Unexpected(err.without_url().to_string()))?;
        if status.is_success() {
            return Ok(body);
        }
        let error = &body["error"];
        Err(StripeError::Refused {
            status: status.as_u16(),
            code: error["code"].as_str().map(str::to_owned),
            message: error["message"].as_str().unwrap_or("no message").to_owned(),
        })
    }

    /// GET `path` with query pairs.
    pub async fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value, StripeError> {
        self.send(self.http.get(format!("{API}{path}")).query(query))
            .await
    }

    /// POST `path` with form pairs (an update, or a create a person runs
    /// again by hand: `stripe-setup`).
    pub async fn post(&self, path: &str, form: &[(String, String)]) -> Result<Value, StripeError> {
        self.send(self.http.post(format!("{API}{path}")).form(form))
            .await
    }

    /// POST `path` with an idempotency key made of the operation (never
    /// random): asked again, Stripe answers what it did the first time
    /// instead of doing it twice.
    pub async fn create(
        &self,
        path: &str,
        form: &[(String, String)],
        key: &str,
    ) -> Result<Value, StripeError> {
        self.send(
            self.http
                .post(format!("{API}{path}"))
                .header("Idempotency-Key", key)
                .form(form),
        )
        .await
    }

    /// DELETE `path`, with an idempotency key made of the operation.
    pub async fn delete(&self, path: &str, key: &str) -> Result<Value, StripeError> {
        self.send(
            self.http
                .delete(format!("{API}{path}"))
                .header("Idempotency-Key", key),
        )
        .await
    }
}

/// The idempotency key of an organization's customer: one, however often
/// its creation is asked for.
pub fn customer_key(org: Uuid) -> String {
    format!("romulus-customer-{org}")
}

/// The idempotency key of a checkout: the organization, the price, the
/// language (the page's words), the promotion code applied, and the nonce
/// kept until the session is known.
pub fn checkout_key(
    org: Uuid,
    price: &str,
    locale: Locale,
    promotion_code: Option<&str>,
    nonce: &str,
) -> String {
    let promotion = promotion_code.map_or_else(String::new, |code| format!("-{code}"));
    format!(
        "romulus-checkout-{org}-{price}-{}{promotion}-{nonce}",
        locale.tag()
    )
}

/// Form pairs, Stripe's nested keys spelled out (`a[b][0]`).
#[derive(Default)]
pub struct Form(Vec<(String, String)>);

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn set(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.push((key.into(), value.into()));
        self
    }

    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }
}

fn customer_form(customer: &NewCustomer, create: bool) -> Form {
    let mut form = Form::new();
    if create {
        form = form
            .set("email", &customer.email)
            .set("metadata[org_id]", customer.org_id.to_string());
    }
    if create || customer.business_name.is_some() {
        form = form.set(
            "name",
            customer.business_name.as_deref().unwrap_or(&customer.name),
        );
    }
    form = form.set("preferred_locales[0]", customer.locale.tag());
    // Empty strings clear them in Stripe, which is what an unset footer means.
    form = form.set(
        "invoice_settings[footer]",
        customer.invoice_footer.clone().unwrap_or_default(),
    );
    if customer.invoice_fields.is_empty() {
        form = form.set("invoice_settings[custom_fields]", "");
    }
    for (n, (name, value)) in customer.invoice_fields.iter().enumerate() {
        form = form
            .set(format!("invoice_settings[custom_fields][{n}][name]"), name)
            .set(
                format!("invoice_settings[custom_fields][{n}][value]"),
                value,
            );
    }
    form
}

fn portal_form(portal: &NewPortal) -> Form {
    let mut form = Form::new()
        .set("customer", &portal.customer)
        .set("return_url", &portal.return_url);
    match &portal.flow {
        None => {}
        Some(PortalFlow::Cancel { subscription }) => {
            form = form
                .set("flow_data[type]", "subscription_cancel")
                .set("flow_data[subscription_cancel][subscription]", subscription)
                .set("flow_data[after_completion][type]", "redirect")
                .set(
                    "flow_data[after_completion][redirect][return_url]",
                    &portal.return_url,
                );
        }
        Some(PortalFlow::Update { subscription }) => {
            form = form
                .set("flow_data[type]", "subscription_update")
                .set("flow_data[subscription_update][subscription]", subscription);
        }
        Some(PortalFlow::Confirm {
            subscription,
            item,
            price,
        }) => {
            form = form
                .set("flow_data[type]", "subscription_update_confirm")
                .set(
                    "flow_data[subscription_update_confirm][subscription]",
                    subscription,
                )
                .set("flow_data[subscription_update_confirm][items][0][id]", item)
                .set(
                    "flow_data[subscription_update_confirm][items][0][price]",
                    price,
                )
                .set(
                    "flow_data[subscription_update_confirm][items][0][quantity]",
                    "1",
                )
                .set("flow_data[after_completion][type]", "redirect")
                .set(
                    "flow_data[after_completion][redirect][return_url]",
                    &portal.return_url,
                );
        }
    }
    form
}

/// A subscription's payment intent from its invoice: the invoice's payments
/// (API versions from basil on), else the older top-level field.
fn payment_intent_of(invoice: &Value) -> Option<String> {
    invoice["payments"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["status"] == "paid" || p["status"].is_null())
        .find_map(|p| id_of(&p["payment"]["payment_intent"]))
        .or_else(|| id_of(&invoice["payment_intent"]))
}

fn checkout_form(checkout: &NewCheckout) -> Form {
    let form = Form::new()
        .set("mode", "subscription")
        .set("locale", checkout.locale.tag())
        // The "Subscribe" button cannot be renamed: the words under it say
        // it is an order with an obligation to pay (Article L221-14). The
        // Terms are accepted on romulus's own page, with their version,
        // before this one: no consent_collection box repeating it here.
        .set(
            "custom_text[submit][message]",
            &super::copy::boxes(checkout.edition, checkout.locale).submit,
        )
        .set(
            "metadata[acceptance_id]",
            checkout.acceptance_id.to_string(),
        )
        .set(
            "subscription_data[metadata][acceptance_id]",
            checkout.acceptance_id.to_string(),
        )
        .set("customer", &checkout.customer)
        .set("client_reference_id", checkout.org_id.to_string())
        .set("line_items[0][price]", &checkout.price)
        .set("line_items[0][quantity]", "1")
        .set(
            "subscription_data[metadata][org_id]",
            checkout.org_id.to_string(),
        )
        .set("metadata[org_id]", checkout.org_id.to_string())
        // The billing address, and a business name and VAT number for who
        // buys as a business; names and addresses go on the customer.
        .set("billing_address_collection", "required")
        .set("tax_id_collection[enabled]", "true")
        .set("customer_update[address]", "auto")
        .set("customer_update[name]", "auto")
        // No Stripe Tax: VAT is not applicable (the invoices say why).
        .set("automatic_tax[enabled]", "false")
        .set("success_url", &checkout.success_url)
        .set("cancel_url", &checkout.cancel_url);
    // An offer's promotion code, applied without the buyer typing it (no
    // promotion code field on the page: Stripe takes one or the other).
    let form = match &checkout.promotion_code {
        Some(code) => form.set("discounts[0][promotion_code]", code),
        None => form,
    };
    let form = match checkout.expires_at {
        Some(at) => form.set("expires_at", at.to_string()),
        None => form,
    };
    // A business's name, required on the page (Stripe refuses `optional`
    // without `enabled`, so a consumer's form names neither).
    if checkout.business {
        form.set("name_collection[business][enabled]", "true")
            .set("name_collection[business][optional]", "false")
    } else {
        form
    }
}

/// A discount's coupon: under `source` (API versions from basil on), else on
/// the discount itself; `None` when not expanded.
fn coupon_of(discount: &Value) -> Option<&Value> {
    [&discount["source"]["coupon"], &discount["coupon"]]
        .into_iter()
        .find(|coupon| coupon.is_object())
}

/// A subscription's discounts (`discounts` expanded, else the older single
/// `discount`): those whose coupon is not expanded are left out.
fn parse_discounts(value: &Value) -> Vec<Discount> {
    let discounts: Vec<&Value> = match value["discounts"].as_array() {
        Some(list) if !list.is_empty() => list.iter().collect(),
        _ => vec![&value["discount"]],
    };
    discounts
        .into_iter()
        .filter_map(coupon_of)
        .filter_map(|coupon| {
            Some(Discount {
                coupon: text(&coupon["id"])?,
                offer: text(&coupon["metadata"]["romulus_offer"]),
                amount_off: coupon["amount_off"].as_i64(),
                percent_off: coupon["percent_off"].as_f64().map(hundredths),
                forever: coupon["duration"] == "forever",
                products: coupon["applies_to"]["products"]
                    .as_array()
                    .filter(|products| !products.is_empty())
                    .map(|products| products.iter().filter_map(text).collect()),
            })
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a percentage, far within i64"
)]
fn hundredths(percent: f64) -> i64 {
    (percent * 100.0).round() as i64
}

/// A promotion code from Stripe's JSON (its coupon included, as Stripe
/// sends it: `coupon`, or `promotion.coupon` in later versions).
pub fn parse_promotion_code(value: &Value) -> Result<PromotionCode, StripeError> {
    let coupon = [&value["coupon"], &value["promotion"]["coupon"]]
        .into_iter()
        .find(|coupon| coupon.is_object());
    Ok(PromotionCode {
        id: text(&value["id"])
            .ok_or_else(|| StripeError::Unexpected("promotion code id".into()))?,
        code: text(&value["code"]).unwrap_or_default(),
        active: value["active"].as_bool().unwrap_or(false),
        times_redeemed: value["times_redeemed"].as_i64().unwrap_or(0),
        max_redemptions: value["max_redemptions"].as_i64(),
        amount_off: coupon.and_then(|c| c["amount_off"].as_i64()),
        coupon: coupon.and_then(|c| text(&c["id"])),
    })
}

fn time(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(value.as_i64()?, 0)
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// An id, whether the field holds it or the expanded object.
fn id_of(value: &Value) -> Option<String> {
    text(value).or_else(|| text(&value["id"]))
}

/// A subscription from Stripe's JSON (its schedule expanded, if any).
pub fn parse_subscription(value: &Value) -> Result<Subscription, StripeError> {
    let id = text(&value["id"]).ok_or_else(|| StripeError::Unexpected("subscription id".into()))?;
    let first = &value["items"]["data"][0];
    let item = first.is_object().then(|| Item {
        id: text(&first["id"]).unwrap_or_default(),
        price: id_of(&first["price"]).unwrap_or_default(),
        lookup_key: text(&first["price"]["lookup_key"]),
        unit_amount: first["price"]["unit_amount"].as_i64(),
        currency: text(&first["price"]["currency"]),
        quantity: first["quantity"].as_i64().unwrap_or(1),
        product: id_of(&first["price"]["product"]),
    });
    let schedule = &value["schedule"];
    let scheduled = schedule["phases"].as_array().and_then(|phases| {
        let now = Utc::now();
        phases.iter().find_map(|phase| {
            let at = time(&phase["start_date"])?;
            if at <= now {
                return None;
            }
            let lookup_key = text(&phase["items"][0]["price"]["lookup_key"])?;
            Some(Scheduled { lookup_key, at })
        })
    });
    Ok(Subscription {
        id,
        customer: id_of(&value["customer"]).unwrap_or_default(),
        status: text(&value["status"]).unwrap_or_default(),
        cancel_at_period_end: value["cancel_at_period_end"].as_bool().unwrap_or(false)
            // A cancellation at a set date (the portal may use cancel_at).
            || (value["cancel_at"].is_i64() && value["status"] != "canceled"),
        current_period_end: time(&first["current_period_end"])
            .or_else(|| time(&value["current_period_end"])),
        item,
        org_id: value["metadata"]["org_id"]
            .as_str()
            .and_then(|id| id.parse().ok()),
        acceptance_id: value["metadata"]["acceptance_id"]
            .as_str()
            .and_then(|id| id.parse().ok()),
        started: time(&value["start_date"]),
        discounts: parse_discounts(value),
        latest_invoice: id_of(&value["latest_invoice"]),
        scheduled,
        cancellation: Some(Cancellation {
            reason: text(&value["cancellation_details"]["reason"]),
            feedback: text(&value["cancellation_details"]["feedback"]),
            comment: text(&value["cancellation_details"]["comment"]),
        })
        .filter(|said| *said != Cancellation::default()),
    })
}

pub fn parse_invoice(value: &Value) -> Result<Invoice, StripeError> {
    Ok(Invoice {
        id: text(&value["id"]).ok_or_else(|| StripeError::Unexpected("invoice id".into()))?,
        number: text(&value["number"]),
        status: text(&value["status"]),
        total: value["total"].as_i64().unwrap_or(0),
        currency: text(&value["currency"]).unwrap_or_else(|| "eur".to_owned()),
        created: time(&value["created"]).unwrap_or_else(Utc::now),
        due: time(&value["due_date"])
            .or_else(|| time(&value["status_transitions"]["finalized_at"]))
            .or_else(|| time(&value["created"])),
        hosted_invoice_url: text(&value["hosted_invoice_url"]),
        invoice_pdf: text(&value["invoice_pdf"]),
        subscription: id_of(&value["parent"]["subscription_details"]["subscription"])
            .or_else(|| id_of(&value["subscription"])),
        customer: id_of(&value["customer"]),
    })
}

/// A paid invoice from Stripe's JSON (its payments expanded): the period is
/// its lines' (the first line's start to the last one's end).
pub fn parse_paid_invoice(value: &Value) -> Result<PaidInvoice, StripeError> {
    let id = text(&value["id"]).ok_or_else(|| StripeError::Unexpected("invoice id".into()))?;
    let lines = value["lines"]["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let start = lines
        .iter()
        .filter_map(|l| time(&l["period"]["start"]))
        .min();
    let end = lines.iter().filter_map(|l| time(&l["period"]["end"])).max();
    let (Some(period_start), Some(period_end)) = (start, end) else {
        return Err(StripeError::Unexpected(format!("invoice {id}: no period")));
    };
    Ok(PaidInvoice {
        id,
        payment_intent: payment_intent_of(value),
        amount_paid: value["amount_paid"].as_i64().unwrap_or(0),
        currency: text(&value["currency"]).unwrap_or_else(|| "eur".to_owned()),
        period_start,
        period_end,
    })
}

impl StripeApi for Http {
    fn create_customer(&self, customer: NewCustomer) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move {
            let created = self
                .create(
                    "customers",
                    customer_form(&customer, true).pairs(),
                    &customer_key(customer.org_id),
                )
                .await?;
            text(&created["id"]).ok_or_else(|| StripeError::Unexpected("customer id".into()))
        })
    }

    fn update_customer(
        &self,
        id: String,
        customer: NewCustomer,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            self.post(
                &format!("customers/{id}"),
                customer_form(&customer, false).pairs(),
            )
            .await?;
            Ok(())
        })
    }

    fn price(&self, lookup_key: String) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move {
            let found = self
                .get(
                    "prices",
                    &[
                        ("lookup_keys[]".into(), lookup_key.clone()),
                        ("active".into(), "true".into()),
                    ],
                )
                .await?;
            text(&found["data"][0]["id"]).ok_or_else(|| StripeError::Refused {
                status: 404,
                code: Some("price_missing".into()),
                message: format!("no active price {lookup_key}: run `romulus-api stripe-setup`"),
            })
        })
    }

    fn checkout(
        &self,
        checkout: NewCheckout,
    ) -> BoxFuture<'_, Result<CheckoutSession, StripeError>> {
        Box::pin(async move {
            let session = self
                .create(
                    "checkout/sessions",
                    checkout_form(&checkout).pairs(),
                    &checkout.idempotency_key,
                )
                .await?;
            Ok(CheckoutSession {
                id: text(&session["id"])
                    .ok_or_else(|| StripeError::Unexpected("checkout id".into()))?,
                url: text(&session["url"])
                    .ok_or_else(|| StripeError::Unexpected("checkout url".into()))?,
            })
        })
    }

    fn open_checkouts(&self, customer: String) -> BoxFuture<'_, Result<Vec<String>, StripeError>> {
        Box::pin(async move {
            let list = self
                .get(
                    "checkout/sessions",
                    &[
                        ("customer".into(), customer),
                        ("status".into(), "open".into()),
                        ("limit".into(), "100".into()),
                    ],
                )
                .await?;
            Ok(list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|session| text(&session["id"]))
                .collect())
        })
    }

    fn expire_checkout(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            match self
                .post(&format!("checkout/sessions/{id}/expire"), &[])
                .await
            {
                Ok(_) => Ok(()),
                // Not open any more: expired is fine, paid is not.
                Err(StripeError::Refused { status: 400, .. }) => {
                    let session = self.get(&format!("checkout/sessions/{id}"), &[]).await?;
                    match session["status"].as_str() {
                        Some("expired") => Ok(()),
                        _ => Err(StripeError::Refused {
                            status: 409,
                            code: Some("checkout_complete".into()),
                            message: "a checkout of this organization was just paid".into(),
                        }),
                    }
                }
                Err(err) => Err(err),
            }
        })
    }

    fn live_subscriptions(
        &self,
        customer: String,
    ) -> BoxFuture<'_, Result<Vec<Subscription>, StripeError>> {
        Box::pin(async move {
            let list = self
                .get(
                    "subscriptions",
                    &[
                        ("customer".into(), customer),
                        ("status".into(), "all".into()),
                        ("limit".into(), "20".into()),
                    ],
                )
                .await?;
            Ok(list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| parse_subscription(value).ok())
                .filter(|subscription| super::state::is_live(&subscription.status))
                .collect())
        })
    }

    fn cancel_duplicate(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Option<Refund>, StripeError>> {
        Box::pin(async move {
            let read = parse_subscription(
                &self
                    .get(&format!("subscriptions/{subscription}"), &[])
                    .await?,
            )?;
            if read.status != "canceled" {
                self.delete(
                    &format!("subscriptions/{subscription}"),
                    &format!("romulus-cancel-{subscription}"),
                )
                .await?;
            }
            let Some(invoice) = read.latest_invoice else {
                return Ok(None);
            };
            let invoice = self
                .get(
                    &format!("invoices/{invoice}"),
                    &[("expand[]".into(), "payments".into())],
                )
                .await?;
            let paid = invoice["amount_paid"].as_i64().unwrap_or(0);
            let Some(intent) = payment_intent_of(&invoice).filter(|_| paid > 0) else {
                return Ok(None);
            };
            let refund = self
                .create(
                    "refunds",
                    Form::new()
                        .set("payment_intent", intent)
                        .set("reason", "duplicate")
                        .set("metadata[subscription]", &subscription)
                        .pairs(),
                    &format!("romulus-refund-{subscription}"),
                )
                .await?;
            Ok(Some(Refund {
                amount: refund["amount"].as_i64().unwrap_or(paid),
                currency: text(&refund["currency"]).unwrap_or_else(|| "eur".into()),
            }))
        })
    }

    fn paid_invoices(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Vec<PaidInvoice>, StripeError>> {
        Box::pin(async move {
            let list = self
                .get(
                    "invoices",
                    &[
                        ("subscription".into(), subscription),
                        ("status".into(), "paid".into()),
                        ("limit".into(), "20".into()),
                        ("expand[]".into(), "data.payments".into()),
                    ],
                )
                .await?;
            list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|invoice| invoice["amount_paid"].as_i64().unwrap_or(0) > 0)
                .map(parse_paid_invoice)
                .collect()
        })
    }

    fn withdraw(
        &self,
        subscription: String,
        withdrawal: Uuid,
        refunds: Vec<PlannedRefund>,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            let read = parse_subscription(
                &self
                    .get(&format!("subscriptions/{subscription}"), &[])
                    .await?,
            )?;
            if read.status != "canceled" {
                // Defaults: no proration credit, no final invoice; the
                // refunds below are the withdrawal's own.
                self.delete(
                    &format!("subscriptions/{subscription}"),
                    &format!("romulus-withdraw-{subscription}"),
                )
                .await?;
            }
            for refund in refunds.into_iter().filter(|r| r.amount > 0) {
                // What its payment had refunded already: this withdrawal's
                // own refund (made by an attempt that ended before romulus
                // heard back) is not made again, and no refund gives back
                // more than what is left of the payment.
                let made = parse_refunds(
                    &self
                        .get(
                            "refunds",
                            &[
                                ("payment_intent".into(), refund.payment_intent.clone()),
                                ("limit".into(), "100".into()),
                            ],
                        )
                        .await?,
                );
                let amount = still_due(&refund, withdrawal, &made);
                if amount <= 0 {
                    continue;
                }
                let made = self
                    .create(
                        "refunds",
                        Form::new()
                            .set("payment_intent", &refund.payment_intent)
                            .set("amount", amount.to_string())
                            .set("reason", "requested_by_customer")
                            .set("metadata[withdrawal]", withdrawal.to_string())
                            .set("metadata[invoice]", &refund.invoice)
                            .set("metadata[subscription]", &subscription)
                            .pairs(),
                        &format!("romulus-withdrawal-refund-{}-{amount}", refund.invoice),
                    )
                    .await;
                match made {
                    Ok(_) => {}
                    // Refunded already (by hand, in the Dashboard): nothing
                    // left to give back for it.
                    Err(StripeError::Refused {
                        code: Some(code), ..
                    }) if code == "charge_already_refunded" => {}
                    Err(err) => return Err(err),
                }
            }
            Ok(())
        })
    }

    fn portal(&self, portal: NewPortal) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move {
            let mut form = portal_form(&portal);
            if let Some(configuration) = self.portal_configuration().await? {
                form = form.set("configuration", configuration);
            }
            let session = self.post("billing_portal/sessions", form.pairs()).await?;
            text(&session["url"]).ok_or_else(|| StripeError::Unexpected("portal url".into()))
        })
    }

    fn subscription(&self, id: String) -> BoxFuture<'_, Result<Subscription, StripeError>> {
        Box::pin(async move {
            let value = self
                .get(
                    &format!("subscriptions/{id}"),
                    &[
                        ("expand[]".into(), "schedule.phases.items.price".into()),
                        // Their coupons, and the products each applies
                        // to: what each period costs.
                        ("expand[]".into(), "discounts.source.coupon".into()),
                        (
                            "expand[]".into(),
                            "discounts.source.coupon.applies_to".into(),
                        ),
                    ],
                )
                .await?;
            parse_subscription(&value)
        })
    }

    fn invoice(&self, id: String) -> BoxFuture<'_, Result<Invoice, StripeError>> {
        Box::pin(async move { parse_invoice(&self.get(&format!("invoices/{id}"), &[]).await?) })
    }

    fn invoices(&self, customer: String) -> BoxFuture<'_, Result<Vec<Invoice>, StripeError>> {
        Box::pin(async move {
            let list = self
                .get(
                    "invoices",
                    &[("customer".into(), customer), ("limit".into(), "24".into())],
                )
                .await?;
            list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .map(parse_invoice)
                .collect()
        })
    }

    fn promotion_code(&self, id: String) -> BoxFuture<'_, Result<PromotionCode, StripeError>> {
        Box::pin(async move {
            parse_promotion_code(&self.get(&format!("promotion_codes/{id}"), &[]).await?)
        })
    }

    fn deactivate_promotion_code(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            self.post(
                &format!("promotion_codes/{id}"),
                Form::new().set("active", "false").pairs(),
            )
            .await?;
            Ok(())
        })
    }

    fn set_discounts(
        &self,
        subscription: String,
        coupons: Vec<String>,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            // An empty value removes them all.
            let mut form = Form::new();
            if coupons.is_empty() {
                form = form.set("discounts", "");
            }
            for (n, coupon) in coupons.into_iter().enumerate() {
                form = form.set(format!("discounts[{n}][coupon]"), coupon);
            }
            self.post(
                &format!("subscriptions/{subscription}"),
                form.set("proration_behavior", "none").pairs(),
            )
            .await?;
            Ok(())
        })
    }
}

impl Http {
    /// The portal configuration `stripe-setup` made (metadata `romulus:
    /// portal`), or the account's default when there is none.
    async fn portal_configuration(&self) -> Result<Option<String>, StripeError> {
        let list = self
            .get(
                "billing_portal/configurations",
                &[
                    ("active".into(), "true".into()),
                    ("limit".into(), "100".into()),
                ],
            )
            .await?;
        Ok(list["data"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["metadata"]["romulus"] == "portal")
            .and_then(|c| text(&c["id"])))
    }
}

/// Stripe in memory: customers, prices named after their lookup keys,
/// checkouts a test (or the e2e suite's fake checkout page) completes,
/// subscriptions it changes, and every portal session asked for.
#[derive(Clone, Default)]
pub struct Fake(Arc<Mutex<FakeState>>);

#[derive(Default)]
pub struct FakeState {
    pub customers: HashMap<String, NewCustomer>,
    /// Checkout session id → what it was for.
    pub checkouts: HashMap<String, NewCheckout>,
    pub subscriptions: HashMap<String, Subscription>,
    pub invoices: Vec<Invoice>,
    pub portals: Vec<NewPortal>,
    /// Promotion code id → the code, its redemptions counted as checkouts
    /// using it complete ([`Fake::add_promotion_code`]).
    pub promotion_codes: HashMap<String, PromotionCode>,
    /// Coupon id → the discount it makes (a promotion code's coupon).
    pub coupons: HashMap<String, Discount>,
    /// Promotion codes read so far (tests: the count is cached).
    pub promotion_reads: usize,
    /// Subscriptions' discounts changed by romulus (tests).
    pub discount_changes: usize,
    /// Promotion code reads fail (tests: Stripe down).
    pub promotions_down: bool,
    /// Checkout session id → `open`, `complete` or `expired`.
    pub session_status: HashMap<String, String>,
    /// Idempotency key → what the first request with it made.
    pub keys: HashMap<String, String>,
    /// Subscriptions cancelled as duplicates, with what was refunded.
    pub refunds: Vec<(String, i64)>,
    /// Withdrawals' refunds: invoice, amount.
    pub withdrawal_refunds: Vec<(String, i64)>,
    /// Every refund made on a payment intent (withdrawals' and others',
    /// e.g. by hand in the Dashboard: tests add them).
    pub payment_refunds: HashMap<String, Vec<MadeRefund>>,
    /// Paid invoices with no payment intent (paid from the balance).
    pub without_payment: Vec<String>,
    /// The next withdrawal fails (tests: Stripe down midway).
    pub fail_withdrawal: bool,
    /// Every withdrawal fails (tests: Stripe down for good).
    pub withdrawals_down: bool,
    /// Subscriptions read so far (tests: a webhook reading again).
    pub reads: usize,
    /// The next subscription read signals the first, then waits for the
    /// second (tests: what holds while Stripe answers).
    pub read_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    next: u64,
    /// Ids are unique across processes (a database outlives the fake: its
    /// `customer_id` and `subscription_id` are unique).
    nonce: String,
    /// Where the fake checkout page lives (`/api/billing/fake/checkout/{id}`).
    pub base_url: String,
}

impl Fake {
    /// Its checkout URLs go to `base_url` + `/api/billing/fake/checkout/{id}`.
    pub fn new(base_url: &str) -> Self {
        let fake = Self::default();
        base_url
            .trim_end_matches('/')
            .clone_into(&mut fake.state().base_url);
        fake.state().nonce = crate::token::generate()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        fake
    }

    /// A promotion code `id` of a coupon taking `amount_off` cents off every
    /// period, forever, at most `max_redemptions` times.
    /// A promotion code `id` of `coupon` (as [`Fake::add_coupon`] made it),
    /// at most `max_redemptions` times.
    pub fn add_promotion_code(&self, id: &str, code: &str, coupon: &str, max_redemptions: i64) {
        let mut state = self.state();
        let amount_off = state.coupons.get(coupon).and_then(|c| c.amount_off);
        state.promotion_codes.insert(
            id.to_owned(),
            PromotionCode {
                id: id.to_owned(),
                code: code.to_owned(),
                active: true,
                times_redeemed: 0,
                max_redemptions: Some(max_redemptions),
                amount_off,
                coupon: Some(coupon.to_owned()),
            },
        );
    }

    /// A coupon: `discount`, under its own id.
    pub fn add_coupon(&self, discount: Discount) {
        self.state()
            .coupons
            .insert(discount.coupon.clone(), discount);
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn id(&self, prefix: &str) -> String {
        let mut state = self.state();
        state.next += 1;
        format!("{prefix}_fake{}_{}", state.nonce, state.next)
    }

    /// Completes checkout `session`: an active subscription to its price,
    /// for a month or a year from now. Its id.
    /// `None` when it is not open (expired, or paid already).
    pub fn complete_checkout(&self, session: &str) -> Option<String> {
        let checkout = self.state().checkouts.get(session).cloned()?;
        if self.state().session_status.get(session).map(String::as_str) != Some("open") {
            return None;
        }
        let id = self.id("sub");
        let lookup_key = checkout.price.trim_start_matches("price_").to_owned();
        let year = lookup_key.ends_with("_yearly");
        let (plan, interval) = crate::domain::Plan::from_lookup_key(&lookup_key)?;
        // An expired session cannot be paid.
        if checkout
            .expires_at
            .is_some_and(|at| at <= Utc::now().timestamp())
        {
            self.state()
                .session_status
                .insert(session.to_owned(), "expired".into());
            return None;
        }
        // Its promotion code redeemed, as Stripe counts it once paid; one
        // used up or switched off refuses the payment.
        let discounts = match &checkout.promotion_code {
            None => Vec::new(),
            Some(code) => {
                let mut state = self.state();
                let promotion = state.promotion_codes.get_mut(code)?;
                if !promotion.active
                    || promotion
                        .max_redemptions
                        .is_some_and(|max| promotion.times_redeemed >= max)
                {
                    return None;
                }
                promotion.times_redeemed += 1;
                let coupon = promotion.coupon.clone()?;
                vec![state.coupons.get(&coupon).cloned()?]
            }
        };
        let subscription = Subscription {
            id: id.clone(),
            customer: checkout.customer.clone(),
            status: "active".into(),
            cancel_at_period_end: false,
            current_period_end: Some(
                Utc::now() + chrono::Duration::days(if year { 365 } else { 30 }),
            ),
            item: Some(Item {
                id: self.id("si"),
                price: checkout.price.clone(),
                lookup_key: Some(lookup_key),
                unit_amount: plan.price_cents(interval).map(i64::from),
                currency: Some("eur".into()),
                quantity: 1,
                product: plan.product_id(),
            }),
            org_id: Some(checkout.org_id),
            acceptance_id: Some(checkout.acceptance_id),
            started: Some(Utc::now()),
            discounts,
            latest_invoice: None,
            scheduled: None,
            cancellation: None,
        };
        let invoice_id = self.id("in");
        let subscription = Subscription {
            latest_invoice: Some(invoice_id.clone()),
            ..subscription
        };
        let invoice = Invoice {
            id: invoice_id,
            number: Some(format!("ROM-{}", self.state().invoices.len() + 1)),
            status: Some("paid".into()),
            total: subscription
                .item
                .as_ref()
                .and_then(|i| i.unit_amount)
                .unwrap_or(0)
                - subscription.discount_cents().unwrap_or(0),
            currency: "eur".into(),
            created: Utc::now(),
            due: Some(Utc::now()),
            hosted_invoice_url: Some("https://invoice.stripe.com/i/fake".into()),
            invoice_pdf: Some("https://pay.stripe.com/invoice/fake/pdf".into()),
            subscription: Some(id.clone()),
            customer: Some(checkout.customer),
        };
        let mut state = self.state();
        state.invoices.push(invoice);
        state.subscriptions.insert(id.clone(), subscription);
        state
            .session_status
            .insert(session.to_owned(), "complete".into());
        Some(id)
    }

    /// Changes a subscription the way Stripe would (a renewal, a failed
    /// payment, a cancellation).
    pub fn update(&self, id: &str, change: impl FnOnce(&mut Subscription)) {
        if let Some(subscription) = self.state().subscriptions.get_mut(id) {
            change(subscription);
        }
    }

    pub fn insert(&self, subscription: Subscription) {
        self.state()
            .subscriptions
            .insert(subscription.id.clone(), subscription);
    }

    pub fn last_portal(&self) -> Option<NewPortal> {
        self.state().portals.last().cloned()
    }

    pub fn last_checkout(&self) -> Option<(String, NewCheckout)> {
        let state = self.state();
        let mut all: Vec<_> = state.checkouts.iter().collect();
        // The newest: the highest number, after the last `_`.
        all.sort_by_key(|(id, _)| {
            id.rsplit('_')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        });
        all.last().map(|(id, c)| ((*id).clone(), (*c).clone()))
    }
}

/// The fake's portal URLs: Stripe's host, so a test sees where the button
/// goes (and the e2e suite intercepts it).
pub const FAKE_PORTAL: &str = "https://billing.stripe.com/p/session/test_fake";

impl StripeApi for Fake {
    fn create_customer(&self, customer: NewCustomer) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move {
            let key = customer_key(customer.org_id);
            if let Some(id) = self.state().keys.get(&key) {
                return Ok(id.clone());
            }
            let id = self.id("cus");
            let mut state = self.state();
            state.customers.insert(id.clone(), customer);
            state.keys.insert(key, id.clone());
            Ok(id)
        })
    }

    fn update_customer(
        &self,
        id: String,
        customer: NewCustomer,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            self.state().customers.insert(id, customer);
            Ok(())
        })
    }

    fn price(&self, lookup_key: String) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move { Ok(format!("price_{lookup_key}")) })
    }

    fn checkout(
        &self,
        checkout: NewCheckout,
    ) -> BoxFuture<'_, Result<CheckoutSession, StripeError>> {
        Box::pin(async move {
            let known = self.state().keys.get(&checkout.idempotency_key).cloned();
            let id = if let Some(id) = known {
                id
            } else {
                let id = self.id("cs");
                let mut state = self.state();
                state
                    .keys
                    .insert(checkout.idempotency_key.clone(), id.clone());
                state.session_status.insert(id.clone(), "open".into());
                state.checkouts.insert(id.clone(), checkout);
                id
            };
            let url = format!("{}/api/billing/fake/checkout/{id}", self.state().base_url);
            Ok(CheckoutSession { id, url })
        })
    }

    fn open_checkouts(&self, customer: String) -> BoxFuture<'_, Result<Vec<String>, StripeError>> {
        Box::pin(async move {
            let state = self.state();
            Ok(state
                .checkouts
                .iter()
                .filter(|(id, c)| {
                    c.customer == customer
                        && state.session_status.get(*id).map(String::as_str) == Some("open")
                })
                .map(|(id, _)| id.clone())
                .collect())
        })
    }

    fn expire_checkout(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            let mut state = self.state();
            if state.session_status.get(&id).map(String::as_str) == Some("complete") {
                return Err(StripeError::Refused {
                    status: 409,
                    code: Some("checkout_complete".into()),
                    message: "a checkout of this organization was just paid".into(),
                });
            }
            state.session_status.insert(id, "expired".into());
            Ok(())
        })
    }

    fn live_subscriptions(
        &self,
        customer: String,
    ) -> BoxFuture<'_, Result<Vec<Subscription>, StripeError>> {
        Box::pin(async move {
            Ok(self
                .state()
                .subscriptions
                .values()
                .filter(|s| s.customer == customer && super::state::is_live(&s.status))
                .cloned()
                .collect())
        })
    }

    fn cancel_duplicate(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Option<Refund>, StripeError>> {
        Box::pin(async move {
            let mut state = self.state();
            if let Some((_, amount)) = state.refunds.iter().find(|(id, _)| *id == subscription) {
                return Ok(Some(Refund {
                    amount: *amount,
                    currency: "eur".into(),
                }));
            }
            let Some(read) = state.subscriptions.get_mut(&subscription) else {
                return Err(StripeError::Refused {
                    status: 404,
                    code: Some("resource_missing".into()),
                    message: format!("No such subscription: '{subscription}'"),
                });
            };
            read.status = "canceled".into();
            let charged = read.item.as_ref().and_then(|i| i.unit_amount).unwrap_or(0);
            if charged == 0 {
                return Ok(None);
            }
            state.refunds.push((subscription, charged));
            Ok(Some(Refund {
                amount: charged,
                currency: "eur".into(),
            }))
        })
    }

    fn paid_invoices(
        &self,
        subscription: String,
    ) -> BoxFuture<'_, Result<Vec<PaidInvoice>, StripeError>> {
        Box::pin(async move {
            let state = self.state();
            let end = state
                .subscriptions
                .get(&subscription)
                .and_then(|s| s.current_period_end)
                .unwrap_or_else(Utc::now);
            Ok(state
                .invoices
                .iter()
                .filter(|i| {
                    i.subscription.as_deref() == Some(subscription.as_str())
                        && i.status.as_deref() == Some("paid")
                        && i.total > 0
                })
                .map(|i| PaidInvoice {
                    id: i.id.clone(),
                    payment_intent: (!state.without_payment.contains(&i.id))
                        .then(|| format!("pi_for_{}", i.id)),
                    amount_paid: i.total,
                    currency: i.currency.clone(),
                    period_start: i.created,
                    period_end: end,
                })
                .collect())
        })
    }

    fn withdraw(
        &self,
        subscription: String,
        withdrawal: Uuid,
        refunds: Vec<PlannedRefund>,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            let mut state = self.state();
            if std::mem::take(&mut state.fail_withdrawal) || state.withdrawals_down {
                return Err(StripeError::Unreachable("the fake is down".into()));
            }
            if let Some(read) = state.subscriptions.get_mut(&subscription) {
                read.status = "canceled".into();
                read.cancel_at_period_end = false;
            }
            for refund in refunds.into_iter().filter(|r| r.amount > 0) {
                let made = state
                    .payment_refunds
                    .get(&refund.payment_intent)
                    .cloned()
                    .unwrap_or_default();
                let amount = still_due(&refund, withdrawal, &made);
                if amount <= 0 {
                    continue;
                }
                state
                    .payment_refunds
                    .entry(refund.payment_intent.clone())
                    .or_default()
                    .push(MadeRefund {
                        amount,
                        status: "succeeded".into(),
                        withdrawal: Some(withdrawal.to_string()),
                    });
                state.withdrawal_refunds.push((refund.invoice, amount));
            }
            Ok(())
        })
    }

    fn portal(&self, portal: NewPortal) -> BoxFuture<'_, Result<String, StripeError>> {
        Box::pin(async move {
            let flow = match &portal.flow {
                None => "home",
                Some(PortalFlow::Cancel { .. }) => "subscription_cancel",
                Some(PortalFlow::Update { .. }) => "subscription_update",
                Some(PortalFlow::Confirm { .. }) => "subscription_update_confirm",
            };
            self.state().portals.push(portal);
            Ok(format!("{FAKE_PORTAL}?flow={flow}"))
        })
    }

    fn subscription(&self, id: String) -> BoxFuture<'_, Result<Subscription, StripeError>> {
        Box::pin(async move {
            let gate = {
                let mut state = self.state();
                state.reads += 1;
                state.read_gate.take()
            };
            if let Some((reached, release)) = gate {
                reached.notify_one();
                release.notified().await;
            }
            self.state()
                .subscriptions
                .get(&id)
                .cloned()
                .ok_or_else(|| StripeError::Refused {
                    status: 404,
                    code: Some("resource_missing".into()),
                    message: format!("No such subscription: '{id}'"),
                })
        })
    }

    fn invoice(&self, id: String) -> BoxFuture<'_, Result<Invoice, StripeError>> {
        Box::pin(async move {
            self.state()
                .invoices
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .ok_or_else(|| StripeError::Refused {
                    status: 404,
                    code: Some("resource_missing".into()),
                    message: format!("No such invoice: '{id}'"),
                })
        })
    }

    fn invoices(&self, customer: String) -> BoxFuture<'_, Result<Vec<Invoice>, StripeError>> {
        Box::pin(async move {
            let mut invoices: Vec<Invoice> = self
                .state()
                .invoices
                .iter()
                .filter(|i| i.customer.as_deref() == Some(customer.as_str()))
                .cloned()
                .collect();
            invoices.reverse();
            Ok(invoices)
        })
    }

    fn promotion_code(&self, id: String) -> BoxFuture<'_, Result<PromotionCode, StripeError>> {
        Box::pin(async move {
            let mut state = self.state();
            state.promotion_reads += 1;
            if state.promotions_down {
                return Err(StripeError::Unreachable("the fake is down".into()));
            }
            state
                .promotion_codes
                .get(&id)
                .cloned()
                .ok_or_else(|| StripeError::Refused {
                    status: 404,
                    code: Some("resource_missing".into()),
                    message: format!("No such promotion code: '{id}'"),
                })
        })
    }

    fn deactivate_promotion_code(&self, id: String) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            if let Some(code) = self.state().promotion_codes.get_mut(&id) {
                code.active = false;
            }
            Ok(())
        })
    }

    fn set_discounts(
        &self,
        subscription: String,
        coupons: Vec<String>,
    ) -> BoxFuture<'_, Result<(), StripeError>> {
        Box::pin(async move {
            let mut state = self.state();
            let discounts = coupons
                .iter()
                .map(|coupon| {
                    state
                        .coupons
                        .get(coupon)
                        .cloned()
                        .ok_or_else(|| StripeError::Refused {
                            status: 400,
                            code: Some("resource_missing".into()),
                            message: format!("No such coupon: '{coupon}'"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let read =
                state
                    .subscriptions
                    .get_mut(&subscription)
                    .ok_or_else(|| StripeError::Refused {
                        status: 404,
                        code: Some("resource_missing".into()),
                        message: format!("No such subscription: '{subscription}'"),
                    })?;
            read.discounts = discounts;
            state.discount_changes += 1;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_withdrawal_refund_is_made_once_and_never_more_than_is_left() {
        let ours = Uuid::from_u128(7);
        let planned = PlannedRefund {
            invoice: "in_1".into(),
            payment_intent: "pi_1".into(),
            amount: 1_700,
            paid: 1_900,
        };
        let made = |amount: i64, status: &str, withdrawal: Option<Uuid>| MadeRefund {
            amount,
            status: status.into(),
            withdrawal: withdrawal.map(|w| w.to_string()),
        };
        assert_eq!(still_due(&planned, ours, &[]), 1_700);
        // Made by an earlier attempt of this withdrawal: not again.
        assert_eq!(
            still_due(&planned, ours, &[made(1_700, "succeeded", Some(ours))]),
            0
        );
        assert_eq!(
            still_due(&planned, ours, &[made(1_700, "pending", Some(ours))]),
            0
        );
        // That attempt's refund failed: it is made again.
        assert_eq!(
            still_due(&planned, ours, &[made(1_700, "failed", Some(ours))]),
            1_700
        );
        // 500 refunded by hand: at most the 1,400 left.
        assert_eq!(
            still_due(&planned, ours, &[made(500, "succeeded", None)]),
            1_400
        );
        // All of it refunded already: nothing.
        assert_eq!(
            still_due(&planned, ours, &[made(1_900, "succeeded", None)]),
            0
        );
        // Another withdrawal's refund counts as refunded, not as ours.
        assert_eq!(
            still_due(
                &planned,
                ours,
                &[made(100, "succeeded", Some(Uuid::from_u128(8)))]
            ),
            1_700
        );
    }

    #[test]
    fn a_subscription_reads_its_period_from_its_item_or_itself() {
        let basil = json!({
            "id": "sub_1", "customer": "cus_1", "status": "active", "cancel_at_period_end": false,
            "items": {"data": [{"id": "si_1", "quantity": 1, "current_period_end": 1_800_000_000,
                "price": {"id": "price_1", "lookup_key": "team_yearly", "unit_amount": 49000, "currency": "eur"}}]},
            "metadata": {"org_id": "0192f0c4-0000-7000-8000-000000000001"},
            "latest_invoice": "in_1", "schedule": null
        });
        let parsed = parse_subscription(&basil).unwrap();
        assert_eq!(
            parsed.current_period_end,
            DateTime::from_timestamp(1_800_000_000, 0)
        );
        assert_eq!(
            parsed.item.as_ref().unwrap().lookup_key.as_deref(),
            Some("team_yearly")
        );
        assert_eq!(parsed.item.as_ref().unwrap().unit_amount, Some(49_000));
        assert!(parsed.org_id.is_some());
        assert_eq!(parsed.latest_invoice.as_deref(), Some("in_1"));
        assert!(!parsed.cancel_at_period_end);

        let older = json!({
            "id": "sub_2", "customer": {"id": "cus_2"}, "status": "past_due",
            "current_period_end": 1_700_000_000, "cancel_at_period_end": true,
            "items": {"data": [{"id": "si_2", "price": {"id": "price_2"}}]}
        });
        let parsed = parse_subscription(&older).unwrap();
        assert_eq!(parsed.customer, "cus_2");
        assert_eq!(
            parsed.current_period_end,
            DateTime::from_timestamp(1_700_000_000, 0)
        );
        assert!(parsed.cancel_at_period_end);
        assert_eq!(parsed.item.unwrap().lookup_key, None);
    }

    #[test]
    fn a_scheduled_downgrade_is_the_next_phase_to_start() {
        let future = Utc::now().timestamp() + 86_400;
        let value = json!({
            "id": "sub_1", "status": "active",
            "schedule": {"phases": [
                {"start_date": 1_600_000_000, "items": [{"price": {"lookup_key": "team_monthly"}}]},
                {"start_date": future, "items": [{"price": {"lookup_key": "starter_monthly"}}]}
            ]}
        });
        let scheduled = parse_subscription(&value).unwrap().scheduled.unwrap();
        assert_eq!(scheduled.lookup_key, "starter_monthly");
        assert_eq!(scheduled.at.timestamp(), future);
    }

    #[test]
    fn an_invoice_finds_its_subscription_under_parent_or_on_itself() {
        let basil = json!({"id": "in_1", "total": 1900, "currency": "eur", "created": 1_700_000_000,
            "parent": {"subscription_details": {"subscription": "sub_1"}}, "due_date": null,
            "status_transitions": {"finalized_at": 1_700_000_100}});
        let parsed = parse_invoice(&basil).unwrap();
        assert_eq!(parsed.subscription.as_deref(), Some("sub_1"));
        assert_eq!(parsed.due, DateTime::from_timestamp(1_700_000_100, 0));
        let older = json!({"id": "in_2", "subscription": "sub_2", "created": 1_700_000_000});
        assert_eq!(
            parse_invoice(&older).unwrap().subscription.as_deref(),
            Some("sub_2")
        );
    }

    #[test]
    fn the_cancel_flow_goes_straight_to_the_subscription_cancel_page() {
        let form = portal_form(&NewPortal {
            customer: "cus_1".into(),
            return_url: "https://r.example/app/acme/settings/billing".into(),
            flow: Some(PortalFlow::Cancel {
                subscription: "sub_1".into(),
            }),
        });
        let pairs: HashMap<_, _> = form.pairs().iter().cloned().collect();
        assert_eq!(pairs["flow_data[type]"], "subscription_cancel");
        assert_eq!(
            pairs["flow_data[subscription_cancel][subscription]"],
            "sub_1"
        );
        assert_eq!(pairs["flow_data[after_completion][type]"], "redirect");
    }

    #[test]
    fn checkout_collects_the_address_and_tax_ids_without_stripe_tax() {
        let form = checkout_form(&NewCheckout {
            org_id: Uuid::nil(),
            customer: "cus_1".into(),
            price: "price_1".into(),
            success_url: "s".into(),
            cancel_url: "c".into(),
            idempotency_key: checkout_key(Uuid::nil(), "price_1", Locale::Fr, None, "n"),
            acceptance_id: Uuid::from_u128(5),
            locale: Locale::Fr,
            edition: crate::domain::TermsEdition::Professional,
            business: true,
            promotion_code: None,
            expires_at: None,
        });
        let pairs: HashMap<_, _> = form.pairs().iter().cloned().collect();
        assert_eq!(pairs["mode"], "subscription");
        assert_eq!(pairs["billing_address_collection"], "required");
        assert_eq!(pairs["tax_id_collection[enabled]"], "true");
        assert_eq!(pairs["automatic_tax[enabled]"], "false");
        assert_eq!(pairs["customer_update[name]"], "auto");
        assert_eq!(
            pairs["subscription_data[metadata][org_id]"],
            Uuid::nil().to_string()
        );
        // The payment page in the buyer's language, its button an order
        // with an obligation to pay, the record carried along.
        assert_eq!(pairs["locale"], "fr");
        assert!(pairs["custom_text[submit][message]"].contains("obligation de paiement"));
        assert!(pairs["custom_text[submit][message]"].contains("hors taxes"));
        // The business named on the payment page is the Customer.
        assert_eq!(pairs["name_collection[business][enabled]"], "true");
        assert_eq!(pairs["name_collection[business][optional]"], "false");
        assert_eq!(
            pairs["subscription_data[metadata][acceptance_id]"],
            Uuid::from_u128(5).to_string()
        );
        assert!(!pairs.contains_key("consent_collection[terms_of_service]"));
        // A consumer is asked no business name (and Stripe refuses
        // `optional` alone).
        let consumer = checkout_form(&NewCheckout {
            business: false,
            ..NewCheckout {
                org_id: Uuid::nil(),
                customer: "cus_1".into(),
                price: "price_1".into(),
                success_url: "s".into(),
                cancel_url: "c".into(),
                idempotency_key: "k".into(),
                acceptance_id: Uuid::nil(),
                locale: Locale::En,
                edition: crate::domain::TermsEdition::Full,
                business: true,
                promotion_code: Some("promo_1".into()),
                expires_at: Some(1_800_001_800),
            }
        });
        assert!(
            consumer
                .pairs()
                .iter()
                .all(|(key, _)| !key.starts_with("name_collection"))
        );
        // An offer's promotion code is applied, not typed; none otherwise.
        let consumer: HashMap<_, _> = consumer.pairs().iter().cloned().collect();
        assert_eq!(consumer["discounts[0][promotion_code]"], "promo_1");
        // An offered session holds its place for a while only.
        assert_eq!(consumer["expires_at"], "1800001800");
        assert!(!pairs.contains_key("expires_at"));
        assert!(!pairs.contains_key("discounts[0][promotion_code]"));
        assert!(!pairs.contains_key("allow_promotion_codes"));
    }

    #[test]
    fn a_subscription_takes_off_what_its_forever_coupons_for_its_product_do() {
        let item = json!({"data": [{"id": "si_1",
            "price": {"id": "price_1", "unit_amount": 4900, "product": "romulus_team"}}]});
        let founding = json!({"id": "founding_team_monthly", "amount_off": 2000,
            "duration": "forever", "metadata": {"romulus_offer": "founding"},
            "applies_to": {"products": ["romulus_team"]}});
        let basil = json!({"id": "sub_1", "items": item, "discounts": [
            {"id": "di_1", "source": {"type": "coupon", "coupon": founding}},
            {"id": "di_2", "source": {"type": "coupon", "coupon": {"id": "welcome",
                "amount_off": 500, "duration": "once"}}}
        ]});
        let read = parse_subscription(&basil).unwrap();
        assert_eq!(read.discount_cents(), Some(2_000));
        assert_eq!(
            read.item.as_ref().unwrap().product.as_deref(),
            Some("romulus_team")
        );
        assert_eq!(
            read.offer_discounts("founding")
                .map(|d| d.coupon.as_str())
                .collect::<Vec<_>>(),
            ["founding_team_monthly"]
        );
        // The same coupon on another product's item takes nothing off.
        let business = json!({"data": [{"id": "si_1",
            "price": {"id": "price_2", "unit_amount": 14900, "product": "romulus_business"}}]});
        let moved = json!({"id": "sub_1", "items": business, "discounts": [
            {"id": "di_1", "source": {"type": "coupon", "coupon": founding}}]});
        assert_eq!(parse_subscription(&moved).unwrap().discount_cents(), None);
        // Older versions: one discount, its coupon on it; no restriction.
        let older = json!({"id": "sub_2", "items": item,
            "discount": {"coupon": {"id": "c", "percent_off": 10.0, "duration": "forever"}}});
        assert_eq!(
            parse_subscription(&older).unwrap().discount_cents(),
            Some(490)
        );
        // Not expanded, or none: nothing known to take off.
        let ids = json!({"id": "sub_3", "items": item, "discounts": ["di_1"]});
        assert_eq!(parse_subscription(&ids).unwrap().discount_cents(), None);
        assert_eq!(
            parse_subscription(&json!({"id": "sub_4", "items": item, "discounts": []}))
                .unwrap()
                .discount_cents(),
            None
        );
    }

    #[test]
    fn a_promotion_code_reads_its_redemptions_and_coupon() {
        let basil = json!({"id": "promo_1", "code": "FOUNDINGTEAM", "active": true,
            "times_redeemed": 3, "max_redemptions": 20,
            "coupon": {"id": "founding_team_monthly", "amount_off": 2000}});
        let read = parse_promotion_code(&basil).unwrap();
        assert_eq!(
            (
                read.active,
                read.times_redeemed,
                read.max_redemptions,
                read.amount_off
            ),
            (true, 3, Some(20), Some(2_000))
        );
        assert_eq!(read.coupon.as_deref(), Some("founding_team_monthly"));
        let later = json!({"id": "promo_2", "active": false,
            "promotion": {"type": "coupon", "coupon": {"amount_off": 20000}}});
        let read = parse_promotion_code(&later).unwrap();
        assert_eq!(
            (read.active, read.times_redeemed, read.amount_off),
            (false, 0, Some(20_000))
        );
    }

    #[test]
    fn idempotency_keys_are_made_of_the_operation_never_random() {
        let org = Uuid::from_u128(7);
        assert_eq!(customer_key(org), customer_key(org));
        assert_ne!(customer_key(org), customer_key(Uuid::from_u128(8)));
        let key = checkout_key(org, "price_team_yearly", Locale::En, None, "n1");
        assert_eq!(
            key,
            checkout_key(org, "price_team_yearly", Locale::En, None, "n1")
        );
        assert_ne!(
            key,
            checkout_key(org, "price_team_monthly", Locale::En, None, "n1")
        );
        assert_ne!(
            key,
            checkout_key(org, "price_team_yearly", Locale::En, None, "n2")
        );
        assert_ne!(
            key,
            checkout_key(org, "price_team_yearly", Locale::Fr, None, "n1")
        );
        assert_ne!(
            key,
            checkout_key(org, "price_team_yearly", Locale::En, Some("promo_1"), "n1")
        );
        assert!(key.len() <= 255, "Stripe takes keys up to 255 characters");
    }

    #[test]
    fn a_refund_finds_the_payment_intent_in_the_invoice_payments_or_on_it() {
        let basil = json!({"payments": {"data": [
            {"status": "canceled", "payment": {"payment_intent": "pi_old"}},
            {"status": "paid", "payment": {"type": "payment_intent", "payment_intent": "pi_1"}}
        ]}});
        assert_eq!(payment_intent_of(&basil).as_deref(), Some("pi_1"));
        let older = json!({"payment_intent": {"id": "pi_2"}});
        assert_eq!(payment_intent_of(&older).as_deref(), Some("pi_2"));
        assert_eq!(payment_intent_of(&json!({})), None);
    }

    #[test]
    fn the_customer_carries_the_invoice_mentions() {
        let customer = NewCustomer {
            org_id: Uuid::nil(),
            name: "Acme".into(),
            email: "a@acme.test".into(),
            invoice_footer: Some("Footer".into()),
            invoice_fields: vec![("Seller".into(), "Someone EI".into())],
            locale: Locale::Fr,
            business_name: Some("Acme SAS".into()),
        };
        let pairs: HashMap<_, _> = customer_form(&customer, true)
            .pairs()
            .iter()
            .cloned()
            .collect();
        assert_eq!(pairs["invoice_settings[footer]"], "Footer");
        assert_eq!(pairs["invoice_settings[custom_fields][0][name]"], "Seller");
        assert_eq!(
            pairs["invoice_settings[custom_fields][0][value]"],
            "Someone EI"
        );
        assert_eq!(pairs["metadata[org_id]"], Uuid::nil().to_string());
        assert_eq!(pairs["name"], "Acme SAS");
        assert_eq!(pairs["preferred_locales[0]"], "fr");
    }

    #[test]
    fn a_paid_invoice_covers_its_lines_period() {
        let value = json!({"id": "in_1", "amount_paid": 1900, "currency": "eur",
        "payments": {"data": [{"status": "paid", "payment": {"payment_intent": "pi_1"}}]},
        "lines": {"data": [
            {"period": {"start": 1_800_000_000, "end": 1_802_678_400}},
            {"period": {"start": 1_800_086_400, "end": 1_802_678_400}}
        ]}});
        let paid = parse_paid_invoice(&value).unwrap();
        assert_eq!(paid.payment_intent.as_deref(), Some("pi_1"));
        assert_eq!(paid.period_start.timestamp(), 1_800_000_000);
        assert_eq!(paid.period_end.timestamp(), 1_802_678_400);
        assert!(parse_paid_invoice(&json!({"id": "in_2"})).is_err());
    }
}
