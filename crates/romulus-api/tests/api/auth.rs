//! Sessions, `/api/me`, logout and the dev sign-in.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use romulus_api::{Config, token};
use serde_json::json;
use sqlx::PgPool;

use crate::harness::{ORIGIN, Session, TestApp, signed_in, user};

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn me_is_the_signed_in_user(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let response = TestApp::new(db).as_user(&alice).get("/api/me").await;
    assert_eq!(response.status, StatusCode::OK);
    let me = response.json();
    assert_eq!(me["email"], "alice@romulus.test");
    assert_eq!(me["name"], "Alice");
    assert_eq!(me["email_verified"], true);
    assert_eq!(me["id"], alice.user.to_string());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn signed_out_is_unauthorized(db: PgPool) {
    let response = TestApp::new(db).get("/api/me").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.code(), "unauthorized");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unknown_or_malformed_cookie_is_an_expired_session(db: PgPool) {
    let app = TestApp::new(db);
    for token in [token::generate(), "garbage".to_owned()] {
        let ghost = Session {
            user: uuid::Uuid::nil(),
            token,
        };
        let response = app.as_user(&ghost).get("/api/me").await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        assert_eq!(response.code(), "session_expired");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_expired_session_is_refused(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    sqlx::query!(
        "UPDATE sessions SET expires_at = now() - interval '1 second' WHERE id_hash = $1",
        token::hash(&alice.token)
    )
    .execute(&db)
    .await
    .unwrap();
    let response = TestApp::new(db).as_user(&alice).get("/api/me").await;
    assert_eq!(response.code(), "session_expired");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn using_a_session_slides_its_expiry(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    // Last used ten minutes ago, about to expire.
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now() - interval '10 minutes',
                expires_at = now() + interval '1 minute'
         WHERE id_hash = $1",
        token::hash(&alice.token)
    )
    .execute(&db)
    .await
    .unwrap();

    let app = TestApp::new(db.clone());
    assert_eq!(
        app.as_user(&alice).get("/api/me").await.status,
        StatusCode::OK
    );
    let left = sqlx::query_scalar!(
        r#"SELECT extract(epoch FROM expires_at - now())::float8 AS "s!" FROM sessions WHERE id_hash = $1"#,
        token::hash(&alice.token)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(left > 6.9 * 24.0 * 3600.0, "slid to a week, got {left}s");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn sliding_never_passes_the_absolute_lifetime(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    // Signed in 29 days and 23 hours ago: one hour of life left, however used.
    sqlx::query!(
        "UPDATE sessions SET created_at = now() - interval '29 days 23 hours',
                last_seen_at = now() - interval '10 minutes'
         WHERE id_hash = $1",
        token::hash(&alice.token)
    )
    .execute(&db)
    .await
    .unwrap();

    let app = TestApp::new(db.clone());
    assert_eq!(
        app.as_user(&alice).get("/api/me").await.status,
        StatusCode::OK
    );
    let left = sqlx::query_scalar!(
        r#"SELECT extract(epoch FROM expires_at - now())::float8 AS "s!" FROM sessions WHERE id_hash = $1"#,
        token::hash(&alice.token)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(
        left <= 3600.0,
        "capped at the absolute lifetime, got {left}s"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_bearer_request_never_uses_the_cookie(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let request = Request::get("/api/me")
        .header(header::COOKIE, alice.cookie())
        .header(header::AUTHORIZATION, "Bearer rom_whatever")
        .body(Body::empty())
        .unwrap();
    let response = TestApp::new(db).send(request).await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.code(), "unauthorized");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn logout_ends_the_session_and_clears_the_cookie(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let app = TestApp::new(db.clone());

    let response = app.as_user(&alice).post_empty("/api/auth/logout").await;
    assert_eq!(response.status, StatusCode::OK);
    // No IdP configured: straight back to the sign-in page.
    assert_eq!(
        response.json()["redirect_url"],
        format!("{ORIGIN}/app/signin")
    );
    let cleared = response.set_cookie("romulus_session").unwrap();
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    assert_eq!(
        app.as_user(&alice).get("/api/me").await.code(),
        "session_expired"
    );
    let left = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM sessions"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn logout_is_idempotent(db: PgPool) {
    let response = TestApp::new(db)
        .post_json("/api/auth/logout", &json!({}))
        .await;
    assert_eq!(response.status, StatusCode::OK);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn logout_needs_the_csrf_header_and_our_origin(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let app = TestApp::new(db);
    for (origin, marked) in [(ORIGIN, false), ("http://evil.test", true)] {
        let mut request = Request::post("/api/auth/logout")
            .header(header::COOKIE, alice.cookie())
            .header(header::ORIGIN, origin);
        if marked {
            request = request.header(romulus_api::csrf::HEADER, "1");
        }
        let response = app.send(request.body(Body::empty()).unwrap()).await;
        assert_eq!(response.code(), "csrf", "{origin} {marked}");
    }
    // Still signed in: a forged logout did nothing.
    assert_eq!(
        app.as_user(&alice).get("/api/me").await.status,
        StatusCode::OK
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn methods_say_what_the_sign_in_page_offers(db: PgPool) {
    let response = TestApp::new(db.clone()).get("/api/auth/methods").await;
    assert_eq!(
        response.json(),
        json!({ "sso": false, "dev_signin": false })
    );

    let response = TestApp::with_config(db, dev_config())
        .get("/api/auth/methods")
        .await;
    assert_eq!(response.json()["dev_signin"], true);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn login_without_an_idp_explains_itself(db: PgPool) {
    let response = TestApp::new(db).get("/api/auth/login?next=/app/acme").await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(
        response.location(),
        "/app/signin?error=sso_unavailable&next=%2Fapp%2Facme"
    );
}

fn dev_config() -> Config {
    let mut config = Config::for_tests();
    config.dev_signin = true;
    config
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn dev_signin_does_not_exist_unless_enabled(db: PgPool) {
    let app = TestApp::new(db);
    for path in [
        "/api/auth/dev-signin?email=alice@romulus.test",
        "/api/auth/dev-signin",
    ] {
        let response = app.get(path).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(response.code(), "not_found");
    }
    let response = app
        .call(Method::POST, "/api/auth/dev-signin?email=a@b.c", None, None)
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn dev_signin_creates_the_user_and_a_session(db: PgPool) {
    let app = TestApp::with_config(db.clone(), dev_config());
    let response = app
        .get("/api/auth/dev-signin?email=carol.diaz@romulus.test&next=/app/invite/xyz")
        .await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.location(), "/app/invite/xyz");
    let cookie = response.set_cookie("romulus_session").unwrap();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/api"), "{cookie}");

    let session = Session {
        user: uuid::Uuid::nil(),
        token: response.cookie_value("romulus_session").unwrap(),
    };
    let me = app.as_user(&session).get("/api/me").await.json();
    assert_eq!(me["name"], "Carol Diaz");
    assert_eq!(me["email_verified"], true);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn dev_signin_reuses_an_existing_user_by_email(db: PgPool) {
    let alice = user(&db, "Alice").await;
    let app = TestApp::with_config(db.clone(), dev_config());
    let response = app
        .get("/api/auth/dev-signin?email=ALICE@romulus.test&verified=false")
        .await;
    let session = Session {
        user: alice,
        token: response.cookie_value("romulus_session").unwrap(),
    };
    let me = app.as_user(&session).get("/api/me").await.json();
    assert_eq!(me["id"], alice.to_string());
    assert_eq!(me["email_verified"], false);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn dev_signin_refuses_to_redirect_off_the_app(db: PgPool) {
    let app = TestApp::with_config(db, dev_config());
    for next in [
        "//evil.test",
        "https://evil.test/",
        "/\\evil.test",
        "/api/me",
    ] {
        let response = app
            .get(&format!(
                "/api/auth/dev-signin?email=a@romulus.test&next={}",
                url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>()
            ))
            .await;
        assert_eq!(response.location(), "/app/", "{next}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn signing_in_again_replaces_the_previous_session(db: PgPool) {
    let app = TestApp::with_config(db.clone(), dev_config());
    let first = app.get("/api/auth/dev-signin?email=a@romulus.test").await;
    let first = Session {
        user: uuid::Uuid::nil(),
        token: first.cookie_value("romulus_session").unwrap(),
    };
    let request = Request::get("/api/auth/dev-signin?email=b@romulus.test")
        .header(header::COOKIE, first.cookie())
        .body(Body::empty())
        .unwrap();
    let second = app.send(request).await;
    assert_eq!(second.status, StatusCode::SEE_OTHER);
    assert_eq!(
        app.as_user(&first).get("/api/me").await.code(),
        "session_expired"
    );
}
