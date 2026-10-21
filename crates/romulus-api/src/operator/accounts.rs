//! People and organizations coming and going: a new account (its first
//! sign-in with single sign-on, a password sign-up, `romulus-api setup`), a
//! new organization, an account or an organization deleted (on request with
//! `romulus-api delete-user`/`delete-org`, or after 3 years unused). The
//! callers queue these in their own transactions ([`super::notify`]).

use uuid::Uuid;

use super::{Event, Notice, who};
use crate::retention::{
    deletion::{AccountPlan, By, Fate, OrgPlan, OrgRef},
    idp::IdpOutcome,
};

/// How an account came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined<'a> {
    /// A first sign-in through the identity provider.
    SingleSignOn,
    /// A password sign-up, from an invitation to this organization (its
    /// slug) or not.
    Password { invited_to: Option<&'a str> },
    /// `romulus-api setup`: the install's first admin.
    Setup,
    /// The development sign-in.
    Dev,
}

pub fn account_created(user_id: Uuid, name: &str, email: &str, joined: Joined<'_>) -> Notice {
    let how = match joined {
        Joined::SingleSignOn => "Signed in for the first time with single sign-on.".to_owned(),
        Joined::Password {
            invited_to: Some(org),
        } => format!("Signed up with a password, invited to {org}."),
        Joined::Password { invited_to: None } => "Signed up with a password.".to_owned(),
        Joined::Setup => "Created by romulus-api setup, as the first admin.".to_owned(),
        Joined::Dev => "Created by the development sign-in.".to_owned(),
    };
    Notice {
        event: Event::AccountCreated,
        about: user_id.to_string(),
        subject: format!("New account: {}", who(name, email)),
        lines: vec![
            format!("New account: {}.", who(name, email)),
            how,
            format!("Account id: {user_id}"),
        ],
    }
}

/// A new organization, and who made it (`None`: `romulus-api setup`).
pub fn org_created(org_id: Uuid, name: &str, slug: &str, by: Option<(&str, &str)>) -> Notice {
    let mut lines = vec![format!("New organization: {name} ({slug}).")];
    lines.push(match by {
        Some((by_name, by_email)) => format!("Created by {}, its admin.", who(by_name, by_email)),
        None => "Created by romulus-api setup.".to_owned(),
    });
    lines.push(format!("Organization id: {org_id}"));
    Notice {
        event: Event::OrgCreated,
        about: org_id.to_string(),
        subject: format!("New organization: {name} ({slug})"),
        lines,
    }
}

fn orgs<'a>(orgs: impl Iterator<Item = &'a OrgRef>) -> String {
    orgs.map(|org| format!("{} ({})", org.name, org.slug))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An account deleted, with what went with it; `appointed`: the new admins
/// (organization, name) where it was the only one.
pub fn account_deleted(
    plan: &AccountPlan,
    by: By,
    idp: &IdpOutcome,
    appointed: &[(OrgRef, String)],
) -> Notice {
    let why = match by {
        By::Request => "on request (romulus-api delete-user)",
        By::Inactivity => "after 3 years without use (its owner was emailed 30 days before)",
    };
    let mut lines = vec![format!(
        "The account of {} is deleted, {why}.",
        who(&plan.name, &plan.email)
    )];
    let deleted = plan.deletes();
    if !deleted.is_empty() {
        lines.push(format!(
            "Deleted with it, as its only member: {}.",
            orgs(deleted.iter().map(|o| &o.org))
        ));
    }
    let left: Vec<&OrgRef> = plan
        .memberships
        .iter()
        .filter(|m| !matches!(m.fate, Fate::DeletedWith(_)))
        .map(|m| &m.org)
        .collect();
    if !left.is_empty() {
        lines.push(format!("Left: {}.", orgs(left.into_iter())));
    }
    for (org, successor) in appointed {
        lines.push(format!(
            "{successor} is now an admin of {} ({}), its only admin gone.",
            org.name, org.slug
        ));
    }
    match idp {
        IdpOutcome::NotThere => {}
        IdpOutcome::Deleted => lines.push("Its Zitadel user is deleted too.".to_owned()),
        IdpOutcome::AlreadyGone => lines.push("Its Zitadel user was gone already.".to_owned()),
        IdpOutcome::Manual { user_id } => lines.push(format!(
            "Delete its Zitadel user {user_id} in Zitadel's console: ROMULUS_ZITADEL_TOKEN is not set."
        )),
    }
    lines.push(format!("Account id: {}", plan.user_id));
    Notice {
        event: Event::AccountDeleted,
        about: plan.user_id.to_string(),
        subject: format!(
            "Account deleted: {} ({})",
            plan.email,
            match by {
                By::Request => "on request",
                By::Inactivity => "inactive",
            }
        ),
        lines,
    }
}

/// An organization deleted on request (`romulus-api delete-org`); one going
/// with its only member's account is in that account's notification.
pub fn org_deleted(plan: &OrgPlan) -> Notice {
    let org = &plan.org;
    Notice {
        event: Event::OrgDeleted,
        about: org.id.to_string(),
        subject: format!("Organization deleted: {} ({})", org.name, org.slug),
        lines: vec![
            format!(
                "The organization {} ({}) is deleted, on request (romulus-api delete-org).",
                org.name, org.slug
            ),
            format!(
                "It held {} databases, {} versions, {} drafts, {} members, {} pending invitations and {} playgrounds.",
                plan.databases,
                plan.versions,
                plan.drafts,
                plan.members,
                plan.invitations,
                plan.playgrounds
            ),
            format!("Organization id: {}", org.id),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_account_says_who_and_how() {
        let id = Uuid::from_u128(7);
        let notice = account_created(
            id,
            "Ada Lovelace",
            "ada@example.com",
            Joined::Password {
                invited_to: Some("acme"),
            },
        );
        assert_eq!(
            notice.subject,
            "New account: Ada Lovelace <ada@example.com>"
        );
        assert_eq!(notice.about, id.to_string());
        assert!(
            notice
                .lines
                .contains(&"Signed up with a password, invited to acme.".to_owned())
        );
    }
}
