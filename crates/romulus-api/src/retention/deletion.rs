//! Deleting an account or an organization with everything it alone holds
//! (docs/MVP.md "Retention and deletion"; the hosted Terms promise it within
//! 30 days of a request).
//!
//! - **An organization** goes in one statement: its databases, versions,
//!   drafts, comments, tokens, playgrounds and tests, members, invitations,
//!   audit log, billing rows and playground hours (`ON DELETE CASCADE`).
//!   Refused while it has a Stripe subscription that is not over for good
//!   (`canceled`, `incomplete_expired`: `billing::state::ENDED_STATUSES`):
//!   cancel it in Stripe first (its invoices stay there, as the law
//!   requires). Its admins are emailed.
//! - **An account** goes with its sessions, password, devices, links,
//!   personal access tokens, connected apps (their grants, codes and
//!   tokens), memberships, approvals and unread markers, and
//!   with every organization it is the only member of (refused, as above,
//!   when one has a subscription). What it wrote in the organizations it leaves
//!   is theirs and stays (versions, comments, approvals: by a deleted
//!   account; the audit log keeps the name it recorded); so do invitations
//!   others sent to its address (they expire). A single sign-on account is
//!   deleted at the identity provider first ([`super::idp`]). The address is
//!   emailed that it is done. Its contract records (`legal_acceptances`,
//!   `withdrawals`) stay, with its address, until their own retention ends
//!   ([`crate::legal::purge`]).
//! - **The last admin**: an organization never has members without an admin
//!   (`organization_needs_an_admin`). Deleting, on request, the only admin of
//!   an organization others belong to is refused: they appoint another admin
//!   first, or ask for the organization to be deleted too. Deleting an
//!   inactive account ([`By::Inactivity`]) appoints one instead: its most
//!   recently active developer, else reviewer (audited `admin_appointed`,
//!   emailed).
//!
//! Each deletion is a sign-in log event (`account_deleted`,
//! `organization_deleted`: who and what, kept its 90 days), tells the
//! running API what it ended ([`super::live`]) and the operators what went
//! (`crate::operator`), in its transaction.

use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use uuid::Uuid;

use super::{
    idp::{self, IdpError, IdpOutcome, Zitadel},
    live::{self, Notice},
};
use crate::{
    Config, audit,
    auth::password::{
        events::{self, Event},
        throttle,
    },
    billing::state::ENDED_STATUSES,
    config::OperatorConfig,
    domain::{AuditAction, AuthEventKind, Role},
    mail::{Letter, Mailer},
};

/// Why an account is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// Its owner asked (`romulus-api delete-user`).
    Request,
    /// Unused for 3 years, warned 30 days before (`super::inactive`).
    Inactivity,
}

impl By {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Inactivity => "inactivity",
        }
    }
}

/// An organization, as a deletion names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRef {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

/// One membership of an account about to be deleted, and what becomes of
/// its organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub org: OrgRef,
    pub role: Role,
    pub fate: Fate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// Others stay, an admin among them: the account just leaves.
    Leaves,
    /// The account is its only member: the organization goes with it.
    DeletedWith(OrgPlan),
    /// The account is its only admin, others are members.
    LastAdmin { members: i64 },
}

/// What deleting an account would do, read before anything goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPlan {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    pub sign_in: idp::SignIn,
    pub memberships: Vec<Membership>,
}

impl AccountPlan {
    fn subject(&self) -> Option<&str> {
        match &self.sign_in {
            idp::SignIn::SingleSignOn(id) => Some(id),
            _ => None,
        }
    }

    /// The organizations it is the only admin of while others stay.
    pub fn last_admin_of(&self) -> Vec<&OrgRef> {
        self.memberships
            .iter()
            .filter(|m| matches!(m.fate, Fate::LastAdmin { .. }))
            .map(|m| &m.org)
            .collect()
    }

    /// The organizations that would go with it.
    pub fn deletes(&self) -> Vec<&OrgPlan> {
        self.memberships
            .iter()
            .filter_map(|m| match &m.fate {
                Fate::DeletedWith(plan) => Some(plan),
                _ => None,
            })
            .collect()
    }
}

/// What deleting an organization would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgPlan {
    pub org: OrgRef,
    pub databases: i64,
    /// Deleted ones not yet purged, counted in `databases` too.
    pub deleted_databases: i64,
    pub versions: i64,
    pub drafts: i64,
    pub members: i64,
    pub invitations: i64,
    pub playgrounds: i64,
    /// The status of a Stripe subscription that is not over for good
    /// (anything but `canceled` and `incomplete_expired`: it may still
    /// charge, or resume): refused until it is.
    pub subscription: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeletionError {
    #[error("no such account or organization")]
    NotFound,
    /// On request, the only admin of organizations others belong to.
    #[error("the only admin of {}", slugs(.0))]
    LastAdmin(Vec<OrgRef>),
    /// An organization that would go has a subscription not over for good.
    #[error("{} still has a subscription", slugs(.0))]
    Paying(Vec<OrgRef>),
    /// An inactive account used again since it was found.
    #[error("the account was used again")]
    NoLongerDue,
    #[error("deleting the user at the identity provider")]
    Idp(#[from] IdpError),
    #[error("database")]
    Database(#[from] sqlx::Error),
}

fn slugs(orgs: &[OrgRef]) -> String {
    orgs.iter()
        .map(|org| org.slug.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a deletion did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeleted {
    pub plan: AccountPlan,
    pub idp: IdpOutcome,
    /// Members made admins where the account was the only one (inactivity).
    pub appointed: Vec<(OrgRef, String)>,
    /// The address was told (mail on).
    pub emailed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDeleted {
    pub plan: OrgPlan,
    /// The admins told, by address.
    pub emailed: Vec<String>,
}

/// Deletes accounts and organizations: the database, the mail it confirms
/// with, the identity provider it deletes single sign-on users from.
#[derive(Clone)]
pub struct Deleter {
    pub db: PgPool,
    pub mailer: Mailer,
    pub zitadel: Option<Zitadel>,
    /// `ROMULUS_PUBLIC_URL`'s origin, named in the emails.
    pub origin: String,
    /// Who hears of a deletion (`Config::operator`).
    pub operators: Option<OperatorConfig>,
}

impl Deleter {
    pub fn new(db: PgPool, config: &Config, mailer: Mailer) -> Self {
        Self {
            db,
            mailer,
            zitadel: Zitadel::from_config(config),
            origin: config.public_origin(),
            operators: config.operator.clone(),
        }
    }

    /// The accounts with `email` (a password account and a single sign-on
    /// one can share an address), oldest first.
    pub async fn accounts_with_email(&self, email: &str) -> Result<Vec<Uuid>, sqlx::Error> {
        sqlx::query_scalar!(
            "SELECT id FROM users WHERE lower(email COLLATE pg_unicode_fast) = lower($1 COLLATE pg_unicode_fast)
             ORDER BY created_at",
            crate::email::normalize(email),
        )
        .fetch_all(&self.db)
        .await
    }

    pub async fn org_by_slug(&self, slug: &str) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar!("SELECT id FROM organizations WHERE slug = $1", slug)
            .fetch_optional(&self.db)
            .await
    }

    pub async fn plan_account(&self, user_id: Uuid) -> Result<Option<AccountPlan>, sqlx::Error> {
        let mut conn = self.db.acquire().await?;
        plan_account(&mut conn, user_id).await
    }

    pub async fn plan_org(&self, org_id: Uuid) -> Result<Option<OrgPlan>, sqlx::Error> {
        plan_org(&self.db, org_id).await
    }

    /// Deletes the account `user_id`: see the module documentation.
    ///
    /// Everything happens under the account's row lock, the call to the
    /// identity provider included: a sign-in or a token's use waits for it
    /// (they write the row, `retention::activity`), so an inactive account
    /// used meanwhile is seen used and kept, at Zitadel too.
    pub async fn delete_account(
        &self,
        user_id: Uuid,
        by: By,
    ) -> Result<AccountDeleted, DeletionError> {
        let mut tx = crate::db::begin_read_committed(&self.db).await?;
        let locked = sqlx::query_scalar!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user_id)
            .fetch_optional(&mut *tx)
            .await?;
        if locked.is_none() {
            return Err(DeletionError::NotFound);
        }
        let plan = plan_account(&mut tx, user_id)
            .await?
            .ok_or(DeletionError::NotFound)?;
        refuse(&plan, by)?;
        if by == By::Inactivity && !still_due(&mut *tx, user_id).await? {
            return Err(DeletionError::NoLongerDue);
        }
        // The identity provider before the rows: should they fail, a run
        // again finds the user gone there and finishes; the other way round,
        // the person could sign in again into a fresh, empty account.
        let idp = match plan.subject() {
            Some(subject) => idp::delete(self.zitadel.as_ref(), subject).await?,
            None => IdpOutcome::NotThere,
        };
        if let IdpOutcome::Manual {
            user_id: zitadel_user,
        } = &idp
        {
            tracing::warn!(
                account = %user_id,
                zitadel_user = %zitadel_user,
                "deleting an account: delete this user in Zitadel's console too (ROMULUS_ZITADEL_TOKEN is not set)"
            );
        }
        let appointed = match delete_rows(tx, &plan, by, &idp, self.operators.as_ref()).await {
            Ok(appointed) => appointed,
            Err(err) => {
                if idp == IdpOutcome::Deleted {
                    tracing::error!(
                        account = %user_id,
                        zitadel_user = plan.subject().unwrap_or_default(),
                        error = %crate::error::Chain(&err),
                        "deleting an account: its Zitadel user is deleted but the account is not; run the deletion again (Zitadel now answers that user gone)"
                    );
                }
                return Err(err.into());
            }
        };
        tracing::info!(account = %user_id, by = by.as_str(), "account deleted");

        let emailed = self.tell_account(&plan, by).await;
        for (org, successor) in &appointed {
            self.tell_appointed(org, successor, &plan.name).await;
        }
        Ok(AccountDeleted {
            plan,
            idp,
            appointed: appointed
                .into_iter()
                .map(|(org, successor)| (org, successor.name))
                .collect(),
            emailed,
        })
    }

    /// Deletes the organization `org_id` with everything it holds.
    pub async fn delete_org(&self, org_id: Uuid) -> Result<OrgDeleted, DeletionError> {
        let admins = sqlx::query_scalar!(
            "SELECT u.email FROM memberships m JOIN users u ON u.id = m.user_id
             WHERE m.org_id = $1 AND m.role = 'admin' ORDER BY m.created_at",
            org_id,
        )
        .fetch_all(&self.db)
        .await?;
        let mut tx = crate::db::begin_read_committed(&self.db).await?;
        let locked = sqlx::query_scalar!(
            "SELECT id FROM organizations WHERE id = $1 FOR UPDATE",
            org_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if locked.is_none() {
            return Err(DeletionError::NotFound);
        }
        let plan = plan_org(&mut *tx, org_id)
            .await?
            .ok_or(DeletionError::NotFound)?;
        if plan.subscription.is_some() {
            return Err(DeletionError::Paying(vec![plan.org.clone()]));
        }
        delete_org_rows(&mut tx, &plan, "delete-org").await?;
        crate::operator::notify(
            &mut tx,
            self.operators.as_ref(),
            &crate::operator::accounts::org_deleted(&plan),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(org = %plan.org.slug, "organization deleted");

        let mut emailed = Vec::new();
        for admin in admins {
            if self.tell_org_admin(&plan, &admin).await {
                emailed.push(admin);
            }
        }
        Ok(OrgDeleted { plan, emailed })
    }

    async fn send(&self, letter: Letter<'_>) -> bool {
        if !self.mailer.is_on() {
            return false;
        }
        let kind = letter.kind;
        match self.mailer.send_now(&letter.email()).await {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(kind, error = %crate::error::Chain(&err), "mail: not sent");
                false
            }
        }
    }

    async fn tell_account(&self, plan: &AccountPlan, by: By) -> bool {
        let mut paragraphs = vec![match by {
            By::Request => format!(
                "Your romulus account ({}) is deleted, as you asked.",
                plan.email
            ),
            By::Inactivity => format!(
                "Your romulus account ({}) is deleted: nobody had signed in to it for 3 years, and we wrote 30 days ago that it would be.",
                plan.email
            ),
        }];
        let deleted = plan.deletes();
        if !deleted.is_empty() {
            paragraphs.push(format!(
                "The organizations you were the only member of are deleted with it: {}.",
                names(deleted.iter().map(|o| &o.org))
            ));
        }
        paragraphs.push(
            "What you wrote in organizations you shared stays theirs, shown as by a deleted account."
                .to_owned(),
        );
        self.send(Letter {
            kind: "account_deleted",
            to: &plan.email,
            subject: "Your romulus account is deleted".to_owned(),
            paragraphs,
            action: None,
            footer: format!(
                "Sent by romulus at {} to the address of the account it deleted.",
                self.origin
            ),
        })
        .await
    }

    async fn tell_org_admin(&self, plan: &OrgPlan, to: &str) -> bool {
        self.send(Letter {
            kind: "organization_deleted",
            to,
            subject: format!("{} is deleted from romulus", plan.org.name),
            paragraphs: vec![
                format!(
                    "The organization {} ({}) is deleted, as an admin asked: its databases, versions, drafts, comments, members and invitations with it.",
                    plan.org.name, plan.org.slug
                ),
                "Its members keep their own accounts.".to_owned(),
            ],
            action: None,
            footer: format!(
                "Sent by romulus at {} to the organization's admins.",
                self.origin
            ),
        })
        .await
    }

    async fn tell_appointed(&self, org: &OrgRef, successor: &Successor, former: &str) {
        let url = format!("{}/app/{}/settings/members", self.origin, org.slug);
        self.send(Letter {
            kind: "admin_appointed",
            to: &successor.email,
            subject: format!("You are now an admin of {}", org.name),
            paragraphs: vec![format!(
                "{former}'s romulus account was deleted after 3 years without use, and {} needs an admin: you are one now, as its most recently active developer (or reviewer, where it has no developer). You can manage its members, invitations and settings, and make someone else an admin.",
                org.name
            )],
            action: Some(("Open its members", &url)),
            footer: format!("Sent by romulus at {} to {}'s new admin.", self.origin, org.name),
        })
        .await;
    }
}

fn names<'a>(orgs: impl Iterator<Item = &'a OrgRef>) -> String {
    orgs.map(|org| org.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn idp_word(outcome: &IdpOutcome) -> &'static str {
    match outcome {
        IdpOutcome::NotThere => "none",
        IdpOutcome::Deleted => "deleted",
        IdpOutcome::AlreadyGone => "already_gone",
        IdpOutcome::Manual { .. } => "manual",
    }
}

/// Whether `plan` may go `by` that way. On request: nobody is left without
/// an admin, and nothing paying goes. For inactivity: nothing paying goes
/// (an admin is appointed instead).
pub fn refuse(plan: &AccountPlan, by: By) -> Result<(), DeletionError> {
    if by == By::Request {
        let last = plan.last_admin_of();
        if !last.is_empty() {
            return Err(DeletionError::LastAdmin(
                last.into_iter().cloned().collect(),
            ));
        }
    }
    let paying: Vec<OrgRef> = plan
        .deletes()
        .into_iter()
        .filter(|org| org.subscription.is_some())
        .map(|org| org.org.clone())
        .collect();
    if !paying.is_empty() {
        return Err(DeletionError::Paying(paying));
    }
    Ok(())
}

/// Still unused and warned long enough: see `super::inactive`.
async fn still_due(db: impl PgExecutor<'_>, user_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM users WHERE id = $1 AND inactivity_notice_at IS NOT NULL
                 AND inactivity_notice_at <= now() - make_interval(days => $2)
                 AND last_seen_at <= now() - make_interval(years => $3)
                 AND last_seen_at < inactivity_notice_at
           ) AS "due!""#,
        user_id,
        super::inactive::NOTICE_DAYS,
        super::inactive::UNUSED_YEARS,
    )
    .fetch_one(db)
    .await
}

async fn plan_account(
    conn: &mut PgConnection,
    user_id: Uuid,
) -> Result<Option<AccountPlan>, sqlx::Error> {
    let Some(user) = sqlx::query!(
        "SELECT email, name, oidc_subject FROM users WHERE id = $1",
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let rows = sqlx::query!(
        r#"SELECT o.id, o.slug, o.name, m.role AS "role: Role",
                  (SELECT count(*) FROM memberships x WHERE x.org_id = o.id AND x.user_id <> $1) AS "others!",
                  (SELECT count(*) FROM memberships x
                   WHERE x.org_id = o.id AND x.user_id <> $1 AND x.role = 'admin') AS "other_admins!"
           FROM memberships m JOIN organizations o ON o.id = m.org_id
           WHERE m.user_id = $1 ORDER BY m.created_at"#,
        user_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut memberships = Vec::with_capacity(rows.len());
    for row in rows {
        let org = OrgRef {
            id: row.id,
            slug: row.slug,
            name: row.name,
        };
        let fate = if row.others == 0 {
            match plan_org(&mut *conn, org.id).await? {
                Some(plan) => Fate::DeletedWith(plan),
                None => continue,
            }
        } else if row.role == Role::Admin && row.other_admins == 0 {
            Fate::LastAdmin {
                members: row.others,
            }
        } else {
            Fate::Leaves
        };
        memberships.push(Membership {
            org,
            role: row.role,
            fate,
        });
    }
    Ok(Some(AccountPlan {
        user_id,
        email: user.email,
        name: user.name,
        sign_in: idp::SignIn::of(&user.oidc_subject),
        memberships,
    }))
}

async fn plan_org(db: impl PgExecutor<'_>, org_id: Uuid) -> Result<Option<OrgPlan>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT o.slug, o.name,
                  (SELECT count(*) FROM databases d WHERE d.org_id = o.id) AS "databases!",
                  (SELECT count(*) FROM databases d WHERE d.org_id = o.id AND d.deleted_at IS NOT NULL) AS "deleted_databases!",
                  (SELECT count(*) FROM versions v JOIN databases d ON d.id = v.database_id WHERE d.org_id = o.id) AS "versions!",
                  (SELECT count(*) FROM drafts r JOIN databases d ON d.id = r.database_id WHERE d.org_id = o.id) AS "drafts!",
                  (SELECT count(*) FROM memberships m WHERE m.org_id = o.id) AS "members!",
                  (SELECT count(*) FROM invitations i WHERE i.org_id = o.id AND i.accepted_at IS NULL) AS "invitations!",
                  (SELECT count(*) FROM playgrounds p WHERE p.org_id = o.id) AS "playgrounds!",
                  (SELECT coalesce(b.status, 'unknown') FROM org_billing b
                   WHERE b.org_id = o.id AND b.subscription_id IS NOT NULL
                     AND (b.status IS NULL OR NOT (b.status = ANY($2)))) AS subscription
           FROM organizations o WHERE o.id = $1"#,
        org_id,
        &ENDED_STATUSES.map(str::to_owned) as &[String],
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| OrgPlan {
        org: OrgRef {
            id: org_id,
            slug: row.slug,
            name: row.name,
        },
        databases: row.databases,
        deleted_databases: row.deleted_databases,
        versions: row.versions,
        drafts: row.drafts,
        members: row.members,
        invitations: row.invitations,
        playgrounds: row.playgrounds,
        subscription: row.subscription,
    }))
}

/// Deletes the organization's row (everything cascades), in `tx`: the
/// sign-in log's event, and the notices the running API needs.
async fn delete_org_rows(
    tx: &mut PgConnection,
    plan: &OrgPlan,
    by: &str,
) -> Result<(), sqlx::Error> {
    let databases = sqlx::query_scalar!(
        "SELECT id FROM databases WHERE org_id = $1 AND deleted_at IS NULL",
        plan.org.id
    )
    .fetch_all(&mut *tx)
    .await?;
    let members = sqlx::query_scalar!(
        "SELECT user_id FROM memberships WHERE org_id = $1",
        plan.org.id
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM organizations WHERE id = $1", plan.org.id)
        .execute(&mut *tx)
        .await?;
    for database in databases {
        live::notify(tx, Notice::DatabaseDeleted { database }).await?;
    }
    for user in members {
        live::notify(
            tx,
            Notice::MemberRemoved {
                org: plan.org.id,
                user,
            },
        )
        .await?;
    }
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::OrganizationDeleted).details(json!({
            "by": by,
            "organization": plan.org.id,
            "slug": plan.org.slug,
            "databases": plan.databases,
            "versions": plan.versions,
            "members": plan.members,
        })),
    )
    .await?;
    Ok(())
}

/// The account's rows, in `tx` (holding its row lock), then the commit:
/// the organizations it alone holds, the admins appointed, the sign-in
/// log's event, the notices for the running API.
async fn delete_rows(
    mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
    plan: &AccountPlan,
    by: By,
    idp: &IdpOutcome,
    operators: Option<&OperatorConfig>,
) -> Result<Vec<(OrgRef, Successor)>, sqlx::Error> {
    let user_id = plan.user_id;
    let mut appointed = Vec::new();
    for membership in &plan.memberships {
        match &membership.fate {
            Fate::Leaves => {
                live::notify(
                    &mut tx,
                    Notice::MemberRemoved {
                        org: membership.org.id,
                        user: user_id,
                    },
                )
                .await?;
            }
            Fate::DeletedWith(org) => {
                delete_org_rows(&mut tx, org, &format!("with the account {}", plan.email)).await?;
            }
            Fate::LastAdmin { .. } => {
                // Only for inactivity: `refuse` stopped a request.
                if let Some(successor) = appoint(&mut tx, &membership.org, plan).await? {
                    appointed.push((membership.org.clone(), successor));
                }
                live::notify(
                    &mut tx,
                    Notice::MemberRemoved {
                        org: membership.org.id,
                        user: user_id,
                    },
                )
                .await?;
            }
        }
    }
    throttle::clear(&mut *tx, &plan.email).await?;
    sqlx::query!("DELETE FROM users WHERE id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut *tx,
        Event::new(AuthEventKind::AccountDeleted)
            .email(&plan.email)
            .details(json!({
                "by": by.as_str(),
                "account": user_id,
                "left": plan.memberships.iter()
                    .filter(|m| !matches!(m.fate, Fate::DeletedWith(_)))
                    .map(|m| &m.org.slug).collect::<Vec<_>>(),
                "organizations_deleted": plan.deletes().iter().map(|o| &o.org.slug).collect::<Vec<_>>(),
                "identity_provider": idp_word(idp),
            })),
    )
    .await?;
    let named: Vec<(OrgRef, String)> = appointed
        .iter()
        .map(|(org, successor)| (org.clone(), successor.name.clone()))
        .collect();
    crate::operator::notify(
        &mut tx,
        operators,
        &crate::operator::accounts::account_deleted(plan, by, idp, &named),
    )
    .await?;
    tx.commit().await?;
    Ok(appointed)
}

/// Who becomes an admin.
struct Successor {
    id: Uuid,
    name: String,
    email: String,
}

/// Makes the most recently active developer (else reviewer) of `org` an
/// admin, as the only admin's account goes: audited, and the running API
/// told (their sockets reconnect with the new role).
async fn appoint(
    tx: &mut PgConnection,
    org: &OrgRef,
    former: &AccountPlan,
) -> Result<Option<Successor>, sqlx::Error> {
    let Some(successor) = sqlx::query_as!(
        Successor,
        "SELECT u.id, u.name, u.email FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = $1 AND m.user_id <> $2
         ORDER BY (m.role = 'developer') DESC, u.last_seen_at DESC, m.created_at
         LIMIT 1",
        org.id,
        former.user_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    sqlx::query!(
        "UPDATE memberships SET role = 'admin' WHERE org_id = $1 AND user_id = $2",
        org.id,
        successor.id,
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &audit::Entry {
            org_id: org.id,
            database_id: None,
            action: AuditAction::AdminAppointed,
            actor_id: None,
            actor_name: None,
            version_number: None,
            reason: Some(format!(
                "{}'s account, the only admin's, was deleted after 3 years without use.",
                former.name
            )),
            details: audit::AuditDetails {
                member: Some(audit::AuditActor {
                    id: Some(successor.id),
                    name: successor.name.clone(),
                }),
                ..Default::default()
            },
        },
    )
    .await?;
    live::notify(
        tx,
        Notice::RoleChanged {
            org: org.id,
            user: successor.id,
            role: Role::Admin,
        },
    )
    .await?;
    tracing::info!(org = %org.slug, admin = %successor.id, "admin appointed");
    Ok(Some(successor))
}
