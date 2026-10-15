//! Signing in through the OIDC code flow, against the fake IdP in `idp.rs`.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use sqlx::PgPool;

use crate::{
    harness::{ORIGIN, Session, TestApp, TestResponse},
    idp::{Claims, FakeIdp, Tamper},
};

const SESSION: &str = "romulus_session";
const FLOW: &str = "romulus_session_signin";

struct Login {
    authorize: String,
    flow_cookie: String,
}

async fn login(app: &TestApp, query: &str) -> Login {
    let response = app.get(&format!("/api/auth/login{query}")).await;
    assert_eq!(
        response.status,
        StatusCode::SEE_OTHER,
        "{:?}",
        response.body
    );
    let flow = response.set_cookie(FLOW).expect("the flow cookie");
    assert!(
        flow.contains("HttpOnly") && flow.contains("SameSite=Lax"),
        "{flow}"
    );
    Login {
        authorize: response.location().to_owned(),
        flow_cookie: response.cookie_value(FLOW).unwrap(),
    }
}

async fn callback(app: &TestApp, query: &str, flow_cookie: Option<&str>) -> TestResponse {
    let mut request = Request::get(format!("/api/auth/callback?{query}"));
    if let Some(value) = flow_cookie {
        request = request.header(header::COOKIE, format!("{FLOW}={value}"));
    }
    app.send(request.body(Body::empty()).unwrap()).await
}

fn state_of(url: &url::Url) -> String {
    url.query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("a state")
}

/// The whole round trip: login, the IdP signs `claims` in, callback.
async fn sign_in(app: &TestApp, idp: &FakeIdp, claims: Claims, next: &str) -> TestResponse {
    let login = login(app, &format!("?next={next}")).await;
    let (code, url) = idp.authorize(&login.authorize, claims);
    callback(
        app,
        &format!("code={code}&state={}", state_of(&url)),
        Some(&login.flow_cookie),
    )
    .await
}

fn session_of(response: &TestResponse) -> Session {
    Session {
        user: uuid::Uuid::nil(),
        token: response.cookie_value(SESSION).expect("a session cookie"),
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn login_redirects_to_the_idp_with_pkce_state_and_nonce(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "").await;
    let url: url::Url = login.authorize.parse().unwrap();
    assert!(
        login
            .authorize
            .starts_with(&format!("{}authorize?", idp.issuer))
    );
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(param("response_type").as_deref(), Some("code"));
    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(
        param("redirect_uri").as_deref(),
        Some(format!("{ORIGIN}/api/auth/callback").as_str())
    );
    let scope = param("scope").unwrap();
    assert!(
        scope.contains("openid") && scope.contains("email"),
        "{scope}"
    );
    assert!(
        !scope.contains("offline_access"),
        "no refresh token wanted: {scope}"
    );
    assert!(param("nonce").is_some());
    // The browser holds the state (and where to land), the IdP hands it back.
    let (cookie_state, _) = login.flow_cookie.split_once('.').expect("state.next");
    assert_eq!(param("state").as_deref(), Some(cookie_state));
    assert_eq!(param("prompt"), None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn switching_account_asks_the_idp_to_pick_one(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "?prompt=select_account").await;
    assert!(
        login.authorize.contains("prompt=select_account"),
        "{}",
        login.authorize
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_verified_user_signs_in_and_lands_on_next(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    let claims = Claims::verified("zitadel-1", "alice@romulus.test", "Alice Martin");

    let response = sign_in(&app, &idp, claims, "/app/acme/settings").await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.location(), "/app/acme/settings");
    assert!(response.set_cookie(FLOW).unwrap().contains("Max-Age=0"));

    let me = app
        .as_user(&session_of(&response))
        .get("/api/me")
        .await
        .json();
    assert_eq!(me["email"], "alice@romulus.test");
    assert_eq!(me["name"], "Alice Martin");
    let refresh = sqlx::query_scalar!("SELECT oidc_refresh_token FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(refresh, None, "no IdP token is kept");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn signing_in_again_updates_the_user_from_the_idp(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    sign_in(
        &app,
        &idp,
        Claims::verified("zitadel-1", "alice@old.test", "Alice"),
        "/app/",
    )
    .await;
    let response = sign_in(
        &app,
        &idp,
        Claims::verified("zitadel-1", "alice@new.test", "Alice Martin"),
        "/app/",
    )
    .await;

    let me = app
        .as_user(&session_of(&response))
        .get("/api/me")
        .await
        .json();
    assert_eq!(
        (me["email"].as_str(), me["name"].as_str()),
        (Some("alice@new.test"), Some("Alice Martin"))
    );
    let users = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1, "one subject, one user");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unverified_email_cannot_sign_in(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    let mut claims = Claims::verified("zitadel-2", "mallory@romulus.test", "Mallory");
    claims.email_verified = false;

    let response = sign_in(&app, &idp, claims, "/app/invite/abc").await;
    assert_eq!(
        response.location(),
        "/app/signin?error=email_unverified&next=%2Fapp%2Finvite%2Fabc"
    );
    assert_eq!(response.set_cookie(SESSION), None);
    let users = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_user_without_a_name_is_named_after_their_email(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let mut claims = Claims::verified("zitadel-3", "dana.white@romulus.test", "");
    claims.name = None;
    let response = sign_in(&app, &idp, claims, "/app/").await;
    let me = app
        .as_user(&session_of(&response))
        .get("/api/me")
        .await
        .json();
    assert_eq!(me["name"], "Dana White");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_callback_from_another_browser_is_refused(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "?next=/app/acme").await;
    let (code, url) = idp.authorize(&login.authorize, Claims::verified("s", "a@b.test", "A"));
    let state = state_of(&url);

    // The attacker's callback link, opened in the victim's browser: no flow
    // cookie, or another sign-in's.
    for cookie in [None, Some(romulus_api::token::generate())] {
        let response = callback(
            &app,
            &format!("code={code}&state={state}"),
            cookie.as_deref(),
        )
        .await;
        assert_eq!(response.location(), "/app/signin?error=sign_in_expired");
        assert_eq!(response.set_cookie(SESSION), None);
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_callback_works_once(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "").await;
    let (code, url) = idp.authorize(&login.authorize, Claims::verified("s", "a@b.test", "A"));
    let query = format!("code={code}&state={}", state_of(&url));
    let first = callback(&app, &query, Some(&login.flow_cookie)).await;
    assert_eq!(first.location(), "/app/");
    let replay = callback(&app, &query, Some(&login.flow_cookie)).await;
    assert_eq!(replay.location(), "/app/signin?error=sign_in_expired");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_sign_in_left_at_the_idp_too_long_expires(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    let login = login(&app, "").await;
    let (code, url) = idp.authorize(&login.authorize, Claims::verified("s", "a@b.test", "A"));
    sqlx::query!("UPDATE oidc_flows SET created_at = now() - interval '11 minutes'")
        .execute(&db)
        .await
        .unwrap();
    let response = callback(
        &app,
        &format!("code={code}&state={}", state_of(&url)),
        Some(&login.flow_cookie),
    )
    .await;
    assert_eq!(response.location(), "/app/signin?error=sign_in_expired");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_id_token_for_another_request_or_client_is_rejected(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    for tamper in [
        Tamper {
            wrong_nonce: true,
            ..Tamper::default()
        },
        Tamper {
            wrong_audience: true,
            ..Tamper::default()
        },
    ] {
        idp.tamper(tamper);
        let response = sign_in(&app, &idp, Claims::verified("s", "a@b.test", "A"), "/app/x").await;
        assert_eq!(
            response.location(),
            "/app/signin?error=sso_failed&next=%2Fapp%2Fx"
        );
        assert_eq!(response.set_cookie(SESSION), None);
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_idp_saying_no_is_explained(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "?next=/app/acme").await;
    let state = state_of(&login.authorize.parse().unwrap());
    let response = callback(
        &app,
        &format!("error=access_denied&state={state}"),
        Some(&login.flow_cookie),
    )
    .await;
    assert_eq!(
        response.location(),
        "/app/signin?error=access_denied&next=%2Fapp%2Facme"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_open_redirect_in_next_is_dropped(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let response = sign_in(
        &app,
        &idp,
        Claims::verified("s", "a@b.test", "A"),
        "%2F%2Fevil.test%2Fapp%2F",
    )
    .await;
    assert_eq!(response.location(), "/app/");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn logout_also_ends_the_idp_session(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let response = sign_in(&app, &idp, Claims::verified("s", "a@b.test", "A"), "/app/").await;
    let session = session_of(&response);

    let out = app.as_user(&session).post_empty("/api/auth/logout").await;
    let redirect: url::Url = out.json()["redirect_url"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(redirect.path(), "/logout");
    let param = |name: &str| {
        redirect
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(param("client_id").as_deref(), Some(crate::idp::CLIENT_ID));
    assert_eq!(
        param("post_logout_redirect_uri").as_deref(),
        Some(format!("{ORIGIN}/app/signin").as_str())
    );
    assert_eq!(
        app.as_user(&session).get("/api/me").await.code(),
        "session_expired"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn methods_report_sso_once_configured(db: PgPool) {
    let idp = FakeIdp::start().await;
    let response = TestApp::with_config(db, idp.config())
        .get("/api/auth/methods")
        .await;
    assert_eq!(response.json()["sso"], true);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_invitation_link_signing_in_never_reaches_the_database(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    let invitation = romulus_api::token::generate();
    let next = format!("/app/invite/{invitation}");
    let login = login(&app, &format!("?next={next}")).await;

    let flows = sqlx::query_scalar!(r#"SELECT to_jsonb(f)::text AS "row!" FROM oidc_flows f"#)
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(flows.len(), 1);
    assert!(!flows[0].contains(&invitation), "{}", flows[0]);

    let (code, url) = idp.authorize(&login.authorize, Claims::verified("s", "a@b.test", "A"));
    let response = callback(
        &app,
        &format!("code={code}&state={}", state_of(&url)),
        Some(&login.flow_cookie),
    )
    .await;
    assert_eq!(response.location(), next, "back to the invitation");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_tampered_landing_in_the_flow_cookie_is_checked_again(db: PgPool) {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let login = login(&app, "?next=/app/acme").await;
    let (state, _) = login.flow_cookie.split_once('.').unwrap();
    let forged = format!("{state}.{}", URL_SAFE_NO_PAD.encode("//evil.test/app/"));
    let (code, url) = idp.authorize(&login.authorize, Claims::verified("s", "a@b.test", "A"));
    let response = callback(
        &app,
        &format!("code={code}&state={}", state_of(&url)),
        Some(&forged),
    )
    .await;
    assert_eq!(response.location(), "/app/");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_client_cannot_start_sign_ins_without_end(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db.clone(), idp.config());
    for _ in 0..romulus_api::auth::limit::BURST {
        let response = app.get("/api/auth/login").await;
        assert!(response.location().starts_with(idp.issuer.as_str()));
    }
    let refused = app.get("/api/auth/login?next=/app/acme").await;
    assert_eq!(
        refused.location(),
        "/app/signin?error=too_many_sign_ins&next=%2Fapp%2Facme"
    );
    let flows = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM oidc_flows"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        flows,
        i64::from(romulus_api::auth::limit::BURST),
        "no row for the refused one"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_address_from_the_idp_is_stored_composed(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let claims = Claims::verified("zitadel-9", " E\u{301}lodie@romulus.test", "Élodie");
    let response = sign_in(&app, &idp, claims, "/app/").await;
    let me = app
        .as_user(&session_of(&response))
        .get("/api/me")
        .await
        .json();
    assert_eq!(me["email"], "Élodie@romulus.test");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_invitation_names_the_address_to_sign_in_with(db: PgPool) {
    let idp = FakeIdp::start().await;
    let app = TestApp::with_config(db, idp.config());
    let hint = |authorize: &str| {
        url::Url::parse(authorize)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "login_hint")
            .map(|(_, value)| value.into_owned())
    };
    let invited = login(&app, "?next=/app/invite/x&login_hint=bob%40romulus.test").await;
    assert_eq!(
        hint(&invited.authorize).as_deref(),
        Some("bob@romulus.test")
    );
    // Anything that is not an address is not passed on.
    for garbage in ["not-an-email", "a%20b%40c.d", "%40nope"] {
        let other = login(&app, &format!("?login_hint={garbage}")).await;
        assert_eq!(hint(&other.authorize), None, "{garbage}");
    }
}
