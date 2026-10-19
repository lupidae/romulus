//! What an operator runs from the API's binary (`romulus-api <command>`,
//! `docker compose exec api romulus-api <command>` in the self-host bundle):
//! the first admin, one-time password links, unlocking an address, the
//! sign-in log, a test email. Each writes its auth event (no client address:
//! it ran on the server).
//!
//! These commands print links that sign someone in: run them on the server,
//! and hand the link over only to its owner.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    Config,
    auth::password::{
        self,
        events::{self, Event},
        links::{self, NewLink},
        throttle,
    },
    domain::{AuthEventKind, LinkPurpose, Role},
    mail::{Letter, MailError, Mailer},
    orgs::slug,
};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("{0}")]
    Invalid(String),
    /// Exit 2: `setup` on an install another admin set up.
    #[error("{0}")]
    AlreadyOwned(String),
    #[error("database")]
    Database(#[from] sqlx::Error),
    #[error("sending the test message")]
    Mail(#[from] MailError),
}

/// `romulus-api setup`'s input.
pub struct Setup {
    pub email: String,
    pub name: String,
    pub org: String,
    pub slug: Option<String>,
}

/// What `setup` did.
pub enum SetupOutcome {
    /// The admin has no password yet: the link that sets it.
    Link(String),
    /// Done before, password set: nothing to print.
    AlreadySetUp { org: String },
}

/// The first admin and the organization (the install's one, in
/// single-organization mode). Idempotent: run again with the same address it
/// prints a fresh link until the admin has set a password, then nothing.
/// Refused when organizations exist that this address does not administer:
/// it never takes an install over. (So on a multi-organization server with
/// open sign-up, run it before opening sign-up: docs/self-host/
/// configuration.md, "Accounts".)
pub async fn setup(db: &PgPool, config: &Config, input: Setup) -> Result<SetupOutcome, AdminError> {
    let Checked {
        email,
        name,
        org_name,
        org_slug,
    } = checked(config, input)?;

    let mut tx = crate::db::begin_read_committed(db).await?;
    // One setup at a time.
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtext('romulus.setup'))")
        .execute(&mut *tx)
        .await?;
    let account = password::find_account(&mut *tx, &email).await?;
    let administered = match &account {
        Some(account) => {
            sqlx::query_scalar!(
                r#"SELECT o.slug FROM memberships m JOIN organizations o ON o.id = m.org_id
               WHERE m.user_id = $1 AND m.role = 'admin' ORDER BY o.created_at LIMIT 1"#,
                account.id,
            )
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    let others = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM organizations"#)
        .fetch_one(&mut *tx)
        .await?;
    if administered.is_none() && others > 0 {
        return Err(AdminError::AlreadyOwned(
            "this install is set up already, by another admin: ask them for an invitation, \
             or print a password link for an existing account (romulus-api password-link EMAIL)"
                .to_owned(),
        ));
    }

    let user_id = if let Some(account) = &account {
        account.id
    } else {
        let id = password::create_account(&mut tx, &email, &name).await?;
        events::record(
            &mut *tx,
            Event::new(AuthEventKind::AdminCreated)
                .user(Some(id))
                .email(&email)
                .details(json!({ "by": "setup" })),
        )
        .await?;
        id
    };
    let org = if let Some(slug) = administered {
        slug
    } else {
        let org_id = sqlx::query_scalar!(
            "INSERT INTO organizations (name, slug) VALUES ($1, $2) RETURNING id",
            org_name,
            org_slug,
        )
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)",
            org_id,
            user_id,
            Role::Admin as Role,
        )
        .execute(&mut *tx)
        .await?;
        tracing::info!(org = %org_slug, "setup: organization created");
        org_slug
    };

    if account
        .as_ref()
        .is_some_and(|account| account.hash.is_some())
    {
        tx.commit().await?;
        return Ok(SetupOutcome::AlreadySetUp { org });
    }
    let link = new_link(&mut tx, config, user_id, &email, links::SETUP_TTL, "setup").await?;
    tx.commit().await?;
    Ok(SetupOutcome::Link(link))
}

struct Checked {
    email: String,
    name: String,
    org_name: String,
    org_slug: String,
}

/// `setup`'s input, checked as the API would.
fn checked(config: &Config, input: Setup) -> Result<Checked, AdminError> {
    if !config.auth.password {
        return Err(AdminError::Invalid(
            "setup makes a password account: set ROMULUS_AUTH=password first".to_owned(),
        ));
    }
    let email = crate::email::normalize(&input.email);
    if !email.contains('@') || email.len() > 254 || email.chars().any(char::is_whitespace) {
        return Err(AdminError::Invalid(format!(
            "{email:?} is not an email address"
        )));
    }
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AdminError::Invalid(
            "--name: 1 to 100 characters".to_owned(),
        ));
    }
    let org_name = input.org.trim().to_owned();
    if org_name.is_empty() || org_name.chars().count() > 100 {
        return Err(AdminError::Invalid("--org: 1 to 100 characters".to_owned()));
    }
    let org_slug = input.slug.unwrap_or_else(|| slugify(&org_name));
    if let Some(problem) = slug::problem(&org_slug) {
        return Err(AdminError::Invalid(format!(
            "--slug {org_slug:?}: {problem}"
        )));
    }
    Ok(Checked {
        email,
        name,
        org_name,
        org_slug,
    })
}

/// A one-time link setting a new password for the account with `email`
/// (an admin hands it over: no mail, or a locked-out person).
pub async fn password_link(
    db: &PgPool,
    config: &Config,
    email: &str,
) -> Result<String, AdminError> {
    let email = crate::email::normalize(email);
    let account = password::find_account(db, &email).await?.ok_or_else(|| {
        AdminError::Invalid(format!("no password account has the address {email}"))
    })?;
    let mut tx = db.begin().await?;
    let link = new_link(
        &mut tx,
        config,
        account.id,
        &account.email,
        links::ADMIN_TTL,
        "password-link",
    )
    .await?;
    tx.commit().await?;
    Ok(link)
}

async fn new_link(
    tx: &mut sqlx::PgConnection,
    config: &Config,
    user_id: Uuid,
    email: &str,
    ttl: std::time::Duration,
    by: &str,
) -> Result<String, sqlx::Error> {
    let token = links::create(
        &mut *tx,
        NewLink {
            purpose: LinkPurpose::ResetPassword,
            user_id: Some(user_id),
            email,
            name: None,
            invitation_id: None,
            ttl,
        },
    )
    .await?;
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::PasswordLinkCreated)
            .user(Some(user_id))
            .email(email)
            .details(json!({ "by": by, "valid_for_secs": ttl.as_secs() })),
    )
    .await?;
    Ok(links::reset_url(&config.public_url, &token))
}

/// Clears `email`'s failed sign-ins and lock. Whether it had any.
pub async fn unlock(db: &PgPool, email: &str) -> Result<bool, AdminError> {
    let email = crate::email::normalize(email);
    let mut tx = db.begin().await?;
    let cleared = throttle::clear(&mut *tx, &email).await?;
    let user_id = password::find_account(&mut *tx, &email)
        .await?
        .map(|account| account.id);
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::Unlocked)
            .user(user_id)
            .email(&email)
            .details(json!({ "had_failures": cleared })),
    )
    .await?;
    tx.commit().await?;
    Ok(cleared)
}

/// What `verify-email` did.
pub struct Verified {
    /// Password accounts marked verified.
    pub accounts: u64,
    /// Single sign-on accounts with the address, left alone: their
    /// provider says whether the address is verified, at every sign-in.
    pub skipped_sso: u64,
}

/// Marks the password account with `email` verified. Single sign-on
/// accounts are never touched: an admin cannot vouch for an address their
/// identity provider does not.
pub async fn verify_email(db: &PgPool, email: &str) -> Result<Verified, AdminError> {
    let email = crate::email::normalize(email);
    let mut tx = db.begin().await?;
    let users = sqlx::query_scalar!(
        "UPDATE users SET email_verified = true
         WHERE oidc_subject LIKE 'password|%'
           AND lower(email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)
         RETURNING id",
        email,
    )
    .fetch_all(&mut *tx)
    .await?;
    for user_id in &users {
        events::record(
            &mut *tx,
            Event::new(AuthEventKind::EmailVerified)
                .user(Some(*user_id))
                .email(&email)
                .details(json!({ "by": "verify-email" })),
        )
        .await?;
    }
    let skipped_sso = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM users
           WHERE oidc_subject NOT LIKE 'password|%'
             AND lower(email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)"#,
        email,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Verified {
        accounts: users.len() as u64,
        skipped_sso: u64::try_from(skipped_sso).unwrap_or_default(),
    })
}

/// The sign-in log, newest first.
pub async fn auth_events(
    db: &PgPool,
    email: Option<&str>,
    limit: i64,
) -> Result<Vec<events::Listed>, AdminError> {
    let email = email.map(crate::email::normalize);
    Ok(events::list(db, email.as_deref(), limit.clamp(1, 10_000)).await?)
}

/// Sends a test message to `to` and waits for the server's answer.
pub async fn mail_test(config: &Config, to: &str) -> Result<(), AdminError> {
    let mailer = Mailer::from_config(config);
    let email = Letter {
        kind: "test",
        to,
        subject: "romulus can send email".to_owned(),
        paragraphs: vec![format!(
            "This is a test from the romulus server at {}. Invitations, sign-up links and password resets will arrive like this one.",
            config.public_origin()
        )],
        action: None,
        footer: "Sent by `romulus-api mail-test`.".to_owned(),
    }
    .email();
    Ok(mailer.send_now(&email).await?)
}

/// "Acme Labs, Inc." → "acme-labs-inc": a slug from a name (ASCII letters
/// and digits; anything else separates words).
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug
        .trim_end_matches('-')
        .chars()
        .take(slug::MAX_LEN)
        .collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() || slug::RESERVED.contains(&slug.as_str()) {
        format!("org-{slug}").trim_end_matches('-').to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_come_from_names() {
        assert_eq!(slugify("Acme Labs, Inc."), "acme-labs-inc");
        assert_eq!(slugify("  Été 2026  "), "t-2026");
        assert_eq!(slugify("***"), "org");
        assert_eq!(slugify("Settings"), "org-settings");
        assert!(slug::problem(&slugify(&"word ".repeat(30))).is_none());
    }
}
