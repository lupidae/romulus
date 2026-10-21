//! The hosted service's Terms: asked before anything else (the professional
//! edition with its attestation, or the full one), recorded for good (the
//! records outlive the organization and the account, then go with their
//! retention), the person's language, and the change notices the operator
//! sends (`legal-notice`, `price-change`).

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use romulus_api::{
    AppState, Config,
    billing::{copy, notices},
    domain::{LegalDocument, TermsEdition},
    legal::{
        self,
        notice::{Change, NoticeError},
    },
    mail::Mailer,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{
    billing::{acceptances, billed, business_checkout, legal_config},
    harness::{Team, TestApp, signed_in},
};

/// The hosted Terms on (businesses only unless `consumer_sales`), billing off.
fn legal_app(db: &PgPool, consumer_sales: bool) -> TestApp {
    let mut config = Config::for_tests();
    config.legal = Some(legal_config(consumer_sales));
    TestApp::with_config(db.clone(), config)
}

fn acceptance(consumer_sales: bool, professional_use: bool, locale: &str) -> Value {
    let edition = if consumer_sales {
        TermsEdition::Full
    } else {
        TermsEdition::Professional
    };
    json!({
        "version": "1.0", "locale": locale, "professional_use": professional_use,
        "copy_version": copy::version(edition),
    })
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_server_without_terms_asks_nothing(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let status = app.as_user(&alice).get("/api/me/terms").await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(
        (
            status.json()["required"].clone(),
            status.json()["accepted"].clone()
        ),
        (json!(false), json!(true))
    );
    assert!(status.json()["documents"].is_null());
    assert!(app.get("/api/instance").await.json()["legal"].is_null());
    let created = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let accepted = app
        .as_user(&alice)
        .post("/api/me/terms", &acceptance(false, true, "en"))
        .await;
    assert_eq!(accepted.status, StatusCode::NOT_FOUND);
    assert_eq!(acceptances(&db).await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_first_sign_in_asks_for_the_terms_and_professional_use_before_any_organization(
    db: PgPool,
) {
    let app = legal_app(&db, false);
    // Someone who signed in before the Terms existed is asked like a newcomer.
    let alice = signed_in(&db, "Alice").await;
    let status = app.as_user(&alice).get("/api/me/terms").await.json();
    assert_eq!(status["required"], true);
    assert_eq!(status["accepted"], false);
    assert!(status["accepted_version"].is_null());
    let documents = &status["documents"];
    assert_eq!(documents["terms_version"], "1.0");
    assert_eq!(documents["edition"], "professional");
    assert_eq!(documents["consumer_sales"], false);
    assert_eq!(
        documents["en"]["terms"],
        "https://romulus.example/legal/terms"
    );
    assert_eq!(
        documents["fr"]["terms"],
        "https://romulus.example/fr/legal/conditions"
    );
    assert_eq!(
        documents["fr"]["privacy"],
        "https://romulus.example/fr/legal/confidentialite"
    );
    assert_eq!(documents["en"]["dpa"], "https://romulus.example/legal/dpa");
    assert_eq!(documents["contact"], "legal@romulus.example");
    assert_eq!(
        documents["copy_version"],
        copy::version(TermsEdition::Professional)
    );
    // The sign-in pages link them too.
    assert_eq!(
        app.get("/api/instance").await.json()["legal"]["en"]["privacy"],
        "https://romulus.example/legal/privacy"
    );

    // Nothing joins the Service before: no organization, no invitation.
    let refused = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::FORBIDDEN, "terms_required")
    );
    let joined = app
        .as_user(&alice)
        .post_empty("/api/invitations/not-even-a-token/accept")
        .await;
    assert_eq!(joined.code(), "terms_required");

    // The attestation is required, and so are the Terms and wording in force.
    let without = app
        .as_user(&alice)
        .post("/api/me/terms", &acceptance(false, false, "fr"))
        .await;
    assert_eq!(without.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(without.json()["details"]["professional_use"].is_string());
    let mut old = acceptance(false, true, "fr");
    old["version"] = json!("0.9");
    assert_eq!(
        app.as_user(&alice).post("/api/me/terms", &old).await.code(),
        "terms_changed"
    );
    let mut wording = acceptance(false, true, "fr");
    wording["copy_version"] = json!(copy::version(TermsEdition::Full));
    assert_eq!(
        app.as_user(&alice)
            .post("/api/me/terms", &wording)
            .await
            .code(),
        "terms_changed"
    );
    assert_eq!(acceptances(&db).await, 0);

    let accepted = app
        .as_user(&alice)
        .post("/api/me/terms", &acceptance(false, true, "fr"))
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.json());
    assert_eq!(accepted.json()["accepted"], true);
    assert_eq!(accepted.json()["accepted_version"], "1.0");
    assert_eq!(accepted.json()["locale"], "fr");
    let record = sqlx::query!(
        r#"SELECT kind::text AS "kind!", user_id, user_email, user_name, terms_version,
                  edition::text AS "edition!", locale::text AS "locale!", professional_use,
                  copy_version, org_id
           FROM legal_acceptances"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (
            record.kind.as_str(),
            record.edition.as_str(),
            record.locale.as_str()
        ),
        ("terms", "professional", "fr")
    );
    assert_eq!(record.user_id, Some(alice.user));
    assert_eq!(
        (record.user_email.as_str(), record.user_name.as_str()),
        ("alice@romulus.test", "Alice")
    );
    assert_eq!(record.terms_version, "1.0");
    assert_eq!(record.professional_use, Some(true));
    assert_eq!(
        record.copy_version.as_deref(),
        Some(copy::version(TermsEdition::Professional))
    );
    assert_eq!(record.org_id, None);

    // Then the Service opens; accepting again records nothing more.
    let created = app
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    app.as_user(&alice)
        .post("/api/me/terms", &acceptance(false, true, "en"))
        .await;
    assert_eq!(acceptances(&db).await, 1);
    assert_eq!(
        app.as_user(&alice).get("/api/me/terms").await.json()["locale"],
        "en"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn another_edition_of_the_terms_is_accepted_again_without_closing_the_service(db: PgPool) {
    let pro = legal_app(&db, false);
    let alice = signed_in(&db, "Alice").await;
    pro.as_user(&alice)
        .post("/api/me/terms", &acceptance(false, true, "en"))
        .await;
    // Consumer sales open: the full edition is in force.
    let full = legal_app(&db, true);
    let status = full.as_user(&alice).get("/api/me/terms").await.json();
    assert_eq!(status["accepted"], false);
    assert_eq!(status["accepted_version"], "1.0");
    assert_eq!(status["documents"]["edition"], "full");
    assert_eq!(
        status["documents"]["en"]["terms"],
        "https://romulus.example/legal/full/terms"
    );
    // The one accepted applies meanwhile: the Service stays open.
    let created = full
        .as_user(&alice)
        .post("/api/orgs", &json!({"name": "Acme", "slug": "acme"}))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    // The full edition asks no attestation.
    let accepted = full
        .as_user(&alice)
        .post("/api/me/terms", &acceptance(true, false, "en"))
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.json());
    assert_eq!(accepted.json()["accepted"], true);
    let editions = sqlx::query_scalar!(
        r#"SELECT edition::text AS "edition!" FROM legal_acceptances ORDER BY accepted_at"#
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(editions, ["professional", "full"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_language_is_the_persons_own(db: PgPool) {
    let app = legal_app(&db, false);
    let alice = signed_in(&db, "Alice").await;
    assert!(app.as_user(&alice).get("/api/me/terms").await.json()["locale"].is_null());
    let changed = app
        .as_user(&alice)
        .patch("/api/me/preferences", &json!({"locale": "fr"}))
        .await;
    assert_eq!(changed.status, StatusCode::OK);
    assert_eq!(changed.json()["locale"], "fr");
    let bad = app
        .as_user(&alice)
        .patch("/api/me/preferences", &json!({"locale": "de"}))
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        legal::locale_of(&db, alice.user).await.unwrap(),
        romulus_api::domain::Locale::Fr
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn acceptance_records_outlive_the_organization_and_the_account_and_never_change(db: PgPool) {
    let b = billed(&db).await;
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "month", false),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.json());
    sqlx::query!("DELETE FROM organizations WHERE id = $1", b.team.org)
        .execute(&db)
        .await
        .unwrap();
    let kept =
        sqlx::query!("SELECT org_id, org_name, user_id, account_deleted_at FROM legal_acceptances")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        (kept.org_id, kept.org_name.as_deref()),
        (Some(b.team.org), Some("acme"))
    );
    assert_eq!(kept.account_deleted_at, None);
    sqlx::query!("DELETE FROM users WHERE id = $1", b.team.admin.user)
        .execute(&db)
        .await
        .unwrap();
    let kept =
        sqlx::query!("SELECT user_id, user_email, account_deleted_at FROM legal_acceptances")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(kept.user_id, None);
    assert_eq!(kept.user_email, "admin-acme@romulus.test");
    assert!(
        kept.account_deleted_at.is_some(),
        "the contract's end is noted"
    );
    // What a record says is never rewritten.
    let rewritten = sqlx::query!("UPDATE legal_acceptances SET terms_version = '2.0'")
        .execute(&db)
        .await;
    assert!(rewritten.is_err());
}

/// A record of `kind` and `edition`, accepted `accepted_years` ago, its
/// account deleted `deleted_years` ago (`None`: alive).
async fn record(
    db: &PgPool,
    kind: &str,
    edition: &str,
    accepted_years: i32,
    deleted_years: Option<i32>,
) {
    sqlx::query!(
        "INSERT INTO legal_acceptances
             (kind, accepted_at, user_email, user_name, terms_version, edition, locale,
              professional_use, account_deleted_at, org_id, org_name, plan, interval, buyer,
              company_name, company_country, business_confirmed, copy_version, checkout_session_id)
         SELECT $1::text::legal_acceptance_kind, now() - make_interval(years => $3), 'x@y.z', 'X', '1.0',
                $2::text::terms_edition, 'en', CASE WHEN $2 = 'professional' THEN true END,
                now() - make_interval(years => $4),
                c.org, c.name, c.plan, c.interval, c.buyer, c.company, c.country, c.confirmed, 'v', c.session
         FROM (SELECT
                 CASE WHEN $1 = 'checkout' THEN gen_random_uuid() END AS org,
                 CASE WHEN $1 = 'checkout' THEN 'acme' END AS name,
                 CASE WHEN $1 = 'checkout' THEN 'team'::billing_plan END AS plan,
                 CASE WHEN $1 = 'checkout' THEN 'month'::billing_interval END AS interval,
                 CASE WHEN $1 = 'checkout' THEN 'business'::buyer END AS buyer,
                 CASE WHEN $1 = 'checkout' THEN 'Acme SAS' END AS company,
                 CASE WHEN $1 = 'checkout' THEN 'FR' END AS country,
                 CASE WHEN $1 = 'checkout' THEN true END AS confirmed,
                 CASE WHEN $1 = 'checkout' THEN 'cs_' || gen_random_uuid() END AS session) c",
        kind,
        edition,
        accepted_years,
        deleted_years,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn records_go_with_their_retention_and_not_before(db: PgPool) {
    // The professional edition: the contract and 5 years after it.
    record(&db, "terms", "professional", 8, Some(6)).await; // gone
    record(&db, "terms", "professional", 8, Some(4)).await; // kept
    record(&db, "checkout", "professional", 12, None).await; // kept: the contract runs
    record(&db, "checkout", "professional", 8, Some(6)).await; // gone
    // The full edition: a checkout 10 years (L213-1), a sign-in 5 after the end.
    record(&db, "checkout", "full", 11, None).await; // gone
    record(&db, "checkout", "full", 9, Some(6)).await; // kept
    record(&db, "terms", "full", 11, None).await; // kept
    record(&db, "terms", "full", 7, Some(6)).await; // gone
    assert_eq!(legal::purge(&db).await.unwrap(), 4);
    assert_eq!(acceptances(&db).await, 4);
    assert_eq!(legal::purge(&db).await.unwrap(), 0);
}

fn change(document: LegalDocument, days: i64) -> Change {
    Change {
        document,
        version: "1.1".to_owned(),
        effective: (Utc::now() + Duration::days(days)).date_naive(),
        summary_en: "Section 4.6 now names the hosting jurisdiction.".to_owned(),
        summary_fr: "L'article 4.6 précise désormais la juridiction de l'hébergement.".to_owned(),
        required_by_law: false,
        dry_run: false,
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_change_notice_goes_once_to_whom_it_concerns_in_their_language(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    sqlx::query!(
        "UPDATE users SET locale = 'fr' WHERE id = $1",
        team.developer.user
    )
    .execute(&db)
    .await
    .unwrap();
    let mut config = Config::for_tests();
    config.legal = Some(legal_config(false));
    let (mailer, outbox) = Mailer::memory();

    // Too soon for a change the law does not require; nothing without mail.
    assert!(matches!(
        legal::notice::send(&db, &config, &mailer, &change(LegalDocument::Terms, 10)).await,
        Err(NoticeError::TooSoon)
    ));
    assert!(matches!(
        legal::notice::send(
            &db,
            &config,
            &Mailer::off(),
            &change(LegalDocument::Terms, 31)
        )
        .await,
        Err(NoticeError::NoMail)
    ));
    let mut dry = change(LegalDocument::Terms, 31);
    dry.dry_run = true;
    let would = legal::notice::send(&db, &config, &Mailer::off(), &dry)
        .await
        .unwrap();
    assert_eq!((would.sent, would.already), (4, 0));
    assert_eq!(outbox.all(), Vec::new());

    // The Terms: everyone, once.
    let terms = change(LegalDocument::Terms, 31);
    let sent = legal::notice::send(&db, &config, &mailer, &terms)
        .await
        .unwrap();
    assert_eq!((sent.sent, sent.already), (4, 0));
    let again = legal::notice::send(&db, &config, &mailer, &terms)
        .await
        .unwrap();
    assert_eq!((again.sent, again.already), (0, 4));
    let english = &outbox.to("admin-acme@romulus.test")[0];
    assert_eq!(english.kind, "legal_notice");
    assert!(
        english.subject.contains("Terms of Service"),
        "{}",
        english.subject
    );
    assert!(
        english.text.contains("Version 1.1 takes effect"),
        "{}",
        english.text
    );
    assert!(english.text.contains("hosting jurisdiction"));
    assert!(english.text.contains("https://romulus.example/legal/terms"));
    assert!(english.text.contains("legal@romulus.example"));
    let french = &outbox.to("developer-acme@romulus.test")[0];
    assert!(
        french.subject.contains("Conditions générales"),
        "{}",
        french.subject
    );
    assert!(french.text.contains("juridiction de l'hébergement"));
    assert!(
        french
            .text
            .contains("https://romulus.example/fr/legal/conditions")
    );
    assert!(french.html.contains("lang=\"fr\""));

    // The DPA: admins only (one here).
    let dpa = legal::notice::send(&db, &config, &mailer, &change(LegalDocument::Dpa, 30))
        .await
        .unwrap();
    assert_eq!(dpa.sent, 1);
    assert_eq!(
        outbox
            .all()
            .iter()
            .filter(|m| m.subject.contains("Data Processing Agreement"))
            .count(),
        1
    );
    // A change the law requires may come sooner.
    let mut urgent = change(LegalDocument::Privacy, 3);
    urgent.required_by_law = true;
    assert_eq!(
        legal::notice::send(&db, &config, &mailer, &urgent)
            .await
            .unwrap()
            .sent,
        4
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_price_is_announced_to_subscriptions_still_on_the_old_one(db: PgPool) {
    let b = billed(&db).await;
    let started = b
        .app
        .as_user(&b.team.admin)
        .post(
            &b.team.path("/billing/checkout"),
            &business_checkout("team", "month", false),
        )
        .await;
    assert_eq!(started.status, StatusCode::OK);
    let (session, _) = b.stripe.last_checkout().unwrap();
    let subscription = b.stripe.complete_checkout(&session).unwrap();
    romulus_api::billing::webhook::apply_subscription(
        &b.app.state,
        "evt_price",
        "checkout.session.completed",
        &subscription,
        None,
    )
    .await
    .unwrap();
    let state: &AppState = &b.app.state;
    // On the plan table's price: nothing to announce.
    assert_eq!(
        notices::price_changes(state, None, None, false)
            .await
            .unwrap(),
        Vec::new()
    );
    // An older price.
    sqlx::query!(
        "UPDATE org_billing SET amount_cents = 3900 WHERE org_id = $1",
        b.team.org
    )
    .execute(&db)
    .await
    .unwrap();
    let dry = notices::price_changes(state, None, None, true)
        .await
        .unwrap();
    assert_eq!(dry.len(), 1);
    assert!(!dry[0].sent);
    let mails = |kind: &str| {
        b.outbox
            .to("admin-acme@romulus.test")
            .into_iter()
            .filter(|m| m.kind == kind)
            .collect::<Vec<_>>()
    };
    assert_eq!(mails("billing_price_change"), Vec::new());
    let told = notices::price_changes(state, None, None, false)
        .await
        .unwrap();
    assert_eq!(told.len(), 1);
    assert!(told[0].sent);
    assert_eq!((told[0].org.as_str(), told[0].cents), ("acme", 4900));
    assert!(told[0].from >= Utc::now() + Duration::days(30));
    let letter = &mails("billing_price_change")[0];
    assert!(letter.text.contains("€49.00 a month"), "{}", letter.text);
    assert!(
        letter.text.contains("Last day to cancel: "),
        "{}",
        letter.text
    );
    // Once per change; another plan is not this one's.
    assert!(
        !notices::price_changes(state, None, None, false)
            .await
            .unwrap()[0]
            .sent
    );
    assert_eq!(
        notices::price_changes(state, Some(romulus_api::domain::Plan::Starter), None, false)
            .await
            .unwrap(),
        Vec::new()
    );
}
