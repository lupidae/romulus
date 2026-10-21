//! The messages password sign-in sends. Each says why it arrived and what to
//! do if it was not expected; none carries a secret but its one-time link.

use std::time::Duration;

use url::Url;

use crate::mail::{Email, Letter};

fn duration(ttl: Duration) -> String {
    let minutes = ttl.as_secs() / 60;
    match minutes {
        m if m % (24 * 60) == 0 && m >= 24 * 60 => plural(m / (24 * 60), "day"),
        m if m % 60 == 0 && m >= 60 => plural(m / 60, "hour"),
        m => plural(m, "minute"),
    }
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

fn sign_in_url(public_url: &Url) -> String {
    public_url
        .join("/app/signin")
        .expect("a path joins any base URL")
        .into()
}

/// The link that finishes a sign-up (it proves the address).
pub fn sign_up(to: &str, link: &str, ttl: Duration, org: Option<&str>) -> Email {
    let why = match org {
        Some(org) => format!("To join {org} on romulus, finish creating your account:"),
        None => "Finish creating your romulus account:".to_owned(),
    };
    Letter {
        kind: "sign_up",
        to,
        subject: "Finish creating your romulus account".to_owned(),
        paragraphs: vec![
            why,
            format!("The link works once, for {}.", duration(ttl)),
        ],
        action: Some(("Create my account", link)),
        footer: format!(
            "Someone asked for a romulus account for {to}. If it was not you, ignore this message: no account is created without this link."
        ),
    }
    .email()
}

/// Someone tried to sign up with an address that has an account already.
pub fn account_exists(to: &str, public_url: &Url) -> Email {
    let sign_in = sign_in_url(public_url);
    Letter {
        kind: "account_exists",
        to,
        subject: "You already have a romulus account".to_owned(),
        paragraphs: vec![
            format!("Someone tried to create a romulus account for {to}, which already has one."),
            "If it was you, sign in instead. Forgot your password? Choose \"Forgot password?\" on the sign-in page.".to_owned(),
        ],
        action: Some(("Sign in", &sign_in)),
        footer: "If it was not you, you can ignore this message: nothing changed.".to_owned(),
    }
    .email()
}

/// A password reset link.
pub fn reset(to: &str, link: &str, ttl: Duration) -> Email {
    Letter {
        kind: "reset",
        to,
        subject: "Reset your romulus password".to_owned(),
        paragraphs: vec![
            "Someone asked to reset the password of your romulus account.".to_owned(),
            format!(
                "Choose a new password with this link. It works once, for {}, and signs you out everywhere else.",
                duration(ttl)
            ),
        ],
        action: Some(("Choose a new password", link)),
        footer: "If you did not ask for this, ignore this message: your password stays as it is.".to_owned(),
    }
    .email()
}

/// The password of an account changed (a reset or a change).
/// The password changed (or was reset); `tokens` access tokens were revoked
/// with the sessions.
pub fn password_changed(to: &str, public_url: &Url, tokens: u64) -> Email {
    let sign_in = sign_in_url(public_url);
    let mut paragraphs = vec![
        "The password of your romulus account just changed, and every other session was signed out.".to_owned(),
    ];
    if tokens > 0 {
        paragraphs.push(format!(
            "Your {tokens} access token{} for AI agents {} revoked too: create new ones (account menu, Access tokens) for the agents you still use.",
            if tokens == 1 { "" } else { "s" },
            if tokens == 1 { "was" } else { "were" },
        ));
    }
    paragraphs.push(
        "If you did not do this, reset your password now (\"Forgot password?\" on the sign-in page) and tell whoever runs your romulus server.".to_owned(),
    );
    Letter {
        kind: "password_changed",
        to,
        subject: "Your romulus password changed".to_owned(),
        paragraphs,
        action: Some(("Open romulus", &sign_in)),
        footer: format!("Sent to {to} because its romulus account changed."),
    }
    .email()
}

/// Too many failed sign-ins locked the account's address.
pub fn locked(to: &str, lock: Duration) -> Email {
    Letter {
        kind: "locked",
        to,
        subject: "Sign-ins to your romulus account are paused".to_owned(),
        paragraphs: vec![
            format!(
                "Someone entered a wrong password for {to} many times in a row, so romulus paused sign-ins with it for {}.",
                duration(lock)
            ),
            "If it was you, wait, or reset your password (\"Forgot password?\" on the sign-in page). If it was not, your password was not found: nothing else is needed, though a longer password never hurts.".to_owned(),
        ],
        action: None,
        footer: format!("Sent to {to} because its romulus account was locked."),
    }
    .email()
}

/// An invitation to an organization.
pub fn invitation(
    to: &str,
    inviter: &str,
    org: &str,
    role: &str,
    link: &str,
    ttl: Duration,
) -> Email {
    Letter {
        kind: "invitation",
        to,
        // No free text in the subject: names are anyone's to choose.
        subject: "Your invitation to romulus".to_owned(),
        paragraphs: vec![
            format!("{inviter} invited you to join {org} on romulus as {role}."),
            format!(
                "romulus keeps {org}'s Postgres schemas: their versions, diffs, and the drafts people design together. The invitation works for {}.",
                duration(ttl)
            ),
        ],
        action: Some((&format!("Join {org}"), link)),
        footer: format!(
            "Sent to {to} by romulus on {inviter}'s behalf. Not expecting it? Ignore this message."
        ),
    }
    .email()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(Duration::from_mins(30)), "30 minutes");
        assert_eq!(duration(Duration::from_hours(1)), "1 hour");
        assert_eq!(duration(Duration::from_hours(24)), "1 day");
        assert_eq!(duration(Duration::from_hours(7 * 24)), "7 days");
    }

    #[test]
    fn a_reset_carries_its_link_and_no_other_secret() {
        let email = reset(
            "bob@x.io",
            "https://r.example/app/signin/reset#token=t",
            Duration::from_mins(30),
        );
        assert!(
            email
                .text
                .contains("https://r.example/app/signin/reset#token=t")
        );
        assert!(email.text.contains("30 minutes"));
        assert_eq!(email.to, "bob@x.io");
    }
}
