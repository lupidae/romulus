//! Signup sources: what the app reports after a sign-up (`POST
//! /api/me/signup-source`: once, first with something wins, only for an
//! account made within the hour) and the operators' CSV. A password
//! sign-up's own (its form, its link): `passwords.rs`.

use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, Utc};
use romulus_api::{
    domain::{Plan, Role},
    signups,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::harness::{TestApp, database, member, model, org, session, signed_in, user};

const PATH: &str = "/api/me/signup-source";

fn newsletter() -> Value {
    json!({
        "utm_source": "newsletter",
        "utm_medium": "email",
        "utm_campaign": "launch",
        "utm_content": "header",
        "referrer": "https://news.example.org/item?id=42&email=someone@example.org",
    })
}

async fn stored(db: &PgPool, user: Uuid) -> Option<Value> {
    sqlx::query_scalar!(
        r#"SELECT to_jsonb(s) - 'user_id' - 'created_at' AS "row!" FROM signup_sources s WHERE user_id = $1"#,
        user,
    )
    .fetch_optional(db)
    .await
    .unwrap()
}

async fn age(db: &PgPool, user: Uuid, by: Duration) {
    sqlx::query!(
        "UPDATE users SET created_at = now() - make_interval(secs => $2) WHERE id = $1",
        user,
        f64::from(i32::try_from(by.num_seconds()).unwrap()),
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_account_records_where_it_came_from_once(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;

    let first = app.as_user(&alice).post(PATH, &newsletter()).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json(), json!({"recorded": true, "new_account": true}));
    // The referrer's origin only: never its path or query.
    assert_eq!(
        stored(&db, alice.user).await,
        Some(json!({
            "utm_source": "newsletter",
            "utm_medium": "email",
            "utm_campaign": "launch",
            "utm_content": "header",
            "referrer": "https://news.example.org",
        }))
    );

    // First touch wins, and the sign-up completes once.
    let again = app
        .as_user(&alice)
        .post(PATH, &json!({"utm_source": "ads"}))
        .await;
    assert_eq!(
        again.json(),
        json!({"recorded": false, "new_account": false})
    );
    assert_eq!(
        stored(&db, alice.user).await.unwrap()["utm_source"],
        "newsletter"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_direct_visit_gives_way_to_a_page_load_that_carries_a_source(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    // This site as the referrer is no source.
    let direct = json!({"referrer": "http://localhost:41000/pricing"});

    let first = app.as_user(&alice).post(PATH, &direct).await;
    assert_eq!(
        first.json(),
        json!({"recorded": false, "new_account": true})
    );
    assert_eq!(
        stored(&db, alice.user).await,
        Some(
            json!({"utm_source": null, "utm_medium": null, "utm_campaign": null,
                    "utm_content": null, "referrer": null})
        )
    );

    let with_params = app.as_user(&alice).post(PATH, &newsletter()).await;
    assert_eq!(
        with_params.json(),
        json!({"recorded": true, "new_account": false})
    );
    assert_eq!(
        stored(&db, alice.user).await.unwrap()["utm_source"],
        "newsletter"
    );

    let later = app
        .as_user(&alice)
        .post(PATH, &json!({"utm_source": "ads"}))
        .await;
    assert_eq!(
        later.json(),
        json!({"recorded": false, "new_account": false})
    );
    assert_eq!(
        stored(&db, alice.user).await.unwrap()["utm_source"],
        "newsletter"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_an_account_made_within_the_hour_reports(db: PgPool) {
    let app = TestApp::new(db.clone());
    // A campaign link followed by someone who signed up two hours ago is
    // not how they signed up.
    let bob = signed_in(&db, "Bob").await;
    age(&db, bob.user, Duration::hours(2)).await;
    let late = app.as_user(&bob).post(PATH, &newsletter()).await;
    assert_eq!(
        late.json(),
        json!({"recorded": false, "new_account": false})
    );
    assert_eq!(stored(&db, bob.user).await, None);

    let carol = signed_in(&db, "Carol").await;
    age(&db, carol.user, Duration::minutes(50)).await;
    let soon = app.as_user(&carol).post(PATH, &newsletter()).await;
    assert_eq!(soon.json(), json!({"recorded": true, "new_account": true}));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reports_need_the_person_signed_in_and_a_well_formed_body(db: PgPool) {
    let app = TestApp::new(db.clone());
    assert_eq!(
        app.post_json(PATH, &newsletter()).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A token is never the person (CurrentUser refuses bearers).
    let bearer = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri(PATH)
                .header(header::AUTHORIZATION, "Bearer rmls_pat_nothing")
                .header(header::CONTENT_TYPE, "application/json")
                .body(newsletter().to_string().into())
                .unwrap(),
        )
        .await;
    assert_eq!(bearer.status, StatusCode::UNAUTHORIZED);

    let alice = signed_in(&db, "Alice").await;
    let wrong = app
        .as_user(&alice)
        .post(PATH, &json!({"utm_source": 5}))
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong.code(), "validation");
    // Overlong values are cut, never refused: they come from links.
    let long = app
        .as_user(&alice)
        .post(PATH, &json!({"utm_campaign": "x".repeat(5000)}))
        .await;
    assert_eq!(long.json()["recorded"], true);
    assert_eq!(
        stored(&db, alice.user).await.unwrap()["utm_campaign"]
            .as_str()
            .unwrap()
            .len(),
        200
    );
}

async fn subscribe(db: &PgPool, org: Uuid, plan: Plan, status: &str) {
    sqlx::query!(
        "INSERT INTO org_billing (org_id, subscription_id, plan, interval, status)
         VALUES ($1, $2, $3, 'month', $4)",
        org,
        format!("sub_{org}"),
        plan as Plan,
        status,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_export_lists_signups_with_their_source_plan_activation_and_payment(db: PgPool) {
    let app = TestApp::new(db.clone());
    // Alice: from the newsletter, edited a draft on her second day, pays Team.
    let alice = signed_in(&db, "Alice").await;
    app.as_user(&alice).post(PATH, &newsletter()).await;
    age(&db, alice.user, Duration::days(3)).await;
    let acme = org(&db, "acme").await;
    member(&db, acme, alice.user, Role::Admin).await;
    subscribe(&db, acme, Plan::Team, "active").await;
    let shop = database(&db, acme, "shop").await;
    let draft = sqlx::query_scalar!(
        "INSERT INTO drafts (database_id, name) VALUES ($1, 'first') RETURNING id",
        shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO draft_editors (draft_id, user_id, first_edit_at) VALUES ($1, $2, now() - interval '2 days')",
        draft,
        alice.user,
    )
    .execute(&db)
    .await
    .unwrap();

    // Bob: never reported, made his first version 9 days in, on a trial.
    let bob = user(&db, "Bob").await;
    age(&db, bob, Duration::days(10)).await;
    let beta = org(&db, "beta").await;
    member(&db, beta, bob, Role::Admin).await;
    subscribe(&db, beta, Plan::Starter, "trialing").await;
    let lab = database(&db, beta, "lab").await;
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, created_by, created_at)
         VALUES ($1, 1, $2, '', 'sql', 'upload', $3, now() - interval '1 day')",
        lab,
        serde_json::to_value(model(&["users"])).unwrap(),
        bob,
    )
    .execute(&db)
    .await
    .unwrap();

    // Carol: at an ignored domain.
    let carol = user(&db, "Carol").await;
    sqlx::query!(
        "UPDATE users SET email = 'carol@example.org' WHERE id = $1",
        carol
    )
    .execute(&db)
    .await
    .unwrap();
    let _ = session(&db, carol).await;

    let ignored = ["example.org".to_owned()];
    let all = signups::export(&db, None, &ignored).await.unwrap();
    assert_eq!(all.len(), 2, "{all:?}");
    let (bob_row, alice_row) = (&all[0], &all[1]);
    assert_eq!(alice_row.source.utm_source.as_deref(), Some("newsletter"));
    assert_eq!(
        alice_row.source.referrer.as_deref(),
        Some("https://news.example.org")
    );
    assert_eq!(
        (alice_row.plan, alice_row.activated, alice_row.paying),
        (Plan::Team, true, true)
    );
    assert!(bob_row.source.is_empty());
    assert_eq!(
        (bob_row.plan, bob_row.activated, bob_row.paying),
        (Plan::Starter, false, false)
    );

    // Since a day: Bob signed up before it.
    let since = (Utc::now() - Duration::days(5)).date_naive();
    let recent = signups::export(&db, Some(since), &ignored).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].source.utm_campaign.as_deref(), Some("launch"));

    let mut csv = Vec::new();
    signups::write_csv(&recent, &mut csv).unwrap();
    let csv = String::from_utf8(csv).unwrap();
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines[0], signups::CSV_HEADER);
    assert!(
        lines[1]
            .ends_with(",newsletter,email,launch,header,https://news.example.org,team,true,true"),
        "{}",
        lines[1]
    );
    assert_eq!(lines[2], "");

    // Without ignored domains, Carol is there, on Free.
    let everyone = signups::export(&db, None, &[]).await.unwrap();
    assert_eq!(everyone.len(), 3);
    assert_eq!(everyone[2].plan, Plan::Free);
}
