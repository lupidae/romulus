//! The founding-team offer (`billing::offer`, Terms 7.11): Team at €29 a
//! month on its own prices while the subscription stays on Team, the first
//! 20 teams, once per organization. Checkout uses the founding price for
//! Team only when asked for (emails and the billing page say €29, and no
//! price-change notice is ever owed for it); the portal of a founding
//! subscription moves between the founding prices, and one moved off Team
//! is back to the usual portal; the count of places left read without a
//! session, places held by open checkouts.

use axum::http::StatusCode;
use chrono::Utc;
use romulus_api::{
    Config,
    billing::{notices, plans::lookup_cents, stripe::PortalFlow},
    config::BillingConfig,
    domain::Role,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{
    billing::{
        Billed, billed, billed_with_billing, billing_config, business_checkout, deliver,
        subscription_object,
    },
    harness::{Team, TestApp, member, org},
};

const COUNT: &str = "/api/billing/offers/founding";

/// Billing on with the founding offer (the fake prices every lookup key).
async fn offered(db: &PgPool) -> Billed {
    billed_with_billing(
        db,
        false,
        Config::for_tests(),
        BillingConfig {
            founding: true,
            ..billing_config()
        },
    )
    .await
}

/// A business's checkout of `plan`, asking for the founding offer.
fn founding_checkout(plan: &str, interval: &str) -> Value {
    let mut body = business_checkout(plan, interval, false);
    body["offer"] = json!("founding");
    body
}

/// `n` other organizations that had the offer (deleted since, for all it
/// matters: their places stay taken).
async fn taken_by_others(db: &PgPool, n: usize) {
    for _ in 0..n {
        sqlx::query!("INSERT INTO founding_offer_uses (org_id) VALUES (uuidv7())")
            .execute(db)
            .await
            .unwrap();
    }
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

/// Stripe moves `subscription` to the price of `key` (what the portal
/// does once the customer confirms); its webhook arrives.
async fn move_to(b: &Billed, subscription: &str, key: &str) {
    let cents = lookup_cents(key).unwrap();
    b.stripe.update(subscription, |s| {
        let item = s.item.as_mut().unwrap();
        item.price = format!("price_{key}");
        item.lookup_key = Some(key.to_owned());
        item.unit_amount = Some(i64::from(cents));
    });
    deliver(
        b,
        "customer.subscription.updated",
        subscription_object(subscription),
    )
    .await;
}

/// What acme's row says: the amount, on a founding price, its use, its hold.
async fn row(b: &Billed) -> (Option<i64>, bool, bool, bool) {
    let r = sqlx::query!(
        r#"SELECT amount_cents, founding, checkout_offer_until,
                  EXISTS (SELECT 1 FROM founding_offer_uses u WHERE u.org_id = b.org_id) AS "used!"
           FROM org_billing b WHERE org_id = $1"#,
        b.team.org
    )
    .fetch_one(&b.app.state.db)
    .await
    .unwrap();
    (
        r.amount_cents,
        r.founding,
        r.used,
        r.checkout_offer_until.is_some(),
    )
}

/// The portal acme's admin opens to change plan: the price it confirms, and
/// whether it is the founding configuration.
async fn portal_to(b: &Billed, plan: &str, interval: &str) -> (Option<String>, bool) {
    let opened = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/portal"),
            &json!({"flow": {"type": "change_plan", "plan": plan, "interval": interval}}),
        )
        .await;
    assert_eq!(opened.status, StatusCode::OK, "{}", opened.json());
    let portal = b.stripe.last_portal().unwrap();
    let price = match portal.flow {
        Some(PortalFlow::Confirm { price, .. }) => Some(price),
        _ => None,
    };
    (price, portal.founding)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_team_checkout_with_the_offer_is_at_the_founding_price(db: PgPool) {
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
            offer["used"].as_bool(),
        ),
        (
            Some("team"),
            Some(2_900),
            Some(29_000),
            Some(4_900),
            Some(20),
            Some(false)
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
    assert_eq!(checkout.price, "price_team_founding_monthly");
    // The session expires in about half an hour; its place is held a day
    // longer, so a payment at the last minute whose webhook is late counts.
    let expires = checkout.expires_at.expect("a founding session expires");
    let ahead = expires - Utc::now().timestamp();
    assert!((30 * 60..=31 * 60).contains(&ahead), "{ahead} s");
    let held = sqlx::query_scalar!(
        r#"SELECT checkout_offer_until AS "until!" FROM org_billing WHERE org_id = $1"#,
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(held.timestamp(), expires + 24 * 3_600);
    assert_eq!(row(&b).await, (None, false, false, true));
    assert_eq!(b.app.get(COUNT).await.json()["left"], 19);
    let details = sqlx::query_scalar!(
        r#"SELECT details AS "details!" FROM audit_events WHERE action = 'terms_accepted'"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(details["billing"]["offer"], "founding");

    pay_on_the_fake_page(&b).await;
    // Team at €29, on its founding price; the org's one use, its hold gone.
    assert_eq!(row(&b).await, (Some(2_900), true, true, false));
    let page = admin.get(&b.team.path("/billing")).await.json();
    assert_eq!(page["plan"], "team");
    assert_eq!(page["subscription"]["amount_cents"], 2_900);
    assert_eq!(
        page["subscription"]["founding"],
        json!({"monthly_cents": 2_900, "yearly_cents": 29_000})
    );
    assert_eq!(page["founding_offer"]["used"], true);
    assert_eq!(b.app.get(COUNT).await.json()["left"], 19);
    let confirmed = b
        .mail()
        .await
        .all()
        .into_iter()
        .find(|m| m.kind == "billing_subscription_confirmed")
        .expect("the confirmation");
    assert!(confirmed.text.contains("€29.00"), "{}", confirmed.text);
    assert!(!confirmed.text.contains("€49.00"), "{}", confirmed.text);
    // Its price is fixed (Terms 7.11): no price change is ever owed to it.
    let owed = notices::price_changes(&b.app.state, None, None, true)
        .await
        .unwrap();
    assert!(owed.is_empty(), "{owed:?}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_yearly_founding_checkout_costs_290(db: PgPool) {
    let b = offered(&db).await;
    subscribe_founding(&b, "year").await;
    assert_eq!(
        b.stripe.last_checkout().unwrap().1.price,
        "price_team_founding_yearly"
    );
    assert_eq!(row(&b).await, (Some(29_000), true, true, false));
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
    let plain = admin
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "month", false),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.json());
    let (_, checkout) = b.stripe.last_checkout().unwrap();
    assert_eq!(
        (checkout.price.as_str(), checkout.expires_at),
        ("price_team_monthly", None)
    );
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
            assert!(!checkout.price.contains("founding"), "{plan} {interval}");
            assert_eq!(checkout.expires_at, None);
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
    assert_eq!(row(&b).await, (Some(149_000), false, false, false));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_the_offer_off_a_checkout_asking_for_it_is_refused_never_charged_49(db: PgPool) {
    let b = billed(&db).await;
    let refused = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "offer_ended")
    );
    assert!(b.stripe.last_checkout().is_none());
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
async fn the_count_reads_without_a_session(db: PgPool) {
    let b = offered(&db).await;
    let count = b.app.get(COUNT).await;
    assert_eq!(count.status, StatusCode::OK, "{}", count.json());
    assert_eq!(
        (
            count.json()["left"].as_i64(),
            count.json()["monthly_cents"].as_i64(),
            count.json()["used"].as_bool()
        ),
        (Some(20), Some(2_900), Some(false))
    );
    taken_by_others(&db, 7).await;
    assert_eq!(b.app.get(COUNT).await.json()["left"], 13);
    // A hold past its expiry counts for nothing.
    let gone = org(&db, "gone").await;
    sqlx::query!(
        "INSERT INTO org_billing (org_id, checkout_offer_until) VALUES ($1, now() - interval '1 minute')",
        gone
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(b.app.get(COUNT).await.json()["left"], 13);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_no_place_left_a_checkout_with_the_offer_is_refused_and_one_without_goes(db: PgPool) {
    let b = offered(&db).await;
    taken_by_others(&db, 20).await;
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
    let page = admin.get(&b.team.path("/billing")).await.json();
    assert_eq!(page["founding_offer"]["left"], 0);
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    let plain = admin
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "year", false),
        )
        .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.json());
    assert_eq!(
        b.stripe.last_checkout().unwrap().1.price,
        "price_team_yearly"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_open_founding_checkout_holds_a_place_until_it_expires(db: PgPool) {
    let b = offered(&db).await;
    taken_by_others(&db, 19).await;
    let checkout = |team: &Team, interval: &str| {
        let body = founding_checkout("team", interval);
        let path = team.path("/billing/checkout");
        let session = team.admin.clone();
        let app = &b.app;
        async move { app.as_user(&session).post(&path, &body).await }
    };
    // acme opens one: the last place, held.
    let started = checkout(&b.team, "month").await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    // Asked again by acme itself, its own hold does not count against it.
    let again = checkout(&b.team, "year").await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json());

    // Another organization finds none left.
    let beta = Team::new(&db, "beta").await;
    let refused = checkout(&beta, "month").await;
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
    let took = checkout(&beta, "month").await;
    assert_eq!(took.status, StatusCode::OK, "{}", took.json());
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_offer_is_once_per_organization(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "month").await;

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
    assert_eq!(page["founding_offer"]["used"], true);
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
    // the offer, one place fewer.
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
    assert_eq!(
        (
            &other["founding_offer"]["left"],
            &other["founding_offer"]["used"]
        ),
        (&json!(19), &json!(false))
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_founding_subscription_moves_between_its_founding_prices(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "month").await;

    // Its portal is the founding one, and yearly Team is the founding price.
    assert_eq!(
        portal_to(&b, "team", "year").await,
        (Some("price_team_founding_yearly".to_owned()), true)
    );
    move_to(&b, &subscription, "team_founding_yearly").await;
    assert_eq!(row(&b).await, (Some(29_000), true, true, false));

    // And back to monthly: €29 a month.
    assert_eq!(
        portal_to(&b, "team", "month").await,
        (Some("price_team_founding_monthly".to_owned()), true)
    );
    move_to(&b, &subscription, "team_founding_monthly").await;
    assert_eq!(row(&b).await, (Some(2_900), true, true, false));
    // Still no price change owed.
    let owed = notices::price_changes(&b.app.state, None, None, true)
        .await
        .unwrap();
    assert!(owed.is_empty(), "{owed:?}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn leaving_team_ends_the_offer_for_good(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "year").await;

    // To Business at its own price.
    assert_eq!(
        portal_to(&b, "business", "year").await,
        (Some("price_business_yearly".to_owned()), true)
    );
    move_to(&b, &subscription, "business_yearly").await;
    assert_eq!(row(&b).await, (Some(149_000), false, true, false));

    // Its portal is the usual one now: Team at its own price.
    assert_eq!(
        portal_to(&b, "team", "month").await,
        (Some("price_team_monthly".to_owned()), false)
    );
    move_to(&b, &subscription, "team_monthly").await;
    assert_eq!(row(&b).await, (Some(4_900), false, true, false));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_billing_off_the_count_does_not_exist(db: PgPool) {
    let app = TestApp::compiling(db);
    assert_eq!(app.get(COUNT).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_founding_session_expired_or_cancelled_frees_its_place(db: PgPool) {
    let b = offered(&db).await;
    taken_by_others(&db, 19).await;
    let admin = b.app.as_user(&b.team.admin);
    let path = b.team.path("/billing/checkout");
    let body = founding_checkout("team", "month");
    let start = || admin.post(&path, &body);
    assert_eq!(start().await.status, StatusCode::OK);
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);

    // Stripe says it expired unpaid.
    let (session, _) = b.stripe.last_checkout().unwrap();
    deliver(
        &b,
        "checkout.session.expired",
        json!({"id": session, "object": "checkout.session"}),
    )
    .await;
    assert_eq!(b.app.get(COUNT).await.json()["left"], 1);

    // Back from the payment page cancelled: the session expired, the place
    // freed.
    assert_eq!(start().await.status, StatusCode::OK);
    let (session, _) = b.stripe.last_checkout().unwrap();
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    let cancelled = admin
        .post(&b.team.path("/billing/checkout/cancel"), &json!({}))
        .await;
    assert_eq!(cancelled.status, StatusCode::NO_CONTENT);
    assert_eq!(b.stripe.state().session_status[&session], "expired");
    assert!(b.stripe.complete_checkout(&session).is_none());
    assert_eq!(b.app.get(COUNT).await.json()["left"], 1);
    // Nothing held: nothing to do.
    let again = admin
        .post(&b.team.path("/billing/checkout/cancel"), &json!({}))
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);

    // A session paid meanwhile keeps its place until its subscription is
    // stored.
    assert_eq!(start().await.status, StatusCode::OK);
    let (session, _) = b.stripe.last_checkout().unwrap();
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    let late = admin
        .post(&b.team.path("/billing/checkout/cancel"), &json!({}))
        .await;
    assert_eq!(late.status, StatusCode::NO_CONTENT);
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
    deliver(
        &b,
        "checkout.session.completed",
        json!({"id": session, "object": "checkout.session", "subscription": subscription,
               "client_reference_id": b.team.org}),
    )
    .await;
    assert_eq!(row(&b).await, (Some(2_900), true, true, false));
    assert_eq!(b.app.get(COUNT).await.json()["left"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_person_holds_one_founding_place_at_a_time(db: PgPool) {
    let b = offered(&db).await;
    // acme's admin runs a second organization.
    let second = org(&db, "second").await;
    member(&db, second, b.team.admin.user, Role::Admin).await;
    let admin = b.app.as_user(&b.team.admin);
    let first = admin
        .post(
            &b.team.path("/billing/checkout"),
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json());
    let other = admin
        .post(
            "/api/orgs/second/billing/checkout",
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::CONFLICT, "offer_held")
    );
    // Cancelled, the second one goes.
    let cancelled = admin
        .post(&b.team.path("/billing/checkout/cancel"), &json!({}))
        .await;
    assert_eq!(cancelled.status, StatusCode::NO_CONTENT);
    let other = admin
        .post(
            "/api/orgs/second/billing/checkout",
            &founding_checkout("team", "month"),
        )
        .await;
    assert_eq!(other.status, StatusCode::OK, "{}", other.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_deleted_organization_keeps_its_place_taken(db: PgPool) {
    let b = offered(&db).await;
    subscribe_founding(&b, "month").await;
    assert_eq!(b.app.get(COUNT).await.json()["left"], 19);
    sqlx::query!("DELETE FROM organizations WHERE id = $1", b.team.org)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(b.app.get(COUNT).await.json()["left"], 19);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_portal_follows_the_price_stripe_has_not_the_stored_flag(db: PgPool) {
    let b = offered(&db).await;
    let subscription = subscribe_founding(&b, "month").await;
    // Moved to Business in Stripe, its webhook not here yet: the stored
    // row still says founding.
    b.stripe.update(&subscription, |s| {
        let item = s.item.as_mut().unwrap();
        item.price = "price_business_monthly".into();
        item.lookup_key = Some("business_monthly".into());
        item.unit_amount = Some(14_900);
    });
    assert!(row(&b).await.1, "still stored as founding");
    // Back to Team is at Team's own price, in the usual portal.
    assert_eq!(
        portal_to(&b, "team", "month").await,
        (Some("price_team_monthly".to_owned()), false)
    );
}
