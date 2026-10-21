//! Email: what romulus sends (sign-up links, password resets, invitations,
//! security notices, billing and contract letters) and how
//! (`ROMULUS_SMTP_*`, `Config::mail`). A letter may carry attachments (the
//! Terms as a PDF, a durable medium) and one framed line (a deadline).
//!
//! Sending never holds up or fails the request that asked for it: a message
//! goes out from its own task, and a failure is logged (never the address).
//! That also keeps answers the same whether a message went out or not, which
//! the sign-up and reset endpoints rely on to say nothing about who has an
//! account. Without `ROMULUS_SMTP_HOST` the mailer is off and nothing is
//! sent; tests use [`Mailer::memory`] and read what would have been.
//!
//! Contract and billing letters go through [`outbox`] instead: queued with
//! what decides them, marked sent once the mail server accepted them, tried
//! again until it does.

pub mod outbox;

use std::{
    borrow::Cow,
    fmt::Write as _,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Attachment as Part, Mailbox, MultiPart, SinglePart, header::ContentType},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use tokio::sync::Notify;

use crate::{
    Config,
    config::{MailConfig, SmtpTls},
    error::Chain,
};

/// What one message says. Built by [`Letter`]; `to` is the address as
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    /// What kind of message this is, for logs (never the address).
    pub kind: Cow<'static, str>,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub attachments: Vec<Attachment>,
}

/// A file sent with a message (the Terms as a PDF): embedded in the binary,
/// so attaching it never fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    /// `application/pdf`.
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

/// Sends email, or does not. Cheap to clone.
#[derive(Clone)]
pub struct Mailer {
    inner: Inner,
    /// Wakes the outbox's worker: a letter was queued.
    wake: Arc<Notify>,
}

#[derive(Clone)]
enum Inner {
    Off,
    Smtp(Arc<Smtp>),
    Memory(Arc<Memory>),
}

#[derive(Default)]
struct Memory {
    emails: Mutex<Vec<Email>>,
    /// How many of the next messages the "server" refuses (tests).
    refuse: AtomicUsize,
}

struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

/// What a [`Mailer::memory`] was asked to send.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Memory>);

impl Outbox {
    /// Every message so far, oldest first.
    pub fn all(&self) -> Vec<Email> {
        self.0
            .emails
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The next `n` messages are refused, as a server down would (tests of
    /// what retries).
    pub fn refuse_next(&self, n: usize) {
        self.0.refuse.store(n, Ordering::SeqCst);
    }

    /// The messages sent to `to` (compared as addresses are: `email::key`).
    pub fn to(&self, to: &str) -> Vec<Email> {
        let key = crate::email::key(to);
        self.all()
            .into_iter()
            .filter(|email| crate::email::key(&email.to) == key)
            .collect()
    }
}

impl Mailer {
    /// From `Config::mail`: SMTP, or off.
    pub fn from_config(config: &Config) -> Self {
        match &config.mail {
            None => Self::new(Inner::Off),
            Some(mail) => Self::new(Inner::Smtp(Arc::new(Smtp {
                transport: transport(mail),
                from: mail.from.clone(),
            }))),
        }
    }

    fn new(inner: Inner) -> Self {
        Self {
            inner,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Sends nothing.
    pub fn off() -> Self {
        Self::new(Inner::Off)
    }

    /// Keeps every message in memory instead of sending it (tests).
    pub fn memory() -> (Self, Outbox) {
        let outbox = Outbox::default();
        (Self::new(Inner::Memory(outbox.0.clone())), outbox)
    }

    /// Whether messages go anywhere: the app offers what needs them (a
    /// reset by email) only then.
    pub fn is_on(&self) -> bool {
        !matches!(self.inner, Inner::Off)
    }

    /// Tells the outbox's worker a letter waits ([`outbox::queue`]).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Until [`Mailer::wake`] is called.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    /// Sends `email` in the background. Returns at once, whatever happens.
    pub fn send(&self, email: Email) {
        match &self.inner {
            Inner::Off => tracing::debug!(kind = %email.kind, "mail: off, not sent"),
            Inner::Memory(memory) => memory
                .emails
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(email),
            Inner::Smtp(smtp) => {
                let message = match message(&smtp.from, &email) {
                    Ok(message) => message,
                    Err(err) => {
                        tracing::warn!(kind = %email.kind, error = %Chain(&err), "mail: cannot build the message");
                        return;
                    }
                };
                let smtp = smtp.clone();
                let kind = email.kind;
                tokio::spawn(async move {
                    match smtp.transport.send(message).await {
                        Ok(_) => tracing::info!(kind = %kind, "mail: sent"),
                        Err(err) => {
                            tracing::warn!(kind = %kind, error = %Chain(&err), "mail: not sent");
                        }
                    }
                });
            }
        }
    }

    /// Connects and says hello, for `romulus-api mail-check`: whether the
    /// settings work, before anyone needs a message.
    pub async fn check(&self) -> Result<bool, lettre::transport::smtp::Error> {
        match &self.inner {
            Inner::Smtp(smtp) => smtp.transport.test_connection().await,
            Inner::Off | Inner::Memory(_) => Ok(false),
        }
    }

    /// Sends one message and waits for the server's answer (the CLI's test
    /// message, the outbox).
    pub async fn send_now(&self, email: &Email) -> Result<(), MailError> {
        match &self.inner {
            Inner::Off => Err(MailError::Off),
            Inner::Memory(memory) => {
                let refused = memory
                    .refuse
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if refused {
                    return Err(MailError::Refused);
                }
                memory
                    .emails
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(email.clone());
                Ok(())
            }
            Inner::Smtp(smtp) => {
                smtp.transport.send(message(&smtp.from, email)?).await?;
                Ok(())
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("mail is off: set ROMULUS_SMTP_HOST and ROMULUS_MAIL_FROM")]
    Off,
    #[error("the message")]
    Message(#[from] lettre::error::Error),
    #[error("the recipient's address")]
    Address(#[from] lettre::address::AddressError),
    #[error("the SMTP server")]
    Smtp(#[from] lettre::transport::smtp::Error),
    #[error("an attachment's content type: {0}")]
    ContentType(&'static str),
    /// A test mailer told to refuse ([`Outbox::refuse_next`]).
    #[error("the (test) server refused the message")]
    Refused,
}

fn transport(mail: &MailConfig) -> AsyncSmtpTransport<Tokio1Executor> {
    let tls = || {
        TlsParameters::new(mail.host.clone())
            .expect("TLS parameters for a host name always build with webpki roots")
    };
    let mut builder =
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&mail.host).port(mail.port);
    builder = match mail.tls {
        SmtpTls::StartTls => builder.tls(Tls::Required(tls())),
        SmtpTls::Implicit => builder.tls(Tls::Wrapper(tls())),
        SmtpTls::None => builder.tls(Tls::None),
    };
    if let Some((user, password)) = &mail.credentials {
        builder = builder.credentials(Credentials::new(user.clone(), password.expose().to_owned()));
    }
    builder
        .timeout(Some(std::time::Duration::from_secs(20)))
        .build()
}

fn message(from: &Mailbox, email: &Email) -> Result<Message, MailError> {
    let body = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(email.text.clone()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(email.html.clone()),
        );
    let builder = Message::builder()
        .from(from.clone())
        .to(Mailbox::new(None, email.to.parse()?))
        .subject(&email.subject);
    if email.attachments.is_empty() {
        return Ok(builder.multipart(body)?);
    }
    let mut mixed = MultiPart::mixed().multipart(body);
    for attachment in &email.attachments {
        mixed = mixed.singlepart(
            Part::new(attachment.filename.clone()).body(
                attachment.bytes.to_vec(),
                ContentType::parse(attachment.content_type)
                    .map_err(|_| MailError::ContentType(attachment.content_type))?,
            ),
        );
    }
    Ok(builder.multipart(mixed)?)
}

/// A message in romulus's one layout: a few paragraphs, at most one button
/// (whose link is also spelled out, for clients that drop it), and why it
/// was sent. Text and HTML say the same thing.
pub struct Letter<'a> {
    pub kind: &'static str,
    pub to: &'a str,
    pub subject: String,
    pub paragraphs: Vec<String>,
    pub action: Option<(&'a str, &'a str)>,
    /// Below a rule: why this arrived, what to do if it was not expected.
    pub footer: String,
}

/// What a [`Letter`] may add to the one layout: its language, one framed
/// line after the paragraphs (a deadline the reader must not miss), files.
#[derive(Debug, Default)]
pub struct Extras {
    /// The `lang` of the HTML (`en` when empty).
    pub lang: &'static str,
    /// Shown in a box after the paragraphs, before the button.
    pub framed: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl Letter<'_> {
    pub fn email(self) -> Email {
        self.email_with(Extras::default())
    }

    pub fn email_with(self, extras: Extras) -> Email {
        let mut text = String::new();
        let mut html = format!(
            "<!doctype html><html lang=\"{}\"><body style=\"margin:0;padding:24px;background:#f6f5f3;\">\
             <div style=\"max-width:520px;margin:0 auto;background:#ffffff;border:1px solid #e7e5e2;\
             border-radius:8px;padding:32px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',\
             Inter,Roboto,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.55;color:#1c1917;\">\
             <p style=\"margin:0 0 24px;font-weight:600;font-size:16px;\">romulus</p>",
            if extras.lang.is_empty() {
                "en"
            } else {
                extras.lang
            },
        );
        for paragraph in &self.paragraphs {
            text.push_str(paragraph);
            text.push_str("\n\n");
            html.push_str("<p style=\"margin:0 0 16px;\">");
            html.push_str(&escape(paragraph));
            html.push_str("</p>");
        }
        if let Some(framed) = &extras.framed {
            let rule = "=".repeat(framed.chars().count().min(60));
            let _ = write!(text, "{rule}\n{framed}\n{rule}\n\n");
            let _ = write!(
                html,
                "<p style=\"margin:24px 0;padding:14px 16px;border:2px solid #1c1917;border-radius:6px;\
                 font-weight:600;font-size:16px;\">{}</p>",
                escape(framed)
            );
        }
        if let Some((label, url)) = self.action {
            text.push_str(url);
            text.push_str("\n\n");
            let _ = write!(
                html,
                "<p style=\"margin:24px 0;\"><a href=\"{url}\" style=\"display:inline-block;\
                 background:#b4532a;color:#ffffff;text-decoration:none;font-weight:600;\
                 padding:10px 18px;border-radius:6px;\">{label}</a></p>\
                 <p style=\"margin:0 0 16px;font-size:13px;color:#57534e;word-break:break-all;\">\
                 Or open this link: {url}</p>",
                url = escape(url),
                label = escape(label),
            );
        }
        text.push_str("--\n");
        text.push_str(&self.footer);
        text.push('\n');
        let _ = write!(
            html,
            "<hr style=\"border:0;border-top:1px solid #e7e5e2;margin:24px 0 16px;\">\
             <p style=\"margin:0;font-size:13px;color:#57534e;\">{}</p></div></body></html>",
            escape(&self.footer)
        );
        Email {
            to: self.to.to_owned(),
            kind: Cow::Borrowed(self.kind),
            subject: self.subject,
            text,
            html,
            attachments: extras.attachments,
        }
    }
}

/// Text into HTML: the five characters that could start markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_say_the_same_in_text_and_html_and_escape_names() {
        let email = Letter {
            kind: "test",
            to: "bob@x.io",
            subject: "Hello".to_owned(),
            paragraphs: vec!["<script>alert(1)</script> invited you.".to_owned()],
            action: Some(("Join", "https://r.example/app/invite/abc?x=1&y=2")),
            footer: "You got this because…".to_owned(),
        }
        .email();
        assert!(
            email
                .text
                .contains("<script>alert(1)</script> invited you.")
        );
        assert!(
            email
                .text
                .contains("https://r.example/app/invite/abc?x=1&y=2")
        );
        assert!(!email.html.contains("<script>"));
        assert!(email.html.contains("&lt;script&gt;"));
        assert!(
            email
                .html
                .contains("href=\"https://r.example/app/invite/abc?x=1&amp;y=2\"")
        );
    }

    #[test]
    fn a_memory_mailer_keeps_what_it_is_given() {
        let (mailer, outbox) = Mailer::memory();
        assert!(mailer.is_on());
        mailer.send(Email {
            to: "Bob@X.io".to_owned(),
            kind: "test".into(),
            subject: "s".to_owned(),
            text: "t".to_owned(),
            html: "h".to_owned(),
            attachments: Vec::new(),
        });
        assert_eq!(outbox.to("bob@x.io").len(), 1);
        assert!(!Mailer::off().is_on());
    }

    #[tokio::test]
    async fn messages_build_with_a_display_name_sender() {
        let from: Mailbox = "romulus <romulus@example.com>".parse().unwrap();
        let email = Letter {
            kind: "test",
            to: "élodie@x.io",
            subject: "Réinitialiser".to_owned(),
            paragraphs: vec!["Bonjour".to_owned()],
            action: None,
            footer: "f".to_owned(),
        }
        .email();
        let formatted = String::from_utf8(message(&from, &email).unwrap().formatted()).unwrap();
        assert!(formatted.contains("From: romulus <romulus@example.com>"));
        assert!(formatted.contains("multipart/alternative"));
    }
}
