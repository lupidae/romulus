//! `romulus-api stripe-setup`: the Stripe side of billing, from the plan
//! table, idempotently.
//!
//! - a product per paid plan (`romulus_starter`…), and its monthly and yearly
//!   prices in EUR under their lookup keys (`starter_monthly`…, a year at
//!   ten months). A price whose amount changed is created again and the key
//!   moved to it (`transfer_lookup_key`): subscriptions stay on their price
//!   until moved, with notice (`notices::price_change`);
//! - the Customer Portal configuration (metadata `romulus: portal`):
//!   cancelling at the period's end, switching plans (upgrades now, prorated
//!   by Stripe; downgrades and shorter intervals at the period's end), the
//!   payment method, the invoice history, the customer's details;
//! - with `--webhook-url`, the webhook endpoint and its events (its signing
//!   secret written to `--secret-file`, mode 0600, never printed); without,
//!   the URL and events to register;
//! - with `--founding-offer`, the founding-team offer (`plans::FOUNDING`):
//!   a coupon per interval (`founding_team_monthly`: the plan's price less
//!   the offer's off every period, forever, Team only, metadata
//!   `romulus_offer: founding`, no cap of its own: a subscription moving to
//!   the other interval is given its coupon, a redemption too) and its
//!   promotion code (`FOUNDINGTEAM`, `FOUNDINGTEAMYEARLY`, at most 20
//!   redemptions),
//!   printing the ids to configure (`ROMULUS_FOUNDING_PROMO_MONTHLY`,
//!   `_YEARLY`). A code that exists is never made again, even switched off
//!   (that is how the offer ends early); a coupon with other terms is an
//!   error (Stripe never changes a coupon's amount).
//!
//! It reads `STRIPE_SECRET_KEY` and refuses a live key unless `--live`.

use std::{fmt::Write as _, io::Write};

use super::{
    plans::{FOUNDING, Offer, PAID, YEARLY_MONTHS},
    stripe::{Form, Http, StripeError},
};
use crate::{
    config::Secret,
    domain::{Interval, Plan},
};

/// The events romulus handles (`billing::webhook`).
pub const EVENTS: [&str; 6] = [
    "checkout.session.completed",
    "customer.subscription.created",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "invoice.paid",
    "invoice.payment_failed",
];

#[derive(Debug, Default)]
pub struct Setup {
    pub public_url: Option<String>,
    pub terms_url: Option<String>,
    pub privacy_url: Option<String>,
    pub webhook_url: Option<String>,
    pub secret_file: Option<std::path::PathBuf>,
    /// The founding-team offer's coupons and promotion codes too.
    pub founding_offer: bool,
    pub live: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("STRIPE_SECRET_KEY is not set")]
    NoKey,
    #[error("STRIPE_SECRET_KEY is a live key: pass --live to set up live mode")]
    Live,
    #[error(transparent)]
    Stripe(#[from] StripeError),
    #[error("writing the webhook secret")]
    Io(#[from] std::io::Error),
    #[error("coupon {0} exists with other terms; delete it in the Dashboard, then run again")]
    Coupon(String),
}

fn product_description(plan: Plan) -> String {
    let l = plan.limits();
    let mut text = format!("{} databases", l.databases);
    match l.versions_per_database {
        Some(n) => {
            let _ = write!(text, ", {n} versions each");
        }
        None => text.push_str(", unlimited versions"),
    }
    let _ = write!(
        text,
        ", {} tables and views per schema, {} playground hours a month",
        l.schema_objects, l.playground_hours
    );
    text
}

fn interval_word(interval: Interval) -> &'static str {
    match interval {
        Interval::Month => "month",
        Interval::Year => "year",
    }
}

/// Runs the setup, saying what it did on `out`.
pub async fn run(
    setup: &Setup,
    key: Option<String>,
    out: &mut impl Write,
) -> Result<(), SetupError> {
    let key = key.ok_or(SetupError::NoKey)?;
    if (key.starts_with("sk_live_") || key.starts_with("rk_live_")) && !setup.live {
        return Err(SetupError::Live);
    }
    let stripe = Http::new(Secret::new(key));
    let mut prices: Vec<(Plan, Vec<String>)> = Vec::new();

    for plan in PAID {
        let product = plan.product_id().unwrap_or_default();
        let fields = Form::new()
            .set("name", format!("romulus {}", plan.name()))
            .set("description", product_description(plan))
            .set("metadata[romulus_plan]", plan.slug());
        match stripe.get(&format!("products/{product}"), &[]).await {
            Ok(_) => {
                stripe
                    .post(
                        &format!("products/{product}"),
                        fields.set("active", "true").pairs(),
                    )
                    .await?;
                writeln!(out, "product {product}: up to date")?;
            }
            Err(err) if err.is_missing() => {
                stripe
                    .post("products", fields.set("id", &product).pairs())
                    .await?;
                writeln!(out, "product {product}: created")?;
            }
            Err(err) => return Err(err.into()),
        }
        let mut ids = Vec::new();
        for interval in [Interval::Month, Interval::Year] {
            let lookup_key = plan.lookup_key(interval).unwrap_or_default();
            let cents = plan.price_cents(interval).unwrap_or_default();
            let found = stripe
                .get(
                    "prices",
                    &[
                        ("lookup_keys[]".into(), lookup_key.clone()),
                        ("active".into(), "true".into()),
                    ],
                )
                .await?;
            let current = &found["data"][0];
            let same = current["unit_amount"].as_i64() == Some(i64::from(cents))
                && current["currency"] == "eur"
                && current["recurring"]["interval"] == interval_word(interval)
                && current["product"] == product.as_str();
            let id = if same {
                writeln!(out, "price {lookup_key}: up to date ({cents} cents)")?;
                current["id"].as_str().unwrap_or_default().to_owned()
            } else {
                let created = stripe
                    .post(
                        "prices",
                        Form::new()
                            .set("product", &product)
                            .set("currency", "eur")
                            .set("unit_amount", cents.to_string())
                            .set("recurring[interval]", interval_word(interval))
                            .set("lookup_key", &lookup_key)
                            .set("transfer_lookup_key", "true")
                            .set(
                                "nickname",
                                format!("{} {}ly", plan.name(), interval_word(interval)),
                            )
                            .set("metadata[romulus_plan]", plan.slug())
                            .set("tax_behavior", "unspecified")
                            .pairs(),
                    )
                    .await?;
                writeln!(
                    out,
                    "price {lookup_key}: created ({cents} cents{})",
                    if current.is_object() {
                        ", the key moved to it"
                    } else {
                        ""
                    }
                )?;
                created["id"].as_str().unwrap_or_default().to_owned()
            };
            ids.push(id);
        }
        prices.push((plan, ids));
    }
    writeln!(out, "yearly prices are {YEARLY_MONTHS} months")?;

    portal(setup, &stripe, &prices, out).await?;
    webhook(setup, &stripe, out).await?;
    if setup.founding_offer {
        offer(&stripe, FOUNDING, out).await?;
    }
    Ok(())
}

/// The coupon an offer has for `interval`: what it takes off, forever, on
/// its plan's product only.
fn coupon_form(offer: Offer, interval: Interval) -> Form {
    let name = match interval {
        Interval::Month => "Founding team",
        Interval::Year => "Founding team (yearly)",
    };
    Form::new()
        .set("id", offer.coupon_id(interval))
        .set("name", name)
        .set("amount_off", offer.amount_off(interval).to_string())
        .set("currency", "eur")
        .set("duration", "forever")
        .set(
            "applies_to[products][0]",
            offer.plan.product_id().unwrap_or_default(),
        )
        .set("metadata[romulus_offer]", offer.slug)
}

/// Whether coupon `found` (its `applies_to` expanded) is the one `offer`
/// makes for `interval`: the amount, forever, on its plan only, no cap of
/// its own, marked as the offer's.
fn same_coupon(found: &serde_json::Value, offer: Offer, interval: Interval) -> bool {
    let product = offer.plan.product_id().unwrap_or_default();
    found["amount_off"].as_i64() == Some(i64::from(offer.amount_off(interval)))
        && found["currency"] == "eur"
        && found["duration"] == "forever"
        && found["applies_to"]["products"] == serde_json::json!([product])
        && found["max_redemptions"].is_null()
        && found["metadata"]["romulus_offer"] == offer.slug
}

/// The offer's coupons and promotion codes, made when missing; the ids to
/// configure printed.
async fn offer(stripe: &Http, offer: Offer, out: &mut impl Write) -> Result<(), SetupError> {
    let mut ids = Vec::new();
    for interval in [Interval::Month, Interval::Year] {
        let coupon = offer.coupon_id(interval);
        match stripe
            .get(
                &format!("coupons/{coupon}"),
                &[("expand[]".into(), "applies_to".into())],
            )
            .await
        {
            Ok(found) if same_coupon(&found, offer, interval) => {
                writeln!(out, "coupon {coupon}: up to date")?;
            }
            Ok(_) => return Err(SetupError::Coupon(coupon)),
            Err(err) if err.is_missing() => {
                stripe
                    .post("coupons", coupon_form(offer, interval).pairs())
                    .await?;
                writeln!(
                    out,
                    "coupon {coupon}: created ({} cents off a {}, forever)",
                    offer.amount_off(interval),
                    interval_word(interval)
                )?;
            }
            Err(err) => return Err(err.into()),
        }
        let code = offer.code(interval);
        let list = stripe
            .get(
                "promotion_codes",
                &[
                    ("coupon".into(), coupon.clone()),
                    ("code".into(), code.clone()),
                    ("limit".into(), "100".into()),
                ],
            )
            .await?;
        let found = list["data"].as_array().into_iter().flatten().find(|p| {
            p["code"]
                .as_str()
                .is_some_and(|c| c.eq_ignore_ascii_case(&code))
        });
        let id = if let Some(found) = found {
            let id = found["id"].as_str().unwrap_or_default().to_owned();
            if found["active"] == true {
                writeln!(
                    out,
                    "promotion code {code} ({id}): up to date, {} of {} redeemed",
                    found["times_redeemed"].as_i64().unwrap_or(0),
                    offer.teams
                )?;
            } else {
                writeln!(
                    out,
                    "promotion code {code} ({id}): switched off, so the offer is off (not made again)"
                )?;
            }
            id
        } else {
            let created = stripe
                .post(
                    "promotion_codes",
                    Form::new()
                        .set("coupon", &coupon)
                        .set("code", &code)
                        .set("max_redemptions", offer.teams.to_string())
                        .set("metadata[romulus_offer]", offer.slug)
                        .pairs(),
                )
                .await?;
            let id = created["id"].as_str().unwrap_or_default().to_owned();
            writeln!(out, "promotion code {code} ({id}): created")?;
            id
        };
        ids.push(id);
    }
    writeln!(
        out,
        "founding offer: configure ROMULUS_FOUNDING_PROMO_MONTHLY={} ROMULUS_FOUNDING_PROMO_YEARLY={}",
        ids[0], ids[1]
    )?;
    Ok(())
}

/// The portal's settings, apart from the plans it may switch between.
fn portal_form() -> Form {
    let mut form = Form::new()
        .set("business_profile[headline]", "romulus: plans and invoices")
        .set("features[customer_update][enabled]", "true")
        .set("features[customer_update][allowed_updates][0]", "email")
        .set("features[customer_update][allowed_updates][1]", "address")
        .set("features[customer_update][allowed_updates][2]", "name")
        .set("features[customer_update][allowed_updates][3]", "tax_id")
        .set("features[invoice_history][enabled]", "true")
        .set("features[payment_method_update][enabled]", "true")
        .set("features[subscription_cancel][enabled]", "true")
        .set("features[subscription_cancel][mode]", "at_period_end")
        .set("features[subscription_cancel][proration_behavior]", "none")
        // No survey on the way out: cancelling takes as few steps as
        // subscribing.
        .set(
            "features[subscription_cancel][cancellation_reason][enabled]",
            "false",
        )
        .set("features[subscription_update][enabled]", "true")
        .set(
            "features[subscription_update][default_allowed_updates][0]",
            "price",
        )
        .set(
            "features[subscription_update][proration_behavior]",
            "always_invoice",
        )
        .set(
            "features[subscription_update][schedule_at_period_end][conditions][0][type]",
            "decreasing_item_amount",
        )
        .set(
            "features[subscription_update][schedule_at_period_end][conditions][1][type]",
            "shortening_interval",
        )
        .set("metadata[romulus]", "portal");
    // Stripe requires the reasons even with the survey off (a create without
    // them is refused); they are never shown.
    for (n, reason) in CANCELLATION_REASONS.iter().enumerate() {
        form = form.set(
            format!("features[subscription_cancel][cancellation_reason][options][{n}]"),
            *reason,
        );
    }
    form
}

const CANCELLATION_REASONS: [&str; 4] = ["too_expensive", "missing_features", "unused", "other"];

async fn portal(
    setup: &Setup,
    stripe: &Http,
    prices: &[(Plan, Vec<String>)],
    out: &mut impl Write,
) -> Result<(), SetupError> {
    let mut form = portal_form();
    for (n, (plan, ids)) in prices.iter().enumerate() {
        form = form
            .set(
                format!("features[subscription_update][products][{n}][product]"),
                plan.product_id().unwrap_or_default(),
            )
            // A flat price: one of it, never more.
            .set(
                format!(
                    "features[subscription_update][products][{n}][adjustable_quantity][enabled]"
                ),
                "false",
            );
        for (k, id) in ids.iter().enumerate() {
            form = form.set(
                format!("features[subscription_update][products][{n}][prices][{k}]"),
                id,
            );
        }
    }
    if let Some(terms) = &setup.terms_url {
        form = form.set("business_profile[terms_of_service_url]", terms);
    }
    if let Some(privacy) = &setup.privacy_url {
        form = form.set("business_profile[privacy_policy_url]", privacy);
    }
    if let Some(public) = &setup.public_url {
        form = form.set(
            "default_return_url",
            format!("{}/app/", public.trim_end_matches('/')),
        );
    }
    let list = stripe
        .get(
            "billing_portal/configurations",
            &[
                ("active".into(), "true".into()),
                ("limit".into(), "100".into()),
            ],
        )
        .await?;
    let existing = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["metadata"]["romulus"] == "portal")
        .and_then(|c| c["id"].as_str().map(str::to_owned));
    if let Some(id) = existing {
        stripe
            .post(&format!("billing_portal/configurations/{id}"), form.pairs())
            .await?;
        writeln!(out, "portal configuration {id}: up to date")?;
    } else {
        let created = stripe
            .post("billing_portal/configurations", form.pairs())
            .await?;
        writeln!(
            out,
            "portal configuration {}: created",
            created["id"].as_str().unwrap_or("?")
        )?;
    }
    Ok(())
}

async fn webhook(setup: &Setup, stripe: &Http, out: &mut impl Write) -> Result<(), SetupError> {
    let Some(url) = &setup.webhook_url else {
        let shown = setup.public_url.as_deref().map_or_else(
            || "https://<public host>/api/billing/webhook".to_owned(),
            |public| format!("{}/api/billing/webhook", public.trim_end_matches('/')),
        );
        writeln!(
            out,
            "webhook: not registered (pass --webhook-url). Register {shown} for: {}",
            EVENTS.join(", ")
        )?;
        return Ok(());
    };
    let mut form = Form::new()
        .set("url", url)
        .set("description", "romulus billing")
        .set("api_version", super::stripe::API_VERSION);
    for (n, event) in EVENTS.iter().enumerate() {
        form = form.set(format!("enabled_events[{n}]"), *event);
    }
    let list = stripe
        .get("webhook_endpoints", &[("limit".into(), "100".into())])
        .await?;
    let existing = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["url"] == url.as_str())
        .and_then(|e| e["id"].as_str().map(str::to_owned));
    if let Some(id) = existing {
        // The API version is fixed at creation.
        let mut update = Form::new().set("url", url).set("disabled", "false");
        for (n, event) in EVENTS.iter().enumerate() {
            update = update.set(format!("enabled_events[{n}]"), *event);
        }
        stripe
            .post(&format!("webhook_endpoints/{id}"), update.pairs())
            .await?;
        writeln!(
            out,
            "webhook {id}: up to date ({url}); its signing secret is unchanged (Dashboard → Developers → Webhooks to reveal it)"
        )?;
        return Ok(());
    }
    let created = stripe.post("webhook_endpoints", form.pairs()).await?;
    let id = created["id"].as_str().unwrap_or("?");
    let secret = created["secret"].as_str().unwrap_or_default();
    match &setup.secret_file {
        Some(path) => {
            write_secret(path, secret)?;
            writeln!(
                out,
                "webhook {id}: created ({url}); its signing secret is in {} as STRIPE_WEBHOOK_SECRET",
                path.display()
            )?;
        }
        None => writeln!(
            out,
            "webhook {id}: created ({url}); reveal its signing secret in the Dashboard (Developers → Webhooks), or delete it and run again with --secret-file"
        )?,
    }
    Ok(())
}

/// `STRIPE_WEBHOOK_SECRET=…` in a file only its owner reads.
fn write_secret(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "STRIPE_WEBHOOK_SECRET={secret}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_live_key_is_refused_without_live() {
        let mut out = Vec::new();
        let err = run(&Setup::default(), Some("sk_live_x".into()), &mut out)
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::Live));
        assert!(matches!(
            run(&Setup::default(), None, &mut out).await.unwrap_err(),
            SetupError::NoKey
        ));
    }

    #[test]
    fn the_founding_coupons_take_the_difference_off_team_forever() {
        let pairs = |interval| -> std::collections::HashMap<String, String> {
            coupon_form(FOUNDING, interval)
                .pairs()
                .iter()
                .cloned()
                .collect()
        };
        let monthly = pairs(Interval::Month);
        assert_eq!(monthly["id"], "founding_team_monthly");
        assert_eq!(monthly["amount_off"], "2000");
        assert_eq!(monthly["duration"], "forever");
        assert_eq!(monthly["currency"], "eur");
        assert_eq!(monthly["applies_to[products][0]"], "romulus_team");
        assert_eq!(monthly["metadata[romulus_offer]"], "founding");
        // No cap of its own: the promotion codes hold the 20.
        assert!(!monthly.contains_key("max_redemptions"));
        assert_eq!(pairs(Interval::Year)["amount_off"], "20000");
        let found = serde_json::json!({"amount_off": 2000, "currency": "eur", "duration": "forever",
            "applies_to": {"products": ["romulus_team"]}, "max_redemptions": null,
            "metadata": {"romulus_offer": "founding"}});
        assert!(same_coupon(&found, FOUNDING, Interval::Month));
        assert!(!same_coupon(&found, FOUNDING, Interval::Year));
        let mut everything = found.clone();
        everything["applies_to"] = serde_json::Value::Null;
        assert!(!same_coupon(&everything, FOUNDING, Interval::Month));
        let mut capped = found.clone();
        capped["max_redemptions"] = serde_json::json!(20);
        assert!(!same_coupon(&capped, FOUNDING, Interval::Month));
    }

    #[test]
    fn products_say_what_the_plan_holds() {
        assert_eq!(
            product_description(Plan::Starter),
            "3 databases, 500 versions each, 200 tables and views per schema, 20 playground hours a month"
        );
        assert!(product_description(Plan::Team).contains("unlimited versions"));
    }
}
