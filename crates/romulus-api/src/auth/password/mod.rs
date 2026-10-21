//! Built-in email + password sign-in (`ROMULUS_AUTH=password`), for installs
//! without an identity provider. Same sessions, cookie and CSRF rules as
//! single sign-on: only how a person proves who they are differs.
//!
//! Endpoints (all `POST`, JSON, CSRF-checked like every mutation; 404 while
//! password sign-in is off):
//! - `/auth/password/sign-in` `{email, password}`: a session. Wrong address
//!   or password: 401 `invalid_credentials`, the same for both.
//! - `/auth/password/sign-up` `{email, name, password?, invitation?}`: with
//!   mail, sends a link that finishes the sign-up (202, whatever the address:
//!   one that has an account gets a "you already have one" message
//!   instead); without, an invitation's holder creates the account at once
//!   (201, signed in, joined).
//! - `/auth/password/sign-up/check` `{token}`, `/sign-up/finish` `{token,
//!   name, password}`: the page a sign-up link opens.
//! - `/auth/password/forgot` `{email}`: emails a reset link to an account
//!   with that address (202 whatever the address; 503 `mail_unavailable`
//!   without mail: admins print links with `romulus-api password-link`).
//! - `/auth/password/reset/check` `{token}`, `/reset` `{token, password}`:
//!   the page a reset (or the first admin's) link opens. Setting a password
//!   ends every session of the account and signs this browser in.
//! - `/auth/password/change` `{current_password, new_password}` [signed in,
//!   password account]: ends the account's other sessions.
//!
//! Who may create an account: `Config::signup` (`invite`: the holder of an
//! invitation link, with the invited address; `open`: anyone, mail required).
//! Accounts are only ever created by someone who proved the address (they
//! received the sign-up link) or holds an invitation to it when the server
//! sends no mail, so every password account's address is verified.
//!
//! Security (OWASP ASVS chapter 2): Argon2id ([`hash`]), the rules in
//! [`policy`], guesses limited per client and locked per address, except
//! from a browser that signed in to the account before (its device cookie:
//! [`throttle`]), answers that never say whether an address has an account
//! (sign-in, sign-up, forgotten password: same status, same body, same work),
//! one-time links ([`links`]), notices by email when a password changes or an
//! address locks ([`letters`]), and the security log ([`events`]).
//!
//! Tables: `password_credentials`, `sign_in_failures`, `auth_links`,
//! `auth_events`; password accounts are `users` rows whose `oidc_subject` is
//! `password|<uuid>`.

pub mod events;
pub mod hash;
pub mod letters;
pub mod links;
pub mod policy;
pub mod throttle;

use std::net::{IpAddr, SocketAddr};

use axum::{
    Extension,
    extract::{ConnectInfo, State},
    http::StatusCode,
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use self::{events::Event, links::NewLink};
use super::{CurrentUser, access_tokens};
use crate::{
    ApiError, ApiResult, AppState,
    config::SignUpPolicy,
    domain::{AuthEventKind, LinkPurpose},
    error::{Chain, ErrorBody},
    extract::Json,
    orgs::{Org, invitations},
    token,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(sign_in))
        .routes(routes!(sign_up))
        .routes(routes!(check_sign_up))
        .routes(routes!(finish_sign_up))
        .routes(routes!(forgot))
        .routes(routes!(check_reset))
        .routes(routes!(reset))
        .routes(routes!(change))
}

/// The client's address (the edge's or a trusted proxy's view of it, see
/// `crate::forwarded`), for limits and the log.
type Client = Option<Extension<ConnectInfo<SocketAddr>>>;

fn client_ip(client: Client) -> Option<IpAddr> {
    client.map(|Extension(ConnectInfo(addr))| addr.ip().to_canonical())
}

/// 404 unless password sign-in is on: the endpoints do not exist then.
fn enabled(state: &AppState) -> ApiResult<()> {
    if state.config.auth.password {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "Password sign-in is off on this server.",
        ))
    }
}

/// Whether an unauthenticated request may make romulus email `email` now:
/// a few per address, and a cap on all of them. Past either, the request is
/// answered as usual and nothing is sent.
fn may_email(state: &AppState, email: &str) -> bool {
    if state
        .passwords
        .mails
        .check(crate::email::key(email))
        .is_err()
    {
        tracing::info!("mail: this address got enough messages for now");
        return false;
    }
    if state.passwords.outbound.check(()).is_err() {
        tracing::warn!("mail: too many messages asked for, from everywhere; not sent");
        return false;
    }
    true
}

fn invalid(field: &str, message: &str) -> ApiError {
    ApiError::Validation {
        message: message.to_owned(),
        details: Some(json!({ field: message })),
    }
}

/// A plausible address (something@something, no spaces, at most 254 bytes),
/// in its stored form.
fn checked_email(email: &str) -> ApiResult<String> {
    let email = crate::email::normalize(email);
    let plausible = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
    if plausible {
        Ok(email)
    } else {
        Err(invalid(
            "email",
            "Enter an email address, like name@company.com.",
        ))
    }
}

fn checked_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("name", "Enter your name, as your team knows you."));
    }
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(invalid(
            "name",
            "Keep your name to 100 characters or fewer.",
        ));
    }
    Ok(name.to_owned())
}

/// The new password, or the 422 saying why not (on `field`).
fn checked_password(field: &str, password: &str, email: &str, name: &str) -> ApiResult<()> {
    match policy::problem(password, email, name) {
        Some(problem) => Err(invalid(field, problem)),
        None => Ok(()),
    }
}

fn internal(err: hash::HashError) -> ApiError {
    match err {
        hash::HashError::Busy => ApiError::Down {
            code: "busy",
            message: "Too many sign-ins at once. Try again in a moment.".to_owned(),
        },
        err @ hash::HashError::Failed(_) => ApiError::internal(err),
    }
}

/// The device cookie (`throttle`'s device cookies): a random token, its
/// sha256 in `sign_in_devices`, sent to the password endpoints only.
fn device_cookie_name(config: &crate::Config) -> String {
    format!("{}_device", config.session_cookie)
}

fn device_cookie(
    config: &crate::Config,
    token: String,
) -> axum_extra::extract::cookie::Cookie<'static> {
    axum_extra::extract::cookie::Cookie::build((device_cookie_name(config), token))
        .http_only(true)
        .secure(config.public_url.scheme() == "https")
        .same_site(axum_extra::extract::cookie::SameSite::Strict)
        .path("/api/auth/password")
        .max_age(super::session::cookie_duration(throttle::DEVICE_TTL))
        .build()
}

/// The hash of this browser's device cookie, if it has a well-formed one.
fn device_hash(config: &crate::Config, jar: &CookieJar) -> Option<Vec<u8>> {
    jar.get(&device_cookie_name(config))
        .map(axum_extra::extract::cookie::Cookie::value)
        .filter(|value| token::is_well_formed(value))
        .map(token::hash)
}

/// After a password sign-in (or a password set): this browser becomes, or
/// stays, a known device of the account.
async fn remember_device(state: &AppState, jar: CookieJar, user_id: Uuid) -> ApiResult<CookieJar> {
    if let Some(hash) = device_hash(&state.config, &jar)
        && throttle::known_device(&state.db, user_id, &hash).await?
    {
        return Ok(jar);
    }
    let token = token::generate();
    throttle::remember_device(&state.db, user_id, &token::hash(&token)).await?;
    Ok(jar.add(device_cookie(&state.config, token)))
}

/// What an attempt counted against.
enum Counter {
    /// This browser, a known device of the address's account.
    Device(Vec<u8>),
    /// The address.
    Address,
}

/// Counts an attempt with `email` before its password is checked: against
/// this browser when it is one of the account's devices, else against the
/// address. `Err` holds the answer to a locked attempt (refused unchecked).
async fn count_attempt(
    state: &AppState,
    jar: &CookieJar,
    email: &str,
) -> ApiResult<Result<(Counter, throttle::Attempt), ApiError>> {
    // A known device first: guesses from elsewhere never lock it out.
    if let Some(hash) = device_hash(&state.config, jar) {
        match throttle::device_attempt(&state.db, &hash, email).await? {
            throttle::DeviceAttempt::Counted(attempt) => {
                return Ok(Ok((Counter::Device(hash), attempt)));
            }
            throttle::DeviceAttempt::Locked(until) => {
                return Ok(Err(ApiError::RateLimited {
                    retry_after: throttle::remaining(until),
                }));
            }
            throttle::DeviceAttempt::Unknown => {}
        }
    }
    if let Some(attempt) = throttle::attempt(&state.db, email).await? {
        return Ok(Ok((Counter::Address, attempt)));
    }
    let until = throttle::locked_until(&state.db, email).await?;
    Ok(Err(ApiError::RateLimited {
        retry_after: until.map_or(throttle::LOCK_FOR, throttle::remaining),
    }))
}

/// A series ends: the password was right.
async fn end_series(state: &AppState, counter: &Counter, email: &str) -> ApiResult<()> {
    match counter {
        Counter::Device(hash) => throttle::device_clear(&state.db, hash).await?,
        Counter::Address => {
            throttle::clear(&state.db, email).await?;
        }
    }
    Ok(())
}

/// A password account, by address.
pub struct Account {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    /// `None`: no password set yet (the first admin before their link).
    pub hash: Option<String>,
}

pub async fn find_account(
    db: impl sqlx::PgExecutor<'_>,
    email: &str,
) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as!(
        Account,
        r#"SELECT u.id, u.email, u.name,
                  (SELECT c.hash FROM password_credentials c WHERE c.user_id = u.id) AS "hash?"
           FROM users u
           WHERE u.oidc_subject LIKE 'password|%'
             AND lower(u.email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)"#,
        email,
    )
    .fetch_optional(db)
    .await
}

/// A new password account (address verified: whoever creates one proved
/// it, or an admin vouches for it).
pub async fn create_account(
    db: &mut PgConnection,
    email: &str,
    name: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name)
         VALUES ('password|' || gen_random_uuid(), $1, true, $2)
         RETURNING id",
        email,
        name,
    )
    .fetch_one(db)
    .await
}

/// Stores `phc` as `user_id`'s password and retires their reset links.
pub async fn store_password(
    db: &mut PgConnection,
    user_id: Uuid,
    phc: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO password_credentials (user_id, hash) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET hash = EXCLUDED.hash, changed_at = now()",
        user_id,
        phc,
    )
    .execute(&mut *db)
    .await?;
    links::retire(&mut *db, user_id).await
}

/// Ends `user_id`'s sessions but `keep` (a session's `id_hash`); answers the
/// ended ones, for [`crate::auth::events::SessionEvents`] once committed.
async fn end_sessions(
    db: &mut PgConnection,
    user_id: Uuid,
    keep: Option<&[u8]>,
) -> Result<Vec<Vec<u8>>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM sessions WHERE user_id = $1 AND id_hash IS DISTINCT FROM $2 RETURNING id_hash",
        user_id,
        keep,
    )
    .fetch_all(db)
    .await
}

/// Forgets `user_id`'s devices but `keep` (a device cookie's hash): after a
/// password change, only this browser skips the address's lock.
async fn forget_devices(
    db: &mut PgConnection,
    user_id: Uuid,
    keep: Option<&[u8]>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM sign_in_devices WHERE user_id = $1 AND token_hash IS DISTINCT FROM $2",
        user_id,
        keep,
    )
    .execute(db)
    .await
    .map(drop)
}

fn duplicate_account(err: sqlx::Error) -> ApiError {
    if crate::db::constraint(&err) == Some("users_password_email_key") {
        ApiError::Conflict {
            code: "account_exists",
            message: "You already have an account with this address. Sign in instead.".to_owned(),
        }
    } else {
        err.into()
    }
}

// ---- Sign in ------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct PasswordSignIn {
    pub email: String,
    pub password: String,
}

/// Signs in with an email address and a password: sets the session cookie.
#[utoipa::path(
    post,
    path = "/auth/password/sign-in",
    operation_id = "password_sign_in",
    tag = "auth",
    request_body = PasswordSignIn,
    responses(
        (status = 204, description = "Signed in: the session cookie is set"),
        (status = 401, description = "`invalid_credentials`: the same for an unknown address and a wrong password", body = ErrorBody),
        (status = 404, description = "Password sign-in is off", body = ErrorBody),
        (status = 429, description = "`rate_limited`: too many attempts from this client, or the address is locked (`details.retry_after`, seconds)", body = ErrorBody),
    )
)]
async fn sign_in(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<PasswordSignIn>,
) -> ApiResult<(StatusCode, CookieJar)> {
    enabled(&state)?;
    let ip = client_ip(client);
    state.passwords.sign_in.check(throttle::client_key(ip))?;
    // Anything unlike an address cannot match an account: the same answer.
    // (Measured lowercased: lowering can lengthen an address.)
    let email = crate::email::normalize(&body.email);
    if email.is_empty()
        || crate::email::key(&email).chars().count() > 254
        || body.password.chars().count() > 1024
    {
        return Err(ApiError::InvalidCredentials);
    }

    // Counted before the password is checked: concurrent guesses cannot
    // outrun the lock (`throttle::attempt`), nor lock out a browser that
    // signed in before (`throttle::device_attempt`).
    let (counter, attempt) = match count_attempt(&state, &jar, &email).await? {
        Ok(counted) => counted,
        Err(locked) => return Err(locked),
    };

    let account = find_account(&state.db, &email).await?;
    let phc = account.as_ref().and_then(|account| account.hash.clone());
    let verified = hash::verify(&body.password, phc.clone())
        .await
        .map_err(internal)?;
    let Some(account) = account.filter(|_| verified) else {
        return Err(failed(&state, &email, ip, &counter, &attempt).await?);
    };

    end_series(&state, &counter, &email).await?;
    if phc.as_deref().is_some_and(hash::needs_rehash) {
        let phc = hash::hash(&body.password).await.map_err(internal)?;
        sqlx::query!(
            "UPDATE password_credentials SET hash = $2 WHERE user_id = $1",
            account.id,
            phc
        )
        .execute(&state.db)
        .await?;
    }
    events::record(
        &state.db,
        Event::new(AuthEventKind::SignedIn)
            .user(Some(account.id))
            .email(&account.email)
            .ip(ip),
    )
    .await?;
    let jar = super::start_session(&state, jar, account.id).await?;
    let jar = remember_device(&state, jar, account.id).await?;
    Ok((StatusCode::NO_CONTENT, jar))
}

/// A failed attempt (already counted, `attempt` of its series): logged; at
/// the limit the lock it set is answered and, for an address, announced to
/// the account's owner by email (once per series, at its stored address).
async fn failed(
    state: &AppState,
    email: &str,
    ip: Option<IpAddr>,
    counter: &Counter,
    attempt: &throttle::Attempt,
) -> ApiResult<ApiError> {
    let account = find_account(&state.db, email).await?;
    let device = matches!(counter, Counter::Device(_));
    events::record(
        &state.db,
        Event::new(AuthEventKind::SignInFailed)
            .user(account.as_ref().map(|account| account.id))
            .email(email)
            .ip(ip)
            .details(json!({ "failures": attempt.number, "device": device })),
    )
    .await?;
    if attempt.number < throttle::LOCK_AFTER {
        return Ok(ApiError::InvalidCredentials);
    }
    // One event per lock: the refused attempts while it lasts are not written.
    events::record(
        &state.db,
        Event::new(AuthEventKind::SignInLocked)
            .user(account.as_ref().map(|account| account.id))
            .email(email)
            .ip(ip)
            .details(
                json!({ "failures": attempt.number, "locks": attempt.locks, "device": device }),
            ),
    )
    .await?;
    if attempt.locks >= REPEATED_LOCKS {
        // Someone keeps at this address: worth an operator's look
        // (`romulus-api auth-events --email …` says who and from where).
        tracing::warn!(
            locks = attempt.locks,
            account = account.is_some(),
            "password sign-in: an address locked again and again today"
        );
    } else {
        tracing::info!(device, "password sign-in: locked");
    }
    // Its owner hears of it at the first lock of the day, at their address.
    if let Some(account) = account.filter(|_| !device && attempt.locks == 1) {
        state
            .mailer
            .send(letters::locked(&account.email, throttle::lock_duration(1)));
    }
    Ok(ApiError::RateLimited {
        retry_after: attempt
            .locked_until
            .map_or(throttle::LOCK_FOR, throttle::remaining),
    })
}

/// Locks in a day past which an address is logged as targeted.
const REPEATED_LOCKS: i32 = 3;

// ---- Sign up ------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct PasswordSignUp {
    pub email: String,
    /// How the team will see them: 1 to 100 characters.
    pub name: String,
    /// Only when the server sends no email (`mail` false in
    /// `GET /api/instance`): the password, set now. Otherwise ignored: it is
    /// chosen on the page the emailed link opens.
    pub password: Option<String>,
    /// The token of the invitation link this sign-up comes from. Required
    /// unless sign-up is open; its address must be `email`.
    pub invitation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignUpOutcome {
    /// A link went to the address (or, when it has an account, a message
    /// saying so): the sign-up finishes from it.
    EmailSent,
    /// The account exists and this browser is signed in.
    SignedIn,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SignedUp {
    pub outcome: SignUpOutcome,
    /// The organization joined, when the sign-up came with an invitation
    /// and finished.
    pub org: Option<Org>,
}

/// Creates a password account: with mail, emails a link that finishes it;
/// without, creates it from an invitation at once and signs in.
#[utoipa::path(
    post,
    path = "/auth/password/sign-up",
    operation_id = "password_sign_up",
    tag = "auth",
    request_body = PasswordSignUp,
    responses(
        (status = 201, description = "Created and signed in (no mail: from an invitation)", body = SignedUp),
        (status = 202, description = "A link was sent; the same answer whether the address has an account or not", body = SignedUp),
        (status = 404, description = "Password sign-in is off; or the invitation link does not work", body = ErrorBody),
        (status = 409, description = "`account_exists` (no mail, from an invitation)", body = ErrorBody),
        (status = 422, description = "`validation` (`email`, `name`, `password`, `invitation`)", body = ErrorBody),
        (status = 429, body = ErrorBody),
    )
)]
async fn sign_up(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<PasswordSignUp>,
) -> ApiResult<(StatusCode, CookieJar, Json<SignedUp>)> {
    enabled(&state)?;
    let ip = client_ip(client);
    state.passwords.requests.check(throttle::client_key(ip))?;
    let name = checked_name(&body.name)?;
    let email = checked_email(&body.email)?;

    let invitation = match body.invitation.as_deref() {
        Some(link) => {
            let invitation = invitations::live(&state.db, link)
                .await?
                .ok_or_else(|| {
                    ApiError::not_found(
                        "This invitation link does not work any more. Ask an admin of the organization for a new one.",
                    )
                })?;
            if crate::email::key(&invitation.email) != crate::email::key(&email) {
                return Err(invalid(
                    "email",
                    &format!(
                        "This invitation is for {}: create the account with that address.",
                        invitation.email
                    ),
                ));
            }
            Some(invitation)
        }
        None if state.config.signup == SignUpPolicy::Open => None,
        None => {
            return Err(invalid(
                "invitation",
                "Accounts are created from an invitation on this server. Ask an admin for one.",
            ));
        }
    };

    if state.mailer.is_on() {
        sign_up_by_email(&state, ip, email, name, invitation).await?;
        // The same answer whatever happened.
        return Ok((
            StatusCode::ACCEPTED,
            jar,
            Json(SignedUp {
                outcome: SignUpOutcome::EmailSent,
                org: None,
            }),
        ));
    }

    // No mail: only an invitation's holder gets here (config refuses open
    // sign-up without mail), and the invitation vouches for the address.
    let (Some(invitation), Some(link)) = (invitation, body.invitation.as_deref()) else {
        return Err(invalid(
            "invitation",
            "Accounts are created from an invitation on this server. Ask an admin for one.",
        ));
    };
    let password = body.password.as_deref().unwrap_or_default();
    checked_password("password", password, &email, &name)?;
    let phc = hash::hash(password).await.map_err(internal)?;
    let mut tx = crate::db::begin_read_committed(&state.db).await?;
    let user_id = create_account(&mut tx, &email, &name)
        .await
        .map_err(duplicate_account)?;
    store_password(&mut tx, user_id, &phc).await?;
    let org = invitations::claim_for(
        &state.billing,
        &mut tx,
        invitation.id,
        Some(link),
        user_id,
        &email,
    )
        .await?
        .ok_or_else(|| {
            ApiError::not_found(
                "This invitation link does not work any more. Ask an admin of the organization for a new one.",
            )
        })?;
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::AccountCreated)
            .user(Some(user_id))
            .email(&email)
            .ip(ip)
            .details(json!({ "invitation": invitation.id, "org": org.slug })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(org = %org.slug, "password account created from an invitation");
    let jar = super::start_session(&state, jar, user_id).await?;
    let jar = remember_device(&state, jar, user_id).await?;
    Ok((
        StatusCode::CREATED,
        jar,
        Json(SignedUp {
            outcome: SignUpOutcome::SignedIn,
            org: Some(org),
        }),
    ))
}

/// Sign-up with mail: emails a link that finishes it, or, to an address
/// that has an account, a message saying so. Nothing of it shows in the
/// answer (the caller's is always the same).
async fn sign_up_by_email(
    state: &AppState,
    ip: Option<IpAddr>,
    email: String,
    name: String,
    invitation: Option<invitations::LiveInvitation>,
) -> ApiResult<()> {
    let existing = find_account(&state.db, &email).await?;
    events::record(
        &state.db,
        Event::new(AuthEventKind::SignUpRequested)
            .user(existing.as_ref().map(|account| account.id))
            .email(&email)
            .ip(ip)
            .details(json!({ "existing": existing.is_some() })),
    )
    .await?;
    if !may_email(state, &email) {
        return Ok(());
    }
    // After answering: whether a link was made (a write) or not must not
    // show in how long the answer took.
    let (state, invitation) = (state.clone(), invitation.map(|i| (i.id, i.org_name)));
    tokio::spawn(async move {
        let sent = async {
            if let Some(existing) = existing {
                state.mailer.send(letters::account_exists(
                    &existing.email,
                    &state.config.public_url,
                ));
                return Ok::<_, sqlx::Error>(());
            }
            let mut db = state.db.acquire().await?;
            let token = links::create(
                &mut db,
                NewLink {
                    purpose: LinkPurpose::SignUp,
                    user_id: None,
                    email: &email,
                    name: Some(&name),
                    invitation_id: invitation.as_ref().map(|(id, _)| *id),
                    ttl: links::SIGN_UP_TTL,
                },
            )
            .await?;
            state.mailer.send(letters::sign_up(
                &email,
                &links::sign_up_url(&state.config.public_url, &token),
                links::SIGN_UP_TTL,
                invitation.as_ref().map(|(_, org)| org.as_str()),
            ));
            Ok(())
        }
        .await;
        if let Err(err) = sent {
            tracing::warn!(error = %Chain(&err), "sign-up: the link was not made");
        }
    });
    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkToken {
    /// From the link's fragment (`#token=…`).
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SignUpLink {
    pub email: String,
    pub name: String,
    /// The organization the account joins once created.
    pub org: Option<String>,
}

fn link_gone() -> ApiError {
    ApiError::not_found(
        "This link does not work any more: each one works once, for a limited time. Ask for a new one.",
    )
}

/// What a sign-up link creates, without using it.
#[utoipa::path(
    post,
    path = "/auth/password/sign-up/check",
    operation_id = "check_sign_up_link",
    tag = "auth",
    request_body = LinkToken,
    responses(
        (status = 200, body = SignUpLink),
        (status = 404, description = "Unknown, used or expired link (or password sign-in is off)", body = ErrorBody),
        (status = 429, body = ErrorBody),
    )
)]
async fn check_sign_up(
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<LinkToken>,
) -> ApiResult<Json<SignUpLink>> {
    enabled(&state)?;
    state
        .passwords
        .links
        .check(throttle::client_key(client_ip(client)))?;
    let link = links::find(&state.db, &body.token, LinkPurpose::SignUp)
        .await?
        .ok_or_else(link_gone)?;
    let org = match link.invitation_id {
        Some(id) => invitations::org_name(&state.db, id).await?,
        None => None,
    };
    Ok(Json(SignUpLink {
        email: link.email,
        name: link.name.unwrap_or_default(),
        org,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FinishSignUp {
    pub token: String,
    pub name: String,
    pub password: String,
}

/// Finishes a sign-up from its link: creates the account (and joins the
/// invitation's organization), signs in.
#[utoipa::path(
    post,
    path = "/auth/password/sign-up/finish",
    operation_id = "finish_sign_up",
    tag = "auth",
    request_body = FinishSignUp,
    responses(
        (status = 200, description = "Created and signed in", body = SignedUp),
        (status = 404, description = "Unknown, used or expired link", body = ErrorBody),
        (status = 409, description = "`account_exists`: the address got an account meanwhile", body = ErrorBody),
        (status = 422, description = "`validation` (`name`, `password`)", body = ErrorBody),
        (status = 429, body = ErrorBody),
    )
)]
async fn finish_sign_up(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<FinishSignUp>,
) -> ApiResult<(CookieJar, Json<SignedUp>)> {
    enabled(&state)?;
    let ip = client_ip(client);
    state.passwords.links.check(throttle::client_key(ip))?;
    let name = checked_name(&body.name)?;
    let preview = links::find(&state.db, &body.token, LinkPurpose::SignUp)
        .await?
        .ok_or_else(link_gone)?;
    checked_password("password", &body.password, &preview.email, &name)?;
    let phc = hash::hash(&body.password).await.map_err(internal)?;

    let mut tx = crate::db::begin_read_committed(&state.db).await?;
    let link = links::consume(&mut *tx, &body.token, LinkPurpose::SignUp)
        .await?
        .ok_or_else(link_gone)?;
    let user_id = create_account(&mut tx, &link.email, &name)
        .await
        .map_err(duplicate_account)?;
    store_password(&mut tx, user_id, &phc).await?;
    // Joined when the invitation still stands; the account exists either way.
    let org = match link.invitation_id {
        Some(id) => {
            invitations::claim_for(&state.billing, &mut tx, id, None, user_id, &link.email).await?
        }
        None => None,
    };
    if org.is_none() && state.config.signup == SignUpPolicy::Invite {
        // Only an invitation let this account be made (every link here came
        // with one): revoked, used or expired since, so is the right.
        return Err(ApiError::not_found(
            "The invitation this link came from was revoked or expired. Ask an admin of the organization for a new one.",
        ));
    }
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::AccountCreated)
            .user(Some(user_id))
            .email(&link.email)
            .ip(ip)
            .details(json!({ "invitation": link.invitation_id, "org": org.as_ref().map(|org| &org.slug) })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!("password account created");
    let jar = super::start_session(&state, jar, user_id).await?;
    let jar = remember_device(&state, jar, user_id).await?;
    Ok((
        jar,
        Json(SignedUp {
            outcome: SignUpOutcome::SignedIn,
            org,
        }),
    ))
}

// ---- Forgotten passwords ---------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPassword {
    pub email: String,
}

/// Emails a reset link to the account with this address, if there is one.
/// Answers the same either way.
#[utoipa::path(
    post,
    path = "/auth/password/forgot",
    operation_id = "forgot_password",
    tag = "auth",
    request_body = ForgotPassword,
    responses(
        (status = 202, description = "If an account has this address, a link is on its way"),
        (status = 404, description = "Password sign-in is off", body = ErrorBody),
        (status = 422, body = ErrorBody),
        (status = 429, body = ErrorBody),
        (status = 503, description = "`mail_unavailable`: this server sends no email; an admin prints a link (`romulus-api password-link`)", body = ErrorBody),
    )
)]
async fn forgot(
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<ForgotPassword>,
) -> ApiResult<StatusCode> {
    enabled(&state)?;
    if !state.mailer.is_on() {
        return Err(ApiError::Down {
            code: "mail_unavailable",
            message: "This server sends no email. Ask whoever runs it for a reset link.".to_owned(),
        });
    }
    let ip = client_ip(client);
    state.passwords.requests.check(throttle::client_key(ip))?;
    let email = checked_email(&body.email)?;
    let account = find_account(&state.db, &email).await?;
    events::record(
        &state.db,
        Event::new(AuthEventKind::PasswordResetRequested)
            .user(account.as_ref().map(|account| account.id))
            .email(&email)
            .ip(ip)
            .details(json!({ "existing": account.is_some() })),
    )
    .await?;
    let Some(account) = account.filter(|account| may_email(&state, &account.email)) else {
        return Ok(StatusCode::ACCEPTED);
    };
    // After answering: making the link (writes) must not show in how long
    // the answer took.
    tokio::spawn(async move {
        let sent = async {
            let mut db = state.db.acquire().await?;
            let token = links::create(
                &mut db,
                NewLink {
                    purpose: LinkPurpose::ResetPassword,
                    user_id: Some(account.id),
                    email: &account.email,
                    name: None,
                    invitation_id: None,
                    ttl: links::RESET_TTL,
                },
            )
            .await?;
            state.mailer.send(letters::reset(
                &account.email,
                &links::reset_url(&state.config.public_url, &token),
                links::RESET_TTL,
            ));
            Ok::<_, sqlx::Error>(())
        }
        .await;
        if let Err(err) = sent {
            tracing::warn!(error = %Chain(&err), "reset: the link was not made");
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResetLink {
    pub email: String,
    /// The account has no password yet (the first admin's link): "set", not
    /// "reset".
    pub first_password: bool,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// What a reset link is for, without using it.
#[utoipa::path(
    post,
    path = "/auth/password/reset/check",
    operation_id = "check_reset_link",
    tag = "auth",
    request_body = LinkToken,
    responses(
        (status = 200, body = ResetLink),
        (status = 404, description = "Unknown, used or expired link (or password sign-in is off)", body = ErrorBody),
        (status = 429, body = ErrorBody),
    )
)]
async fn check_reset(
    State(state): State<AppState>,
    client: Client,
    Json(body): Json<LinkToken>,
) -> ApiResult<Json<ResetLink>> {
    enabled(&state)?;
    state
        .passwords
        .links
        .check(throttle::client_key(client_ip(client)))?;
    let link = links::find(&state.db, &body.token, LinkPurpose::ResetPassword)
        .await?
        .ok_or_else(link_gone)?;
    Ok(Json(ResetLink {
        email: link.email,
        first_password: link.first_password,
        expires_at: link.expires_at,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPassword {
    pub token: String,
    pub password: String,
}

/// Sets the password a reset link is for: ends every session of the
/// account, clears its address's lock, signs this browser in.
#[utoipa::path(
    post,
    path = "/auth/password/reset",
    operation_id = "reset_password",
    tag = "auth",
    request_body = ResetPassword,
    responses(
        (status = 204, description = "Set, and signed in"),
        (status = 404, description = "Unknown, used or expired link", body = ErrorBody),
        (status = 422, description = "`validation` (`password`)", body = ErrorBody),
        (status = 429, body = ErrorBody),
    )
)]
async fn reset(
    State(state): State<AppState>,
    client: Client,
    jar: CookieJar,
    Json(body): Json<ResetPassword>,
) -> ApiResult<(StatusCode, CookieJar)> {
    enabled(&state)?;
    let ip = client_ip(client);
    state.passwords.links.check(throttle::client_key(ip))?;
    let preview = links::find(&state.db, &body.token, LinkPurpose::ResetPassword)
        .await?
        .ok_or_else(link_gone)?;
    let user_id = preview.user_id.ok_or_else(link_gone)?;
    let name = sqlx::query_scalar!("SELECT name FROM users WHERE id = $1", user_id)
        .fetch_one(&state.db)
        .await?;
    checked_password("password", &body.password, &preview.email, &name)?;
    let phc = hash::hash(&body.password).await.map_err(internal)?;

    let mut tx = crate::db::begin_read_committed(&state.db).await?;
    let link = links::consume(&mut *tx, &body.token, LinkPurpose::ResetPassword)
        .await?
        .ok_or_else(link_gone)?;
    store_password(&mut tx, user_id, &phc).await?;
    // Receiving the link proved the address (or an admin vouched for it).
    sqlx::query!(
        "UPDATE users SET email_verified = true WHERE id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    let ended = end_sessions(&mut tx, user_id, None).await?;
    // Whoever had a device of this account (and maybe the old password)
    // starts from nothing too, and so do the agents its tokens let in.
    forget_devices(&mut tx, user_id, None).await?;
    let tokens = access_tokens::revoke_all(&mut tx, user_id).await?;
    let apps = crate::oauth::apps::revoke_all(&mut tx, user_id).await?;
    throttle::clear(&mut *tx, &link.email).await?;
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::PasswordReset)
            .user(Some(user_id))
            .email(&link.email)
            .ip(ip)
            .details(json!({
                "first": link.first_password,
                "sessions_ended": ended.len(),
                "access_tokens_revoked": tokens,
                "app_grants_revoked": apps,
            })),
    )
    .await?;
    tx.commit().await?;
    for id_hash in ended {
        state.sessions.ended(id_hash);
    }
    if !link.first_password {
        state.mailer.send(letters::password_changed(
            &link.email,
            &state.config.public_url,
            tokens,
        ));
    }
    let jar = super::start_session(&state, jar, user_id).await?;
    let jar = remember_device(&state, jar, user_id).await?;
    Ok((StatusCode::NO_CONTENT, jar))
}

// ---- Changing one's password ------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

/// Changes the signed-in person's password. Their other sessions end; this
/// one goes on under a new cookie.
#[utoipa::path(
    post,
    path = "/auth/password/change",
    operation_id = "change_password",
    tag = "auth",
    request_body = ChangePassword,
    responses(
        (status = 204, description = "Changed"),
        (status = 401, body = ErrorBody),
        (status = 403, description = "`not_a_password_account`: this account signs in with single sign-on", body = ErrorBody),
        (status = 422, description = "`validation` (`current_password`, `new_password`)", body = ErrorBody),
        (status = 429, description = "`rate_limited` (also when the address is locked)", body = ErrorBody),
    )
)]
async fn change(
    State(state): State<AppState>,
    user: CurrentUser,
    client: Client,
    jar: CookieJar,
    Json(body): Json<ChangePassword>,
) -> ApiResult<(StatusCode, CookieJar)> {
    enabled(&state)?;
    let ip = client_ip(client);
    state.passwords.change.check(user.id)?;
    let account = find_account(&state.db, &user.email)
        .await?
        .filter(|account| account.id == user.id)
        .ok_or(ApiError::Denied {
            code: "not_a_password_account",
            message: "This account signs in with single sign-on: change its password there."
                .to_owned(),
        })?;
    // A stolen session must not make guessing the password cheap: attempts
    // count toward the lock as sign-ins do (this browser's, or the address's).
    let (counter, attempt) = match count_attempt(&state, &jar, &account.email).await? {
        Ok(counted) => counted,
        Err(locked) => return Err(locked),
    };
    if !hash::verify(&body.current_password, account.hash.clone())
        .await
        .map_err(internal)?
    {
        if attempt.number >= throttle::LOCK_AFTER {
            return Err(failed(&state, &account.email, ip, &counter, &attempt).await?);
        }
        events::record(
            &state.db,
            Event::new(AuthEventKind::SignInFailed)
                .user(Some(user.id))
                .email(&account.email)
                .ip(ip)
                .details(json!({ "failures": attempt.number, "change": true })),
        )
        .await?;
        return Err(invalid(
            "current_password",
            "That is not your current password.",
        ));
    }
    // Right: the series ends here, whatever happens next.
    end_series(&state, &counter, &account.email).await?;
    checked_password(
        "new_password",
        &body.new_password,
        &account.email,
        &account.name,
    )?;
    if body.new_password == body.current_password {
        return Err(invalid(
            "new_password",
            "Choose a password other than your current one.",
        ));
    }
    let phc = hash::hash(&body.new_password).await.map_err(internal)?;

    let current = jar
        .get(&state.config.session_cookie)
        .map(|cookie| token::hash(cookie.value()));
    let mut tx = crate::db::begin_read_committed(&state.db).await?;
    store_password(&mut tx, user.id, &phc).await?;
    let mut ended = end_sessions(&mut tx, user.id, current.as_deref()).await?;
    let device = device_hash(&state.config, &jar);
    forget_devices(&mut tx, user.id, device.as_deref()).await?;
    // Signed out everywhere else: agents too.
    let tokens = access_tokens::revoke_all(&mut tx, user.id).await?;
    let apps = crate::oauth::apps::revoke_all(&mut tx, user.id).await?;
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::PasswordChanged)
            .user(Some(user.id))
            .email(&account.email)
            .ip(ip)
            .details(json!({
                "sessions_ended": ended.len(),
                "access_tokens_revoked": tokens,
                "app_grants_revoked": apps,
            })),
    )
    .await?;
    tx.commit().await?;
    // This browser keeps going, under a new session id.
    ended.extend(current);
    for id_hash in ended {
        state.sessions.ended(id_hash);
    }
    state.mailer.send(letters::password_changed(
        &account.email,
        &state.config.public_url,
        tokens,
    ));
    let jar = super::start_session(&state, jar, user.id).await?;
    let jar = remember_device(&state, jar, user.id).await?;
    Ok((StatusCode::NO_CONTENT, jar))
}

// ---- Housekeeping ---------------------------------------------------------------

/// How often [`sweeper`] runs.
pub const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_hours(1);

/// Forgets stale failures, expired links and events past their retention:
/// now, then every `every`.
pub fn sweeper(db: PgPool, every: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        loop {
            ticks.tick().await;
            let swept = async {
                let failures = throttle::sweep(&db).await?;
                let links = links::sweep(&db).await?;
                let events = events::sweep(&db).await?;
                Ok::<_, sqlx::Error>((failures, links, events))
            }
            .await;
            match swept {
                Ok((0, 0, 0)) => {}
                Ok((failures, links, events)) => {
                    tracing::info!(failures, links, events, "password sign-in: swept");
                }
                Err(err) => tracing::warn!(error = %Chain(&err), "password sign-in: sweep failed"),
            }
        }
    })
}
