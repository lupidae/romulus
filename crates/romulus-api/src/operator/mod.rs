//! Operator notifications: what the people running an install hear about as
//! it happens, and a daily digest (`ROMULUS_OPERATOR_EMAILS`; docs/MVP.md
//! "Operator notifications", docs/self-host/configuration.md). Off (unset or
//! empty, the default) nothing is written or sent. Hosted or self-hosted
//! alike: billing's events only happen where billing is on.
//!
//! - **Instant notifications** ([`Notice`]), one short email each, subject
//!   prefixed `[romulus]`: a new account ([`accounts`]), a new organization,
//!   a subscription started, upgraded or downgraded, a cancellation
//!   scheduled, a subscription cancelled (Stripe's reason when it has one),
//!   a failed payment, a grace period over, a payment after it (back from
//!   Free), a consumer's withdrawal ([`billing`]), an account or
//!   organization deleted (on request or for inactivity), a plan's cap
//!   reached ([`limits`]: once per organization and cap in 30 days). Each
//!   says which organization, which plan, who, and links to Stripe's
//!   dashboard where there is something to look at. Personal data: names,
//!   addresses and a business's name only.
//! - **What people typed stays text**: every value from outside (names,
//!   addresses, business names, a cancellation's comment) goes through
//!   [`flat`], so it cannot start a line of its own; only the links the
//!   code built ([`Link`]: Stripe's dashboard, the install's origin) are
//!   links.
//! - **Through the mail outbox** (`mail_outbox`, [`crate::mail::outbox`]):
//!   [`notify`] queues an email per operator in the transaction of what it
//!   reports, so nothing is told that did not happen and sending never fails
//!   anyone's action; the outbox's worker sends it (marked sent once the
//!   mail server took it, retried, given up after 3 days). Its key (the
//!   event, what it is about, the address) makes a second queue of the same
//!   event a no-op.
//! - **The digest** ([`digest`]): every day from
//!   `ROMULUS_OPERATOR_DIGEST_HOUR` (UTC, 7 by default), the day recorded
//!   (`operator_digests`) in the transaction that queues it: once a day,
//!   whatever the processes.
//!
//! `romulus-api operator-test` sends a test notification and the digest at
//! once ([`send_test`]).

pub mod accounts;
pub mod billing;
pub mod digest;
pub mod limits;

use std::{fmt::Write as _, time::Duration};

use chrono::{DateTime, NaiveDate, Timelike as _, Utc};
use sha2::{Digest as _, Sha256};
use sqlx::PgConnection;

use crate::{
    AppState,
    config::{OperatorConfig, Secret, StripeKeys},
    domain::Interval,
    mail::{Email, MailError, escape},
};

/// Before every subject.
pub const PREFIX: &str = "[romulus]";
/// How often the operator task looks whether the day's digest is due.
pub const DIGEST_EVERY: Duration = Duration::from_secs(60);

const SUBJECT_MAX: usize = 300;
const BODY_MAX: usize = 60_000;
/// `mail_outbox.key`'s limit.
const KEY_MAX: usize = 500;
/// What a customer wrote (a cancellation's comment) is cut to this.
pub(crate) const QUOTE_MAX: usize = 500;

/// What a notification reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    AccountCreated,
    OrgCreated,
    SubscriptionStarted,
    /// Upgraded or downgraded, or a change scheduled for the period's end.
    PlanChanged,
    CancellationScheduled,
    SubscriptionCancelled,
    PaymentFailed,
    /// The grace period after a failed payment ended: on Free.
    GraceEnded,
    /// Paid again, on the same subscription, after the grace period ended:
    /// back from Free.
    Recovered,
    /// A consumer withdrew (and is refunded the days not used).
    Withdrawal,
    AccountDeleted,
    OrgDeleted,
    /// An organization reached a cap of its plan (a 402 `plan_limit`).
    PlanLimitReached,
    Digest,
    /// `romulus-api operator-test`'s message: sent at once, never queued.
    Test,
}

impl Event {
    /// For logs, the mailer (`Email::kind`) and the outbox's key.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::AccountCreated => "operator_account_created",
            Self::OrgCreated => "operator_org_created",
            Self::SubscriptionStarted => "operator_subscription_started",
            Self::PlanChanged => "operator_plan_changed",
            Self::CancellationScheduled => "operator_cancellation_scheduled",
            Self::SubscriptionCancelled => "operator_subscription_cancelled",
            Self::PaymentFailed => "operator_payment_failed",
            Self::GraceEnded => "operator_grace_ended",
            Self::Recovered => "operator_recovered",
            Self::Withdrawal => "operator_withdrawal",
            Self::AccountDeleted => "operator_account_deleted",
            Self::OrgDeleted => "operator_org_deleted",
            Self::PlanLimitReached => "operator_plan_limit_reached",
            Self::Digest => "operator_digest",
            Self::Test => "operator_test",
        }
    }
}

/// One notification, as it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub event: Event,
    /// What it is about (a user, a subscription, an invoice, a day): queued
    /// once per event, key and operator.
    pub about: String,
    /// After [`PREFIX`].
    pub subject: String,
    /// The body, a line each.
    pub lines: Vec<String>,
    /// After the lines, a line each: the only links in the email.
    pub links: Vec<Link>,
}

/// A link the code built (never one from what someone typed): `label: url`
/// in the text, the URL a link in the HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub label: &'static str,
    pub url: String,
}

/// A value from outside (a name, an address, a business's name, a comment)
/// on one line: control characters, newlines included, become spaces.
pub(crate) fn flat(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Queues `notice` for each operator (`to`: `Config::operator`) in the
/// caller's transaction, to be sent by the mail outbox once it commits;
/// nothing when notifications are off.
pub async fn notify(
    conn: &mut PgConnection,
    to: Option<&OperatorConfig>,
    notice: &Notice,
) -> Result<(), sqlx::Error> {
    let Some(to) = to else {
        return Ok(());
    };
    for address in &to.emails {
        let email = email(&to.origin, address, notice);
        crate::mail::outbox::queue(conn, &key(notice, address), &email).await?;
    }
    Ok(())
}

/// The outbox key of `notice` to `to`: its kind, what it is about and the
/// address, hashed past the column's limit.
fn key(notice: &Notice, to: &str) -> String {
    let kind = notice.event.kind();
    let to = crate::email::key(to);
    let key = format!("{kind}:{}:{to}", notice.about);
    if key.len() <= KEY_MAX {
        return key;
    }
    let digest = Sha256::digest(format!("{}\n{to}", notice.about));
    format!("{kind}:sha256:{}", crate::collab::store::hex(&digest))
}

/// A notification as an email to `to`: its lines, its links, and why it
/// arrived (`origin`: the install's), in text and in a minimal HTML version
/// (monospace; the links of [`Notice::links`] and the origin are the only
/// links). The subject and every line are made [`flat`] once more here.
pub(crate) fn email(origin: &str, to: &str, notice: &Notice) -> Email {
    let subject = clip(&flat(&format!("{PREFIX} {}", notice.subject)), SUBJECT_MAX);
    let lines: Vec<String> = notice.lines.iter().map(|line| flat(line)).collect();
    let body = clip(&lines.join("\n"), BODY_MAX);
    let mut text = body.clone();
    let mut html = escape(&body);
    for link in &notice.links {
        let _ = write!(text, "\n{}: {}", link.label, link.url);
        let _ = write!(
            html,
            "\n{}: <a href=\"{url}\" style=\"color:#b4532a;\">{url}</a>",
            escape(link.label),
            url = escape(&link.url),
        );
    }
    let footer = format!("Sent by romulus at {origin} to its operators (ROMULUS_OPERATOR_EMAILS).");
    let origin_link = format!(
        "<a href=\"{url}\" style=\"color:#57534e;\">{url}</a>",
        url = escape(origin)
    );
    let html = format!(
        "<!doctype html><html><body style=\"margin:0;padding:16px;background:#ffffff;color:#1c1917;\">\
         <pre style=\"margin:0;font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
         white-space:pre-wrap;word-break:break-word;\">{html}</pre>\
         <p style=\"margin:16px 0 0;font:12px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,\
         Helvetica,Arial,sans-serif;color:#57534e;\">Sent by romulus at {origin_link} to its operators \
         (ROMULUS_OPERATOR_EMAILS).</p></body></html>",
    );
    Email {
        to: to.to_owned(),
        kind: notice.event.kind().into(),
        subject,
        text: format!("{text}\n\n--\n{footer}\n"),
        html,
        attachments: Vec::new(),
    }
}

/// `text` cut to at most `max` characters, an ellipsis saying so.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `Name <address>`, [`flat`].
pub(crate) fn who(name: &str, email: &str) -> String {
    format!("{} <{}>", flat(name), flat(email))
}

/// `Name (slug)`, [`flat`].
pub(crate) fn org_label(name: &str, slug: &str) -> String {
    format!("{} ({})", flat(name), flat(slug))
}

/// `October 21, 2026`.
pub(crate) fn day(at: DateTime<Utc>) -> String {
    at.format("%B %-d, %Y").to_string()
}

/// What a price comes to in a month: a yearly one divided by 12.
pub(crate) fn monthly(cents: i64, interval: Interval) -> i64 {
    match interval {
        Interval::Month => cents,
        Interval::Year => cents / 12,
    }
}

/// `€49.00 a month`, `€490.00 a year (€40.83 a month)`.
pub(crate) fn price(cents: i64, currency: &str, interval: Interval) -> String {
    let money = |cents| crate::billing::money(cents, currency);
    match interval {
        Interval::Month => format!("{} a month", money(cents)),
        Interval::Year => format!(
            "{} a year ({} a month)",
            money(cents),
            money(monthly(cents, interval))
        ),
    }
}

/// Links into Stripe's dashboard, in test or live mode as the key is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dashboard {
    base: &'static str,
}

impl Dashboard {
    /// From billing's keys: live for a live key, test otherwise (a test key,
    /// the fake). `None` with billing off.
    pub fn of(state: &AppState) -> Option<Self> {
        let inner = state.billing.inner()?;
        Some(Self::for_keys(&inner.config.stripe))
    }

    pub fn for_keys(keys: &StripeKeys) -> Self {
        let live = match keys {
            StripeKeys::Stripe { secret_key, .. } => is_live_key(secret_key),
            StripeKeys::Fake { .. } => false,
        };
        Self {
            base: if live {
                "https://dashboard.stripe.com/"
            } else {
                "https://dashboard.stripe.com/test/"
            },
        }
    }

    /// `…/customers/<id>`; `None` for an id that is not one of Stripe's
    /// (letters, digits, `_`).
    pub fn customer(self, id: &str) -> Option<Link> {
        self.link("Stripe customer", "customers", id)
    }

    pub fn subscription(self, id: &str) -> Option<Link> {
        self.link("Stripe subscription", "subscriptions", id)
    }

    pub fn invoice(self, id: &str) -> Option<Link> {
        self.link("Stripe invoice", "invoices", id)
    }

    fn link(self, label: &'static str, kind: &str, id: &str) -> Option<Link> {
        let stripes = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        stripes.then(|| Link {
            label,
            url: format!("{}{kind}/{id}", self.base),
        })
    }

    /// The customer's and the subscription's, for what is known.
    pub(crate) fn links(self, customer: Option<&str>, subscription: Option<&str>) -> Vec<Link> {
        customer
            .and_then(|id| self.customer(id))
            .into_iter()
            .chain(subscription.and_then(|id| self.subscription(id)))
            .collect()
    }
}

fn is_live_key(key: &Secret) -> bool {
    let key = key.expose();
    key.starts_with("sk_live_") || key.starts_with("rk_live_")
}

/// The operator task, when notifications are on: the day's digest, queued
/// once due (looked at every [`DIGEST_EVERY`]); the mail outbox sends it.
/// Empty when they are off.
pub fn tasks(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    let Some(operator) = state.config.operator.clone() else {
        return Vec::new();
    };
    tracing::info!(
        operators = operator.emails.len(),
        digest_hour = operator.digest_hour,
        "operator notifications are on"
    );
    let state = state.clone();
    vec![tokio::spawn(async move {
        let mut tick = tokio::time::interval(DIGEST_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut done: Option<NaiveDate> = None;
        loop {
            tick.tick().await;
            let now = Utc::now();
            if now.hour() < operator.digest_hour || done == Some(now.date_naive()) {
                continue;
            }
            match digest::due(&state, now).await {
                Ok(queued) => {
                    done = Some(now.date_naive());
                    if queued {
                        state.mailer.wake();
                    }
                }
                Err(err) => tracing::warn!(error = %err, "operator: the digest failed"),
            }
        }
    })]
}

/// What `romulus-api operator-test` did for one operator: the test
/// notification, then the digest.
pub struct Tested {
    pub to: String,
    pub notice: Result<(), MailError>,
    pub digest: Result<(), MailError>,
}

/// Sends a test notification and today's digest to every operator now,
/// waiting for the mail server's answer each time. Nothing is recorded:
/// the day's digest still goes at its hour.
pub async fn send_test(state: &AppState) -> Result<Vec<Tested>, sqlx::Error> {
    let now = Utc::now();
    let mut conn = state.db.acquire().await?;
    let today = digest::compute(&mut conn, state, now.date_naive(), now).await?;
    let previous = digest::previous(&mut conn, now.date_naive()).await?;
    let origin = state.config.public_origin();
    let summary = digest::notice(&today, previous.as_ref(), &origin);
    let hour = state
        .config
        .operator
        .as_ref()
        .map_or(OperatorConfig::DEFAULT_DIGEST_HOUR, |o| o.digest_hour);
    let test = Notice {
        event: Event::Test,
        about: String::new(),
        subject: "Test notification".to_owned(),
        lines: vec![
            format!(
                "This is a test from romulus at {origin}: notifications to its operators arrive like this one."
            ),
            format!(
                "They go to {}, the digest every day from {hour}:00 UTC.",
                state.config.operators().join(", "),
            ),
        ],
        links: Vec::new(),
    };
    let mut tested = Vec::new();
    for to in state.config.operators() {
        let notice = state.mailer.send_now(&email(&origin, to, &test)).await;
        let digest = state.mailer.send_now(&email(&origin, to, &summary)).await;
        tested.push(Tested {
            to: to.clone(),
            notice,
            digest,
        });
    }
    Ok(tested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_read_per_month_and_a_year_says_its_month_too() {
        assert_eq!(price(4_900, "eur", Interval::Month), "€49.00 a month");
        assert_eq!(
            price(49_000, "eur", Interval::Year),
            "€490.00 a year (€40.83 a month)"
        );
        assert_eq!(monthly(49_000, Interval::Year), 4_083);
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("ééééé", 3), "éé…");
    }

    fn notice(body: &[&str], stripe: Vec<Link>) -> Notice {
        Notice {
            event: Event::AccountCreated,
            about: "u_1".into(),
            subject: "New account: <b>".into(),
            lines: body.iter().map(|&line| line.to_owned()).collect(),
            links: stripe,
        }
    }

    #[test]
    fn values_from_outside_stay_on_their_line() {
        assert_eq!(
            flat("Acme\nStripe customer:\r\tx\u{2028}y\u{85}z"),
            "Acme Stripe customer:  x y z"
        );
        assert_eq!(
            who("Ada\n\nRefund approved", "ada@example.com"),
            "Ada  Refund approved <ada@example.com>"
        );
        assert_eq!(org_label("Acme\r\nInc", "acme"), "Acme  Inc (acme)");
    }

    #[test]
    fn the_html_escapes_the_text_and_links_only_what_the_code_built() {
        let dashboard = Dashboard::for_keys(&StripeKeys::Fake {
            webhook_secret: None,
        });
        // A name carrying a line and a link of its own.
        let forged = "Mallory\nStripe customer: https://evil.example/pay https://dashboard.stripe.com/test/customers/cus_x";
        let email = email(
            "https://romulus.example",
            "ops@example.com",
            &notice(
                &[&format!("Name: <script>x</script> & co, {forged}")],
                dashboard.links(Some("cus_1"), None),
            ),
        );
        assert_eq!(email.subject, "[romulus] New account: <b>");
        assert_eq!(email.kind, "operator_account_created");
        let text_lines: Vec<&str> = email.text.lines().collect();
        assert_eq!(
            text_lines[..2],
            [
                "Name: <script>x</script> & co, Mallory Stripe customer: https://evil.example/pay https://dashboard.stripe.com/test/customers/cus_x",
                "Stripe customer: https://dashboard.stripe.com/test/customers/cus_1",
            ]
        );
        assert!(email.text.ends_with(
            "Sent by romulus at https://romulus.example to its operators (ROMULUS_OPERATOR_EMAILS).\n"
        ));
        assert!(!email.html.contains("<script>"));
        assert!(
            email
                .html
                .contains("&lt;script&gt;x&lt;/script&gt; &amp; co")
        );
        let hrefs: Vec<&str> = email
            .html
            .split("href=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert_eq!(
            hrefs,
            [
                "https://dashboard.stripe.com/test/customers/cus_1",
                "https://romulus.example"
            ]
        );
    }

    #[test]
    fn a_notice_is_keyed_by_its_event_what_it_is_about_and_the_address() {
        let mut notice = Notice {
            event: Event::PaymentFailed,
            about: "in_1".into(),
            subject: String::new(),
            lines: Vec::new(),
            links: Vec::new(),
        };
        assert_eq!(
            key(&notice, "Ops@Example.com"),
            "operator_payment_failed:in_1:ops@example.com"
        );
        assert_eq!(
            key(&notice, "ops@example.com"),
            key(&notice, "OPS@example.com")
        );
        notice.about = "x".repeat(600);
        let long = key(&notice, "ops@example.com");
        assert!(long.starts_with("operator_payment_failed:sha256:"));
        assert!(long.len() <= KEY_MAX);
        assert_ne!(long, key(&notice, "founder@example.com"));
    }

    #[test]
    fn the_dashboard_links_follow_the_key_mode_and_take_stripes_ids_only() {
        let url = |link: Option<Link>| link.map(|link| link.url);
        let live = Dashboard::for_keys(&StripeKeys::Stripe {
            secret_key: Secret::new("sk_live_abc"),
            webhook_secret: Secret::new("whsec_x"),
        });
        assert_eq!(
            url(live.customer("cus_1")).as_deref(),
            Some("https://dashboard.stripe.com/customers/cus_1")
        );
        let test = Dashboard::for_keys(&StripeKeys::Stripe {
            secret_key: Secret::new("rk_test_abc"),
            webhook_secret: Secret::new("whsec_x"),
        });
        assert_eq!(
            url(test.subscription("sub_1")).as_deref(),
            Some("https://dashboard.stripe.com/test/subscriptions/sub_1")
        );
        let fake = Dashboard::for_keys(&StripeKeys::Fake {
            webhook_secret: None,
        });
        assert_eq!(
            url(fake.invoice("in_1")).as_deref(),
            Some("https://dashboard.stripe.com/test/invoices/in_1")
        );
        assert_eq!(fake.invoice("in_1/../../evil"), None);
        assert_eq!(fake.customer(""), None);
    }
}
