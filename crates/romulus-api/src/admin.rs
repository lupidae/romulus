//! What an operator runs from the API's binary (`romulus-api <command>`,
//! `docker compose exec api romulus-api <command>` in the self-host bundle):
//! the first admin, one-time password links, unlocking an address, the
//! sign-in log, a test email, deleting an account or an organization on
//! request (`retention::deletion`). Each writes its auth event (no client
//! address: it ran on the server).
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
    retention::{
        deletion::{
            self, AccountDeleted, AccountPlan, By, Deleter, DeletionError, Fate, OrgDeleted,
            OrgPlan,
        },
        idp::{IdpOutcome, SignIn},
    },
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
    /// Exit 4: something must be done first (another admin appointed, a
    /// subscription cancelled).
    #[error("{0}")]
    Refused(String),
    #[error("deleting the user at the identity provider")]
    Idp(#[from] crate::retention::idp::IdpError),
}

impl From<DeletionError> for AdminError {
    fn from(err: DeletionError) -> Self {
        match err {
            DeletionError::LastAdmin(_) | DeletionError::Paying(_) => {
                Self::Refused(refusal(&err).unwrap_or_default())
            }
            DeletionError::NotFound | DeletionError::NoLongerDue => {
                Self::Invalid("it is gone already".to_owned())
            }
            DeletionError::Idp(err) => Self::Idp(err),
            DeletionError::Database(err) => Self::Database(err),
        }
    }
}

/// What to do first, for a deletion refused until something is done.
fn refusal(err: &DeletionError) -> Option<String> {
    Some(match err {
        DeletionError::LastAdmin(orgs) => format!(
            "the only admin of {}: make another member an admin first, or delete the organization too ({})",
            orgs.iter()
                .map(|o| o.slug.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            orgs.iter()
                .map(|o| format!("romulus-api delete-org {}", o.slug))
                .collect::<Vec<_>>()
                .join("; "),
        ),
        DeletionError::Paying(orgs) => format!(
            "{} still has a Stripe subscription that is not over (only canceled or incomplete_expired are): cancel it in Stripe first (at once, refunding what is owed), then run this again",
            orgs.iter()
                .map(|o| o.slug.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => return None,
    })
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

/// What `delete-user` did, or would do (`--dry-run`).
pub enum UserDeletion {
    /// A dry run, or a deletion refused (nothing deleted either way).
    Planned {
        plans: Vec<AccountPlan>,
        /// `ROMULUS_ZITADEL_TOKEN` is set: single sign-on users go too.
        zitadel: bool,
        /// Why it would not go (exit 4), after the plan is printed.
        refused: Option<String>,
    },
    Deleted(Vec<AccountDeleted>),
}

/// What `delete-org` did, or would do.
pub enum OrgDeletion {
    /// A dry run, or a deletion refused.
    Planned {
        plan: OrgPlan,
        refused: Option<String>,
    },
    Deleted(OrgDeleted),
}

/// Deletes every account with the address `email`, with what each alone
/// holds (`retention::deletion`), or with `dry_run` says what it would.
/// Refused, before anything goes, when one is the only admin of an
/// organization others belong to, or would take an organization with a
/// subscription: then the plan comes back with why
/// ([`UserDeletion::refused`]).
pub async fn delete_user(
    db: &PgPool,
    config: &Config,
    mailer: Mailer,
    email: &str,
    dry_run: bool,
) -> Result<UserDeletion, AdminError> {
    let deleter = Deleter::new(db.clone(), config, mailer);
    let email = crate::email::normalize(email);
    let ids = deleter.accounts_with_email(&email).await?;
    if ids.is_empty() {
        return Err(AdminError::Invalid(format!(
            "no account has the address {email}"
        )));
    }
    let mut plans = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(plan) = deleter.plan_account(id).await? {
            plans.push(plan);
        }
    }
    // All or none: every refusal is said before one account goes.
    let refused = plans
        .iter()
        .find_map(|plan| deletion::refuse(plan, By::Request).err())
        .and_then(|err| refusal(&err));
    if dry_run || refused.is_some() {
        return Ok(UserDeletion::Planned {
            plans,
            zitadel: deleter.zitadel.is_some(),
            refused,
        });
    }
    let mut done = Vec::with_capacity(plans.len());
    for plan in plans {
        done.push(deleter.delete_account(plan.user_id, By::Request).await?);
    }
    Ok(UserDeletion::Deleted(done))
}

/// Deletes the organization `slug` with everything it holds, or with
/// `dry_run` says what that is. Refused while it has a subscription not
/// over for good: then the plan comes back with why.
pub async fn delete_org(
    db: &PgPool,
    config: &Config,
    mailer: Mailer,
    slug: &str,
    dry_run: bool,
) -> Result<OrgDeletion, AdminError> {
    let deleter = Deleter::new(db.clone(), config, mailer);
    let id = deleter
        .org_by_slug(slug)
        .await?
        .ok_or_else(|| AdminError::Invalid(format!("no organization has the slug {slug}")))?;
    let plan = deleter
        .plan_org(id)
        .await?
        .ok_or_else(|| AdminError::Invalid(format!("no organization has the slug {slug}")))?;
    let refused = plan
        .subscription
        .is_some()
        .then(|| refusal(&DeletionError::Paying(vec![plan.org.clone()])))
        .flatten();
    if dry_run || refused.is_some() {
        return Ok(OrgDeletion::Planned { plan, refused });
    }
    Ok(OrgDeletion::Deleted(deleter.delete_org(id).await?))
}

impl UserDeletion {
    /// Why it was refused, if it was: print the report, then this (exit 4).
    pub fn refused(&self) -> Option<&str> {
        match self {
            Self::Planned { refused, .. } => refused.as_deref(),
            Self::Deleted(_) => None,
        }
    }

    /// What to print, a line each.
    pub fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match self {
            Self::Planned {
                plans,
                zitadel,
                refused,
            } => {
                lines.push(format!(
                    "{}, nothing deleted: {} would go",
                    if refused.is_some() {
                        "refused"
                    } else {
                        "dry run"
                    },
                    plural(plans.len(), "account")
                ));
                for plan in plans {
                    account_lines(&mut lines, plan, None, *zitadel);
                }
            }
            Self::Deleted(deleted) => {
                for done in deleted {
                    account_lines(&mut lines, &done.plan, Some(done), true);
                }
            }
        }
        lines.push(
            "kept: what it wrote in organizations it leaves (theirs, by a deleted account), their audit logs, the sign-in log (90 days)"
                .to_owned(),
        );
        lines
    }
}

fn account_lines(
    lines: &mut Vec<String>,
    plan: &AccountPlan,
    done: Option<&AccountDeleted>,
    zitadel: bool,
) {
    let verb = if done.is_some() {
        "deleted"
    } else {
        "would delete"
    };
    let how = match &plan.sign_in {
        SignIn::Password => "password".to_owned(),
        SignIn::Dev => "dev sign-in".to_owned(),
        SignIn::SingleSignOn(id) => format!("single sign-on, Zitadel user {id}"),
    };
    lines.push(format!(
        "{verb} {} <{}> ({how}), {}",
        plan.name, plan.email, plan.user_id
    ));
    for membership in &plan.memberships {
        let role = format!("{:?}", membership.role).to_lowercase();
        match &membership.fate {
            Fate::Leaves => {
                lines.push(format!("  leaves {} ({role})", membership.org.slug));
            }
            Fate::LastAdmin { members } => lines.push(format!(
                "  leaves {}, whose only admin it is ({} other {})",
                membership.org.slug,
                members,
                if *members == 1 { "member" } else { "members" }
            )),
            Fate::DeletedWith(org) => {
                lines.push(format!(
                    "  {verb} {}, its only member: {}{}",
                    org.org.slug,
                    org_counts(org),
                    subscription(org)
                ));
            }
        }
    }
    match done.map(|done| &done.idp) {
        None if matches!(plan.sign_in, SignIn::SingleSignOn(_)) => lines.push(if zitadel {
            "  and its Zitadel user".to_owned()
        } else {
            "  its Zitadel user is yours to delete in Zitadel's console (no ROMULUS_ZITADEL_TOKEN)"
                .to_owned()
        }),
        Some(IdpOutcome::Deleted) => lines.push("  deleted in Zitadel too".to_owned()),
        Some(IdpOutcome::AlreadyGone) => lines.push("  Zitadel had no such user any more".to_owned()),
        Some(IdpOutcome::Manual { user_id }) => lines.push(format!(
            "  TO DO: delete the user {user_id} in Zitadel's console (Users, the user, Delete): ROMULUS_ZITADEL_TOKEN is not set"
        )),
        _ => {}
    }
    if let Some(done) = done {
        lines.push(if done.emailed {
            format!("  {} was emailed that it is done", plan.email)
        } else {
            format!("  mail is off: confirm to {} yourself", plan.email)
        });
    }
}

fn org_counts(plan: &OrgPlan) -> String {
    let count = |n: i64, what| plural(usize::try_from(n).unwrap_or_default(), what);
    format!(
        "{} ({} deleted, not yet purged), {}, {}, {}, {}, {}",
        count(plan.databases, "database"),
        plan.deleted_databases,
        count(plan.versions, "version"),
        count(plan.drafts, "draft"),
        count(plan.members, "member"),
        count(plan.invitations, "pending invitation"),
        count(plan.playgrounds, "playground"),
    )
}

/// ", subscription active": a subscription still keeping it (refused).
fn subscription(plan: &OrgPlan) -> String {
    plan.subscription
        .as_ref()
        .map_or_else(String::new, |status| format!(", subscription {status}"))
}

/// "1 version", "2 versions".
fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

impl OrgDeletion {
    /// Why it was refused, if it was: print the report, then this (exit 4).
    pub fn refused(&self) -> Option<&str> {
        match self {
            Self::Planned { refused, .. } => refused.as_deref(),
            Self::Deleted(_) => None,
        }
    }

    /// What to print, a line each.
    pub fn report(&self) -> Vec<String> {
        match self {
            Self::Planned { plan, refused } => vec![
                format!(
                    "{}, nothing deleted: would delete {} ({}), {}{}",
                    if refused.is_some() {
                        "refused"
                    } else {
                        "dry run"
                    },
                    plan.org.slug,
                    plan.org.name,
                    org_counts(plan),
                    subscription(plan)
                ),
                "its members keep their accounts; invoices stay in Stripe".to_owned(),
            ],
            Self::Deleted(done) => {
                let mut lines = vec![format!(
                    "deleted {} ({}), {}",
                    done.plan.org.slug,
                    done.plan.org.name,
                    org_counts(&done.plan)
                )];
                lines.push(if done.emailed.is_empty() {
                    "mail is off (or no admin was reachable): confirm to its admins yourself"
                        .to_owned()
                } else {
                    format!("emailed its admins: {}", done.emailed.join(", "))
                });
                lines
            }
        }
    }
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
