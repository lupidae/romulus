//! The founding-team offer (`billing::offer`, Terms 7.11): Team at €29 a
//! month while the subscription stays on Team, the first 20 teams, once per
//! organization. Its promotion code applied by checkout to Team only when
//! asked for, the subscription then costing €29 (emails and the billing page
//! say so, and no price-change notice is ever owed for it); the coupon
//! following a move between monthly and yearly, gone for good off Team; the
//! count of places left read without a session and cached, places held by
//! open checkouts, the codes switched off at 20.

use axum::http::StatusCode;
use chrono::Utc;
use romulus_api::{
    Config,
    billing::notices,
    config::{BillingConfig, PromotionCodes},
    domain::Plan,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{
    billing::{
        Billed, billed, billed_with_billing, billing_config, business_checkout, deliver,
        subscription_object,
    },
    harness::{Team, TestApp},
};

const MONTHLY: &str = "promo_founding_month";
const YEARLY: &str = "promo_founding_year";
const COUNT: &str = "/api/billing/offers/founding";

fn with_offer() -> BillingConfig {
    BillingConfig {
        founding: Some(PromotionCodes {
            monthly: MONTHLY.to_owned(),
            yearly: YEARLY.to_owned(),
        }),
        ..billing_config()
    }
}

/// Billing on with the founding offer's codes (the fake makes them, as
/// `stripe-setup` does in Stripe).
async fn offered(db: &PgPool) -> Billed {
    billed_with_billing(db, false, Config::for_tests(), with_offer()).await
}

/// A business's checkout of `plan`, asking for the founding offer.
fn founding_checkout(plan: &str, interval: &str) -> Value {
    let mut body = business_checkout(plan, interval, false);
    body["offer"] = json!("founding");
    body
}

/// Codes redeemed so far, as Stripe counts them.
fn redeemed(b: &Billed, id: &str) -> i64 {
    b.stripe.state().promotion_codes[id].times_redeemed
}

fn set_redeemed(b: &Billed, id: &str, n: i64) {
    b.stripe
        .state()
        .promotion_codes
        .get_mut(id)
        .unwrap()
        .times_redeemed = n;
}

/// Pays the last checkout on the fake checkout page (as the e2e suite
/// does), which applies it as its webhook would.
async fn pay_on_the_fake_page(b: &Billed) {
    let (session, _) = b.stripe.last_checkout().unwrap();
    let paid = b
        .app
        .get(&format!("/api/billing/fake/checkout/{session}"))
        .await;
    assert!(paid.status.is_redirection(), "{}", paid.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_team_checkout_with_the_offer_applies_its_code_and_costs_29(db: PgPool) {
    let b = offered(&db).await;
    let admin = b.app.as_user(&b.team.admin);
    let page = admin.get(&b.team.path("/billing")).await;
    assert_eq!(page.status, StatusCode::OK);
    let offer = &page.json()["founding_offer"];
    assert_eq!(
        (
            offer["plan"].as_str(),
            offer["monthly_cents"].as_i64(),
            offer["yearly_cents"].as_i64(),
            offer["list_monthly_cents"].as_i64(),
            offer["left"].as_i64(),
            offer["teams"].as_i64(),
        ),
        (
            Some("team"),
            Some(2_900),
            Some(29_000),
            Some(4_900),
            Some(20),
            Some(20)
        )
    );

    let started = admin
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    let (_, checkout) = b.stripe.last_checkout().unwrap();
    assert_eq!(checkout.promotion_code.as_deref(), Some(MONTHLY));
    // A key of its own: the same nonce without the offer is another request.
    assert!(checkout.idempotency_key.contains(MONTHLY));
    let details = sqlx::query_scalar!(
        r#"SELECT details AS "details!" FROM audit_events WHERE action = 'terms_accepted'"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(details["billing"]["offer"], "founding");

    pay_on_the_fake_page(&b).await;
    assert_eq!(redeemed(&b, MONTHLY), 1);
    let stored = sqlx::query!(
        "SELECT amount_cents, discount_cents FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    // The price is Team's own, its discount kept apart: €29 a period.
    assert_eq!(
        (stored.amount_cents, stored.discount_cents),
        (Some(4_900), Some(2_000))
    );
    let page = admin.get(&b.team.path("/billing")).await.json();
    assert_eq!(page["plan"], "team");
    assert_eq!(page["subscription"]["amount_cents"], 2_900);
    let confirmed = b
        .mail()
        .await
        .all()
        .into_iter()
        .find(|m| m.kind == "billing_subscription_confirmed")
        .expect("the confirmation");
    assert!(confirmed.text.contains("€29.00"), "{}", confirmed.text);
    assert!(!confirmed.text.contains("€49.00"), "{}", confirmed.text);
    // Its price is the plan table's: no price change is owed to it.
    let owed = notices::price_changes(&b.app.state, None, None, true)
        .await
        .unwrap();
    assert!(owed.is_empty(), "{owed:?}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_yearly_team_checkout_takes_its_own_code_and_costs_290(db: PgPool) {
    let b = offered(&db).await;
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "year"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    let (session, checkout) = b.stripe.last_checkout().unwrap();
    assert_eq!(checkout.promotion_code.as_deref(), Some(YEARLY));
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    deliver(
        &b,
        "checkout.session.completed",
        json!({"id": session, "object": "checkout.session", "subscription": subscription,
               "client_reference_id": b.team.org}),
    )
    .await;
    assert_eq!((redeemed(&b, YEARLY), redeemed(&b, MONTHLY)), (1, 0));
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["subscription"]["interval"], "year");
    assert_eq!(page["subscription"]["amount_cents"], 29_000);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_the_offer_or_for_another_plan_a_checkout_is_at_the_plans_price(db: PgPool) {
    let b = offered(&db).await;
    let admin = b.app.as_user(&b.team.admin);
    // Not asked for: Team at €49.
    let plain = admin
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "month", false),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.json());
    assert_eq!(b.stripe.last_checkout().unwrap().1.promotion_code, None);
    // Asked for with another plan: ignored.
    for plan in ["starter", "business"] {
        for interval in ["month", "year"] {
            let other = admin
                .post(
                    &b.team.path("/billing/checkout"),
                    &founding_checkout(plan, interval),
                )
                .await;
            assert_eq!(other.status, StatusCode::OK, "{}", other.json());
            let (_, checkout) = b.stripe.last_checkout().unwrap();
            assert_eq!(checkout.promotion_code, None, "{plan} {interval}");
        }
    }
    // An unknown offer is a bad request, not a discount.
    let mut unknown = business_checkout("team", "month", false);
    unknown["offer"] = json!("friends");
    let refused = admin
        .post(&b.team.path("/billing/checkout"), &unknown)
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

    pay_on_the_fake_page(&b).await;
    let stored = sqlx::query!(
        "SELECT plan::text AS \"plan!\", amount_cents, discount_cents FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (
            stored.plan.as_str(),
            stored.amount_cents,
            stored.discount_cents
        ),
        ("business", Some(149_000), None)
    );
    assert_eq!((redeemed(&b, MONTHLY), redeemed(&b, YEARLY)), (0, 0));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_the_offer_off_a_checkout_asking_for_it_is_at_the_plans_price(db: PgPool) {
    let b = billed(&db).await;
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    assert_eq!(b.stripe.last_checkout().unwrap().1.promotion_code, None);
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["founding_offer"], Value::Null);
    // The homepage hears there is no place (no error in its console).
    let count = b.app.get(COUNT).await;
    assert_eq!(count.status, StatusCode::OK);
    assert_eq!(count.json()["left"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_count_reads_without_a_session_and_is_cached(db: PgPool) {
    let b = offered(&db).await;
    let count = b.app.get(COUNT).await;
    assert_eq!(count.status, StatusCode::OK, "{}", count.json());
    assert_eq!(count.json()["left"], 20);
    assert_eq!(count.json()["monthly_cents"], 2_900);
    let reads = b.stripe.state().promotion_reads;
    assert_eq!(reads, 2, "both codes read");

    // Redeemed meanwhile: the homepage sees the count it read, for a while.
    set_redeemed(&b, MONTHLY, 5);
    set_redeemed(&b, YEARLY, 3);
    for _ in 0..3 {
        assert_eq!(b.app.get(COUNT).await.json()["left"], 20);
    }
    assert_eq!(b.stripe.state().promotion_reads, reads);

    // A checkout reads it fresh, and keeps what it read for the others.
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    // 20 less 8 redeemed, less the place this checkout holds.
    assert_eq!(b.app.get(COUNT).await.json()["left"], 11);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_no_place_left_a_checkout_with_the_offer_is_refused_and_one_without_goes(db: PgPool) {
    let b = offered(&db).await;
    set_redeemed(&b, MONTHLY, 15);
    set_redeemed(&b, YEARLY, 5);
    let admin = b.app.as_user(&b.team.admin);
    let refused = admin
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "year"),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "offer_ended")
    );
    assert!(b.stripe.last_checkout().is_none(), "no session made");
    // The billing page and the homepage say so at once.
    let page = admin.get(&b.team.path("/billing")).await.json();
    assert_eq!(page["founding_offer"]["left"], 0);
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    // Without it, Team at its price.
    let plain = admin
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "year", false),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.json());
    assert_eq!(b.stripe.last_checkout().unwrap().1.promotion_code, None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_code_switched_off_ends_the_offer(db: PgPool) {
    let b = offered(&db).await;
    b.stripe
        .state()
        .promotion_codes
        .get_mut(YEARLY)
        .unwrap()
        .active = false;
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_count_stripe_does_not_answer_is_an_error_remembered_a_moment(db: PgPool) {
    let b = offered(&db).await;
    b.stripe.state().promotions_down = true;
    let count = b.app.get(COUNT).await;
    assert_eq!(
        (count.status, count.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "billing_unavailable")
    );
    let reads = b.stripe.state().promotion_reads;
    assert_eq!(
        b.app.get(COUNT).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        b.stripe.state().promotion_reads,
        reads,
        "not asked again at once"
    );
    // The billing page still loads, without the offer.
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.json()["founding_offer"], Value::Null);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_billing_off_the_count_does_not_exist(db: PgPool) {
    let app = TestApp::compiling(db);
    assert_eq!(app.get(COUNT).await.status, StatusCode::NOT_FOUND);
}

/// Subscribes acme to Team for `interval` with the offer, through checkout
/// and its webhook; the subscription's id.
async fn subscribe_founding(b: &Billed, interval: &str) -> String {
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", interval),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    let (session, _) = b.stripe.last_checkout().unwrap();
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    deliver(
        b,
        "checkout.session.completed",
        json!({"id": session, "object": "checkout.session", "subscription": subscription,
               "client_reference_id": b.team.org}),
    )
    .await;
    subscription
}

/// The portal moves `subscription` to `plan` for `interval` (`monthly`,
/// `yearly`), its coupons as they were; its webhook arrives.
async fn move_to(b: &Billed, subscription: &str, plan: &str, interval: &str) {
    let key = format!("{plan}_{interval}");
    let (plan, every) = Plan::from_lookup_key(&key).unwrap();
    b.stripe.update(subscription, |s| {
        let item = s.item.as_mut().unwrap();
        item.price = format!("price_{key}");
        item.lookup_key = Some(key.clone());
        item.unit_amount = plan.price_cents(every).map(i64::from);
        item.product = plan.product_id();
    });
    deliver(
        b,
        "customer.subscription.updated",
        subscription_object(subscription),
    )
    .await;
}

/// The coupons Stripe has on `subscription`.
fn coupons(b: &Billed, subscription: &str) -> Vec<String> {
    b.stripe.state().subscriptions[subscription]
        .discounts
        .iter()
        .map(|d| d.coupon.clone())
        .collect()
}

async fn costs(b: &Billed) -> (Option<i64>, Option<i64>, Value) {
    let stored = sqlx::query!(
        "SELECT amount_cents, discount_cents FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&b.app.state.db)
    .await
    .unwrap();
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    (
        stored.amount_cents,
        stored.discount_cents,
        page["subscription"]["amount_cents"].clone(),
    )
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_founding_team_moving_between_monthly_and_yearly_keeps_its_price(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "month").await;
    assert_eq!(coupons(&b, &subscription), ["founding_team_monthly"]);

    // Monthly to yearly: €200 off a year, not €20 (€470).
    move_to(&b, &subscription, "team", "yearly").await;
    assert_eq!(coupons(&b, &subscription), ["founding_team_yearly"]);
    assert_eq!(costs(&b).await, (Some(49_000), Some(20_000), json!(29_000)));

    // Yearly to monthly: €20 off a month, not €200 (nothing to pay).
    move_to(&b, &subscription, "team", "monthly").await;
    assert_eq!(coupons(&b, &subscription), ["founding_team_monthly"]);
    assert_eq!(costs(&b).await, (Some(4_900), Some(2_000), json!(2_900)));

    // The same event again changes nothing more; the codes were redeemed
    // once, by the checkout.
    let changes = b.stripe.state().discount_changes;
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(b.stripe.state().discount_changes, changes);
    assert_eq!((redeemed(&b, MONTHLY), redeemed(&b, YEARLY)), (1, 0));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn leaving_team_ends_the_offer_for_good(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "year").await;

    // To Business: no founding coupon left, Business at its price.
    move_to(&b, &subscription, "business", "yearly").await;
    assert_eq!(coupons(&b, &subscription), Vec::<String>::new());
    assert_eq!(costs(&b).await, (Some(149_000), None, json!(149_000)));

    // Back to Team: at Team's price, the offer does not come back.
    move_to(&b, &subscription, "team", "monthly").await;
    assert_eq!(coupons(&b, &subscription), Vec::<String>::new());
    assert_eq!(costs(&b).await, (Some(4_900), None, json!(4_900)));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_offer_is_once_per_organization(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "month").await;
    let used = sqlx::query_scalar!(
        "SELECT founding_offer_used_at FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(used.is_some());

    // Cancelled: the organization is on Free again, and the offer is gone.
    b.stripe
        .update(&subscription, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    let admin = b.app.as_user(&b.team.admin);
    let page = admin.get(&b.team.path("/billing")).await.json();
    assert_eq!(page["plan"], "free");
    assert_eq!(page["founding_offer"], Value::Null);
    let again = admin
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::CONFLICT, "offer_used")
    );
    // Team at its price is still for sale; another organization still has
    // the offer.
    let plain = admin
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "month", false),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.json());
    let beta = Team::new(&db, "beta").await;
    let other = b
        .app
        .as_user(&beta.admin)
        .get(&beta.path("/billing"))
        .await
        .json();
    assert_eq!(other["founding_offer"]["left"], 19);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_open_offered_checkout_holds_a_place_until_it_expires(db: PgPool) {
    let b = offered(&db).await;
    set_redeemed(&b, MONTHLY, 12);
    set_redeemed(&b, YEARLY, 7);
    // acme opens one: the last place, held for about half an hour.
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    let (_, checkout) = b.stripe.last_checkout().unwrap();
    let expires = checkout.expires_at.expect("an offered session expires");
    let ahead = expires - Utc::now().timestamp();
    assert!((30 * 60..=33 * 60).contains(&ahead), "{ahead} s");
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);

    // Asked again by acme itself, its own hold does not count against it.
    let again = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "year"),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json());

    // Another organization finds none left.
    let beta = Team::new(&db, "beta").await;
    let checkout = |team: &Team| {
        let body = founding_checkout("team", "month");
        let path = team.path("/billing/checkout");
        let session = team.admin.clone();
        let app = &b.app;
        async move { app.as_user(&session).post(&path, &body).await }
    };
    let refused = checkout(&beta).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "offer_ended")
    );

    // acme's session expired unpaid: its place is free again.
    sqlx::query!(
        "UPDATE org_billing SET checkout_offer_until = now() - interval '1 second' WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    let took = checkout(&beta).await;
    assert_eq!(took.status, StatusCode::OK, "{}", took.json());
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_twentieth_redemption_switches_both_codes_off(db: PgPool) {
    let b = offered(&db).await;
    set_redeemed(&b, MONTHLY, 11);
    set_redeemed(&b, YEARLY, 8);
    subscribe_founding(&b, "month").await;
    assert_eq!(redeemed(&b, MONTHLY), 12);
    let active = |id: &str| b.stripe.state().promotion_codes[id].active;
    assert!(!active(MONTHLY));
    assert!(!active(YEARLY));
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    // The checkout's own hold went with the subscription stored.
    let held = sqlx::query_scalar!(
        "SELECT checkout_offer_until FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(held, None);
}
