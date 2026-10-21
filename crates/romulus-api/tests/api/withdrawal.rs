//! What the hosted Terms promise after a checkout: the subscription
//! confirmed with the Terms as a PDF (and, for a consumer, the requests made
//! and how to withdraw), a cancellation confirmed, a consumer's withdrawal
//! within 14 days (cancelled, refunded for the days not used, Free at once,
//! acknowledged), never a business's; each email in its reader's language.

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use romulus_api::billing::notices;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::billing::{
    Billed, audits, billed, billed_with, consumer_checkout, deliver, plan_of, subscribe,
    subscription_object,
};

const ADMIN: &str = "admin-acme@romulus.test";

/// Subscribes acme as a consumer (consumer sales on): the subscription id.
async fn subscribe_as_consumer(b: &Billed, plan: &str, interval: &str) -> String {
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &consumer_checkout(plan, interval),
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

/// The subscription began `days` ago (its invoice too).
async fn started_days_ago(b: &Billed, db: &PgPool, days: i64) {
    let at = Utc::now() - Duration::days(days);
    sqlx::query!(
        "UPDATE org_billing SET started_at = $2 WHERE org_id = $1",
        b.team.org,
        at
    )
    .execute(db)
    .await
    .unwrap();
    let mut stripe = b.stripe.state();
    for invoice in &mut stripe.invoices {
        invoice.created = at;
    }
    // Its period moves with it: a year is still 365 days.
    for subscription in stripe.subscriptions.values_mut() {
        subscription.current_period_end = subscription
            .current_period_end
            .map(|end| end - Duration::days(days));
    }
}

async fn mails(b: &Billed, to: &str, kind: &str) -> Vec<romulus_api::mail::Email> {
    b.mail()
        .await
        .to(to)
        .into_iter()
        .filter(|m| m.kind == kind)
        .collect()
}

async fn withdraw(b: &Billed, body: &Value) -> super::harness::TestResponse {
    b.app
        .as_user(&b.team.admin)
        .post(&b.team.path("/billing/withdrawal"), body)
        .await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_business_subscription_is_confirmed_with_the_terms_and_its_cancellation_too(db: PgPool) {
    let b = billed(&db).await;
    let subscription = subscribe(&b, "team", "month").await;
    let confirmed = mails(&b, ADMIN, "billing_subscription_confirmed").await;
    assert_eq!(confirmed.len(), 1, "to the buyer, once");
    let letter = &confirmed[0];
    assert!(
        letter.subject.contains("Team plan for acme has started"),
        "{}",
        letter.subject
    );
    assert!(letter.text.contains("€49.00 a month"), "{}", letter.text);
    assert!(letter.text.contains("excluding taxes"), "{}", letter.text);
    assert!(
        letter.text.contains("renews automatically every month"),
        "{}",
        letter.text
    );
    assert!(
        letter.text.contains("Cancel subscription"),
        "{}",
        letter.text
    );
    assert!(letter.text.contains("legal@romulus.example"));
    assert!(
        !letter.text.contains("withdraw"),
        "a business has no withdrawal: {}",
        letter.text
    );
    let [pdf] = letter.attachments.as_slice() else {
        panic!("the Terms as a PDF: {:?}", letter.attachments);
    };
    assert_eq!(
        pdf.filename,
        "romulus-terms-of-service-professional-1.0.pdf"
    );
    assert_eq!(pdf.content_type, "application/pdf");
    assert!(pdf.bytes.starts_with(b"%PDF-"));
    assert_eq!(
        mails(
            &b,
            "developer-acme@romulus.test",
            "billing_subscription_confirmed"
        )
        .await,
        Vec::new()
    );
    // Another event about it confirms nothing more.
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(
        mails(&b, ADMIN, "billing_subscription_confirmed")
            .await
            .len(),
        1
    );
    // A business never sees a withdrawal.
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert!(page["withdrawal"].is_null());
    let refused = withdraw(&b, &json!({})).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::FORBIDDEN, "not_a_consumer")
    );

    // Cancelled in the portal: confirmed once, with its end and what it changes.
    let ends = Utc::now() + Duration::days(20);
    b.stripe.update(&subscription, |s| {
        s.cancel_at_period_end = true;
        s.current_period_end = Some(ends);
    });
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let cancelled = mails(&b, ADMIN, "billing_cancellation_confirmed").await;
    assert_eq!(cancelled.len(), 1);
    let text = &cancelled[0].text;
    assert!(
        text.contains(&ends.format("%B %-d, %Y").to_string()),
        "{text}"
    );
    assert!(
        text.contains("moves to the Free plan. Nothing is deleted"),
        "{text}"
    );
    assert!(text.contains("Renew it from Manage billing"), "{text}");
    assert!(
        !text.contains("L215-1-1"),
        "no consumer law for businesses: {text}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn billing_emails_speak_each_readers_language(db: PgPool) {
    let b = billed(&db).await;
    sqlx::query!(
        "UPDATE users SET locale = 'fr' WHERE id = $1",
        b.team.admin.user
    )
    .execute(&db)
    .await
    .unwrap();
    // The checkout's language is the contract's: French here.
    let mut body = super::billing::business_checkout("team", "year", false);
    body["locale"] = json!("fr");
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(&b.team.path("/billing/checkout"), &body)
        .await;
    assert_eq!(started.status, StatusCode::OK);
    let (session, _) = b.stripe.last_checkout().unwrap();
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    deliver(
        &b,
        "checkout.session.completed",
        json!({"id": session, "subscription": subscription}),
    )
    .await;
    let letter = &mails(&b, ADMIN, "billing_subscription_confirmed").await[0];
    assert!(
        letter.subject.contains("Votre offre romulus Team"),
        "{}",
        letter.subject
    );
    assert!(
        letter.text.contains("490,00\u{a0}€ par an"),
        "{}",
        letter.text
    );
    assert!(
        letter
            .text
            .contains("https://romulus.example/fr/legal/conditions")
    );
    assert!(letter.html.contains("lang=\"fr\""));
    assert_eq!(
        letter.attachments[0].filename,
        "romulus-conditions-generales-professionnels-1.0.pdf"
    );
    // A cancellation, too.
    b.stripe
        .update(&subscription, |s| s.cancel_at_period_end = true);
    deliver(
        &b,
        "customer.subscription.updated",
        subscription_object(&subscription),
    )
    .await;
    let cancelled = &mails(&b, ADMIN, "billing_cancellation_confirmed").await[0];
    assert!(
        cancelled.subject.contains("est résiliée"),
        "{}",
        cancelled.subject
    );
    assert!(cancelled.text.contains("passe ensuite à l'offre gratuite"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_consumer_withdraws_within_fourteen_days_refunded_pro_rata_and_on_free_at_once(
    db: PgPool,
) {
    let b = billed_with(&db, true).await;
    let subscription = subscribe_as_consumer(&b, "team", "year").await;
    // The confirmation carries the requests and the way to withdraw.
    let confirmed = &mails(&b, ADMIN, "billing_subscription_confirmed").await[0];
    assert!(
        confirmed
            .text
            .contains("I expressly ask for my plan to start now")
    );
    assert!(
        confirmed
            .text
            .contains("I acknowledge that if I withdraw within these 14 days")
    );
    assert!(
        confirmed.text.contains("Withdraw from contract here"),
        "{}",
        confirmed.text
    );
    assert!(
        confirmed
            .text
            .contains("https://romulus.example/legal/full/terms")
    );
    assert_eq!(
        confirmed.attachments[0].filename,
        "romulus-terms-of-service-1.0.pdf"
    );
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["buyer"], "consumer");
    assert!(page["withdrawal"]["until"].is_string(), "{page}");

    // Three whole days of a year used: 490 € × 3 / 365 kept.
    started_days_ago(&b, &db, 3).await;
    // Only admins, in person.
    let developer = b
        .app
        .as_user(&b.team.developer)
        .post(&b.team.path("/billing/withdrawal"), &json!({}))
        .await;
    assert_eq!(developer.status, StatusCode::FORBIDDEN);
    let done = withdraw(&b, &json!({"locale": "fr"})).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    let done = done.json();
    assert_eq!(done["paid_cents"], 49_000);
    assert_eq!(done["refund_cents"], 49_000 - 49_000 * 3 / 365);
    assert_eq!(done["completed"], true);
    // Stripe: cancelled now, the days not used refunded.
    assert_eq!(
        b.stripe.state().subscriptions[&subscription].status,
        "canceled"
    );
    assert_eq!(
        b.stripe
            .state()
            .withdrawal_refunds
            .iter()
            .map(|(_, amount)| *amount)
            .collect::<Vec<_>>(),
        [49_000 - 49_000 * 3 / 365]
    );
    // Free at once; nothing deleted; the withdrawal on the page.
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "free");
    assert!(page["subscription"].is_null());
    assert_eq!(
        page["withdrawal"]["withdrawn"]["refund_cents"],
        49_000 - 49_000 * 3 / 365
    );
    assert_eq!(page["withdrawal"]["withdrawn"]["completed"], true);
    assert_eq!(audits(&db, "contract_withdrawn").await, 1);
    let record = sqlx::query!(
        r#"SELECT user_email, plan::text AS "plan!", subscription_id, completed_at, acceptance_id
           FROM withdrawals"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(record.user_email, ADMIN);
    assert_eq!(record.plan, "team");
    assert_eq!(record.subscription_id, subscription);
    assert!(record.completed_at.is_some());
    assert!(record.acceptance_id.is_some());
    // The acknowledgement, in the page's language, timestamped.
    let ack = &mails(&b, ADMIN, "billing_withdrawal_acknowledged").await[0];
    assert!(ack.subject.contains("rétractation"), "{}", ack.subject);
    assert!(ack.text.contains("Reçue le "), "{}", ack.text);
    assert!(ack.text.contains(" UTC"), "{}", ack.text);
    assert!(ack.text.contains("485,98\u{a0}€"), "{}", ack.text);
    assert!(ack.text.contains("support durable"));

    // Once only; Stripe's own deletion event changes nothing.
    assert_eq!(withdraw(&b, &json!({})).await.code(), "already_withdrawn");
    deliver(
        &b,
        "customer.subscription.deleted",
        subscription_object(&subscription),
    )
    .await;
    assert_eq!(plan_of(&db, b.team.org).await.as_deref(), Some("team"));
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "free");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn withdrawal_ends_with_the_fourteen_days_and_needs_a_subscription(db: PgPool) {
    let b = billed_with(&db, true).await;
    let refused = withdraw(&b, &json!({})).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "no_subscription")
    );
    subscribe_as_consumer(&b, "starter", "month").await;
    started_days_ago(&b, &db, 17).await;
    let late = withdraw(&b, &json!({})).await;
    assert_eq!(
        (late.status, late.code().as_str()),
        (StatusCode::CONFLICT, "withdrawal_period_over")
    );
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert!(
        page["withdrawal"].is_null(),
        "no button once the period is over"
    );
    assert_eq!(page["plan"], "starter");
    assert_eq!(b.stripe.state().withdrawal_refunds, Vec::new());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_withdrawal_stripe_could_not_finish_is_finished_by_the_notices_task(db: PgPool) {
    let b = billed_with(&db, true).await;
    let subscription = subscribe_as_consumer(&b, "starter", "month").await;
    b.stripe.state().fail_withdrawal = true;
    let done = withdraw(&b, &json!({})).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    assert_eq!(done.json()["completed"], false);
    // Recorded and acknowledged anyway: the withdrawal counts from the ask.
    assert_eq!(
        mails(&b, ADMIN, "billing_withdrawal_acknowledged")
            .await
            .len(),
        1
    );
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "free");
    assert_eq!(page["withdrawal"]["withdrawn"]["completed"], false);
    assert_ne!(
        b.stripe.state().subscriptions[&subscription].status,
        "canceled"
    );

    sqlx::query!("UPDATE withdrawals SET requested_at = now() - interval '5 minutes'")
        .execute(&db)
        .await
        .unwrap();
    notices::due(&b.app.state, Utc::now()).await.unwrap();
    assert_eq!(
        b.stripe.state().subscriptions[&subscription].status,
        "canceled"
    );
    let invoice = b.stripe.state().invoices[0].id.clone();
    assert_eq!(b.stripe.state().withdrawal_refunds, [(invoice, 1_900)]);
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["withdrawal"]["withdrawn"]["completed"], true);
    // A new subscription afterwards is not taken for a duplicate.
    let again = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &consumer_checkout("starter", "month"),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json());
}

/// The withdrawal's id and Stripe payment of the subscription's invoice.
async fn withdrawal_and_payment(b: &Billed, db: &PgPool) -> (uuid::Uuid, String) {
    let id = sqlx::query_scalar!("SELECT id FROM withdrawals")
        .fetch_one(db)
        .await
        .unwrap();
    let invoice = b.stripe.state().invoices[0].id.clone();
    (id, format!("pi_for_{invoice}"))
}

/// The notices task's next pass, a few minutes on.
async fn retry(b: &Billed, db: &PgPool) {
    sqlx::query!("UPDATE withdrawals SET requested_at = requested_at - interval '3 minutes'")
        .execute(db)
        .await
        .unwrap();
    romulus_api::billing::withdrawal::complete_pending(&b.app.state)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_refund_already_made_is_not_made_again(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    b.stripe.state().fail_withdrawal = true;
    assert_eq!(withdraw(&b, &json!({})).await.json()["completed"], false);
    // Stripe made the refund after all; its answer was lost.
    let (id, payment) = withdrawal_and_payment(&b, &db).await;
    b.stripe.state().payment_refunds.insert(
        payment,
        vec![romulus_api::billing::stripe::MadeRefund {
            amount: 1_900,
            status: "succeeded".into(),
            withdrawal: Some(id.to_string()),
        }],
    );
    retry(&b, &db).await;
    assert_eq!(b.stripe.state().withdrawal_refunds, Vec::new(), "not twice");
    let done = sqlx::query!("SELECT completed_at, attempts FROM withdrawals")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(done.completed_at.is_some());
    assert_eq!(done.attempts, 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_refund_never_gives_back_more_than_is_left_of_the_payment(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    // 5 € refunded by hand in the Dashboard before the withdrawal.
    let invoice = b.stripe.state().invoices[0].id.clone();
    b.stripe.state().payment_refunds.insert(
        format!("pi_for_{invoice}"),
        vec![romulus_api::billing::stripe::MadeRefund {
            amount: 500,
            status: "succeeded".into(),
            withdrawal: None,
        }],
    );
    let done = withdraw(&b, &json!({})).await;
    assert_eq!(done.json()["completed"], true);
    assert_eq!(
        b.stripe.state().withdrawal_refunds,
        [(invoice, 1_900 - 500)],
        "what is left of 19 €"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_withdrawal_stripe_keeps_refusing_is_left_to_the_operator(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    b.stripe.state().withdrawals_down = true;
    assert_eq!(withdraw(&b, &json!({})).await.json()["completed"], false);
    for _ in 0..10 {
        retry(&b, &db).await;
    }
    let given_up =
        sqlx::query!("SELECT attempts, failed_at, last_error, completed_at FROM withdrawals")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        given_up.attempts,
        romulus_api::billing::withdrawal::MAX_ATTEMPTS,
        "no attempt after giving up"
    );
    assert!(given_up.failed_at.is_some());
    assert!(given_up.last_error.is_some_and(|e| e.contains("down")));
    assert!(given_up.completed_at.is_none());
    // The organization stays on Free all along: the withdrawal counts.
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    assert_eq!(page["plan"], "free");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_withdrawal_a_week_old_is_left_to_the_operator(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    b.stripe.state().withdrawals_down = true;
    withdraw(&b, &json!({})).await;
    sqlx::query!("UPDATE withdrawals SET requested_at = now() - interval '8 days'")
        .execute(&db)
        .await
        .unwrap();
    romulus_api::billing::withdrawal::complete_pending(&b.app.state)
        .await
        .unwrap();
    let given_up = sqlx::query!("SELECT attempts, failed_at FROM withdrawals")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(given_up.attempts, 2);
    assert!(given_up.failed_at.is_some());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_co_admins_withdrawal_is_acknowledged_to_the_buyer_too(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    let page = b
        .app
        .as_user(&b.team.admin)
        .get(&b.team.path("/billing"))
        .await
        .json();
    // The last day, as Paris counts it.
    let last_day = page["withdrawal"]["last_day"].as_str().unwrap().to_owned();
    assert_eq!(last_day.len(), "2026-11-04".len(), "{page}");
    sqlx::query!(
        "UPDATE memberships SET role = 'admin' WHERE user_id = $1",
        b.team.developer.user
    )
    .execute(&db)
    .await
    .unwrap();
    let done = b
        .app
        .as_user(&b.team.developer)
        .post(
            &b.team.path("/billing/withdrawal"),
            &json!({"locale": "fr"}),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    let theirs = &mails(
        &b,
        "developer-acme@romulus.test",
        "billing_withdrawal_acknowledged",
    )
    .await[0];
    assert!(
        theirs.subject.contains("rétractation"),
        "{}",
        theirs.subject
    );
    // The buyer hears of it, in their language, and who withdrew.
    let buyers = mails(&b, ADMIN, "billing_withdrawal_acknowledged").await;
    assert_eq!(buyers.len(), 1);
    assert!(
        buyers[0].subject.contains("withdrawal"),
        "{}",
        buyers[0].subject
    );
    assert!(
        buyers[0].text.contains("developer-acme@romulus.test"),
        "{}",
        buyers[0].text
    );
    // The contract the checkout concluded ended now.
    let ended = sqlx::query_scalar!(
        "SELECT a.contract_ended_at FROM legal_acceptances a JOIN withdrawals w ON w.acceptance_id = a.id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(ended.is_some());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_payment_romulus_cannot_refund_sends_the_consumer_to_email(db: PgPool) {
    let b = billed_with(&db, true).await;
    subscribe_as_consumer(&b, "starter", "month").await;
    let invoice = b.stripe.state().invoices[0].id.clone();
    b.stripe.state().without_payment.push(invoice);
    let refused = withdraw(&b, &json!({})).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "withdrawal_by_email")
    );
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("legal@romulus.example")
    );
    // Nothing recorded, nothing acknowledged with an amount it cannot keep.
    assert_eq!(
        sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM withdrawals"#)
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        mails(&b, ADMIN, "billing_withdrawal_acknowledged").await,
        Vec::new()
    );
}
