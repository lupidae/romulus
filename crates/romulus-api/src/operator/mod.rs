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
//!   a failed payment, a grace period over, a consumer's withdrawal
//!   ([`billing`]), an account or organization deleted (on request or for
//!   inactivity), a plan's cap reached ([`limits`]: once per organization
//!   and cap in 30 days). Each says which organization, which plan, who,
//!   and links to Stripe's dashboard where there is something to look at.
//!   Personal data: names, addresses and a business's name only.
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

use std::time::Duration;

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
    /// A consumer withdrew (and is refunded the days not used).
    Withdrawal,
    AccountDeleted,
    OrgDeleted,
    /// An organization reached a cap of its plan (a 402 `plan_limit`).
    PlanLimitReached,
    Digest,
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
            Self::Withdrawal => "operator_withdrawal",
            Self::AccountDeleted => "operator_account_deleted",
            Self::OrgDeleted => "operator_org_deleted",
            Self::PlanLimitReached => "operator_plan_limit_reached",
            Self::Digest => "operator_digest",
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
    let subject = clip(&format!("{PREFIX} {}", notice.subject), SUBJECT_MAX);
    let body = clip(&notice.lines.join("\n"), BODY_MAX);
    for address in &to.emails {
        let email = email(&to.origin, notice.event.kind(), address, &subject, &body);
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

/// A notification as an email: its text and why it arrived (`origin`: the
/// install's), and the same in a minimal HTML version (monospace, links
/// clickable).
pub(crate) fn email(
    origin: &str,
    kind: &'static str,
    to: &str,
    subject: &str,
    body: &str,
) -> Email {
    let footer = format!("Sent by romulus at {origin} to its operators (ROMULUS_OPERATOR_EMAILS).");
    let html = format!(
        "<!doctype html><html><body style=\"margin:0;padding:16px;background:#ffffff;color:#1c1917;\">\
         <pre style=\"margin:0;font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
         white-space:pre-wrap;word-break:break-word;\">{}</pre>\
         <p style=\"margin:16px 0 0;font:12px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,\
         Helvetica,Arial,sans-serif;color:#57534e;\">{}</p></body></html>",
        linked(body),
        escape(&footer),
    );
    Email {
        to: to.to_owned(),
        kind: kind.into(),
        subject: subject.to_owned(),
        text: format!("{body}\n\n--\n{footer}\n"),
        html,
        attachments: Vec::new(),
    }
}

/// `text` as HTML, its http(s) URLs as links.
fn linked(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            line.split(' ')
                .map(|word| {
                    let escaped = escape(word);
                    if word.starts_with("https://") || word.starts_with("http://") {
                        format!("<a href=\"{escaped}\" style=\"color:#b4532a;\">{escaped}</a>")
                    } else {
                        escaped
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// `Name <address>`.
pub(crate) fn who(name: &str, email: &str) -> String {
    format!("{name} <{email}>")
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

    pub fn customer(self, id: &str) -> String {
        format!("{}customers/{id}", self.base)
    }

    pub fn subscription(self, id: &str) -> String {
        format!("{}subscriptions/{id}", self.base)
    }

    pub fn invoice(self, id: &str) -> String {
        format!("{}invoices/{id}", self.base)
    }

    /// A `Stripe customer: …` and a `Stripe subscription: …` line, for what
    /// is known.
    pub(crate) fn lines(self, customer: Option<&str>, subscription: Option<&str>) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(customer) = customer {
            lines.push(format!("Stripe customer: {}", self.customer(customer)));
        }
        if let Some(subscription) = subscription {
            lines.push(format!(
                "Stripe subscription: {}",
                self.subscription(subscription)
            ));
        }
        lines
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
    let body = [
        format!(
            "This is a test from romulus at {origin}: notifications to its operators arrive like this one."
        ),
        format!(
            "They go to {}, the digest every day from {hour}:00 UTC.",
            state.config.operators().join(", "),
        ),
    ]
    .join("\n");
    let mut tested = Vec::new();
    for to in state.config.operators() {
        let test = email(
            &origin,
            "operator_test",
            to,
            &format!("{PREFIX} Test notification"),
            &body,
        );
        let digest = email(
            &origin,
            Event::Digest.kind(),
            to,
            &format!("{PREFIX} {}", summary.subject),
            &summary.lines.join("\n"),
        );
        let notice = state.mailer.send_now(&test).await;
        let digest = state.mailer.send_now(&digest).await;
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

    #[test]
    fn the_html_escapes_the_text_and_links_its_urls() {
        let email = email(
            "https://romulus.example",
            "operator_test",
            "ops@example.com",
            "[romulus] New account: <b>",
            "Name: <script>x</script> & co\nStripe: https://dashboard.stripe.com/test/customers/cus_1",
        );
        assert!(email.text.starts_with("Name: <script>x</script> & co\n"));
        assert!(email.text.ends_with(
            "Sent by romulus at https://romulus.example to its operators (ROMULUS_OPERATOR_EMAILS).\n"
        ));
        assert!(!email.html.contains("<script>"));
        assert!(
            email
                .html
                .contains("&lt;script&gt;x&lt;/script&gt; &amp; co")
        );
        assert!(
            email
                .html
                .contains("<a href=\"https://dashboard.stripe.com/test/customers/cus_1\"")
        );
    }

    #[test]
    fn a_notice_is_keyed_by_its_event_what_it_is_about_and_the_address() {
        let mut notice = Notice {
            event: Event::PaymentFailed,
            about: "in_1".into(),
            subject: String::new(),
            lines: Vec::new(),
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
    fn the_dashboard_links_follow_the_key_mode() {
        let live = Dashboard::for_keys(&StripeKeys::Stripe {
            secret_key: Secret::new("sk_live_abc"),
            webhook_secret: Secret::new("whsec_x"),
        });
        assert_eq!(
            live.customer("cus_1"),
            "https://dashboard.stripe.com/customers/cus_1"
        );
        let test = Dashboard::for_keys(&StripeKeys::Stripe {
            secret_key: Secret::new("rk_test_abc"),
            webhook_secret: Secret::new("whsec_x"),
        });
        assert_eq!(
            test.subscription("sub_1"),
            "https://dashboard.stripe.com/test/subscriptions/sub_1"
        );
        let fake = Dashboard::for_keys(&StripeKeys::Fake {
            webhook_secret: None,
        });
        assert_eq!(
            fake.invoice("in_1"),
            "https://dashboard.stripe.com/test/invoices/in_1"
        );
    }
}
