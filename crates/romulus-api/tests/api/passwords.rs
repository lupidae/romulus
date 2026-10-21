//! Email + password sign-in (`ROMULUS_AUTH=password`): sign-in, lockout,
//! sign-up (with and without mail, invite-only and open), forgotten and
//! changed passwords, the admin commands, single-organization mode.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use romulus_api::{
    AppState, Config, admin,
    auth::password::{self, hash, throttle},
    config::{MailConfig, SignUpPolicy, SmtpTls},
    csrf,
    domain::{AccessTokenScope, Role},
    mail::{Mailer, Outbox},
    rate::Limiter,
    token,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    access_tokens::initialize,
    harness::{ORIGIN, Session, TestApp, TestResponse, member, org, session, signed_in, user},
};

const PASSWORD: &str = "violet tractor mango";
const COOKIE: &str = "romulus_session";

fn config(mail: bool, signup: SignUpPolicy) -> Config {
    let mut config = Config::for_tests();
    config.auth.password = true;
    config.signup = signup;
    if mail {
        config.mail = Some(MailConfig {
            host: "localhost".to_owned(),
            port: 1025,
            tls: SmtpTls::None,
            credentials: None,
            from: "romulus <romulus@example.com>".parse().unwrap(),
        });
    }
    config
}

/// Limits wide enough that only the lock (or the limit a test lowers) bites.
fn wide_limits() -> throttle::Limits {
    throttle::Limits {
        sign_in: Limiter::new(1000, Duration::from_millis(1)),
        requests: Limiter::new(1000, Duration::from_millis(1)),
        mails: Limiter::new(1000, Duration::from_millis(1)),
        outbound: Limiter::new(1000, Duration::from_millis(1)),
        links: Limiter::new(1000, Duration::from_millis(1)),
        change: Limiter::new(1000, Duration::from_millis(1)),
    }
}

/// A password-sign-in app; with `mail`, an outbox to read what it sent.
fn app_with(db: PgPool, mail: bool, signup: SignUpPolicy) -> (TestApp, Outbox) {
    let (mailer, outbox) = if mail {
        Mailer::memory()
    } else {
        (Mailer::off(), Outbox::default())
    };
    let mut state = AppState::new(db, config(mail, signup)).with_mailer(mailer);
    state.passwords = Arc::new(wide_limits());
    (TestApp::with_state(state), outbox)
}

/// A password account with [`PASSWORD`].
async fn account(db: &PgPool, email: &str, name: &str) -> Uuid {
    let mut conn = db.acquire().await.unwrap();
    let id = password::create_account(&mut conn, email, name)
        .await
        .unwrap();
    let phc = hash::hash(PASSWORD).await.unwrap();
    password::store_password(&mut conn, id, &phc).await.unwrap();
    id
}

async fn sign_in(app: &TestApp, email: &str, password: &str) -> TestResponse {
    app.post_json(
        "/api/auth/password/sign-in",
        &json!({ "email": email, "password": password }),
    )
    .await
}

/// The session a response's cookie starts.
fn session_of(response: &TestResponse, user: Uuid) -> Session {
    Session {
        user,
        token: response
            .cookie_value(COOKIE)
            .expect("a session cookie is set"),
    }
}

/// The messages sent to `to` once there are `n` of them (sign-up and reset
/// links go out from a task of their own, after the answer).
async fn mail(outbox: &Outbox, to: &str, n: usize) -> Vec<romulus_api::mail::Email> {
    for _ in 0..200 {
        let sent = outbox.to(to);
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{n} message(s) to {to} never came: {:?}", outbox.all());
}

/// Lets tasks spawned by the last request finish (before asserting that
/// nothing was sent).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// The token in the one link of a message (`…#token=<token>`).
fn token_in(text: &str) -> String {
    let start = text.find("#token=").expect("a link with a token") + "#token=".len();
    text[start..start + 43].to_owned()
}

async fn invitation(db: &PgPool, org: Uuid, admin: Uuid, email: &str, role: Role) -> String {
    let link = token::generate();
    sqlx::query!(
        "INSERT INTO invitations (org_id, email, role, token_hash, expires_at, created_by)
         VALUES ($1, $2, $3, $4, now() + interval '7 days', $5)",
        org,
        email,
        role as Role,
        token::hash(&link),
        admin,
    )
    .execute(db)
    .await
    .unwrap();
    link
}

/// An organization `acme` with an admin who invited `email` as a developer.
async fn invited(db: &PgPool, email: &str) -> (Uuid, String) {
    let acme = org(db, "acme").await;
    let admin = user(db, "Ada").await;
    member(db, acme, admin, Role::Admin).await;
    (
        acme,
        invitation(db, acme, admin, email, Role::Developer).await,
    )
}

async fn role_in(db: &PgPool, org: Uuid, email: &str) -> Option<Role> {
    sqlx::query_scalar!(
        r#"SELECT m.role AS "role: Role" FROM memberships m JOIN users u ON u.id = m.user_id
           WHERE m.org_id = $1 AND lower(u.email) = lower($2)"#,
        org,
        email,
    )
    .fetch_optional(db)
    .await
    .unwrap()
}

async fn events(db: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM auth_events WHERE kind::text = $1"#,
        kind
    )
    .fetch_one(db)
    .await
    .unwrap()
}

// ---- Sign in ------------------------------------------------------------------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_password_signs_in_with_a_session_like_sso(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);

    // Case and Unicode form of the address do not matter.
    let response = sign_in(&app, " ALICE@acme.io ", PASSWORD).await;
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    let cookie = response.set_cookie(COOKIE).unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
            && cookie.contains("Path=/api")
    );

    let me = app
        .as_user(&session_of(&response, id))
        .get("/api/me")
        .await
        .json();
    assert_eq!(me["email"], "alice@acme.io");
    assert_eq!(me["password_account"], true);
    assert_eq!(me["email_verified"], true);
    assert_eq!(events(&db, "signed_in").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_wrong_password_and_an_unknown_address_answer_alike(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let wrong = sign_in(&app, "alice@acme.io", "violet tractor mangos").await;
    let unknown = sign_in(&app, "nobody@acme.io", PASSWORD).await;
    for response in [&wrong, &unknown] {
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        assert_eq!(response.code(), "invalid_credentials");
        assert!(response.set_cookie(COOKIE).is_none());
    }
    assert_eq!(wrong.body, unknown.body);
    assert_eq!(events(&db, "sign_in_failed").await, 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_account_without_a_password_yet_cannot_sign_in(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    password::create_account(&mut conn, "new@acme.io", "New")
        .await
        .unwrap();
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    assert_eq!(
        sign_in(&app, "new@acme.io", "").await.code(),
        "invalid_credentials"
    );
    assert_eq!(
        sign_in(&app, "new@acme.io", PASSWORD).await.code(),
        "invalid_credentials"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn ten_failures_lock_an_address_whether_it_has_an_account_or_not(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    for email in ["alice@acme.io", "nobody@acme.io"] {
        for attempt in 1..throttle::LOCK_AFTER {
            let response = sign_in(&app, email, "wrong password, sorry").await;
            assert_eq!(response.code(), "invalid_credentials", "{email} #{attempt}");
        }
        let locking = sign_in(&app, email, "wrong password, sorry").await;
        assert_eq!(locking.status, StatusCode::TOO_MANY_REQUESTS, "{email}");
        assert_eq!(locking.code(), "rate_limited");
        // Even the right password waits now, unchecked.
        let locked = sign_in(&app, email, PASSWORD).await;
        assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
        let wait: u64 = locked.headers[header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(wait > 14 * 60 && wait <= 15 * 60, "{wait}");
        assert_eq!(locked.json()["details"]["retry_after"], wait);
    }
    // Only the account's owner hears of it, once.
    assert_eq!(outbox.to("alice@acme.io").len(), 1);
    assert_eq!(outbox.to("alice@acme.io")[0].kind, "locked");
    assert_eq!(outbox.to("nobody@acme.io"), []);
    // One event per lock; refused attempts while it lasts write nothing.
    assert_eq!(events(&db, "sign_in_locked").await, 2);

    // The lock runs out: a new series starts (one guess every 15 minutes
    // never keeps the owner out for good).
    sqlx::query!("UPDATE sign_in_failures SET locked_until = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        sign_in(&app, "alice@acme.io", "still wrong, again")
            .await
            .code(),
        "invalid_credentials"
    );
    // An admin unlocks it.
    assert!(admin::unlock(&db, "Alice@acme.io").await.unwrap());
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(events(&db, "unlocked").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn guesses_sent_at_once_cannot_outrun_the_lock(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let guesses = (0..30).map(|i| {
        let app = &app;
        async move { sign_in(app, "alice@acme.io", &format!("guess number {i:03}")).await }
    });
    let answers = futures_util::future::join_all(guesses).await;
    let refused = answers
        .iter()
        .filter(|answer| answer.status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    // Only the first ten were checked at all; the rest were refused unchecked.
    assert_eq!(
        events(&db, "sign_in_failed").await,
        i64::from(throttle::LOCK_AFTER)
    );
    assert_eq!(refused, 30 - 9);
}

/// A sign-in from a browser holding `cookies` (`name=value; …`).
async fn sign_in_from(app: &TestApp, cookies: &str, email: &str, password: &str) -> TestResponse {
    app.send(
        Request::post("/api/auth/password/sign-in")
            .header(header::ORIGIN, ORIGIN)
            .header(csrf::HEADER, "1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookies)
            .body(Body::from(
                json!({ "email": email, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_browser_that_signed_in_before_is_not_locked_out_by_others(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let first = sign_in(&app, "alice@acme.io", PASSWORD).await;
    let device = first
        .cookie_value(&format!("{COOKIE}_device"))
        .expect("a device cookie");
    let cookie = first.set_cookie(&format!("{COOKIE}_device")).unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/api/auth/password"));
    let alices = format!("{COOKIE}_device={device}");

    // Someone else locks the address…
    for _ in 0..throttle::LOCK_AFTER {
        sign_in(&app, "alice@acme.io", "a guess from afar").await;
    }
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // …Alice's own browser still signs in, and keeps its cookie.
    let back = sign_in_from(&app, &alices, "alice@acme.io", PASSWORD).await;
    assert_eq!(back.status, StatusCode::NO_CONTENT);
    assert!(back.set_cookie(&format!("{COOKIE}_device")).is_none());

    // The device counts its own failures, and locks itself.
    for _ in 0..throttle::LOCK_AFTER {
        sign_in_from(&app, &alices, "alice@acme.io", "my own typo here").await;
    }
    let locked = sign_in_from(&app, &alices, "alice@acme.io", PASSWORD).await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    // A device of another account counts for nothing here.
    account(&db, "bob@acme.io", "Bob").await;
    let bobs = sign_in_from(&app, &alices, "bob@acme.io", PASSWORD).await;
    assert_eq!(bobs.status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reset_forgets_every_device(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    sign_in(&app, "alice@acme.io", PASSWORD).await;
    sign_in(&app, "alice@acme.io", PASSWORD).await;
    let devices = || async {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM sign_in_devices WHERE user_id = $1"#,
            id
        )
        .fetch_one(&db)
        .await
        .unwrap()
    };
    assert_eq!(devices().await, 2);
    let link = admin::password_link(&db, &config(false, SignUpPolicy::Invite), "alice@acme.io")
        .await
        .unwrap();
    let done = app
        .post_json(
            "/api/auth/password/reset",
            &json!({ "token": token_in(&link), "password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    // Only the browser that reset it.
    assert_eq!(devices().await, 1);
    assert!(done.cookie_value(&format!("{COOKIE}_device")).is_some());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_address_locked_again_and_again_is_counted_and_told_once(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    for series in 1..=5 {
        let mut last = None;
        for _ in 0..throttle::LOCK_AFTER {
            last = Some(sign_in(&app, "alice@acme.io", "a guess from afar").await);
        }
        let row = sqlx::query!(
            r#"SELECT locks, extract(epoch FROM locked_until - now())::float8 AS "left!"
               FROM sign_in_failures"#
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row.locks, series);
        // 15 minutes, then 30, then an hour, and never more.
        let expected = throttle::lock_duration(series).as_secs_f64();
        assert!(expected <= 3600.0);
        assert!(row.left <= 3600.0, "lock {series}: {} s", row.left);
        assert!(
            (row.left - expected).abs() < 60.0,
            "lock {series}: {} s",
            row.left
        );
        let wait = last.unwrap().json()["details"]["retry_after"]
            .as_f64()
            .unwrap();
        assert!(
            (wait - expected).abs() < 60.0,
            "lock {series}: told {wait} s"
        );
        sqlx::query!("UPDATE sign_in_failures SET locked_until = now() - interval '1 second'")
            .execute(&db)
            .await
            .unwrap();
    }
    let logged = sqlx::query_scalar!(
        r#"SELECT (details->>'locks')::int AS "locks!" FROM auth_events
           WHERE kind = 'sign_in_locked' ORDER BY created_at"#
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(logged, [1, 2, 3, 4, 5]);
    // The owner heard of the first only.
    assert_eq!(outbox.to("alice@acme.io").len(), 1);
    // The right password still works once the lock runs out.
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_success_starts_the_locks_over_and_a_reset_link_works_through_them(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let lock = async || {
        for _ in 0..throttle::LOCK_AFTER {
            sign_in(&app, "alice@acme.io", "a guess from afar").await;
        }
        sqlx::query_scalar!(
            r#"SELECT extract(epoch FROM locked_until - now())::float8 AS "left!" FROM sign_in_failures"#
        )
        .fetch_one(&db)
        .await
        .unwrap()
    };
    let expire = async || {
        sqlx::query!("UPDATE sign_in_failures SET locked_until = now() - interval '1 second'")
            .execute(&db)
            .await
            .unwrap();
    };
    assert!((lock().await - 900.0).abs() < 60.0);
    expire().await;
    assert!((lock().await - 1800.0).abs() < 60.0);
    // Locked for half an hour: a reset link still sets a password and signs in.
    let link = admin::password_link(&db, &config(false, SignUpPolicy::Invite), "alice@acme.io")
        .await
        .unwrap();
    let reset = app
        .post_json(
            "/api/auth/password/reset",
            &json!({ "token": token_in(&link), "password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT);
    // The reset ended the series and the day's locks: the next lock is 15 minutes.
    assert!((lock().await - 900.0).abs() < 60.0);
    expire().await;
    // And a success does the same.
    assert_eq!(
        sign_in(&app, "alice@acme.io", "granite pelican sky")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert!((lock().await - 900.0).abs() < 60.0);
}

/// A sign-in from `ip`, with `cookies` (`name=value; …`, or empty).
async fn sign_in_at(
    app: &TestApp,
    ip: &str,
    cookies: &str,
    email: &str,
    password: &str,
) -> TestResponse {
    let addr = SocketAddr::new(ip.parse().unwrap(), 5000);
    let mut request = Request::post("/api/auth/password/sign-in")
        .header(header::ORIGIN, ORIGIN)
        .header(csrf::HEADER, "1")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(addr));
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, cookies);
    }
    app.send(
        request
            .body(Body::from(
                json!({ "email": email, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_flood_from_four_addresses_denies_neither_a_newcomer_nor_a_known_device(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    account(&db, "carol@acme.io", "Carol").await;
    // The real limits.
    let app = TestApp::with_state(AppState::new(db, config(false, SignUpPolicy::Invite)));
    let first = sign_in_at(&app, "198.51.100.9", "", "alice@acme.io", PASSWORD).await;
    let device = format!(
        "{COOKIE}_device={}",
        first.cookie_value(&format!("{COOKIE}_device")).unwrap()
    );

    // Four addresses (or four /64s of one IPv6 /48, which count as one),
    // as fast as they may.
    let flooders = [
        "203.0.113.1",
        "203.0.113.2",
        "203.0.113.3",
        "2001:db8:1:1::1",
    ];
    let started = std::time::Instant::now();
    let mut passed = [0_u32; 4];
    for round in 0..15 {
        for (i, ip) in flooders.iter().enumerate() {
            let ip = if ip.contains(':') {
                // Another /64 each round, same /48.
                format!("2001:db8:1:{round:x}::1")
            } else {
                (*ip).to_owned()
            };
            let answer = sign_in_at(
                &app,
                &ip,
                "",
                &format!("x{}@acme.io", Uuid::new_v4()),
                "guess guess guess",
            )
            .await;
            if answer.status != StatusCode::TOO_MANY_REQUESTS {
                passed[i] += 1;
            }
        }
    }
    // Each got its burst (10) and what the time spent refilled (one per 3 s),
    // no more: the /64s of one /48 shared one bucket.
    let refilled = u32::try_from(started.elapsed().as_secs() / 3).unwrap() + 1;
    for (ip, passed) in flooders.iter().zip(passed) {
        assert!(passed >= 10 && passed <= 10 + refilled, "{ip}: {passed}");
    }
    // No global cap to fill: Carol, signing in for the first time elsewhere,
    // and Alice's browser get in (only a saturated hashing pool could say
    // 503 `busy`: hash::Slots).
    let alice = sign_in_at(&app, "198.51.100.9", &device, "alice@acme.io", PASSWORD).await;
    assert_eq!(alice.status, StatusCode::NO_CONTENT);
    let carol = sign_in_at(&app, "198.51.100.10", "", "carol@acme.io", PASSWORD).await;
    assert_eq!(carol.status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_success_forgets_failures_and_a_day_does_too(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    for _ in 0..throttle::LOCK_AFTER - 1 {
        sign_in(&app, "alice@acme.io", "wrong password, sorry").await;
    }
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::NO_CONTENT
    );
    for _ in 0..throttle::LOCK_AFTER - 1 {
        sign_in(&app, "alice@acme.io", "wrong password, sorry").await;
    }
    sqlx::query!("UPDATE sign_in_failures SET last_failure_at = now() - interval '25 hours'")
        .execute(&db)
        .await
        .unwrap();
    // The ninth was a day ago: this is the first again, not the tenth.
    assert_eq!(
        sign_in(&app, "alice@acme.io", "wrong password, sorry")
            .await
            .code(),
        "invalid_credentials"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_client_gets_a_burst_of_attempts(db: PgPool) {
    let state = AppState::new(db, config(false, SignUpPolicy::Invite));
    let app = TestApp::with_state(state);
    let from = |ip: &str| {
        let addr: SocketAddr = format!("{ip}:5000").parse().unwrap();
        Request::post("/api/auth/password/sign-in")
            .header(header::ORIGIN, ORIGIN)
            .header(csrf::HEADER, "1")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(addr))
            .body(Body::from(
                json!({ "email": format!("x{}@acme.io", Uuid::new_v4()), "password": "nope nope nope" })
                    .to_string(),
            ))
            .unwrap()
    };
    for _ in 0..10 {
        assert_eq!(
            app.send(from("203.0.113.7")).await.code(),
            "invalid_credentials"
        );
    }
    let limited = app.send(from("203.0.113.7")).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        app.send(from("203.0.113.8")).await.code(),
        "invalid_credentials"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn behind_a_trusted_proxy_the_forwarded_client_is_counted(db: PgPool) {
    let mut config = config(false, SignUpPolicy::Invite);
    config.trusted_proxies = vec!["172.16.0.0/12".parse().unwrap()];
    let app = TestApp::with_state(AppState::new(db.clone(), config));
    let via_proxy = |client: &str| {
        let proxy: SocketAddr = "172.18.0.5:41000".parse().unwrap();
        Request::post("/api/auth/password/sign-in")
            .header(header::ORIGIN, ORIGIN)
            .header(csrf::HEADER, "1")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", format!("6.6.6.6, {client}"))
            .extension(ConnectInfo(proxy))
            .body(Body::from(
                json!({ "email": "a@acme.io", "password": "nope nope nope" }).to_string(),
            ))
            .unwrap()
    };
    app.send(via_proxy("203.0.113.7")).await;
    let ip = sqlx::query_scalar!(r#"SELECT host(ip) AS "ip!" FROM auth_events LIMIT 1"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(ip, "203.0.113.7");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn weaker_hashes_are_upgraded_at_sign_in(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    // A hash with lighter parameters, as an older release might have made.
    let weak = {
        use argon2::{Argon2, Params, PasswordHasher};
        let argon = Argon2::from(Params::new(4096, 1, 1, None).unwrap());
        argon
            .hash_password_with_salt(PASSWORD.as_bytes(), b"saltsaltsaltsalt")
            .unwrap()
            .to_string()
    };
    sqlx::query!(
        "UPDATE password_credentials SET hash = $2 WHERE user_id = $1",
        id,
        weak
    )
    .execute(&db)
    .await
    .unwrap();
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::NO_CONTENT
    );
    let stored = sqlx::query_scalar!(
        "SELECT hash FROM password_credentials WHERE user_id = $1",
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!hash::needs_rehash(&stored), "{stored}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn password_endpoints_do_not_exist_while_it_is_off(db: PgPool) {
    let app = TestApp::new(db);
    for path in [
        "/api/auth/password/sign-in",
        "/api/auth/password/sign-up",
        "/api/auth/password/forgot",
        "/api/auth/password/reset",
    ] {
        let response = app
            .post_json(
                path,
                &json!({ "email": "a@b.c", "password": "x", "name": "x", "token": "x" }),
            )
            .await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
    }
    let methods = app.get("/api/auth/methods").await.json();
    assert_eq!(methods["password"], false);
    assert_eq!(methods["sign_up"], Value::Null);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn sign_in_needs_the_csrf_header_like_every_mutation(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db, false, SignUpPolicy::Invite);
    let response = app
        .send(
            Request::post("/api/auth/password/sign-in")
                .header(header::ORIGIN, "https://evil.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "email": "alice@acme.io", "password": PASSWORD }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(response.code(), "csrf");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_sign_in_page_learns_what_this_server_offers(db: PgPool) {
    let (app, _) = app_with(db.clone(), true, SignUpPolicy::Open);
    let methods = app.get("/api/auth/methods").await.json();
    assert_eq!(methods["password"], true);
    assert_eq!(methods["sign_up"], "open");
    assert_eq!(methods["password_reset"], true);
    assert_eq!(methods["sso_enabled"], true);
    assert_eq!(methods["sso"], false);
    let (app, _) = app_with(db, false, SignUpPolicy::Invite);
    let methods = app.get("/api/auth/methods").await.json();
    assert_eq!(methods["sign_up"], "invite");
    assert_eq!(methods["password_reset"], false);
    assert_eq!(app.get("/api/instance").await.json()["mail"], false);
}

// ---- Sign up ------------------------------------------------------------------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_mail_an_invitation_signs_up_through_an_emailed_link(db: PgPool) {
    let (acme, link) = invited(&db, "bob@acme.io").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    let up = async |body: Value| app.post_json("/api/auth/password/sign-up", &body).await;

    // Invite-only: no invitation, someone else's, a dead one.
    let none = up(json!({ "email": "bob@acme.io", "name": "Bob" })).await;
    assert_eq!(none.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(none.json()["details"]["invitation"].is_string());
    let other = up(json!({ "email": "eve@evil.io", "name": "Eve", "invitation": link })).await;
    assert!(
        other.json()["details"]["email"]
            .as_str()
            .unwrap()
            .contains("bob@acme.io")
    );
    let dead =
        up(json!({ "email": "bob@acme.io", "name": "Bob", "invitation": token::generate() })).await;
    assert_eq!(dead.status, StatusCode::NOT_FOUND);
    settle().await;
    assert_eq!(outbox.all(), []);

    let sent =
        up(json!({ "email": "Bob@acme.io", "name": " Bob Durand ", "invitation": link })).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED);
    assert_eq!(sent.json()["outcome"], "email_sent");
    assert!(sent.set_cookie(COOKIE).is_none());
    // No account until the link is used.
    assert!(
        password::find_account(&db, "bob@acme.io")
            .await
            .unwrap()
            .is_none()
    );

    let mail = &mail(&outbox, "bob@acme.io", 1).await[0];
    assert_eq!(mail.kind, "sign_up");
    assert!(mail.text.contains("join acme"), "{}", mail.text);
    let token = token_in(&mail.text);

    let check = app
        .post_json(
            "/api/auth/password/sign-up/check",
            &json!({ "token": token }),
        )
        .await
        .json();
    assert_eq!(check["email"], "Bob@acme.io");
    assert_eq!(check["name"], "Bob Durand");
    assert_eq!(check["org"], "acme");

    let finish = async |password: &str| {
        app.post_json(
            "/api/auth/password/sign-up/finish",
            &json!({ "token": token, "name": "Bob Durand", "password": password }),
        )
        .await
    };
    let weak = finish("bob durand 2024").await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(weak.json()["details"]["password"].is_string());
    let done = finish(PASSWORD).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.json());
    assert_eq!(done.json()["outcome"], "signed_in");
    assert_eq!(done.json()["org"]["slug"], "acme");
    assert_eq!(done.json()["org"]["role"], "developer");
    assert_eq!(
        role_in(&db, acme, "bob@acme.io").await,
        Some(Role::Developer)
    );
    let bob = password::find_account(&db, "bob@acme.io")
        .await
        .unwrap()
        .unwrap();
    let me = app
        .as_user(&session_of(&done, bob.id))
        .get("/api/me")
        .await
        .json();
    assert_eq!(me["name"], "Bob Durand");

    // The link worked once.
    assert_eq!(finish(PASSWORD).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        sign_in(&app, "bob@acme.io", PASSWORD).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn signing_up_with_an_address_that_has_an_account_says_nothing_to_the_asker(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Open);
    let existing = app
        .post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "alice@acme.io", "name": "Mallory" }),
        )
        .await;
    let new = app
        .post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "carol@acme.io", "name": "Carol" }),
        )
        .await;
    assert_eq!(existing.status, StatusCode::ACCEPTED);
    assert_eq!(
        (existing.status, existing.body.clone()),
        (new.status, new.body.clone())
    );
    // The owner is told; no link that could create a second account.
    let told = &mail(&outbox, "alice@acme.io", 1).await[0];
    assert_eq!(told.kind, "account_exists");
    assert!(!told.text.contains("#token="));
    assert_eq!(mail(&outbox, "carol@acme.io", 1).await[0].kind, "sign_up");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn open_sign_up_needs_no_invitation_and_joins_nothing(db: PgPool) {
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Open);
    let sent = app
        .post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "carol@acme.io", "name": "Carol" }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::ACCEPTED);
    let token = token_in(&mail(&outbox, "carol@acme.io", 1).await[0].text);
    let done = app
        .post_json(
            "/api/auth/password/sign-up/finish",
            &json!({ "token": token, "name": "Carol", "password": PASSWORD }),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK);
    assert_eq!(done.json()["org"], Value::Null);
    // Two links for one address: the second finds the account made.
    let again = app
        .post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "carol@acme.io", "name": "Carol" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::ACCEPTED);
    assert_eq!(
        mail(&outbox, "carol@acme.io", 2).await[1].kind,
        "account_exists"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_revoked_invitation_voids_its_sign_up_link(db: PgPool) {
    let (_, link) = invited(&db, "bob@acme.io").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    app.post_json(
        "/api/auth/password/sign-up",
        &json!({ "email": "bob@acme.io", "name": "Bob", "invitation": link }),
    )
    .await;
    let token = token_in(&mail(&outbox, "bob@acme.io", 1).await[0].text);
    sqlx::query!("DELETE FROM invitations")
        .execute(&db)
        .await
        .unwrap();
    let done = app
        .post_json(
            "/api/auth/password/sign-up/finish",
            &json!({ "token": token, "name": "Bob", "password": PASSWORD }),
        )
        .await;
    assert_eq!(done.status, StatusCode::NOT_FOUND);
    assert!(
        password::find_account(&db, "bob@acme.io")
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_mail_the_invitation_vouches_and_the_account_is_made_at_once(db: PgPool) {
    let (acme, link) = invited(&db, "bob@acme.io").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let up = async |password: &str| {
        app.post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "bob@acme.io", "name": "Bob", "password": password, "invitation": link }),
        )
        .await
    };
    assert!(up("short").await.json()["details"]["password"].is_string());
    let done = up(PASSWORD).await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.json());
    assert_eq!(done.json()["outcome"], "signed_in");
    assert_eq!(done.json()["org"]["slug"], "acme");
    assert_eq!(
        role_in(&db, acme, "bob@acme.io").await,
        Some(Role::Developer)
    );
    assert!(done.set_cookie(COOKIE).is_some());
    // The invitation served.
    assert_eq!(up(PASSWORD).await.status, StatusCode::NOT_FOUND);

    // Another invitation for an address that has an account: sign in instead.
    let admin = sqlx::query_scalar!("SELECT user_id FROM memberships WHERE role = 'admin'")
        .fetch_one(&db)
        .await
        .unwrap();
    let beta = org(&db, "beta").await;
    member(&db, beta, admin, Role::Admin).await;
    let second = invitation(&db, beta, admin, "bob@acme.io", Role::Reviewer).await;
    let exists = app
        .post_json(
            "/api/auth/password/sign-up",
            &json!({ "email": "bob@acme.io", "name": "Bob", "password": PASSWORD, "invitation": second }),
        )
        .await;
    assert_eq!(exists.status, StatusCode::CONFLICT);
    assert_eq!(exists.code(), "account_exists");
}

// ---- Forgotten passwords ------------------------------------------------------------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reset_link_sets_a_new_password_once_and_ends_every_session(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    let elsewhere = session(&db, id).await;
    let agent = super::mcp::pat(&db, &elsewhere, AccessTokenScope::Write).await;
    let connected = super::oauth::connected(&db, &elsewhere, AccessTokenScope::Write).await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    assert_eq!(initialize(&app, &agent).await.status, StatusCode::OK);
    assert_eq!(initialize(&app, &connected).await.status, StatusCode::OK);

    // Unknown and known addresses: the same answer; only the account gets mail.
    let forgot = async |email: &str| {
        app.post_json("/api/auth/password/forgot", &json!({ "email": email }))
            .await
    };
    let unknown = forgot("nobody@acme.io").await;
    let known = forgot("ALICE@acme.io").await;
    assert_eq!(unknown.status, StatusCode::ACCEPTED);
    assert_eq!(
        (unknown.status, unknown.body.clone()),
        (known.status, known.body.clone())
    );
    let mail = &mail(&outbox, "alice@acme.io", 1).await[0];
    settle().await;
    assert_eq!(outbox.to("nobody@acme.io"), []);
    assert_eq!(mail.kind, "reset");
    assert!(
        mail.text
            .contains("http://localhost:41000/app/signin/reset#token=")
    );
    let token = token_in(&mail.text);

    let check = app
        .post_json("/api/auth/password/reset/check", &json!({ "token": token }))
        .await
        .json();
    assert_eq!(check["email"], "alice@acme.io");
    assert_eq!(check["first_password"], false);

    let reset = async |password: &str| {
        app.post_json(
            "/api/auth/password/reset",
            &json!({ "token": token, "password": password }),
        )
        .await
    };
    assert!(reset("alice@acme.io!!").await.json()["details"]["password"].is_string());
    let done = reset("granite pelican sky").await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    assert!(done.set_cookie(COOKIE).is_some());
    assert_eq!(
        reset("another one entirely").await.status,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        app.as_user(&elsewhere).get("/api/me").await.code(),
        "session_expired"
    );
    // Signed out everywhere: agents too, connected apps included.
    assert_eq!(
        initialize(&app, &agent).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        initialize(&app, &connected).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.code(),
        "invalid_credentials"
    );
    assert_eq!(
        sign_in(&app, "alice@acme.io", "granite pelican sky")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let notice = outbox.to("alice@acme.io").last().unwrap().clone();
    assert_eq!(notice.kind, "password_changed");
    assert!(
        notice
            .text
            .contains("Your 1 access token for AI agents was revoked too"),
        "{}",
        notice.text
    );
    assert_eq!(events(&db, "password_reset").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reset_links_expire_and_only_the_newest_works(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    let forgot = async || {
        app.post_json(
            "/api/auth/password/forgot",
            &json!({ "email": "alice@acme.io" }),
        )
        .await
    };
    forgot().await;
    mail(&outbox, "alice@acme.io", 1).await;
    forgot().await;
    let sent = mail(&outbox, "alice@acme.io", 2).await;
    let [first, second] = [0, 1].map(|i| token_in(&sent[i].text));
    let check = async |token: &str| {
        app.post_json("/api/auth/password/reset/check", &json!({ "token": token }))
            .await
    };
    assert_eq!(check(&first).await.status, StatusCode::NOT_FOUND);
    assert_eq!(check(&second).await.status, StatusCode::OK);
    sqlx::query!("UPDATE auth_links SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(check(&second).await.status, StatusCode::NOT_FOUND);
    let reset = app
        .post_json(
            "/api/auth/password/reset",
            &json!({ "token": second, "password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NOT_FOUND);
    // Links are stored hashed.
    let stored: Vec<Vec<u8>> = sqlx::query_scalar!("SELECT token_hash FROM auth_links")
        .fetch_all(&db)
        .await
        .unwrap();
    assert!(stored.contains(&token::hash(&second)));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reset_unlocks_the_address(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    for _ in 0..throttle::LOCK_AFTER {
        sign_in(&app, "alice@acme.io", "wrong password, sorry").await;
    }
    assert_eq!(
        sign_in(&app, "alice@acme.io", PASSWORD).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let link = admin::password_link(&db, &config(false, SignUpPolicy::Invite), "alice@acme.io")
        .await
        .unwrap();
    let token = token_in(&link);
    let done = app
        .post_json(
            "/api/auth/password/reset",
            &json!({ "token": token, "password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    assert_eq!(
        sign_in(&app, "alice@acme.io", "granite pelican sky")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_mail_a_forgotten_password_is_an_admin_s_link(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let response = app
        .post_json(
            "/api/auth/password/forgot",
            &json!({ "email": "alice@acme.io" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.code(), "mail_unavailable");
    let config = config(false, SignUpPolicy::Invite);
    assert!(matches!(
        admin::password_link(&db, &config, "nobody@acme.io").await,
        Err(admin::AdminError::Invalid(_))
    ));
    let link = admin::password_link(&db, &config, "alice@acme.io")
        .await
        .unwrap();
    assert!(link.starts_with("http://localhost:41000/app/signin/reset#token="));
    assert_eq!(events(&db, "password_link_created").await, 1);
}

// ---- Changing one's password ----------------------------------------------------------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn changing_a_password_keeps_this_browser_and_signs_out_the_others(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    let (app, outbox) = app_with(db.clone(), true, SignUpPolicy::Invite);
    let here = session(&db, id).await;
    let elsewhere = session(&db, id).await;
    let agent = super::mcp::pat(&db, &here, AccessTokenScope::Read).await;
    let connected = super::oauth::connected(&db, &here, AccessTokenScope::Read).await;
    let change = async |current: &str, new: &str| {
        app.as_user(&here)
            .post(
                "/api/auth/password/change",
                &json!({ "current_password": current, "new_password": new }),
            )
            .await
    };
    let wrong = change("not my password", "granite pelican sky").await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.json()["details"]["current_password"].is_string());
    assert!(change(PASSWORD, PASSWORD).await.json()["details"]["new_password"].is_string());
    assert!(
        change(PASSWORD, "alice rocks 12345").await.json()["details"]["new_password"].is_string()
    );

    let done = change(PASSWORD, "granite pelican sky").await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    // This browser goes on under a new session id; the old one and the
    // other browser's are over.
    let renewed = session_of(&done, id);
    assert_ne!(renewed.token, here.token);
    assert_eq!(
        app.as_user(&renewed).get("/api/me").await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.as_user(&here).get("/api/me").await.code(),
        "session_expired"
    );
    assert_eq!(
        app.as_user(&elsewhere).get("/api/me").await.code(),
        "session_expired"
    );
    // Every other place is signed out, agents and connected apps included.
    assert_eq!(
        initialize(&app, &agent).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        initialize(&app, &connected).await.status,
        StatusCode::UNAUTHORIZED
    );
    let notice = outbox.to("alice@acme.io").last().unwrap().clone();
    assert_eq!(notice.kind, "password_changed");
    assert!(notice.text.contains("revoked too"), "{}", notice.text);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn single_sign_on_accounts_have_no_password_to_change(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let (app, _) = app_with(db, false, SignUpPolicy::Invite);
    let response = app
        .as_user(&alice)
        .post(
            "/api/auth/password/change",
            &json!({ "current_password": "x", "new_password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.code(), "not_a_password_account");
    assert_eq!(
        app.as_user(&alice).get("/api/me").await.json()["password_account"],
        false
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn wrong_current_passwords_count_toward_the_lock(db: PgPool) {
    let id = account(&db, "alice@acme.io", "Alice").await;
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let here = session(&db, id).await;
    for _ in 0..throttle::LOCK_AFTER {
        app.as_user(&here)
            .post(
                "/api/auth/password/change",
                &json!({ "current_password": "guess guess guess", "new_password": "granite pelican sky" }),
            )
            .await;
    }
    let locked = app
        .as_user(&here)
        .post(
            "/api/auth/password/change",
            &json!({ "current_password": PASSWORD, "new_password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
}

// ---- Setup, single organization, invitations by email ------------------------------------

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn setup_makes_the_first_admin_and_the_organization_once(db: PgPool) {
    let mut config = config(false, SignUpPolicy::Invite);
    config.single_org = true;
    let setup = |email: &str| admin::Setup {
        email: email.to_owned(),
        name: "Ada Admin".to_owned(),
        org: "Acme Labs".to_owned(),
        slug: None,
    };
    let admin::SetupOutcome::Link(link) = admin::setup(&db, &config, setup("ada@acme.io"))
        .await
        .unwrap()
    else {
        panic!("a link for a new admin");
    };
    let ada = password::find_account(&db, "ada@acme.io")
        .await
        .unwrap()
        .unwrap();
    let acme = sqlx::query_scalar!("SELECT id FROM organizations WHERE slug = 'acme-labs'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(role_in(&db, acme, "ada@acme.io").await, Some(Role::Admin));

    // Again before the password is set: a fresh link, nothing duplicated.
    let admin::SetupOutcome::Link(again) = admin::setup(&db, &config, setup("ada@acme.io"))
        .await
        .unwrap()
    else {
        panic!("still a link");
    };
    assert_ne!(again, link);
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    let first = app
        .post_json(
            "/api/auth/password/reset/check",
            &json!({ "token": token_in(&again) }),
        )
        .await
        .json();
    assert_eq!(first["first_password"], true);
    // The first link was retired by the second.
    let stale = app
        .post_json(
            "/api/auth/password/reset/check",
            &json!({ "token": token_in(&link) }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::NOT_FOUND);
    let set = app
        .post_json(
            "/api/auth/password/reset",
            &json!({ "token": token_in(&again), "password": "granite pelican sky" }),
        )
        .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT);
    let me = app
        .as_user(&session_of(&set, ada.id))
        .get("/api/orgs")
        .await
        .json();
    assert_eq!(me[0]["slug"], "acme-labs");

    assert!(matches!(
        admin::setup(&db, &config, setup("ada@acme.io"))
            .await
            .unwrap(),
        admin::SetupOutcome::AlreadySetUp { .. }
    ));
    assert!(matches!(
        admin::setup(&db, &config, setup("mallory@evil.io")).await,
        Err(admin::AdminError::AlreadyOwned(_))
    ));
    let orgs = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM organizations"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(orgs, 1);
    assert_eq!(events(&db, "admin_created").await, 1);

    let mut sso = Config::for_tests();
    sso.single_org = true;
    assert!(matches!(
        admin::setup(&db, &sso, setup("x@acme.io")).await,
        Err(admin::AdminError::Invalid(_))
    ));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_single_organization_install_refuses_another(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let mut config = Config::for_tests();
    config.single_org = true;
    let app = TestApp::with_config(db, config);
    let response = app
        .as_user(&alice)
        .post("/api/orgs", &json!({ "name": "Second", "slug": "second" }))
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.code(), "single_org");
    assert_eq!(app.get("/api/instance").await.json()["single_org"], true);
    // Off by default.
    let normal = TestApp::new(app.state.db.clone());
    assert_eq!(
        normal.get("/api/instance").await.json()["single_org"],
        false
    );
    let created = normal
        .as_user(&alice)
        .post("/api/orgs", &json!({ "name": "Second", "slug": "second" }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn with_mail_invitations_are_emailed_too(db: PgPool) {
    let acme = org(&db, "acme").await;
    let ada = user(&db, "Ada").await;
    member(&db, acme, ada, Role::Admin).await;
    let ada = session(&db, ada).await;
    let (app, outbox) = app_with(db, true, SignUpPolicy::Invite);
    let created = app
        .as_user(&ada)
        .post(
            "/api/orgs/acme/invitations",
            &json!({ "email": "bob@acme.io", "role": "reviewer" }),
        )
        .await
        .json();
    assert_eq!(created["emailed"], true);
    let mail = &outbox.to("bob@acme.io")[0];
    assert_eq!(mail.kind, "invitation");
    assert_eq!(mail.subject, "Your invitation to romulus");
    assert!(mail.text.contains("Ada invited you to join acme"));
    assert!(mail.text.contains(created["url"].as_str().unwrap()));
    assert!(mail.text.contains("as a reviewer"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_sign_in_log_is_append_only(db: PgPool) {
    let (app, _) = app_with(db.clone(), false, SignUpPolicy::Invite);
    sign_in(&app, "nobody@acme.io", PASSWORD).await;
    let update = sqlx::query!("UPDATE auth_events SET email = 'x'")
        .execute(&db)
        .await;
    assert!(update.is_err());
    let delete = sqlx::query!("DELETE FROM auth_events").execute(&db).await;
    assert!(delete.is_err());
    let truncate = sqlx::query!("TRUNCATE auth_events").execute(&db).await;
    assert!(truncate.is_err());
    let listed = admin::auth_events(&db, Some("NOBODY@acme.io"), 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].email.as_deref(), Some("nobody@acme.io"));
    // Old enough, it goes.
    sqlx::query!("ALTER TABLE auth_events DISABLE TRIGGER auth_events_are_append_only")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query!("UPDATE auth_events SET created_at = now() - interval '91 days'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query!("ALTER TABLE auth_events ENABLE TRIGGER auth_events_are_append_only")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(password::events::sweep(&db).await.unwrap(), 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_address_has_at_most_one_password_account(db: PgPool) {
    account(&db, "alice@acme.io", "Alice").await;
    let mut conn = db.acquire().await.unwrap();
    let again = password::create_account(&mut conn, "ALICE@acme.io", "Alice 2").await;
    assert!(again.is_err());
    // A single sign-on account with the same address is another person.
    user(&db, "alice").await;
    let _ = Method::GET;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn verify_email_vouches_for_password_accounts_only(db: PgPool) {
    let password_account = account(&db, "alice@acme.io", "Alice").await;
    let sso = sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name)
         VALUES ('sub-alice', 'alice@acme.io', false, 'Alice') RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE users SET email_verified = false WHERE id = $1",
        password_account
    )
    .execute(&db)
    .await
    .unwrap();
    let verified = admin::verify_email(&db, "ALICE@acme.io").await.unwrap();
    assert_eq!((verified.accounts, verified.skipped_sso), (1, 1));
    let states = sqlx::query!(
        "SELECT id, email_verified FROM users WHERE id = ANY($1)",
        &[password_account, sso]
    )
    .fetch_all(&db)
    .await
    .unwrap();
    for row in states {
        assert_eq!(row.email_verified, row.id == password_account);
    }
}
