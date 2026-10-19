//! One-time links (`auth_links`): finishing a sign-up, setting or resetting
//! a password. The token is 32 random bytes ([`crate::token`]); the table
//! holds its sha256. The link puts it in the URL's fragment
//! (`/app/signin/reset#token=…`), which browsers never send to a server nor
//! in a `Referer`: the page reads it and posts it.
//!
//! A link works once ([`consume`] marks it used in the same statement that
//! reads it) and expires: [`RESET_TTL`] when emailed, [`ADMIN_TTL`] when an
//! admin printed it, [`SETUP_TTL`] for the first admin, [`SIGN_UP_TTL`] to
//! finish a sign-up. A new reset link, or a password set any way, retires
//! the account's other reset links.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor};
use url::Url;
use uuid::Uuid;

use crate::{domain::LinkPurpose, token};

/// A reset link sent by email.
pub const RESET_TTL: Duration = Duration::from_mins(30);
/// A reset link an admin printed (`romulus-api password-link`) and hands
/// over themselves.
pub const ADMIN_TTL: Duration = Duration::from_hours(1);
/// The first admin's link (`romulus-api setup`): the install may finish
/// before they open it.
pub const SETUP_TTL: Duration = Duration::from_hours(24);
/// A sign-up link.
pub const SIGN_UP_TTL: Duration = Duration::from_hours(24);

/// A link to make.
pub struct NewLink<'a> {
    pub purpose: LinkPurpose,
    pub user_id: Option<Uuid>,
    pub email: &'a str,
    pub name: Option<&'a str>,
    pub invitation_id: Option<Uuid>,
    pub ttl: Duration,
}

/// Stores `link` and answers its token. A reset link retires the account's
/// earlier ones (only the newest works).
pub async fn create(db: &mut PgConnection, link: NewLink<'_>) -> Result<String, sqlx::Error> {
    if let (LinkPurpose::ResetPassword, Some(user_id)) = (link.purpose, link.user_id) {
        retire(&mut *db, user_id).await?;
    }
    let token = token::generate();
    sqlx::query!(
        "INSERT INTO auth_links (token_hash, purpose, user_id, email, name, invitation_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))",
        token::hash(&token),
        link.purpose as LinkPurpose,
        link.user_id,
        link.email,
        link.name,
        link.invitation_id,
        link.ttl.as_secs_f64(),
    )
    .execute(&mut *db)
    .await?;
    Ok(token)
}

/// Retires every unused reset link of `user_id` (a password was set).
pub async fn retire(db: impl PgExecutor<'_>, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE auth_links SET used_at = now()
         WHERE user_id = $1 AND purpose = 'reset_password' AND used_at IS NULL",
        user_id,
    )
    .execute(db)
    .await
    .map(drop)
}

/// A live link, as [`find`] and [`consume`] read it.
#[derive(Debug, Clone)]
pub struct Link {
    pub purpose: LinkPurpose,
    pub user_id: Option<Uuid>,
    pub email: String,
    pub name: Option<String>,
    pub invitation_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
    /// The account has no password yet: a reset link sets the first one.
    pub first_password: bool,
}

/// The live (unused, unexpired) link `token` names, without using it.
pub async fn find(
    db: impl PgExecutor<'_>,
    token: &str,
    purpose: LinkPurpose,
) -> Result<Option<Link>, sqlx::Error> {
    if !token::is_well_formed(token) {
        return Ok(None);
    }
    sqlx::query_as!(
        Link,
        r#"SELECT purpose AS "purpose: LinkPurpose", user_id, email, name, invitation_id, expires_at,
                  NOT EXISTS (SELECT 1 FROM password_credentials c WHERE c.user_id = l.user_id)
                    AS "first_password!"
           FROM auth_links l
           WHERE token_hash = $1 AND purpose = $2 AND used_at IS NULL AND expires_at > now()"#,
        token::hash(token),
        purpose as LinkPurpose,
    )
    .fetch_optional(db)
    .await
}

/// Uses the link `token` names: marks it used and answers it, or `None`
/// when it is unknown, used or expired. Two requests racing with one link:
/// one gets it.
pub async fn consume(
    db: impl PgExecutor<'_>,
    token: &str,
    purpose: LinkPurpose,
) -> Result<Option<Link>, sqlx::Error> {
    if !token::is_well_formed(token) {
        return Ok(None);
    }
    sqlx::query_as!(
        Link,
        r#"UPDATE auth_links l SET used_at = now()
           WHERE token_hash = $1 AND purpose = $2 AND used_at IS NULL AND expires_at > now()
           RETURNING purpose AS "purpose: LinkPurpose", user_id, email, name, invitation_id, expires_at,
                     NOT EXISTS (SELECT 1 FROM password_credentials c WHERE c.user_id = l.user_id)
                       AS "first_password!""#,
        token::hash(token),
        purpose as LinkPurpose,
    )
    .fetch_optional(db)
    .await
}

/// Links that expired more than a day ago: the sweeper's.
pub async fn sweep(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query!("DELETE FROM auth_links WHERE expires_at < now() - interval '1 day'")
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// The page a link opens: `/app/signin/<page>#token=…` on the public URL.
pub fn url(public_url: &Url, page: &str, token: &str) -> String {
    let mut url = public_url
        .join(&format!("/app/signin/{page}"))
        .expect("a path joins any base URL");
    url.set_fragment(Some(&format!("token={token}")));
    url.into()
}

/// Where a reset link opens.
pub fn reset_url(public_url: &Url, token: &str) -> String {
    url(public_url, "reset", token)
}

/// Where a sign-up link opens.
pub fn sign_up_url(public_url: &Url, token: &str) -> String {
    url(public_url, "welcome", token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_rides_in_the_fragment() {
        let base: Url = "https://romulus.example/".parse().unwrap();
        assert_eq!(
            reset_url(&base, "abc"),
            "https://romulus.example/app/signin/reset#token=abc"
        );
        assert_eq!(
            sign_up_url(&base, "abc"),
            "https://romulus.example/app/signin/welcome#token=abc"
        );
    }
}
