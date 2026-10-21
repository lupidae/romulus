//! Operator notifications: each event queues one notification per operator
//! in the mail outbox (and nothing when `ROMULUS_OPERATOR_EMAILS` is empty),
//! which sends each once and retries what failed; the digest counts what a
//! seeded database holds and goes once a day.

use axum::http::StatusCode;
use chrono::{DateTime, Datelike as _, Duration, NaiveDate, TimeZone as _, Utc};
use romulus_api::{
    AppState, Config,
    auth::{oidc::Identity, upsert_user},
    billing::{
        Billing, notices,
        stripe::{Cancellation, Fake, Scheduled},
    },
    config::OperatorConfig,
    domain::{Plan, Role},
    mail::{Mailer, outbox},
    operator::{self, accounts, digest},
    retention::deletion::{By, Deleter},
};
use serde_json::json;
use sqlx::PgPool;

use super::{
    billing::{
        Billed, billed_with_config, billing_config, consumer_checkout, deliver, legal_config,
        subscribe, subscription_object,
    },
    harness::{ORIGIN, TestApp, TestResponse, member, model, org, signed_in, user},
};

const OPS: [&str; 2] = ["ops@romulus.test", "founder@romulus.test"];

/// `config` with notifications to [`OPS`], the digest from `hour`.
fn operated_at(config: Config, hour: u32) -> Config {
    Config {
        operator: Some(OperatorConfig {
            emails: OPS.map(str::to_owned).to_vec(),
            digest_hour: hour,
            origin: config.public_origin(),
        }),
        ..config
    }
}

fn operated(config: Config) -> Config {
    operated_at(config, 7)
}

/// What is queued for `event` (`operator_<event>` letters in the mail
/// outbox): (recipient, subject, text), by recipient.
async fn queued(db: &PgPool, event: &str) -> Vec<(String, String, String)> {
    sqlx::query!(
        "SELECT recipient, subject, text_body FROM mail_outbox
         WHERE kind = 'operator_' || $1 ORDER BY recipient, created_at",
        event
    )
    .fetch_all(db)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.recipient, r.subject, r.text_body))
    .collect()
}

/// The one notification of `event` queued for each operator: its subject
/// and body (the same for both).
async fn told(db: &PgPool, event: &str) -> (String, String) {
    let rows = queued(db, event).await;
    assert_eq!(rows.len(), OPS.len(), "{event}: one per operator: {rows:?}");
    let mut recipients: Vec<&str> = rows.iter().map(|(to, _, _)| to.as_str()).collect();
    recipients.sort_unstable();
    let mut expected = OPS.to_vec();
    expected.sort_unstable();
    assert_eq!(recipients, expected);
    assert_eq!(rows[0].1, rows[1].1);
    (rows[0].1.clone(), rows[0].2.clone())
}

async fn total(db: &PgPool) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM mail_outbox WHERE starts_with(kind, 'operator_')"#
    )
    .fetch_one(db)
    .await
    .unwrap()
}

fn dev(config: Config) -> Config {
    Config {
        dev_signin: true,
        ..config
    }
}

async fn dev_sign_in(app: &TestApp, email: &str) {
    let response = app
        .get(&format!(
            "/api/auth/dev-signin?email={email}&name=Ada%20Lovelace"
        ))
        .await;
    assert!(response.status.is_redirection(), "{}", response.status);
}

fn identity(subject: &str) -> Identity {
    Identity {
        subject: subject.to_owned(),
        email: Some("grace@example.com".to_owned()),
        email_verified: true,
        name: Some("Grace Hopper".to_owned()),
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn nothing_is_queued_while_notifications_are_off(db: PgPool) {
    let app = TestApp::with_config(db.clone(), dev(Config::for_tests()));
    dev_sign_in(&app, "ada@example.com").await;
    upsert_user(
        &db,
        app.state.config.operator.as_ref(),
        &identity("sub-grace"),
    )
    .await
    .unwrap();
    let alice = signed_in(&db, "Alice").await;
    let created = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(total(&db).await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_account_and_a_new_organization_are_told_once_to_each_operator(db: PgPool) {
    let app = TestApp::with_config(db.clone(), operated(dev(Config::for_tests())));
    dev_sign_in(&app, "ada@example.com").await;
    dev_sign_in(&app, "ada@example.com").await;
    let (subject, body) = told(&db, "account_created").await;
    assert_eq!(
        subject,
        "[romulus] New account: Ada Lovelace <ada@example.com>"
    );
    assert!(body.contains("development sign-in"), "{body}");

    // A first sign-in with single sign-on, then a second one: one more.
    let operators = app.state.config.operator.as_ref();
    upsert_user(&db, operators, &identity("sub-grace"))
        .await
        .unwrap();
    upsert_user(&db, operators, &identity("sub-grace"))
        .await
        .unwrap();
    let accounts = queued(&db, "account_created").await;
    assert_eq!(accounts.len(), 2 * OPS.len());
    assert!(accounts.iter().any(|(_, subject, body)| {
        subject.contains("Grace Hopper <grace@example.com>") && body.contains("single sign-on")
    }));

    let alice = signed_in(&db, "Alice").await;
    let created = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme Inc", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let (subject, body) = told(&db, "org_created").await;
    assert_eq!(subject, "[romulus] New organization: Acme Inc (acme)");
    assert!(
        body.contains("Created by Alice <alice@romulus.test>, its admin."),
        "{body}"
    );
    // A slug taken: nothing created, nothing told.
    let taken = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme again", "slug": "acme"}))
        .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(queued(&db, "org_created").await.len(), OPS.len());
}

async fn operated_billing(db: &PgPool, consumer_sales: bool) -> Billed {
    billed_with_config(db, consumer_sales, operated(Config::for_tests())).await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_subscriptions_life_is_told_once_per_change(db: PgPool) {
    let b = operated_billing(&db, false).await;
    let subscription = subscribe(&b, "team", "month").await;
    let (subject, body) = told(&db, "subscription_started").await;
    assert_eq!(
        subject,
        "[romulus] New subscription: acme (acme), Team, monthly: €49.00 a month"
    );
    for line in [
        "Bought by Admin-acme <admin-acme@romulus.test>, as a business: Acme SAS (FR).",
        "MRR: +€49.00 a month",
        "Stripe customer: https://dashboard.stripe.com/test/customers/",
        "Stripe subscription: https://dashboard.stripe.com/test/subscriptions/",
    ] {
        assert!(body.contains(line), "{line}\n---\n{body}");
    }
    // Stripe says the same again: nothing new.
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(total(&db).await, 2);

    // Upgraded in the portal.
    let business = i64::from(
        Plan::Business
            .price_cents(romulus_api::domain::Interval::Month)
            .unwrap(),
    );
    b.stripe.update(&subscription, |s| {
        let item = s.item.as_mut().unwrap();
        item.price = "price_business_monthly".into();
        item.lookup_key = Some("business_monthly".into());
        item.unit_amount = Some(business);
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let (subject, body) = told(&db, "plan_changed").await;
    assert_eq!(
        subject,
        "[romulus] Upgraded: acme (acme), Team monthly → Business monthly"
    );
    assert!(body.contains("MRR: +€"), "{body}");

    // A downgrade scheduled for the period's end.
    b.stripe.update(&subscription, |s| {
        s.scheduled = Some(Scheduled {
            lookup_key: "starter_monthly".into(),
            at: Utc::now() + Duration::days(20),
        });
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let changes = queued(&db, "plan_changed").await;
    assert_eq!(changes.len(), 2 * OPS.len());
    assert!(changes.iter().any(|(_, subject, _)| {
        subject.starts_with("[romulus] Downgrade scheduled: acme (acme), Business → Starter on ")
    }));

    // Cancelled in the portal, with a reason.
    b.stripe.update(&subscription, |s| {
        s.cancel_at_period_end = true;
        s.cancellation = Some(Cancellation {
            reason: Some("cancellation_requested".into()),
            feedback: Some("too_expensive".into()),
            comment: Some("Too pricey for us".into()),
        });
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    // Stripe says it again (another event): told once.
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let (subject, body) = told(&db, "cancellation_scheduled").await;
    assert!(
        subject.starts_with("[romulus] Cancellation scheduled: acme (acme), Business (ends "),
        "{subject}"
    );
    assert!(
        body.contains(
            "Reason: the customer asked; feedback: too expensive; comment: “Too pricey for us”."
        ),
        "{body}"
    );

    // The period ends: over.
    b.stripe
        .update(&subscription, |s| s.status = "canceled".into());
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    let (subject, body) = told(&db, "subscription_cancelled").await;
    assert_eq!(
        subject,
        "[romulus] Subscription cancelled: acme (acme), Business"
    );
    assert!(body.contains("The organization is on Free."), "{body}");
    assert!(body.contains("MRR: -€"), "{body}");
    assert_eq!(total(&db).await, 2 * 5, "nothing else");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_failed_payment_is_told_once_per_invoice_and_the_grace_periods_end_once(db: PgPool) {
    let b = operated_billing(&db, false).await;
    let subscription = subscribe(&b, "team", "month").await;
    b.stripe
        .update(&subscription, |s| s.status = "past_due".into());
    let invoice = json!({"id": "in_1", "object": "invoice", "total": 4900, "currency": "eur",
           "number": "ROM-7",
           "created": (Utc::now() - Duration::days(3)).timestamp(),
           "due_date": (Utc::now() - Duration::days(3)).timestamp(),
           "parent": {"subscription_details": {"subscription": subscription}}});
    deliver(&b, "invoice.payment_failed", invoice.clone()).await;
    deliver(&b, "invoice.payment_failed", invoice).await;
    let (subject, body) = told(&db, "payment_failed").await;
    assert_eq!(subject, "[romulus] Payment failed: acme (acme), €49.00");
    assert!(body.contains("(invoice ROM-7)"), "{body}");
    assert!(body.contains("The plan holds until "), "{body}");
    assert!(
        body.contains("Stripe invoice: https://dashboard.stripe.com/test/invoices/in_1"),
        "{body}"
    );

    sqlx::query!(
        "UPDATE org_billing SET past_due_since = now() - interval '15 days' WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    let (subject, body) = told(&db, "grace_ended").await;
    assert_eq!(subject, "[romulus] Moved to Free: acme (acme), Team unpaid");
    assert!(body.contains("within 14 days"), "{body}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_consumers_withdrawal_is_told_with_its_refund(db: PgPool) {
    let b = operated_billing(&db, true).await;
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &consumer_checkout("starter", "month"),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    let (session, _) = b.stripe.last_checkout().unwrap();
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    deliver(
        &b,
        "checkout.session.completed",
        json!({"id": session, "object": "checkout.session", "subscription": subscription,
               "client_reference_id": b.team.org}),
    )
    .await;
    let (_, body) = told(&db, "subscription_started").await;
    assert!(
        body.contains("Bought by Admin-acme <admin-acme@romulus.test>, as a consumer."),
        "{body}"
    );

    let withdrawn = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/withdrawal"),
            &json!({"locale": "en"}),
        )
        .await;
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.json());
    let (subject, body) = told(&db, "withdrawal").await;
    assert_eq!(
        subject,
        "[romulus] Withdrawal: acme (acme), Starter, refund €19.00"
    );
    assert!(
        body.contains("Admin-acme <admin-acme@romulus.test> withdrew from acme (acme)'s Starter monthly subscription, 0 days after it began"),
        "{body}"
    );
    assert!(body.contains("Paid €19.00; refunded €19.00"), "{body}");
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

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reaching_a_plans_cap_is_told_once_in_thirty_days(db: PgPool) {
    let b = operated_billing(&db, false).await;
    assert_eq!(create_database(&b, "one").await.status, StatusCode::CREATED);
    assert_eq!(
        create_database(&b, "two").await.status,
        StatusCode::PAYMENT_REQUIRED
    );
    assert_eq!(
        create_database(&b, "three").await.status,
        StatusCode::PAYMENT_REQUIRED
    );
    let (subject, body) = told(&db, "plan_limit_reached").await;
    assert_eq!(
        subject,
        "[romulus] Plan limit reached: acme (acme), Free, databases: 1 of 1"
    );
    assert!(body.contains("The Starter plan lifts it."), "{body}");
    assert!(
        body.contains("Admins: Admin-acme <admin-acme@romulus.test>."),
        "{body}"
    );
    // A month later, again.
    sqlx::query!("UPDATE operator_limit_notices SET notified_at = now() - interval '31 days'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        create_database(&b, "four").await.status,
        StatusCode::PAYMENT_REQUIRED
    );
    assert_eq!(queued(&db, "plan_limit_reached").await.len(), 2 * OPS.len());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deletions_are_told_with_what_went(db: PgPool) {
    let config = operated(Config::for_tests());
    let deleter = Deleter::new(db.clone(), &config, Mailer::off());
    let ada = user(&db, "Ada").await;
    let alone = org(&db, "solo").await;
    member(&db, alone, ada, Role::Admin).await;
    deleter.delete_account(ada, By::Request).await.unwrap();
    let (subject, body) = told(&db, "account_deleted").await;
    assert_eq!(
        subject,
        "[romulus] Account deleted: ada@romulus.test (on request)"
    );
    assert!(
        body.contains("Deleted with it, as its only member: solo (solo)."),
        "{body}"
    );
    assert!(
        body.contains("Delete its Zitadel user sub-Ada in Zitadel's console"),
        "{body}"
    );
    assert!(
        queued(&db, "org_deleted").await.is_empty(),
        "in the account's"
    );

    // Refused: the only admin of an organization others belong to.
    let bob = user(&db, "Bob").await;
    let cy = user(&db, "Cy").await;
    let shared = org(&db, "shared").await;
    member(&db, shared, bob, Role::Admin).await;
    member(&db, shared, cy, Role::Developer).await;
    assert!(deleter.delete_account(bob, By::Request).await.is_err());
    assert_eq!(queued(&db, "account_deleted").await.len(), OPS.len());

    deleter.delete_org(shared).await.unwrap();
    let (subject, body) = told(&db, "org_deleted").await;
    assert_eq!(subject, "[romulus] Organization deleted: shared (shared)");
    assert!(body.contains("2 members"), "{body}");
}

/// An app with notifications on (the digest from `hour`) and billing on.
fn billed_state(db: &PgPool, hour: u32) -> AppState {
    let config = operated_at(Config::for_tests(), hour);
    let billing = Billing::with_stripe(
        billing_config(),
        legal_config(false),
        config.public_url.clone(),
        db.clone(),
        Fake::new(ORIGIN),
    );
    AppState::new(db.clone(), config).with_billing(billing)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_outbox_sends_each_notification_once_and_retries_what_failed(db: PgPool) {
    let app = TestApp::with_config(db.clone(), operated(Config::for_tests()));
    let alice = signed_in(&db, "Alice").await;
    let created = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(total(&db).await, 2);
    // Queued again (the same organization): nothing more.
    let acme = sqlx::query_scalar!("SELECT id FROM organizations WHERE slug = 'acme'")
        .fetch_one(&db)
        .await
        .unwrap();
    let mut tx = db.begin().await.unwrap();
    operator::notify(
        &mut tx,
        app.state.config.operator.as_ref(),
        &accounts::org_created(acme, "Acme", "acme", None),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(total(&db).await, 2);

    // The mail server refuses: kept for later, not due again yet.
    let (mailer, sent) = Mailer::memory();
    sent.refuse_next(2);
    let done = outbox::deliver(&db, &mailer).await.unwrap();
    assert_eq!((done.sent, done.retrying), (0, 2));
    assert_eq!(outbox::deliver(&db, &mailer).await.unwrap().sent, 0);
    assert_eq!(sent.all().len(), 0, "none accepted");

    // Due again: two workers at once send each notification once.
    sqlx::query!(
        "UPDATE mail_outbox SET next_attempt_at = now() WHERE starts_with(kind, 'operator_')"
    )
    .execute(&db)
    .await
    .unwrap();
    let (one, two) = tokio::join!(outbox::deliver(&db, &mailer), outbox::deliver(&db, &mailer));
    assert_eq!(one.unwrap().sent + two.unwrap().sent, 2);
    assert_eq!(outbox::deliver(&db, &mailer).await.unwrap().sent, 0);
    assert_eq!(sent.all().len(), 2);
    for to in OPS {
        let mine = sent.to(to);
        assert_eq!(mine.len(), 1, "{to}");
        assert_eq!(mine[0].subject, "[romulus] New organization: Acme (acme)");
        assert_eq!(mine[0].kind, "operator_org_created");
        assert!(mine[0].text.contains(&format!(
            "Sent by romulus at {ORIGIN} to its operators (ROMULUS_OPERATOR_EMAILS)."
        )));
        assert!(mine[0].html.contains("New organization: Acme (acme)."));
    }
    let attempts = sqlx::query_scalar!(
        r#"SELECT attempts AS "n!" FROM mail_outbox
           WHERE starts_with(kind, 'operator_') AND sent_at IS NOT NULL"#
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(attempts, [2, 2]);
}

fn midnight(day: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
}

/// Seeds a database whose digest for `day` (computed at `now`) is known.
#[allow(clippy::too_many_lines, reason = "one block per line of the digest")]
async fn seed(db: &PgPool, day: NaiveDate, now: DateTime<Utc>) {
    let end = midnight(day);
    let yesterday = end - Duration::hours(10);
    // Accounts: one yesterday, one 3 days before, one a month before (last
    // seen then), one today.
    let mut users = Vec::new();
    for (name, created, seen) in [
        ("Ada", yesterday, yesterday),
        ("Bob", end - Duration::days(3), end - Duration::days(3)),
        ("Cy", end - Duration::days(30), end - Duration::days(30)),
        ("Dee", now, now),
    ] {
        let id = user(db, name).await;
        sqlx::query!(
            "UPDATE users SET created_at = $2, last_seen_at = $3 WHERE id = $1",
            id,
            created,
            seen
        )
        .execute(db)
        .await
        .unwrap();
        users.push(id);
    }
    // Organizations: acme (Ada's) yesterday, beta (Bob's) 5 days before,
    // gamma (Dee's) today, delta (Cy's) 40 days before.
    let mut orgs = Vec::new();
    for (slug, created, who) in [
        ("acme", yesterday, users[0]),
        ("beta", end - Duration::days(5), users[1]),
        ("gamma", now, users[3]),
        ("delta", end - Duration::days(40), users[2]),
    ] {
        let id = org(db, slug).await;
        sqlx::query!(
            "UPDATE organizations SET created_at = $2 WHERE id = $1",
            id,
            created
        )
        .execute(db)
        .await
        .unwrap();
        member(db, id, who, Role::Admin).await;
        orgs.push(id);
    }
    // Billing: acme on Team monthly, beta on Business yearly ending with
    // its period, gamma's Starter past due 3 days, delta's Team cancelled.
    for (i, (plan, interval, status, amount, cancelling, past_due)) in [
        ("team", "month", "active", 4_900_i64, false, None),
        ("business", "year", "active", 120_000, true, None),
        (
            "starter",
            "month",
            "past_due",
            1_900,
            false,
            Some(now - Duration::days(3)),
        ),
        ("team", "month", "canceled", 4_900, false, None),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query!(
            "INSERT INTO org_billing (org_id, customer_id, subscription_id, plan, interval, status,
                                      current_period_end, cancel_at_period_end, amount_cents,
                                      currency, past_due_since)
             VALUES ($1, $2, $3, $4::text::billing_plan, $5::text::billing_interval, $6, $7, $8,
                     $9, 'eur', $10)",
            orgs[i],
            format!("cus_{i}"),
            format!("sub_{i}"),
            plan,
            interval,
            status,
            now + Duration::days(20),
            cancelling,
            amount,
            past_due,
        )
        .execute(db)
        .await
        .unwrap();
    }
    let month = day.pred_opt().unwrap().with_day(1).unwrap();
    for (i, seconds) in [(0, 5_400_i64), (1, 1_800)] {
        sqlx::query!(
            "INSERT INTO playground_usage (org_id, month, seconds) VALUES ($1, $2, $3)",
            orgs[i],
            month,
            seconds
        )
        .execute(db)
        .await
        .unwrap();
    }
    // Imports: two yesterday (two migrations, then one), one 3 days before.
    let shop = super::harness::database(db, orgs[0], "shop").await;
    let model = serde_json::to_value(model(&["users"])).unwrap();
    for (number, created) in [
        (1, yesterday),
        (2, yesterday),
        (3, yesterday + Duration::hours(1)),
        (4, end - Duration::days(3)),
    ] {
        sqlx::query!(
            "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout,
                                   created_at)
             VALUES ($1, $2, $3, 'CREATE TABLE users ();', 'migration', 'import', '{}', $4)",
            shop,
            number,
            model,
            created,
        )
        .execute(db)
        .await
        .unwrap();
    }
    let version = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = 1",
        shop
    )
    .fetch_one(db)
    .await
    .unwrap();
    // Migration tests: three yesterday (passed, failed, error), one 4 days
    // before.
    for (status, created) in [
        ("passed", yesterday),
        ("failed", yesterday),
        ("error", yesterday),
        ("passed", end - Duration::days(4)),
    ] {
        sqlx::query!(
            "INSERT INTO migration_tests (org_id, database_id, source, from_version_id,
                                          to_version_id, up_sql, down, size, profile, seed, status,
                                          created_at, finished_at)
             VALUES ($1, $2, 'version', $3, $3, 'SELECT 1;', 'none', 'small', 'realistic', 0,
                     $4::text::migration_test_status, $5, $5)",
            orgs[0],
            shop,
            version,
            status,
            created,
        )
        .execute(db)
        .await
        .unwrap();
    }
    // Deletions: an account yesterday and one 5 days before, an
    // organization yesterday.
    for (kind, created) in [
        ("account_deleted", yesterday),
        ("account_deleted", end - Duration::days(5)),
        ("organization_deleted", yesterday),
    ] {
        sqlx::query!(
            "INSERT INTO auth_events (kind, created_at) VALUES ($1::text::auth_event_kind, $2)",
            kind,
            created
        )
        .execute(db)
        .await
        .unwrap();
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_digest_counts_what_the_database_holds(db: PgPool) {
    let now = Utc::now();
    let day = now.date_naive();
    seed(&db, day, now).await;
    let state = billed_state(&db, 0);
    let mut conn = db.acquire().await.unwrap();
    let digest = digest::compute(&mut conn, &state, day, now).await.unwrap();
    assert_eq!(
        (
            digest.accounts.yesterday,
            digest.accounts.week,
            digest.accounts_total
        ),
        (1, 2, 4)
    );
    assert_eq!(
        (digest.orgs.yesterday, digest.orgs.week, digest.orgs_total),
        (1, 2, 4)
    );
    // All but Cy, and their organizations.
    assert_eq!((digest.active_accounts, digest.active_orgs), (3, 3));
    let billing = digest.billing.clone().unwrap();
    assert_eq!(
        billing.paying.into_iter().collect::<Vec<_>>(),
        [(Plan::Starter, 1), (Plan::Team, 1), (Plan::Business, 1)]
    );
    // 49.00 + 1200.00 / 12 + 19.00.
    assert_eq!(billing.mrr.get("eur"), Some(&16_800));
    assert_eq!(billing.cancelling.len(), 1);
    assert_eq!(billing.cancelling[0].0, "beta (beta)");
    assert_eq!(billing.in_grace.len(), 1);
    assert_eq!(billing.in_grace[0].0, "gamma (gamma)");
    assert_eq!(billing.playground_seconds, 7_200);
    assert_eq!(
        (
            digest.imports.yesterday,
            digest.imported_migrations,
            digest.imports.week
        ),
        (2, 3, 3)
    );
    assert_eq!(
        (
            digest.tests.yesterday,
            digest.tests_passed,
            digest.tests_failed,
            digest.tests_errors,
            digest.tests.week
        ),
        (3, 1, 1, 1, 4)
    );
    assert_eq!(
        (
            digest.accounts_deleted.yesterday,
            digest.accounts_deleted.week
        ),
        (1, 2)
    );
    assert_eq!(
        (digest.orgs_deleted.yesterday, digest.orgs_deleted.week),
        (1, 1)
    );

    // As it reads.
    let notice = digest::notice(&digest, None, ORIGIN);
    let text = notice.lines.join("\n");
    for line in [
        "Accounts: +1 (+2 in 7 days), 4 in all; 3 active in 7 days",
        "Paying organizations: 3 (Starter 1, Team 1, Business 1)",
        "MRR: €168.00",
        "Cancellations scheduled: 1: beta (beta), ends ",
        "Failed payments in grace: 1: gamma (gamma), Free on ",
        "Playground hours: 2.0 in ",
        "Imports: 2 (3 migrations); 3 in 7 days",
        "Migration tests: 3 (1 passed, 1 failed, 1 error); 4 in 7 days",
        "Deletions: 1 account and 1 organization (2 and 1 in 7 days)",
    ] {
        assert!(text.contains(line), "{line}\n---\n{text}");
    }
    assert!(
        notice.subject.ends_with("MRR €168.00"),
        "{}",
        notice.subject
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_digest_goes_once_a_day_from_its_hour(db: PgPool) {
    let state = billed_state(&db, 7);
    let at = |day: u32, hour: u32| Utc.with_ymd_and_hms(2026, 10, day, hour, 0, 0).unwrap();
    assert!(!digest::due(&state, at(21, 6)).await.unwrap(), "before 7");
    assert_eq!(total(&db).await, 0);
    let (one, two) = tokio::join!(
        digest::due(&state, at(21, 7)),
        digest::due(&state, at(21, 7))
    );
    assert!(one.unwrap() ^ two.unwrap(), "one of them queues it");
    assert!(!digest::due(&state, at(21, 18)).await.unwrap());
    let (subject, body) = told(&db, "digest").await;
    assert!(
        subject.starts_with("[romulus] Digest for October 20: +0 accounts"),
        "{subject}"
    );
    assert!(body.contains("MRR: €0.00"), "{body}");

    // The next day: another, compared with this one.
    let acme = org(&db, "acme").await;
    sqlx::query!(
        "INSERT INTO org_billing (org_id, subscription_id, plan, interval, status, amount_cents, currency)
         VALUES ($1, 'sub_1', 'team', 'month', 'active', 4900, 'eur')",
        acme
    )
    .execute(&db)
    .await
    .unwrap();
    assert!(digest::due(&state, at(22, 9)).await.unwrap());
    let digests = queued(&db, "digest").await;
    assert_eq!(digests.len(), 2 * OPS.len());
    assert!(
        digests
            .iter()
            .any(|(_, _, body)| body.contains("MRR: €49.00 (+€49.00 since October 21)")),
        "{digests:?}"
    );
    let days = sqlx::query_scalar!("SELECT day FROM operator_digests ORDER BY day")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        days,
        [
            NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
            NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_test_command_sends_a_notification_and_the_digest_without_recording_it(db: PgPool) {
    let (mailer, sent) = Mailer::memory();
    let state = billed_state(&db, 7).with_mailer(mailer);
    let tested = operator::send_test(&state).await.unwrap();
    assert_eq!(tested.len(), OPS.len());
    assert!(tested.iter().all(|t| t.notice.is_ok() && t.digest.is_ok()));
    for to in OPS {
        let mine = sent.to(to);
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].subject, "[romulus] Test notification");
        assert!(mine[1].subject.starts_with("[romulus] Digest for "));
    }
    assert_eq!(total(&db).await, 0);
    let recorded = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM operator_digests"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(recorded, 0);
}
