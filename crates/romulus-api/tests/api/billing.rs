//! Hosted billing: off, nothing changes; on (a fake Stripe), every cap
//! refuses with 402 `plan_limit`, webhooks are signed, idempotent and
//! order-proof, a failed payment holds the plan for its grace period, the
//! renewal reminder goes once, playground hours are metered and stop, and
//! only admins change billing.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use futures_util::{SinkExt as _, StreamExt as _};
use romulus_api::{
    Config,
    billing::{
        Billing, hours, notices,
        stripe::{Fake, PortalFlow},
        webhook::sign,
    },
    config::{BillingConfig, Secret, StripeKeys},
    domain::{Role, TokenScope},
    mail::{Mailer, Outbox},
    playgrounds::host::Host,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

use super::{
    harness::{ORIGIN, Team, TestApp, TestResponse, api_token, database, member, model_json, user},
    live::{live_app, quick, version},
    playgrounds::{FakeHost, V1},
};

const SECRET: &str = "whsec_test_billing";

fn billing_config() -> BillingConfig {
    BillingConfig {
        stripe: StripeKeys::Fake {
            webhook_secret: Some(Secret::new(SECRET)),
        },
        terms_url: Url::parse("https://romulus.example/terms").unwrap(),
        terms_version: "2026-11-01".to_owned(),
        invoice_footer: Some("VAT not applicable".to_owned()),
        invoice_fields: vec![("Seller".to_owned(), "Example EI".to_owned())],
    }
}

pub(super) struct Billed {
    pub(super) app: TestApp,
    pub(super) stripe: Fake,
    pub(super) team: Team,
    host: FakeHost,
    outbox: Outbox,
}

/// Billing on with a fake Stripe, a fake playground host, mail in memory,
/// team `acme` (Free until it subscribes).
pub(super) async fn billed(db: &PgPool) -> Billed {
    let stripe = Fake::new(ORIGIN);
    let host = FakeHost::default();
    let (mailer, outbox) = Mailer::memory();
    let live = live_app(db.clone(), quick());
    let config = Config::for_tests();
    let billing = Billing::with_stripe(
        billing_config(),
        config.public_url.clone(),
        db.clone(),
        stripe.clone(),
    );
    let mut state = live
        .state
        .clone()
        .with_playgrounds(Host::new(host.clone()))
        .with_mailer(mailer)
        .with_billing(billing);
    // Uploads, pushes and playground scripts compile with the hub's fake.
    state.compiler = state.collab.compiler().clone();
    let app = TestApp::with_state(state);
    let team = Team::new(db, "acme").await;
    Billed {
        app,
        stripe,
        team,
        host,
        outbox,
    }
}

/// A signed delivery of `body`, as Stripe sends it.
async fn post_event(app: &TestApp, body: &str, signature: Option<String>) -> TestResponse {
    let mut request =
        Request::post("/api/billing/webhook").header("content-type", "application/json");
    if let Some(signature) = signature {
        request = request.header("stripe-signature", signature);
    }
    app.send(request.body(Body::from(body.to_owned())).unwrap())
        .await
}

fn event(id: &str, kind: &str, object: &Value) -> String {
    json!({
        "id": id, "object": "event", "type": kind, "created": Utc::now().timestamp(),
        "data": { "object": object }
    })
    .to_string()
}

/// Delivers a new event (`evt_<random>`) of `kind` about `object`.
async fn deliver(b: &Billed, kind: &str, object: Value) -> TestResponse {
    let body = event(&format!("evt_{}", Uuid::new_v4().simple()), kind, &object);
    let delivered = post_event(
        &b.app,
        &body,
        Some(sign(SECRET, body.as_bytes(), Utc::now().timestamp())),
    )
    .await;
    assert_eq!(delivered.status, StatusCode::OK, "{}", delivered.json());
    delivered
}

/// Subscribes acme to `plan` through checkout and its webhook; the
/// subscription's id.
async fn subscribe(b: &Billed, plan: &str, interval: &str) -> String {
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &json!({"plan": plan, "interval": interval, "accept_terms": true, "buyer": "business"}),
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

fn subscription_object(id: &str) -> Value {
    json!({"id": id, "object": "subscription"})
}

fn limit(response: &TestResponse) -> Value {
    assert_eq!(
        response.status,
        StatusCode::PAYMENT_REQUIRED,
        "{}",
        response.json()
    );
    assert_eq!(response.code(), "plan_limit");
    response.json()["details"].clone()
}

async fn create_database(b: &Billed, slug: &str) -> TestResponse {
    b.app
        .as_user(&b.team.developer)
        .post(
            &b.team.path("/databases"),
            &json!({"name": slug, "slug": slug}),
        )
        .await
}

async fn plan_of(db: &PgPool, org: Uuid) -> Option<String> {
    sqlx::query_scalar!(
        r#"SELECT plan::text AS "plan!" FROM org_billing WHERE org_id = $1"#,
        org
    )
    .fetch_optional(db)
    .await
    .unwrap()
}

async fn audits(db: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_events WHERE action::text = $1"#,
        action
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_billing_off_nothing_is_capped_and_billing_routes_do_not_exist(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    for slug in ["one", "two", "three"] {
        let created = app
            .as_user(&team.developer)
            .post(
                &team.path("/databases"),
                &json!({"name": slug, "slug": slug}),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
    }
    let invited = app
        .as_user(&team.admin)
        .post(
            &team.path("/invitations"),
            &json!({"email": "zoe@romulus.test", "role": "developer"}),
        )
        .await;
    assert_eq!(invited.status, StatusCode::CREATED);
    let approvals = app
        .as_user(&team.admin)
        .patch(&team.path(""), &json!({"approvals_required": 10}))
        .await;
    assert_eq!(approvals.status, StatusCode::OK);
    assert_eq!(
        app.as_user(&team.admin)
            .get(&team.path("/billing"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let checkout = app
        .as_user(&team.admin)
        .post(&team.path("/billing/checkout"), &json!({"plan": "team", "interval": "month", "accept_terms": true, "buyer": "business"}))
        .await;
    assert_eq!(checkout.status, StatusCode::NOT_FOUND);
    let body = event(
        "evt_1",
        "customer.subscription.updated",
        &subscription_object("sub_1"),
    );
    let webhook = post_event(
        &app,
        &body,
        Some(sign(SECRET, body.as_bytes(), Utc::now().timestamp())),
    )
    .await;
    assert_eq!(webhook.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get("/api/instance").await.json()["billing"], false);
    // Versions are never hidden.
    let shop = database(&db, team.org, "shop").await;
    old_version(&db, shop, 1, 400).await;
    version(&db, shop, 2, V1, json!({})).await;
    let listed = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/versions"))
        .await
        .json();
    assert_eq!(listed["versions"].as_array().unwrap().len(), 2);
    assert_eq!(listed["hidden"], 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_free_organization_keeps_one_database_and_the_refusal_names_the_plan_that_lifts_it(
    db: PgPool,
) {
    let b = billed(&db).await;
    assert_eq!(b.app.get("/api/instance").await.json()["billing"], true);
    assert_eq!(create_database(&b, "one").await.status, StatusCode::CREATED);
    let refused = create_database(&b, "two").await;
    let details = limit(&refused);
    assert_eq!(
        details,
        json!({"cap": "databases", "plan": "free", "limit": 1, "usage": 1, "upgrade_to": "starter"})
    );
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("Starter keeps 3 databases")
    );
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM databases WHERE org_id = $1"#,
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 1, "nothing was created");

    subscribe(&b, "starter", "month").await;
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("starter"));
    assert_eq!(create_database(&b, "two").await.status, StatusCode::CREATED);
    assert_eq!(
        create_database(&b, "three").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        limit(&create_database(&b, "four").await)["upgrade_to"],
        "team"
    );

    let page = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/billing"))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let page = page.json();
    assert_eq!(page["plan"], "starter");
    assert_eq!(
        page["usage"]["databases"],
        json!({"used": 3, "limit": 3, "database": null})
    );
    assert_eq!(page["can_manage"], false);
    assert_eq!(page["plans"].as_array().unwrap().len(), 4);
    assert_eq!(page["subscription"]["status"], "active");
    assert_eq!(page["subscription"]["amount_cents"], 1900);
    assert_eq!(page["terms_url"], "https://romulus.example/terms");
}

/// A version of `database` made `days_ago`.
pub(super) async fn old_version(db: &PgPool, database: Uuid, number: i32, days_ago: i32) -> Uuid {
    let model = serde_json::to_value(super::harness::model(&["customers"])).unwrap();
    sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout, created_at)
         VALUES ($1, $2, $3, $4, 'sql', 'upload', '{}', now() - make_interval(days => $5)) RETURNING id",
        database,
        number,
        model,
        V1,
        days_ago,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn pushes_over_the_version_or_schema_cap_are_refused_with_a_message_and_store_nothing(
    db: PgPool,
) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    let token = api_token(&db, shop, TokenScope::Push).await;
    let first = b
        .app
        .push(shop, &token, Some("application/json"), model_json(&["a"]))
        .await;
    assert_eq!(first.status, StatusCode::CREATED);

    let wide: Vec<String> = (0..51).map(|n| format!("t{n}")).collect();
    let wide: Vec<&str> = wide.iter().map(String::as_str).collect();
    let refused = b
        .app
        .push(shop, &token, Some("application/json"), model_json(&wide))
        .await;
    let details = limit(&refused);
    assert_eq!(
        (
            details["cap"].as_str(),
            details["usage"].as_i64(),
            details["limit"].as_i64()
        ),
        (Some("schema_size"), Some(51), Some(50))
    );
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was stored")
    );

    // 49 more: 50 versions, the Free plan's.
    for number in 2..=50 {
        old_version(&db, shop, number, 1).await;
    }
    let refused = b
        .app
        .push(
            shop,
            &token,
            Some("application/json"),
            model_json(&["a", "b"]),
        )
        .await;
    let details = limit(&refused);
    assert_eq!(
        (
            details["cap"].as_str(),
            details["usage"].as_i64(),
            details["upgrade_to"].as_str()
        ),
        (Some("versions"), Some(50), Some("starter"))
    );
    let message = refused.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("50 versions") && message.contains("Starter keeps 500"),
        "{message}"
    );
    // Unchanged pushes never create anything, so they are never refused.
    let same = b
        .app
        .push(
            shop,
            &token,
            Some("application/json"),
            serde_json::to_string(&super::harness::model(&["customers"])).unwrap(),
        )
        .await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.json());
    // An upload is refused the same way.
    let upload = b
        .app
        .as_user(&b.team.developer)
        .post(
            &b.team.path("/databases/shop/versions"),
            &json!({"sql": "CREATE TABLE x (id int);"}),
        )
        .await;
    assert_eq!(limit(&upload)["cap"], "versions");

    subscribe(&b, "starter", "year").await;
    let pushed = b
        .app
        .push(shop, &token, Some("application/json"), model_json(&wide))
        .await;
    assert_eq!(pushed.status, StatusCode::CREATED, "{}", pushed.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn free_has_one_member_and_paid_plans_any(db: PgPool) {
    let b = billed(&db).await;
    // Acme has three members already (made before the plan applied): over
    // the cap, readable, but no one more.
    let invite = |email: &'static str| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.admin)
                .post(
                    &team.path("/invitations"),
                    &json!({"email": email, "role": "developer"}),
                )
                .await
        }
    };
    let details = limit(&invite("zoe@romulus.test").await);
    assert_eq!(
        (
            details["cap"].as_str(),
            details["limit"].as_i64(),
            details["upgrade_to"].as_str()
        ),
        (Some("members"), Some(1), Some("starter"))
    );
    assert_eq!(
        b.app
            .as_user(&b.team.reviewer)
            .get(&b.team.path("/members"))
            .await
            .status,
        StatusCode::OK
    );
    subscribe(&b, "starter", "month").await;
    assert_eq!(invite("zoe@romulus.test").await.status, StatusCode::CREATED);
    assert_eq!(invite("yan@romulus.test").await.status, StatusCode::CREATED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn approvals_follow_the_plan_and_lowering_always_works(db: PgPool) {
    let b = billed(&db).await;
    let set = |n: i32| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.admin)
                .patch(&team.path(""), &json!({"approvals_required": n}))
                .await
        }
    };
    assert_eq!(limit(&set(1).await)["upgrade_to"], "starter");
    let subscription = subscribe(&b, "starter", "month").await;
    assert_eq!(set(1).await.status, StatusCode::OK);
    assert_eq!(limit(&set(2).await)["upgrade_to"], "team");
    // Cancelled and ended: Free again. The setting stays; it can go down.
    b.stripe
        .update(&subscription, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    let org = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path(""))
        .await
        .json();
    assert_eq!(org["approvals_required"], 1);
    assert_eq!(set(0).await.status, StatusCode::OK);
    assert_eq!(limit(&set(1).await)["plan"], "free");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn versions_past_the_history_window_are_hidden_not_deleted_and_come_back_on_upgrade(
    db: PgPool,
) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    old_version(&db, shop, 1, 120).await;
    let v2 = old_version(&db, shop, 2, 100).await;
    old_version(&db, shop, 3, 95).await;
    let listed = |who| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(who)
                .get(&team.path("/databases/shop/versions"))
                .await
                .json()
        }
    };
    let numbers = |page: &Value| -> Vec<i64> {
        page["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["number"].as_i64().unwrap())
            .collect()
    };
    // The latest is always shown, however old.
    let page = listed(&b.team.reviewer).await;
    assert_eq!(
        (numbers(&page), page["hidden"].as_i64()),
        (vec![3], Some(2))
    );
    let refused = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases/shop/versions/1"))
        .await;
    let details = limit(&refused);
    assert_eq!(
        (
            details["cap"].as_str(),
            details["limit"].as_i64(),
            details["upgrade_to"].as_str()
        ),
        (Some("history"), Some(90), Some("starter"))
    );
    assert!(refused.json()["message"].as_str().unwrap().contains("kept"));
    let diff = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases/shop/diff?from=1&to=3"))
        .await;
    assert_eq!(limit(&diff)["cap"], "history");
    let migration = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases/shop/migration?from=2&to=3"))
        .await;
    assert_eq!(limit(&migration)["cap"], "history");
    // A draft's base stays visible.
    sqlx::query!(
        "INSERT INTO drafts (database_id, name, base_version_id) VALUES ($1, 'from v2', $2)",
        shop,
        v2
    )
    .execute(&db)
    .await
    .unwrap();
    let page = listed(&b.team.reviewer).await;
    assert_eq!(
        (numbers(&page), page["hidden"].as_i64()),
        (vec![3, 2], Some(1))
    );
    assert_eq!(
        b.app
            .as_user(&b.team.reviewer)
            .get(&b.team.path("/databases/shop/versions/2"))
            .await
            .status,
        StatusCode::OK
    );
    let usage = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(usage["usage"]["hidden_versions"], 1);

    subscribe(&b, "team", "month").await;
    let page = listed(&b.team.reviewer).await;
    assert_eq!(
        (numbers(&page), page["hidden"].as_i64()),
        (vec![3, 2, 1], Some(0))
    );
    assert_eq!(
        b.app
            .as_user(&b.team.reviewer)
            .get(&b.team.path("/databases/shop/versions/1"))
            .await
            .status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn webhooks_need_a_valid_fresh_signature_and_a_redelivery_changes_nothing(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    let changed = audits(&db, "subscription_changed").await;
    assert_eq!(changed, 1);

    b.stripe
        .update(&subscription, |s| s.cancel_at_period_end = true);
    let body = event(
        "evt_fixed",
        "customer.subscription.updated",
        &subscription_object(&subscription),
    );
    let now = Utc::now().timestamp();
    // Not signed, signed with another secret, a body changed after signing, too old.
    for signature in [
        None,
        Some(sign("whsec_other", body.as_bytes(), now)),
        Some(sign(SECRET, b"{}", now)),
        Some(sign(SECRET, body.as_bytes(), now - 600)),
    ] {
        let refused = post_event(&b.app, &body, signature).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::BAD_REQUEST, "bad_signature")
        );
    }
    let stored = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert!(!stored.cancel_at_period_end, "nothing applied");

    let signed = Some(sign(SECRET, body.as_bytes(), now));
    assert_eq!(
        post_event(&b.app, &body, signed.clone()).await.status,
        StatusCode::OK
    );
    assert!(
        romulus_api::billing::stored(&db, b.team.org)
            .await
            .unwrap()
            .cancel_at_period_end
    );
    assert_eq!(audits(&db, "subscription_changed").await, 2);
    // The same delivery again (Stripe retries): acknowledged, not applied twice.
    assert_eq!(
        post_event(&b.app, &body, signed).await.status,
        StatusCode::OK
    );
    assert_eq!(audits(&db, "subscription_changed").await, 2);
    let events = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM billing_events WHERE id = 'evt_fixed'"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn events_out_of_order_store_the_subscription_as_stripe_has_it_now(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    // Stripe now says: cancelled at the period's end. The `updated` event
    // arrives first, then a late `created` from when it was not.
    b.stripe
        .update(&subscription, |s| s.cancel_at_period_end = true);
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    deliver(
        &b,
        "customer.subscription.created",
        json!({"id": subscription, "cancel_at_period_end": false, "status": "active"}),
    )
    .await;
    let stored = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert!(stored.cancel_at_period_end);
    assert_eq!(stored.status.as_deref(), Some("active"));

    // A new subscription after the first ended; then a late event about the
    // old one: the new one stays.
    b.stripe
        .update(&subscription, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    let second = subscribe(&b, "business", "year").await;
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let stored = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert_eq!(stored.subscription_id.as_deref(), Some(second.as_str()));
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("business"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_downgrade_applies_when_stripe_changes_the_price_at_the_period_end(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    for slug in ["one", "two", "three", "four"] {
        assert_eq!(create_database(&b, slug).await.status, StatusCode::CREATED);
    }
    // Scheduled in the portal: Stripe says so until the period ends.
    b.stripe.update(&subscription, |s| {
        s.scheduled = Some(romulus_api::billing::stripe::Scheduled {
            lookup_key: "starter_monthly".into(),
            at: Utc::now() + chrono::Duration::days(20),
        });
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "team");
    assert_eq!(page["subscription"]["scheduled"]["plan"], "starter");
    // The period ends: the price changes.
    b.stripe.update(&subscription, |s| {
        s.scheduled = None;
        let item = s.item.as_mut().unwrap();
        item.price = "price_starter_monthly".into();
        item.lookup_key = Some("starter_monthly".into());
        item.unit_amount = Some(1900);
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("starter"));
    // Over the new cap: everything stays readable, creating waits.
    let listed = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases"))
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 4);
    let details = limit(&create_database(&b, "five").await);
    assert_eq!(
        (details["usage"].as_i64(), details["limit"].as_i64()),
        (Some(4), Some(3))
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_failed_payment_holds_the_plan_fourteen_days_from_the_due_date_then_free(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    assert_eq!(create_database(&b, "one").await.status, StatusCode::CREATED);
    b.stripe
        .update(&subscription, |s| s.status = "past_due".into());
    let invoice = |id: &str, days_ago: i64| {
        json!({"id": id, "object": "invoice", "total": 4900, "currency": "eur",
               "created": (Utc::now() - chrono::Duration::days(days_ago)).timestamp(),
               "due_date": (Utc::now() - chrono::Duration::days(days_ago)).timestamp(),
               "parent": {"subscription_details": {"subscription": subscription}}})
    };
    deliver(&b, "invoice.payment_failed", invoice("in_1", 3)).await;
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "team", "within the grace period");
    assert!(page["subscription"]["grace_until"].is_string());
    assert_eq!(create_database(&b, "two").await.status, StatusCode::CREATED);
    let admin_mail = "admin-acme@romulus.test";
    let failed: Vec<_> = b
        .outbox
        .to(admin_mail)
        .into_iter()
        .filter(|m| m.kind == "billing_payment_failed")
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(failed[0].text.contains("€49.00"), "{}", failed[0].text);
    // Stripe retries and fails again: the same invoice, no second email.
    deliver(&b, "invoice.payment_failed", invoice("in_1", 3)).await;
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_payment_failed")
            .count(),
        1
    );
    assert_eq!(audits(&db, "payment_failed").await, 2);

    // Three days before the end, a warning; at the end, Free.
    sqlx::query!(
        "UPDATE org_billing SET past_due_since = now() - interval '12 days' WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_grace_ending")
            .count(),
        1
    );
    sqlx::query!(
        "UPDATE org_billing SET past_due_since = now() - interval '15 days' WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "free");
    assert_eq!(limit(&create_database(&b, "three").await)["plan"], "free");
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    assert_eq!(audits(&db, "plan_downgraded").await, 1);
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_downgraded")
            .count(),
        1
    );
    // Nothing was deleted.
    let listed = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases"))
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 2);

    // Paid at last: the plan is back.
    b.stripe
        .update(&subscription, |s| s.status = "active".into());
    deliver(&b, "invoice.paid", invoice("in_1", 0)).await;
    assert_eq!(
        b.app
            .as_user(&b.team.admin)
            .get(&b.team.path("/billing"))
            .await
            .json()["plan"],
        "team"
    );
    assert!(
        romulus_api::billing::stored(&db, b.team.org)
            .await
            .unwrap()
            .past_due_since
            .is_none()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_yearly_renewal_is_reminded_once_a_period_with_the_date_amount_and_cancel_link(
    db: PgPool,
) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "year").await;
    let admin_mail = "admin-acme@romulus.test";
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    assert!(
        b.outbox
            .to(admin_mail)
            .iter()
            .all(|m| m.kind != "billing_renewal"),
        "a year ahead: not yet"
    );
    b.stripe.update(&subscription, |s| {
        s.current_period_end = Some(Utc::now() + chrono::Duration::days(35));
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    let reminders: Vec<_> = b
        .outbox
        .to(admin_mail)
        .into_iter()
        .filter(|m| m.kind == "billing_renewal")
        .collect();
    assert_eq!(reminders.len(), 1);
    let text = &reminders[0].text;
    assert!(text.contains("€490.00"), "{text}");
    assert!(
        text.contains("/app/acme/settings/billing?action=cancel"),
        "{text}"
    );
    assert!(text.contains("Nothing is deleted"), "{text}");
    // Only admins get it.
    assert_eq!(b.outbox.to("developer-acme@romulus.test"), Vec::new());
    // Cancelled: no reminder for the next period.
    b.stripe.update(&subscription, |s| {
        s.cancel_at_period_end = true;
        s.current_period_end = Some(Utc::now() + chrono::Duration::days(34));
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_renewal")
            .count(),
        1
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_admins_change_billing_and_members_read_it(db: PgPool) {
    let b = billed(&db).await;
    let checkout =
        json!({"plan": "team", "interval": "month", "accept_terms": true, "buyer": "business"});
    for who in [&b.team.developer, &b.team.reviewer] {
        let page = b.app.as_user(who).get(&b.team.path("/billing")).await;
        assert_eq!(page.status, StatusCode::OK);
        assert_eq!(page.json()["can_manage"], false);
        assert_eq!(
            b.app
                .as_user(who)
                .post(&b.team.path("/billing/checkout"), &checkout)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            b.app
                .as_user(who)
                .post(&b.team.path("/billing/portal"), &json!({}))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            b.app
                .as_user(who)
                .get(&b.team.path("/billing/invoices"))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        b.app
            .as_user(&b.team.outsider)
            .get(&b.team.path("/billing"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        b.app
            .as_user(&b.team.admin)
            .get(&b.team.path("/billing"))
            .await
            .json()["can_manage"],
        true
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn checkout_needs_the_terms_and_a_consumers_requests_and_is_audited(db: PgPool) {
    let b = billed(&db).await;
    let start = |body: Value| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.admin)
                .post(&team.path("/billing/checkout"), &body)
                .await
        }
    };
    let refused = start(
        json!({"plan": "team", "interval": "month", "accept_terms": false, "buyer": "business"}),
    )
    .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "validation")
    );
    assert!(refused.json()["details"]["accept_terms"].is_string());
    let consumer = start(json!({"plan": "team", "interval": "month", "accept_terms": true, "buyer": "consumer", "immediate_start": true})).await;
    assert_eq!(consumer.status, StatusCode::UNPROCESSABLE_ENTITY);
    let free = start(
        json!({"plan": "free", "interval": "month", "accept_terms": true, "buyer": "business"}),
    )
    .await;
    assert_eq!(free.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(audits(&db, "terms_accepted").await, 0);

    let started = start(
        json!({"plan": "team", "interval": "year", "accept_terms": true, "buyer": "consumer",
                               "immediate_start": true, "withdrawal_acknowledged": true}),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    assert!(
        started.json()["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{ORIGIN}/api/billing/fake/checkout/"))
    );
    let (_, checkout) = b.stripe.last_checkout().unwrap();
    assert_eq!(checkout.price, "price_team_yearly");
    assert!(
        checkout
            .success_url
            .ends_with("/app/acme/settings/billing?checkout=success")
    );
    let customer = b
        .stripe
        .state()
        .customers
        .get(&checkout.customer)
        .cloned()
        .unwrap();
    assert_eq!(
        customer.invoice_footer.as_deref(),
        Some("VAT not applicable")
    );
    assert_eq!(
        customer.invoice_fields,
        vec![("Seller".to_owned(), "Example EI".to_owned())]
    );
    assert_eq!(customer.email, "admin-acme@romulus.test");
    let accepted = sqlx::query!(
        r#"SELECT actor_id, details FROM audit_events WHERE action = 'terms_accepted'"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(accepted.actor_id, Some(b.team.admin.user));
    let billing = &accepted.details["billing"];
    assert_eq!(billing["terms_version"], "2026-11-01");
    assert_eq!(billing["buyer"], "consumer");
    assert_eq!(billing["immediate_start"], true);
    assert_eq!(billing["withdrawal_acknowledged"], true);
    assert_eq!(billing["plan"], "team");

    // One customer per organization, whatever the attempts.
    start(
        json!({"plan": "starter", "interval": "month", "accept_terms": true, "buyer": "business"}),
    )
    .await;
    assert_eq!(b.stripe.state().customers.len(), 1);
    let session = b.stripe.last_checkout().unwrap().0;
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    deliver(
        &b,
        "checkout.session.completed",
        json!({"id": session, "subscription": subscription}),
    )
    .await;
    let again = start(
        json!({"plan": "team", "interval": "month", "accept_terms": true, "buyer": "business"}),
    )
    .await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::CONFLICT, "already_subscribed")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_cancel_button_opens_the_portal_on_the_subscriptions_cancel_page(db: PgPool) {
    let b = billed(&db).await;
    let portal = |body: Value| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.admin)
                .post(&team.path("/billing/portal"), &body)
                .await
        }
    };
    assert_eq!(portal(json!({})).await.code(), "no_customer");
    let subscription = subscribe(&b, "starter", "month").await;
    let opened = portal(json!({"flow": {"type": "cancel"}})).await;
    assert_eq!(opened.status, StatusCode::OK, "{}", opened.json());
    assert_eq!(
        opened.json()["url"],
        "https://billing.stripe.com/p/session/test_fake?flow=subscription_cancel"
    );
    let asked = b.stripe.last_portal().unwrap();
    assert_eq!(
        asked.flow,
        Some(PortalFlow::Cancel {
            subscription: subscription.clone()
        })
    );
    assert!(asked.return_url.ends_with("/app/acme/settings/billing"));
    // Choosing Team in the plan picker: straight to confirming it.
    portal(json!({"flow": {"type": "change_plan", "plan": "team", "interval": "year"}})).await;
    assert!(matches!(
        b.stripe.last_portal().unwrap().flow,
        Some(PortalFlow::Confirm { ref price, .. }) if price == "price_team_yearly"
    ));
    assert_eq!(
        portal(json!({})).await.json()["url"],
        "https://billing.stripe.com/p/session/test_fake?flow=home"
    );
    let invoices = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing/invoices"))
        .await
        .json();
    assert_eq!(invoices.as_array().unwrap().len(), 1);
    assert_eq!(invoices[0]["total_cents"], 1900);
    // Ended: nothing left to cancel.
    b.stripe
        .update(&subscription, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(
        portal(json!({"flow": {"type": "cancel"}})).await.code(),
        "no_subscription"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn playground_sizes_and_hours_follow_the_plan_and_used_hours_stop_running_ones(db: PgPool) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let create = |size: &'static str| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.developer)
                .post(
                    &team.path("/databases/shop/playgrounds"),
                    &json!({"size": size}),
                )
                .await
        }
    };
    let medium = create("medium").await;
    assert_eq!(limit(&medium)["cap"], "playground_size");
    assert_eq!(limit(&medium)["upgrade_to"], "team");
    let small = create("small").await;
    assert_eq!(small.status, StatusCode::CREATED, "{}", small.json());
    let id: Uuid = small.json()["id"].as_str().unwrap().parse().unwrap();
    for _ in 0..200 {
        let got = b
            .app
            .as_user(&b.team.developer)
            .get(&b.team.path(&format!("/databases/shop/playgrounds/{id}")))
            .await
            .json();
        if got["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b.host.calls().live.contains(&id));

    // It runs for a minute: a minute counted.
    let now = Utc::now();
    hours::look(&b.app.state, now - chrono::Duration::seconds(60), now)
        .await
        .unwrap();
    assert_eq!(hours::used(&db, b.team.org, now).await.unwrap(), 60);
    // A process paused for an hour does not bill its pause.
    hours::look(&b.app.state, now - chrono::Duration::hours(1), now)
        .await
        .unwrap();
    assert_eq!(hours::used(&db, b.team.org, now).await.unwrap(), 60 + 180);

    // 80 % of Free's 2 hours: a warning, once.
    sqlx::query!(
        "UPDATE playground_usage SET seconds = 5700 WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    hours::look(&b.app.state, now - chrono::Duration::seconds(60), now)
        .await
        .unwrap();
    hours::look(&b.app.state, now - chrono::Duration::seconds(60), now)
        .await
        .unwrap();
    let admin_mail = "admin-acme@romulus.test";
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_hours_warning")
            .count(),
        1
    );
    assert_eq!(b.host.calls().stops.len(), 0);
    let page = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["usage"]["playground_hours"]["warning"], true);

    // All used: stopped, with the reason; refused from then on.
    sqlx::query!(
        "UPDATE playground_usage SET seconds = 7190 WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    hours::look(&b.app.state, now - chrono::Duration::seconds(60), now)
        .await
        .unwrap();
    let stops = b.host.calls().stops.clone();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].0, id);
    assert!(stops[0].1.contains("playground hours"), "{}", stops[0].1);
    assert_eq!(audits(&db, "playground_hours_used_up").await, 1);
    assert_eq!(
        b.outbox
            .to(admin_mail)
            .iter()
            .filter(|m| m.kind == "billing_hours_used_up")
            .count(),
        1
    );
    // Still running at the next look (someone started it): stopped again,
    // told once.
    hours::look(&b.app.state, now - chrono::Duration::seconds(60), now)
        .await
        .unwrap();
    assert_eq!(b.host.calls().stops.len(), 2);
    assert_eq!(audits(&db, "playground_hours_used_up").await, 1);
    let refused = b
        .app
        .as_user(&b.team.developer)
        .post(
            &b.team
                .path(&format!("/databases/shop/playgrounds/{id}/reset")),
            &json!({}),
        )
        .await;
    assert_eq!(limit(&refused)["cap"], "playground_hours");
    let test = b
        .app
        .as_user(&b.team.developer)
        .post(
            &b.team.path("/databases/shop/migration-tests"),
            &json!({"version": 1}),
        )
        .await;
    assert_ne!(test.status, StatusCode::CREATED);
    // The playground's data stays: it is still listed.
    let listed = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases/shop/playgrounds"))
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_member_on_free_is_refused_at_acceptance_too(db: PgPool) {
    // An invitation sent while paying, accepted after the plan ended.
    let b = billed(&db).await;
    let solo = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert!(solo.subscription_id.is_none());
    let other = super::harness::org(&db, "solo").await;
    let owner = user(&db, "Owner").await;
    member(&db, other, owner, Role::Admin).await;
    let link = romulus_api::token::generate();
    let invitee = super::harness::signed_in(&db, "Invitee").await;
    sqlx::query!(
        "INSERT INTO invitations (org_id, email, role, token_hash, expires_at, created_by)
         VALUES ($1, 'invitee@romulus.test', 'developer', $2, now() + interval '1 day', $3)",
        other,
        romulus_api::token::hash(&link),
        owner,
    )
    .execute(&db)
    .await
    .unwrap();
    let accepted = b
        .app
        .as_user(&invitee)
        .post(&format!("/api/invitations/{link}/accept"), &json!({}))
        .await;
    assert_eq!(limit(&accepted)["cap"], "members");
}

// ---- Review round: double checkouts, caps on every way in, notices --------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_second_checkout_expires_the_first_so_two_cannot_both_be_paid(db: PgPool) {
    let b = billed(&db).await;
    let start = |plan: &'static str| {
        let app = &b.app;
        let team = &b.team;
        async move {
            app.as_user(&team.admin)
                .post(
                    &team.path("/billing/checkout"),
                    &json!({"plan": plan, "interval": "month", "accept_terms": true, "buyer": "business"}),
                )
                .await
        }
    };
    // Two tabs: each opens a checkout; the second expires the first.
    let first = start("starter").await;
    assert_eq!(first.status, StatusCode::OK);
    let first_session = b.stripe.last_checkout().unwrap().0;
    let second = start("team").await;
    assert_eq!(second.status, StatusCode::OK);
    let second_session = b.stripe.last_checkout().unwrap().0;
    assert_ne!(first_session, second_session);
    assert_eq!(b.stripe.state().session_status[&first_session], "expired");
    assert!(
        b.stripe.complete_checkout(&first_session).is_none(),
        "the expired one cannot be paid"
    );
    let page = b
        .app
        .get(&format!("/api/billing/fake/checkout/{first_session}"))
        .await;
    assert_eq!(page.code(), "checkout_expired");
    // One customer for the organization, whatever the attempts.
    assert_eq!(b.stripe.state().customers.len(), 1);
    let stored = sqlx::query_scalar!(
        "SELECT checkout_session_id FROM org_billing WHERE org_id = $1",
        b.team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(stored.as_deref(), Some(second_session.as_str()));

    // Paid, but its webhook has not arrived: Stripe already has the
    // subscription, so a third checkout is refused.
    b.stripe.complete_checkout(&second_session).unwrap();
    let third = start("business").await;
    assert_eq!(
        (third.status, third.code().as_str()),
        (StatusCode::CONFLICT, "already_subscribed")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_checkout_retried_after_a_timeout_gets_the_same_session(db: PgPool) {
    let b = billed(&db).await;
    let body =
        json!({"plan": "team", "interval": "year", "accept_terms": true, "buyer": "business"});
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(&b.team.path("/billing/checkout"), &body)
        .await;
    assert_eq!(started.status, StatusCode::OK);
    let (session, _) = b.stripe.last_checkout().unwrap();
    // The answer was lost: what a timeout leaves is the pending nonce, and a
    // session Stripe made under its key.
    let nonce = "pending-nonce";
    let key = romulus_api::billing::stripe::checkout_key(b.team.org, "price_team_yearly", nonce);
    b.stripe.state().keys.insert(key, session.clone());
    sqlx::query!(
        "UPDATE org_billing SET checkout_session_id = NULL, checkout_nonce = $2, checkout_price = 'price_team_yearly'
         WHERE org_id = $1",
        b.team.org,
        nonce,
    )
    .execute(&db)
    .await
    .unwrap();
    let again = b
        .app
        .as_user(&b.team.admin)
        .post(&b.team.path("/billing/checkout"), &body)
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(
        again.json()["url"],
        started.json()["url"],
        "the same session, not a second"
    );
    assert_eq!(b.stripe.state().checkouts.len(), 1);
    assert_eq!(b.stripe.state().session_status[&session], "open");
    // The customer's key is the organization's: asked twice, one customer.
    let key = romulus_api::billing::stripe::customer_key(b.team.org);
    assert!(b.stripe.state().keys.contains_key(&key));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_second_live_subscription_is_cancelled_refunded_and_the_first_kept(db: PgPool) {
    let b = billed(&db).await;
    let first = subscribe(&b, "team", "month").await;
    // A second subscription for the same organization (two checkouts paid
    // in the same instant, or one made in the Dashboard).
    let mut second = b.stripe.state().subscriptions[&first].clone();
    second.id = "sub_second".into();
    second.item.as_mut().unwrap().unit_amount = Some(1900);
    second.item.as_mut().unwrap().lookup_key = Some("starter_monthly".into());
    b.stripe.insert(second);
    deliver(
        &b,
        "customer.subscription.created",
        subscription_object("sub_second"),
    )
    .await;

    let stored = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert_eq!(
        stored.subscription_id.as_deref(),
        Some(first.as_str()),
        "the first stays"
    );
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("team"));
    assert_eq!(
        b.stripe.state().subscriptions["sub_second"].status,
        "canceled"
    );
    assert_eq!(
        b.stripe.state().refunds,
        vec![("sub_second".to_owned(), 1900)]
    );
    assert_eq!(audits(&db, "subscription_duplicate_cancelled").await, 1);
    let mails: Vec<_> = b
        .outbox
        .to("admin-acme@romulus.test")
        .into_iter()
        .filter(|m| m.kind == "billing_duplicate_cancelled")
        .collect();
    assert_eq!(mails.len(), 1);
    assert!(mails[0].text.contains("€19.00"), "{}", mails[0].text);
    // Its own events later (the cancellation, its invoice) change nothing.
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object("sub_second"),
    )
    .await;
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object("sub_second"),
    )
    .await;
    assert_eq!(
        romulus_api::billing::stored(&db, b.team.org)
            .await
            .unwrap()
            .subscription_id
            .as_deref(),
        Some(first.as_str())
    );
    assert_eq!(b.stripe.state().refunds.len(), 1, "refunded once");
    assert_eq!(audits(&db, "subscription_duplicate_cancelled").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_webhook_holds_no_lock_while_stripe_answers_and_reads_again_after_a_write(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    let reads = b.stripe.state().reads;
    let (reached, release) = (
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(tokio::sync::Notify::new()),
    );
    b.stripe.state().read_gate = Some((reached.clone(), release.clone()));
    b.stripe
        .update(&subscription, |s| s.cancel_at_period_end = true);
    let body = event(
        "evt_gated",
        "customer.subscription.updated",
        &subscription_object(&subscription),
    );
    let signature = sign(SECRET, body.as_bytes(), Utc::now().timestamp());
    let app = b.app.state.clone();
    let delivery = tokio::spawn(async move {
        let app = TestApp::with_state(app);
        post_event(&app, &body, Some(signature)).await.status
    });
    reached.notified().await;
    // While Stripe "answers", the organization's lock is free, and another
    // event writes the row.
    let mut tx = db.begin().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        romulus_api::billing::lock_org(&mut tx, b.team.org),
    )
    .await
    .expect("the lock is not held across the Stripe call")
    .unwrap();
    sqlx::query!(
        "UPDATE org_billing SET updated_at = clock_timestamp() WHERE org_id = $1",
        b.team.org
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    release.notify_one();
    assert_eq!(delivery.await.unwrap(), StatusCode::OK);
    // It read Stripe again after seeing the write, then stored.
    assert!(b.stripe.state().reads >= reads + 2);
    assert!(
        romulus_api::billing::stored(&db, b.team.org)
            .await
            .unwrap()
            .cancel_at_period_end
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_subscription_to_a_price_romulus_did_not_make_gives_free(db: PgPool) {
    let b = billed(&db).await;
    let first = subscribe(&b, "team", "month").await;
    // The same subscription moved to an unknown price keeps its plan.
    b.stripe.update(&first, |s| {
        let item = s.item.as_mut().unwrap();
        item.lookup_key = None;
        item.price = "price_custom".into();
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&first),
    )
    .await;
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("team"));
    // Another subscription on an unknown price, after the first ended: Free.
    b.stripe.update(&first, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&first),
    )
    .await;
    let mut other = b.stripe.state().subscriptions[&first].clone();
    other.id = "sub_custom".into();
    other.status = "active".into();
    b.stripe.insert(other);
    deliver(
        &b,
        "customer.subscription.created",
        subscription_object("sub_custom"),
    )
    .await;
    let stored = romulus_api::billing::stored(&db, b.team.org).await.unwrap();
    assert_eq!(stored.subscription_id.as_deref(), Some("sub_custom"));
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("free"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_late_payment_failure_for_an_invoice_paid_since_says_nothing(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "starter", "month").await;
    let invoice = b.stripe.state().invoices[0].clone();
    assert_eq!(invoice.status.as_deref(), Some("paid"));
    deliver(
        &b,
        "invoice.payment_failed",
        json!({"id": invoice.id, "object": "invoice", "total": 1900, "currency": "eur",
               "created": Utc::now().timestamp(), "due_date": Utc::now().timestamp(),
               "parent": {"subscription_details": {"subscription": subscription}}}),
    )
    .await;
    assert_eq!(audits(&db, "payment_failed").await, 0);
    assert!(
        b.outbox
            .to("admin-acme@romulus.test")
            .iter()
            .all(|m| m.kind != "billing_payment_failed")
    );
    assert!(
        romulus_api::billing::stored(&db, b.team.org)
            .await
            .unwrap()
            .past_due_since
            .is_none()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_renewal_reminder_states_the_change_scheduled_for_the_renewal(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "year").await;
    let end = Utc::now() + chrono::Duration::days(35);
    b.stripe.update(&subscription, |s| {
        s.current_period_end = Some(end);
        s.scheduled = Some(romulus_api::billing::stripe::Scheduled {
            lookup_key: "starter_monthly".into(),
            at: end,
        });
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    let reminder = b
        .outbox
        .to("admin-acme@romulus.test")
        .into_iter()
        .find(|m| m.kind == "billing_renewal")
        .expect("a reminder");
    assert!(
        reminder.subject.contains("Starter plan renews"),
        "{}",
        reminder.subject
    );
    assert!(
        reminder
            .text
            .contains("renews automatically as the monthly Starter plan, for €19.00 a month"),
        "{}",
        reminder.text
    );
    assert!(!reminder.text.contains("another year"), "{}", reminder.text);
}

/// Password sign-in and billing on, mail off (an invitation vouches).
fn password_app(db: &PgPool) -> TestApp {
    let mut config = Config::for_tests();
    config.auth.password = true;
    let billing = Billing::with_stripe(
        billing_config(),
        config.public_url.clone(),
        db.clone(),
        Fake::new(ORIGIN),
    );
    let mut state = romulus_api::AppState::new(db.clone(), config).with_billing(billing);
    state.passwords = std::sync::Arc::new(romulus_api::auth::password::throttle::Limits::default());
    TestApp::with_state(state)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_password_sign_up_from_an_invitation_respects_the_members_cap(db: PgPool) {
    let app = password_app(&db);
    let org = super::harness::org(&db, "solo").await;
    let owner = user(&db, "Owner").await;
    member(&db, org, owner, Role::Admin).await;
    let link = romulus_api::token::generate();
    sqlx::query!(
        "INSERT INTO invitations (org_id, email, role, token_hash, expires_at, created_by)
         VALUES ($1, 'zoe@acme.io', 'developer', $2, now() + interval '1 day', $3)",
        org,
        romulus_api::token::hash(&link),
        owner,
    )
    .execute(&db)
    .await
    .unwrap();
    let body = json!({"email": "zoe@acme.io", "name": "Zoe", "password": "a long unusual passphrase 2026!", "invitation": link});
    let refused = app.post_json("/api/auth/password/sign-up", &body).await;
    assert_eq!(limit(&refused)["cap"], "members");
    let accounts =
        sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users WHERE email = 'zoe@acme.io'"#)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(accounts, 0, "nothing was created");
    // A paying organization takes her in.
    sqlx::query!(
        "INSERT INTO org_billing (org_id, subscription_id, plan, interval, status) VALUES ($1, 'sub_x', 'starter', 'month', 'active')",
        org
    )
    .execute(&db)
    .await
    .unwrap();
    let joined = app.post_json("/api/auth/password/sign-up", &body).await;
    assert!(joined.status.is_success(), "{}", joined.json());
    let members = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM memberships WHERE org_id = $1"#,
        org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(members, 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn restoring_a_removed_version_is_within_the_versions_cap(db: PgPool) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    for number in 1..=51 {
        old_version(&db, shop, number, 1).await;
    }
    sqlx::query!(
        "UPDATE versions SET removed_at = now(), removed_by = $2, removal_reason = 'tidy' WHERE database_id = $1 AND number = 3",
        shop,
        b.team.admin.user,
    )
    .execute(&db)
    .await
    .unwrap();
    // 50 live: the Free plan's. Restoring v3 would make 51.
    let restore = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/databases/shop/versions/3/restore"),
            &json!({}),
        )
        .await;
    let details = limit(&restore);
    assert_eq!(
        (details["cap"].as_str(), details["usage"].as_i64()),
        (Some("versions"), Some(50))
    );
    let removed = sqlx::query_scalar!(
        r#"SELECT removed_at IS NOT NULL AS "removed!" FROM versions WHERE database_id = $1 AND number = 3"#,
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(removed, "still removed");
    subscribe(&b, "starter", "month").await;
    let restored = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/databases/shop/versions/3/restore"),
            &json!({}),
        )
        .await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_console_past_the_hours_neither_connects_nor_opens(db: PgPool) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let created = b
        .app
        .as_user(&b.team.developer)
        .post(&b.team.path("/databases/shop/playgrounds"), &json!({}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let id = created.json()["id"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let got = b
            .app
            .as_user(&b.team.developer)
            .get(&b.team.path(&format!("/databases/shop/playgrounds/{id}")))
            .await
            .json();
        if got["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let addr = super::live::serve(&b.app).await;
    let url = b
        .team
        .path(&format!("/databases/shop/playgrounds/{id}/console"));
    // Connected while hours remain.
    let mut socket = super::playgrounds::console(addr, &url, Some(&b.team.developer), ORIGIN)
        .await
        .expect("the console opens");
    // All used: `open` (it would start the stopped playground) is refused here.
    let (month, _) = hours::month_bounds(Utc::now());
    sqlx::query!(
        "INSERT INTO playground_usage (org_id, month, seconds) VALUES ($1, $2, 7200)",
        b.team.org,
        month
    )
    .execute(&db)
    .await
    .unwrap();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"open"}"#.into(),
        ))
        .await
        .unwrap();
    let refusal = loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an answer")
            .expect("a frame")
            .unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "unavailable" {
                break value;
            }
        }
    };
    assert_eq!(refusal["code"], "plan_limit");
    assert!(refusal["reason"].as_str().unwrap().contains("playground"));
    // And a new connection is refused before the upgrade.
    let refused = super::playgrounds::console(addr, &url, Some(&b.team.developer), ORIGIN).await;
    assert_eq!(refused.err(), Some(402));
}
