//! Configuration, read from the environment only.
//!
//! In development `main` first loads `.env` (written per worktree by
//! `scripts/dev-env`) and `.dev/zitadel/oidc.env` (written by the Zitadel
//! provisioner); variables already set always win.

use std::{collections::BTreeMap, env, fmt, net::SocketAddr, path::PathBuf};

use url::Url;

use crate::domain::PgMajor;

#[derive(Debug, Clone)]
pub struct Config {
    /// `DATABASE_URL`: the application database.
    pub database_url: String,
    /// `ROMULUS_API_ADDR`: where the API listens.
    pub api_addr: SocketAddr,
    /// `ROMULUS_PUBLIC_URL`: the origin the browser uses (the web app, which
    /// proxies `/api`). Used for `Origin` checks and redirect URLs.
    pub public_url: Url,
    /// An admin connection to each major's sandbox Postgres:
    /// `ROMULUS_SANDBOX_URL` for the default major (17),
    /// `ROMULUS_SANDBOX_URL_PG15`, `…_PG16`, `…_PG18` for the others. A
    /// major without one cannot compile (503), the others work.
    pub sandbox_urls: BTreeMap<PgMajor, String>,
    /// `ROMULUS_SANDBOX_PREFIX`: names of this process's sandbox roles and
    /// databases (`<prefix>_<n>`), `romulus_sandbox` by default. One process
    /// per prefix: a second API on the same sandbox (Playwright's) needs its
    /// own.
    pub sandbox_prefix: String,
    /// `ROMULUS_OIDC_*`: `None` until the Zitadel provisioner has run.
    pub oidc: Option<OidcConfig>,
    /// `ROMULUS_SESSION_COOKIE`: per stack, because cookies are not
    /// port-scoped and every worktree's stack lives on localhost.
    pub session_cookie: String,
    /// `ROMULUS_DEV_SIGNIN=true`: the dev-only sign-in bypass (the auth
    /// module's route), so QA and end-to-end tests do not drive Zitadel's UI.
    /// Refused at startup unless `public_url` is a loopback host, so a
    /// deployed instance can never be started with it.
    pub dev_signin: bool,
    /// `ROMULUS_WEB_DIR`: the web app's production build (`web/dist`), served
    /// at `/app` (the `web` module). Unset in development, where Vite serves
    /// it; the container image sets it.
    pub web_dir: Option<PathBuf>,
    /// `ROMULUS_EDGE_KEY`: when the API is reached through an edge proxy
    /// (the preview's Pages Function), the secret that proxy sends; requests
    /// without it are refused (the `edge` module). At least 32 characters.
    pub edge_key: Option<Secret>,
    /// `ROMULUS_PLAY_URL` and `ROMULUS_PLAY_KEY`: the playground service on
    /// the private link, and the key the API signs its calls with (at least
    /// 32 characters, the service holds the same). Both or neither; neither:
    /// playgrounds are off.
    pub playground: Option<PlaygroundConfig>,
    /// `ROMULUS_AUTH`: how people sign in, a comma-separated list of `oidc`
    /// (an identity provider, the `ROMULUS_OIDC_*` client) and `password`
    /// (romulus's own accounts, `auth::password`). Default `oidc`.
    pub auth: AuthConfig,
    /// `ROMULUS_SIGNUP`: who may create a password account. `invite` (the
    /// default): only from an invitation link, with the invited address.
    /// `open`: anyone, the address proved by email (needs mail).
    pub signup: SignUpPolicy,
    /// `ROMULUS_SINGLE_ORG=true`: one organization per install (the free
    /// self-hosted tier). `romulus-api setup` creates it; the API refuses to
    /// create another and the app hides the way to.
    pub single_org: bool,
    /// `ROMULUS_SMTP_*` and `ROMULUS_MAIL_FROM`: how romulus sends email
    /// (sign-up links, password resets, invitations). `None`: it sends none,
    /// and admins hand links over themselves (`romulus-api password-link`).
    pub mail: Option<MailConfig>,
    /// `ROMULUS_TRUSTED_PROXIES`: comma-separated addresses or CIDR ranges
    /// of reverse proxies whose `X-Forwarded-For` names the client (rate
    /// limits and the sign-in log key by it). Empty: the TCP peer is the
    /// client, whatever a header says.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// `ROMULUS_BILLING`: hosted billing (plans, caps, Stripe). `None`
    /// (unset): off, the self-hosted default: no caps, no billing routes.
    pub billing: Option<BillingConfig>,
    /// `ROMULUS_DELETE_INACTIVE_ACCOUNTS=true`: an account unused for 3
    /// years is deleted, after an email 30 days before (`retention::
    /// inactive`; needs mail, and `ROMULUS_ZITADEL_TOKEN` with the OIDC
    /// client). Off by default: a self-hosted install never loses an
    /// account it did not choose to.
    pub delete_inactive_accounts: bool,
    /// `ROMULUS_ZITADEL_TOKEN`: a Zitadel personal access token allowed to
    /// delete users (the provisioner's `romulus-api` machine user): deleting
    /// an account then deletes its Zitadel user too, through the OIDC
    /// issuer's user API. Unset: the operator deletes it in Zitadel's
    /// console. Ignored without the OIDC client.
    pub zitadel_token: Option<Secret>,
    /// `ROMULUS_SOURCE_URL`: where the source of the running version is
    /// published (AGPL-3.0, section 13), linked from the app at the commit
    /// built ([`COMMIT`]). By default the repository the image was built
    /// from, else <https://github.com/lupidae/romulus>: a fork points to
    /// itself.
    pub source_url: Url,
    /// `ROMULUS_TERMS_VERSION` and `ROMULUS_LEGAL_URL`: the hosted service's
    /// Terms, which everyone accepts at their first sign-in (the `legal`
    /// module). `None` (unset): no Terms, the self-hosted default. Billing
    /// needs them.
    pub legal: Option<LegalConfig>,
    /// `ROMULUS_OPERATOR_EMAILS` and `ROMULUS_OPERATOR_DIGEST_HOUR`: who
    /// runs this install, emailed as things happen and once a day (the
    /// `operator` module). `None` (unset or empty): nothing is written or
    /// sent. Needs mail.
    pub operator: Option<OperatorConfig>,
    /// `ROMULUS_ANALYTICS_SCRIPT_URL` and `ROMULUS_ANALYTICS_WEBSITE_ID`: an
    /// Umami-compatible, cookieless analytics script the web app loads
    /// (page views and a few events: `instance::Analytics`). Both or
    /// neither; neither (the default): the app loads nothing from anywhere
    /// else.
    pub analytics: Option<AnalyticsConfig>,
}

/// Where the web app sends its page views (`ROMULUS_ANALYTICS_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsConfig {
    /// The tracker script, `https` (or `http` on a loopback host, for
    /// trying one locally). Its origin also receives the events.
    pub script_url: Url,
    /// The website's id at that server: 1 to 64 ASCII letters, digits and
    /// `-`, as Umami's ids are (it goes into an HTML attribute).
    pub website_id: String,
}

impl AnalyticsConfig {
    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let (script, id) = match (
            get("ROMULUS_ANALYTICS_SCRIPT_URL"),
            get("ROMULUS_ANALYTICS_WEBSITE_ID"),
        ) {
            (None, None) => return Ok(None),
            (Some(_), None) => return Err(ConfigError::Missing("ROMULUS_ANALYTICS_WEBSITE_ID")),
            (None, Some(_)) => return Err(ConfigError::Missing("ROMULUS_ANALYTICS_SCRIPT_URL")),
            (Some(script), Some(id)) => (script, id),
        };
        let invalid = |reason: &str| ConfigError::Invalid {
            var: "ROMULUS_ANALYTICS_SCRIPT_URL",
            reason: reason.to_owned(),
        };
        let script_url = Url::parse(script.trim()).map_err(|err| invalid(&err.to_string()))?;
        let secure = script_url.scheme() == "https"
            || (script_url.scheme() == "http" && is_loopback(&script_url));
        if !secure || script_url.host().is_none() {
            return Err(invalid(
                "an https URL of the script, e.g. https://stats.example.com/script.js",
            ));
        }
        if !script_url.username().is_empty() || script_url.password().is_some() {
            return Err(invalid("a URL without credentials"));
        }
        let website_id = id.trim().to_owned();
        let well_formed = (1..=64).contains(&website_id.len())
            && website_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !well_formed {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_ANALYTICS_WEBSITE_ID",
                reason: "the website's id at the analytics server: letters, digits and -"
                    .to_owned(),
            });
        }
        Ok(Some(Self {
            script_url,
            website_id,
        }))
    }

    /// The origin the script comes from and sends to.
    pub fn origin(&self) -> String {
        self.script_url.origin().ascii_serialization()
    }
}

/// The operators' notifications (`crate::operator`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorConfig {
    /// `ROMULUS_OPERATOR_EMAILS`: comma-separated addresses, each emailed
    /// on its own (normalized, duplicates dropped).
    pub emails: Vec<String>,
    /// `ROMULUS_OPERATOR_DIGEST_HOUR`: the hour (UTC, 0 to 23) from which
    /// the day's digest goes; 7 by default.
    pub digest_hour: u32,
    /// `ROMULUS_PUBLIC_URL`'s origin: which install an email is from.
    pub origin: String,
    /// `ROMULUS_OPERATOR_IGNORE_DOMAINS`: comma-separated email domains
    /// (lowercase, as [`crate::email::key`] makes them) whose accounts are
    /// left out: no instant notification about them or what they alone
    /// hold, nothing of theirs in the digest's counts ([`Self::ignores`]).
    /// Empty: nothing is.
    pub ignore_domains: Vec<String>,
}

impl OperatorConfig {
    pub const DEFAULT_DIGEST_HOUR: u32 = 7;

    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
        origin: String,
    ) -> Result<Option<Self>, ConfigError> {
        let digest_hour = match get("ROMULUS_OPERATOR_DIGEST_HOUR") {
            None => Self::DEFAULT_DIGEST_HOUR,
            Some(hour) => hour
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|hour| *hour < 24)
                .ok_or_else(|| ConfigError::Invalid {
                    var: "ROMULUS_OPERATOR_DIGEST_HOUR",
                    reason: format!("expected an hour from 0 to 23 (UTC), got {hour:?}"),
                })?,
        };
        let mut emails: Vec<String> = Vec::new();
        for item in get("ROMULUS_OPERATOR_EMAILS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if item.parse::<lettre::Address>().is_err() {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_OPERATOR_EMAILS",
                    reason: format!("{item:?} is not an email address"),
                });
            }
            let email = crate::email::normalize(item);
            if !emails
                .iter()
                .any(|known| crate::email::key(known) == crate::email::key(&email))
            {
                emails.push(email);
            }
        }
        let mut ignore_domains: Vec<String> = Vec::new();
        for item in get("ROMULUS_OPERATOR_IGNORE_DOMAINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let domain = crate::email::key(item.strip_prefix('@').unwrap_or(item));
            let labels_ok = domain.split('.').all(|label| !label.is_empty());
            if !labels_ok || domain.contains(|c: char| c == '@' || c.is_whitespace()) {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_OPERATOR_IGNORE_DOMAINS",
                    reason: format!("{item:?} is not an email domain (e.g. example.com)"),
                });
            }
            if !ignore_domains.contains(&domain) {
                ignore_domains.push(domain);
            }
        }
        Ok((!emails.is_empty()).then_some(Self {
            emails,
            digest_hour,
            origin,
            ignore_domains,
        }))
    }

    /// Whether `email` is at one of [`Self::ignore_domains`] or under one
    /// (`ops.example.com` is under `example.com`), ignoring case. The
    /// database's `email_in_domains` says the same.
    pub fn ignores(&self, email: &str) -> bool {
        let Some((_, domain)) = email.rsplit_once('@') else {
            return false;
        };
        let domain = crate::email::key(domain);
        self.ignore_domains.iter().any(|ignored| {
            domain == *ignored
                || domain
                    .strip_suffix(ignored.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        })
    }
}

/// The hosted service's legal documents, as the homepage publishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalConfig {
    /// `ROMULUS_LEGAL_URL`: the site that publishes them (its origin):
    /// `/legal/terms`, `/fr/legal/conditions`, `/legal/privacy`,
    /// `/fr/legal/confidentialite`, `/legal/dpa`.
    pub site: Url,
    /// `ROMULUS_TERMS_VERSION`: the Terms in force, e.g. `1.0`: what an
    /// acceptance is of; a new version is accepted again.
    pub terms_version: String,
    /// `ROMULUS_LEGAL_CONTACT`: the seller's address the Terms give for
    /// withdrawals, complaints and contract questions, which the contract
    /// emails repeat.
    pub contact: String,
    /// `ROMULUS_CONSUMER_SALES=true`: consumers may buy too (the full
    /// edition of the Terms, the business-or-consumer choice at checkout,
    /// withdrawal). Off (the default): romulus is sold to businesses only,
    /// under the professional edition, and everyone attests professional use.
    /// On only with `ROMULUS_TERMS_EDITION=full` and a version of the Terms
    /// other than `1.0` (published for businesses only): never by a single
    /// switch.
    pub consumer_sales: bool,
}

impl LegalConfig {
    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
        public_url: &Url,
        fake_billing: bool,
    ) -> Result<Option<Self>, ConfigError> {
        if get("ROMULUS_TERMS_URL").is_some() {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_TERMS_URL",
                reason: "replaced by ROMULUS_LEGAL_URL, the origin of the site that publishes the Terms (/legal/terms)".to_owned(),
            });
        }
        let site = get("ROMULUS_LEGAL_URL")
            .map(|site| {
                let url = Url::parse(&site).map_err(|err| ConfigError::Invalid {
                    var: "ROMULUS_LEGAL_URL",
                    reason: err.to_string(),
                })?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    return Err(ConfigError::Invalid {
                        var: "ROMULUS_LEGAL_URL",
                        reason: "expected the site's origin, e.g. https://romulus.example"
                            .to_owned(),
                    });
                }
                Ok(url)
            })
            .transpose()?;
        let version = match get("ROMULUS_TERMS_VERSION") {
            Some(version) if !version.trim().is_empty() && version.chars().count() <= 64 => {
                Some(version.trim().to_owned())
            }
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_TERMS_VERSION",
                    reason: "1 to 64 characters, e.g. 1.0".to_owned(),
                });
            }
            None => None,
        };
        let consumer_sales = flag(get, "ROMULUS_CONSUMER_SALES")?;
        let full = match get("ROMULUS_TERMS_EDITION").as_deref().map(str::trim) {
            None | Some("") => None,
            Some("professional") => Some(false),
            Some("full") => Some(true),
            Some(_) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_TERMS_EDITION",
                    reason: "professional or full".to_owned(),
                });
            }
        };
        match (consumer_sales, full) {
            (true, Some(true)) | (false, None | Some(false)) => {}
            (true, _) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_CONSUMER_SALES",
                    reason: "consumer sales need the full edition of the Terms: set ROMULUS_TERMS_EDITION=full too, with the version that opens them".to_owned(),
                });
            }
            (false, Some(true)) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_TERMS_EDITION",
                    reason: "the full edition is the one for consumer sales: set ROMULUS_CONSUMER_SALES=true too".to_owned(),
                });
            }
        }
        if consumer_sales && version.as_deref() == Some("1.0") {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_TERMS_VERSION",
                reason: "version 1.0 was published for businesses only: consumer sales open with a new version of the Terms".to_owned(),
            });
        }
        let contact = get("ROMULUS_LEGAL_CONTACT").map(|c| c.trim().to_owned());
        if contact.as_deref().is_some_and(|c| {
            c.len() > 254
                || c.split_once('@')
                    .is_none_or(|(l, d)| l.is_empty() || d.is_empty())
        }) {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_LEGAL_CONTACT",
                reason: "expected an email address".to_owned(),
            });
        }
        match (site, version) {
            (Some(site), Some(terms_version)) => Ok(Some(Self {
                site,
                terms_version,
                contact: contact.ok_or(ConfigError::Missing("ROMULUS_LEGAL_CONTACT"))?,
                consumer_sales,
            })),
            (None, None) if fake_billing => Ok(Some(Self {
                site: public_url.clone(),
                terms_version: "test".to_owned(),
                contact: contact.unwrap_or_else(|| "legal@romulus.test".to_owned()),
                consumer_sales,
            })),
            (None, None) => Ok(None),
            (Some(_), None) => Err(ConfigError::Missing("ROMULUS_TERMS_VERSION")),
            (None, Some(_)) => Err(ConfigError::Missing("ROMULUS_LEGAL_URL")),
        }
    }

    /// The edition of the Terms in force.
    pub fn edition(&self) -> crate::domain::TermsEdition {
        if self.consumer_sales {
            crate::domain::TermsEdition::Full
        } else {
            crate::domain::TermsEdition::Professional
        }
    }

    /// The URL of `path` on the legal site.
    pub fn url(&self, path: &str) -> String {
        let mut url = self.site.clone();
        url.set_path(path);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }
}

/// The commit this binary was built from (`ROMULUS_COMMIT` at build time:
/// the Dockerfile's build argument), if the build said.
pub const COMMIT: Option<&str> = match option_env!("ROMULUS_COMMIT") {
    Some(commit) if !commit.is_empty() => Some(commit),
    _ => None,
};

/// The repository [`Config::source_url`] defaults to: the one the build
/// said (`ROMULUS_SOURCE_URL` at build time), else romulus's own.
const DEFAULT_SOURCE_URL: &str = match option_env!("ROMULUS_SOURCE_URL") {
    Some(url) if !url.is_empty() => url,
    _ => "https://github.com/lupidae/romulus",
};

/// Hosted billing: Stripe's keys and the mentions every invoice carries
/// (the Terms checkout has people accept are [`LegalConfig`]'s).
#[derive(Debug, Clone)]
pub struct BillingConfig {
    pub stripe: StripeKeys,
    /// `ROMULUS_INVOICE_FOOTER`: the footer of every invoice (the VAT
    /// mention), set on each customer.
    pub invoice_footer: Option<String>,
    /// `ROMULUS_INVOICE_FIELDS`: up to 4 custom fields on every invoice (the
    /// seller's name, registration number, address), as
    /// `Name=value|Name=value`.
    pub invoice_fields: Vec<(String, String)>,
}

/// How billing reaches Stripe (`ROMULUS_BILLING`).
#[derive(Debug, Clone)]
pub enum StripeKeys {
    /// `stripe`: `STRIPE_SECRET_KEY` and `STRIPE_WEBHOOK_SECRET` (the
    /// endpoint's signing secret).
    Stripe {
        secret_key: Secret,
        webhook_secret: Secret,
    },
    /// `fake`: Stripe in memory, checkouts completed by a local page; for
    /// end-to-end tests, refused unless the public URL and the listening
    /// address are loopback (as the dev sign-in). `STRIPE_WEBHOOK_SECRET`
    /// optional.
    Fake { webhook_secret: Option<Secret> },
}

impl StripeKeys {
    pub fn webhook_secret(&self) -> Option<&Secret> {
        match self {
            Self::Stripe { webhook_secret, .. } => Some(webhook_secret),
            Self::Fake { webhook_secret } => webhook_secret.as_ref(),
        }
    }
}

impl BillingConfig {
    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
        public_url: &Url,
        api_addr: SocketAddr,
    ) -> Result<Option<Self>, ConfigError> {
        let stripe = match get("ROMULUS_BILLING").as_deref() {
            None | Some("off") => {
                for var in ["STRIPE_SECRET_KEY", "STRIPE_WEBHOOK_SECRET"] {
                    if get(var).is_some() {
                        return Err(ConfigError::Invalid {
                            var,
                            reason: "set ROMULUS_BILLING=stripe too, or remove it".to_owned(),
                        });
                    }
                }
                return Ok(None);
            }
            Some("stripe") => {
                let secret_key =
                    get("STRIPE_SECRET_KEY").ok_or(ConfigError::Missing("STRIPE_SECRET_KEY"))?;
                if !(secret_key.starts_with("sk_") || secret_key.starts_with("rk_")) {
                    return Err(ConfigError::Invalid {
                        var: "STRIPE_SECRET_KEY",
                        reason: "expected a secret or restricted key (sk_… or rk_…)".to_owned(),
                    });
                }
                let webhook_secret = get("STRIPE_WEBHOOK_SECRET")
                    .ok_or(ConfigError::Missing("STRIPE_WEBHOOK_SECRET"))?;
                if !webhook_secret.starts_with("whsec_") {
                    return Err(ConfigError::Invalid {
                        var: "STRIPE_WEBHOOK_SECRET",
                        reason: "expected the endpoint's signing secret (whsec_…)".to_owned(),
                    });
                }
                StripeKeys::Stripe {
                    secret_key: Secret(secret_key),
                    webhook_secret: Secret(webhook_secret),
                }
            }
            Some("fake") => {
                if !is_loopback(public_url) || !api_addr.ip().is_loopback() {
                    return Err(ConfigError::Invalid {
                        var: "ROMULUS_BILLING",
                        reason: "fake is for local tests: the public URL and the listening address must be loopback".to_owned(),
                    });
                }
                StripeKeys::Fake {
                    webhook_secret: get("STRIPE_WEBHOOK_SECRET").map(Secret),
                }
            }
            Some(other) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_BILLING",
                    reason: format!("expected stripe or fake, got {other:?}"),
                });
            }
        };
        let invoice_fields = match get("ROMULUS_INVOICE_FIELDS") {
            None => Vec::new(),
            Some(list) => invoice_fields(&list)?,
        };
        let invoice_footer = get("ROMULUS_INVOICE_FOOTER");
        if invoice_footer
            .as_ref()
            .is_some_and(|f| f.chars().count() > 5000)
        {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_INVOICE_FOOTER",
                reason: "Stripe takes at most 5000 characters".to_owned(),
            });
        }
        Ok(Some(Self {
            stripe,
            invoice_footer,
            invoice_fields,
        }))
    }
}

/// `Name=value|Name=value`: at most 4, names up to 40 characters and values
/// up to 140 (Stripe's limits for an invoice's custom fields).
fn invoice_fields(list: &str) -> Result<Vec<(String, String)>, ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        var: "ROMULUS_INVOICE_FIELDS",
        reason,
    };
    let fields: Vec<(String, String)> = list
        .split('|')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (name, value) = field
                .split_once('=')
                .ok_or_else(|| invalid(format!("{field:?} is not Name=value")))?;
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty()
                || name.chars().count() > 40
                || value.is_empty()
                || value.chars().count() > 140
            {
                return Err(invalid(format!(
                    "{name:?}: names take 1 to 40 characters, values 1 to 140"
                )));
            }
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect::<Result<_, _>>()?;
    if fields.len() > 4 {
        return Err(invalid("Stripe takes at most 4 custom fields".to_owned()));
    }
    Ok(fields)
}

/// The sign-in methods `ROMULUS_AUTH` enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthConfig {
    pub oidc: bool,
    pub password: bool,
}

/// Who may create a password account (`ROMULUS_SIGNUP`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SignUpPolicy {
    /// From an invitation link only, with the invited address.
    Invite,
    /// Anyone; the address is proved by a link sent to it.
    Open,
}

/// The SMTP server romulus sends through, and as whom.
#[derive(Debug, Clone)]
pub struct MailConfig {
    /// `ROMULUS_SMTP_HOST`.
    pub host: String,
    /// `ROMULUS_SMTP_PORT`: by default 587 (STARTTLS), 465 (implicit TLS)
    /// or 25 (no TLS).
    pub port: u16,
    /// `ROMULUS_SMTP_TLS`: `starttls` (the default), `implicit` or `none`.
    pub tls: SmtpTls,
    /// `ROMULUS_SMTP_USERNAME` and `ROMULUS_SMTP_PASSWORD`: both or neither.
    pub credentials: Option<(String, Secret)>,
    /// `ROMULUS_MAIL_FROM`: `romulus <romulus@example.com>`.
    pub from: lettre::message::Mailbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS, which must succeed (port 587).
    StartTls,
    /// TLS from the first byte (SMTPS, port 465).
    Implicit,
    /// No TLS: only for a relay on this host (a loopback address).
    None,
}

impl MailConfig {
    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(host) = get("ROMULUS_SMTP_HOST") else {
            for var in [
                "ROMULUS_SMTP_PORT",
                "ROMULUS_SMTP_TLS",
                "ROMULUS_SMTP_USERNAME",
                "ROMULUS_SMTP_PASSWORD",
            ] {
                if get(var).is_some() {
                    return Err(ConfigError::Invalid {
                        var,
                        reason: "set ROMULUS_SMTP_HOST too, or remove it".to_owned(),
                    });
                }
            }
            return Ok(None);
        };
        let tls = match get("ROMULUS_SMTP_TLS").as_deref() {
            None | Some("starttls") => SmtpTls::StartTls,
            Some("implicit") => SmtpTls::Implicit,
            Some("none") => SmtpTls::None,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_SMTP_TLS",
                    reason: format!("expected starttls, implicit or none, got {other:?}"),
                });
            }
        };
        let port = match get("ROMULUS_SMTP_PORT") {
            Some(port) => port.parse().map_err(|_| ConfigError::Invalid {
                var: "ROMULUS_SMTP_PORT",
                reason: format!("{port:?} is not a port number"),
            })?,
            None => match tls {
                SmtpTls::StartTls => 587,
                SmtpTls::Implicit => 465,
                SmtpTls::None => 25,
            },
        };
        let credentials = match (get("ROMULUS_SMTP_USERNAME"), get("ROMULUS_SMTP_PASSWORD")) {
            (None, None) => None,
            (Some(user), Some(password)) => Some((user, Secret(password))),
            (Some(_), None) => return Err(ConfigError::Missing("ROMULUS_SMTP_PASSWORD")),
            (None, Some(_)) => return Err(ConfigError::Missing("ROMULUS_SMTP_USERNAME")),
        };
        // Links that sign people in, in clear over a network: never. A relay
        // on this host (loopback) is the one place TLS may be skipped.
        if tls == SmtpTls::None && !is_loopback_host(&host) {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_SMTP_TLS",
                reason: format!(
                    "none sends sign-in links (and any SMTP password) in clear to {host}: use starttls or implicit, or a relay on 127.0.0.1"
                ),
            });
        }
        let from = get("ROMULUS_MAIL_FROM").ok_or(ConfigError::Missing("ROMULUS_MAIL_FROM"))?;
        let from = from.parse().map_err(|err| ConfigError::Invalid {
            var: "ROMULUS_MAIL_FROM",
            reason: format!("{err} (expected `romulus <romulus@example.com>`)"),
        })?;
        Ok(Some(Self {
            host,
            port,
            tls,
            credentials,
            from,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct PlaygroundConfig {
    pub url: Url,
    pub key: Secret,
}

impl PlaygroundConfig {
    fn from_lookup(
        get: &impl Fn(&'static str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        match (get("ROMULUS_PLAY_URL"), get("ROMULUS_PLAY_KEY")) {
            (None, None) => Ok(None),
            (Some(service), Some(key)) => {
                if key.len() < 32 {
                    return Err(ConfigError::Invalid {
                        var: "ROMULUS_PLAY_KEY",
                        reason: "use at least 32 characters (openssl rand -hex 32)".to_owned(),
                    });
                }
                let mut url = Url::parse(&service).map_err(|err| ConfigError::Invalid {
                    var: "ROMULUS_PLAY_URL",
                    reason: err.to_string(),
                })?;
                // Paths join under it.
                if !url.path().ends_with('/') {
                    url.set_path(&format!("{}/", url.path()));
                }
                Ok(Some(Self {
                    url,
                    key: Secret(key),
                }))
            }
            (Some(_), None) => Err(ConfigError::Missing("ROMULUS_PLAY_KEY")),
            (None, Some(_)) => Err(ConfigError::Missing("ROMULUS_PLAY_URL")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer: Url,
    pub client_id: String,
    pub client_secret: Secret,
}

/// A string that never shows up in logs or `Debug` output.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
    #[error("ROMULUS_DEV_SIGNIN is only allowed when ROMULUS_PUBLIC_URL is local, not {0}")]
    DevSigninNotLocal(String),
    #[error(
        "ROMULUS_DEV_SIGNIN is only allowed when the API listens on a loopback address, not {0}"
    )]
    DevSigninNotLoopbackAddr(SocketAddr),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok().filter(|value| !value.is_empty()))
    }

    /// Fixed values for tests, which build their pool themselves.
    pub fn for_tests() -> Self {
        Self {
            database_url: String::new(),
            api_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            public_url: Url::parse("http://localhost:41000").expect("valid URL"),
            sandbox_urls: BTreeMap::new(),
            sandbox_prefix: "romulus_sandbox".to_owned(),
            oidc: None,
            session_cookie: "romulus_session".to_owned(),
            dev_signin: false,
            web_dir: None,
            edge_key: None,
            playground: None,
            auth: AuthConfig {
                oidc: true,
                password: false,
            },
            signup: SignUpPolicy::Invite,
            single_org: false,
            mail: None,
            trusted_proxies: Vec::new(),
            billing: None,
            delete_inactive_accounts: false,
            zitadel_token: None,
            source_url: Url::parse(DEFAULT_SOURCE_URL).expect("a valid default source URL"),
            legal: None,
            operator: None,
            analytics: None,
        }
    }

    /// Who operator notifications go to: none when they are off.
    pub fn operators(&self) -> &[String] {
        self.operator
            .as_ref()
            .map_or(&[], |operator| operator.emails.as_slice())
    }

    /// The browser origin, e.g. `http://localhost:41000`, as the `Origin`
    /// header spells it.
    pub fn public_origin(&self) -> String {
        self.public_url.origin().ascii_serialization()
    }

    #[allow(clippy::too_many_lines, reason = "every variable, one after the other")]
    fn from_lookup(get: impl Fn(&'static str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |name| get(name).ok_or(ConfigError::Missing(name));
        let url = |name: &'static str, value: &str| {
            Url::parse(value).map_err(|err| ConfigError::Invalid {
                var: name,
                reason: err.to_string(),
            })
        };

        let api_addr = match get("ROMULUS_API_ADDR") {
            Some(value) => {
                value
                    .parse()
                    .map_err(|err: std::net::AddrParseError| ConfigError::Invalid {
                        var: "ROMULUS_API_ADDR",
                        reason: err.to_string(),
                    })?
            }
            None => SocketAddr::from(([127, 0, 0, 1], 41001)),
        };

        let oidc = match (
            get("ROMULUS_OIDC_ISSUER"),
            get("ROMULUS_OIDC_CLIENT_ID"),
            get("ROMULUS_OIDC_CLIENT_SECRET"),
        ) {
            (Some(issuer), Some(client_id), Some(client_secret)) => Some(OidcConfig {
                issuer: url("ROMULUS_OIDC_ISSUER", &issuer)?,
                client_id,
                client_secret: Secret(client_secret),
            }),
            // The issuer is known from the port scheme before Zitadel is
            // provisioned; the client only exists afterwards.
            (_, None, None) => None,
            (None, _, _) => return Err(ConfigError::Missing("ROMULUS_OIDC_ISSUER")),
            (_, None, _) => return Err(ConfigError::Missing("ROMULUS_OIDC_CLIENT_ID")),
            (_, _, None) => return Err(ConfigError::Missing("ROMULUS_OIDC_CLIENT_SECRET")),
        };

        let public_url = url("ROMULUS_PUBLIC_URL", &required("ROMULUS_PUBLIC_URL")?)?;

        let dev_signin = flag(&get, "ROMULUS_DEV_SIGNIN")?;
        if dev_signin && !is_loopback(&public_url) {
            return Err(ConfigError::DevSigninNotLocal(public_url.to_string()));
        }
        // A loopback public URL says nothing about who can reach the port:
        // listening on 0.0.0.0 would hand the dev sign-in to the whole LAN.
        if dev_signin && !api_addr.ip().is_loopback() {
            return Err(ConfigError::DevSigninNotLoopbackAddr(api_addr));
        }

        let sandbox_prefix =
            get("ROMULUS_SANDBOX_PREFIX").unwrap_or_else(|| "romulus_sandbox".to_owned());
        let identifier = sandbox_prefix.len() <= 40
            && sandbox_prefix.starts_with(|c: char| c.is_ascii_lowercase())
            && sandbox_prefix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !identifier {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_SANDBOX_PREFIX",
                reason: "use at most 40 lowercase letters, digits and underscores, starting with a letter".to_owned(),
            });
        }

        // Checked now: a typo would otherwise only show as a 404 page.
        let web_dir = get("ROMULUS_WEB_DIR").map(PathBuf::from);
        if let Some(dir) = &web_dir
            && !dir.join("index.html").is_file()
        {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_WEB_DIR",
                reason: format!("{} has no index.html (build web/ first)", dir.display()),
            });
        }

        let edge_key = get("ROMULUS_EDGE_KEY").map(Secret);
        if edge_key.as_ref().is_some_and(|key| key.0.len() < 32) {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_EDGE_KEY",
                reason: "use at least 32 characters (openssl rand -hex 32)".to_owned(),
            });
        }

        let playground = PlaygroundConfig::from_lookup(&get)?;
        let auth = auth_methods(get("ROMULUS_AUTH").as_deref())?;
        // A client configured for a method that is off is ignored.
        let oidc = oidc.filter(|_| auth.oidc);
        let mail = MailConfig::from_lookup(&get)?;
        let signup = match get("ROMULUS_SIGNUP").as_deref() {
            None | Some("invite") => SignUpPolicy::Invite,
            Some("open") => SignUpPolicy::Open,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_SIGNUP",
                    reason: format!("expected invite or open, got {other:?}"),
                });
            }
        };
        // Without mail nothing proves an address: only an invitation, which
        // an admin handed over, may.
        if signup == SignUpPolicy::Open && mail.is_none() {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_SIGNUP",
                reason: "open needs mail to prove addresses: set ROMULUS_SMTP_HOST, or use invite"
                    .to_owned(),
            });
        }
        let single_org = flag(&get, "ROMULUS_SINGLE_ORG")?;
        let trusted_proxies = match get("ROMULUS_TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| {
                    let net = item
                        .parse::<ipnet::IpNet>()
                        .or_else(|_| item.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                        .map_err(|_| ConfigError::Invalid {
                            var: "ROMULUS_TRUSTED_PROXIES",
                            reason: format!("{item:?} is not an IP address or a CIDR range"),
                        })?;
                    // Everyone would choose their own address.
                    if net.prefix_len() == 0 {
                        return Err(ConfigError::Invalid {
                            var: "ROMULUS_TRUSTED_PROXIES",
                            reason: format!("{item:?} trusts every client: name your proxies"),
                        });
                    }
                    Ok(net)
                })
                .collect::<Result<_, _>>()?,
        };

        let billing = BillingConfig::from_lookup(&get, &public_url, api_addr)?;
        let legal = LegalConfig::from_lookup(
            &get,
            &public_url,
            billing
                .as_ref()
                .is_some_and(|b| matches!(b.stripe, StripeKeys::Fake { .. })),
        )?;
        // What a checkout has people accept.
        if billing.is_some() && legal.is_none() {
            return Err(ConfigError::Missing("ROMULUS_TERMS_VERSION"));
        }
        // The hosted terms promise emails (renewal reminders, payment
        // notices): real billing does not run without mail.
        if billing
            .as_ref()
            .is_some_and(|b| matches!(b.stripe, StripeKeys::Stripe { .. }))
            && mail.is_none()
        {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_BILLING",
                reason: "stripe needs mail for renewal reminders and payment notices: set ROMULUS_SMTP_HOST and ROMULUS_MAIL_FROM".to_owned(),
            });
        }
        let delete_inactive_accounts = flag(&get, "ROMULUS_DELETE_INACTIVE_ACCOUNTS")?;
        // The notice 30 days before is the promise: no mail, no deletion.
        if delete_inactive_accounts && mail.is_none() {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_DELETE_INACTIVE_ACCOUNTS",
                reason: "an account is deleted only after an email notice: set ROMULUS_SMTP_HOST and ROMULUS_MAIL_FROM".to_owned(),
            });
        }
        let zitadel_token = get("ROMULUS_ZITADEL_TOKEN")
            .map(Secret)
            .filter(|_| oidc.is_some());
        // Deleting a single sign-on account on its own would leave its
        // Zitadel user behind, with nobody told to delete it.
        if delete_inactive_accounts && oidc.is_some() && zitadel_token.is_none() {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_DELETE_INACTIVE_ACCOUNTS",
                reason: "with single sign-on, an inactive account is deleted at Zitadel too: set ROMULUS_ZITADEL_TOKEN".to_owned(),
            });
        }
        let source_url = url(
            "ROMULUS_SOURCE_URL",
            &get("ROMULUS_SOURCE_URL").unwrap_or_else(|| DEFAULT_SOURCE_URL.to_owned()),
        )?;
        if !matches!(source_url.scheme(), "https" | "http") {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_SOURCE_URL",
                reason: "an http(s) URL, e.g. https://github.com/<owner>/romulus".to_owned(),
            });
        }
        let operator =
            OperatorConfig::from_lookup(&get, public_url.origin().ascii_serialization())?;
        // Notifications are emails: queued without mail, they would never go.
        if operator.is_some() && mail.is_none() {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_OPERATOR_EMAILS",
                reason: "operator notifications are emails: set ROMULUS_SMTP_HOST and ROMULUS_MAIL_FROM, or leave it empty".to_owned(),
            });
        }
        // Self-hosted installs are never capped.
        if billing.is_some() && single_org {
            return Err(ConfigError::Invalid {
                var: "ROMULUS_BILLING",
                reason: "a single-organization install is self-hosted: it has no billing"
                    .to_owned(),
            });
        }

        Ok(Self {
            database_url: required("DATABASE_URL")?,
            api_addr,
            public_url,
            sandbox_urls: sandbox_urls(&get),
            sandbox_prefix,
            oidc,
            session_cookie: get("ROMULUS_SESSION_COOKIE")
                .unwrap_or_else(|| "romulus_session".to_owned()),
            dev_signin,
            web_dir,
            edge_key,
            playground,
            auth,
            signup,
            single_org,
            mail,
            trusted_proxies,
            billing,
            delete_inactive_accounts,
            zitadel_token,
            source_url,
            legal,
            operator,
            analytics: AnalyticsConfig::from_lookup(&get)?,
        })
    }

    /// Where the running version's source is: [`Self::source_url`] at
    /// [`COMMIT`] (`<repository>/tree/<sha>`), or the repository itself when
    /// the build did not say.
    pub fn source_at_commit(&self) -> String {
        let base = self.source_url.as_str().trim_end_matches('/');
        match COMMIT {
            Some(commit) => format!("{base}/tree/{commit}"),
            None => base.to_owned(),
        }
    }
}

/// `ROMULUS_AUTH`: `oidc` when unset.
fn auth_methods(value: Option<&str>) -> Result<AuthConfig, ConfigError> {
    let Some(value) = value else {
        return Ok(AuthConfig {
            oidc: true,
            password: false,
        });
    };
    let mut auth = AuthConfig {
        oidc: false,
        password: false,
    };
    for method in value.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        match method {
            "oidc" => auth.oidc = true,
            "password" => auth.password = true,
            other => {
                return Err(ConfigError::Invalid {
                    var: "ROMULUS_AUTH",
                    reason: format!("unknown sign-in method {other:?} (oidc, password)"),
                });
            }
        }
    }
    if !(auth.oidc || auth.password) {
        return Err(ConfigError::Invalid {
            var: "ROMULUS_AUTH",
            reason: "name at least one sign-in method: oidc, password".to_owned(),
        });
    }
    Ok(auth)
}

/// A `true`/`false` (or `1`/`0`) variable, false when unset.
fn flag(
    get: &impl Fn(&'static str) -> Option<String>,
    var: &'static str,
) -> Result<bool, ConfigError> {
    match get(var).as_deref() {
        None | Some("false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some(other) => Err(ConfigError::Invalid {
            var,
            reason: format!("expected true or false, got {other:?}"),
        }),
    }
}

/// `localhost` or a loopback IP.
fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Each major's sandbox URL, from `ROMULUS_SANDBOX_URL` (the default
/// major's) and `ROMULUS_SANDBOX_URL_PG<N>`.
fn sandbox_urls(get: impl Fn(&'static str) -> Option<String>) -> BTreeMap<PgMajor, String> {
    let mut urls = BTreeMap::new();
    if let Some(url) = get("ROMULUS_SANDBOX_URL") {
        urls.insert(PgMajor::DEFAULT, url);
    }
    for major in PgMajor::SUPPORTED {
        if let Some(url) = sandbox_var(major).and_then(&get) {
            urls.insert(major, url);
        }
    }
    urls
}

/// The variable naming a major's sandbox other than the default's
/// (`ROMULUS_SANDBOX_URL`).
pub fn sandbox_var(major: PgMajor) -> Option<&'static str> {
    match major.get() {
        15 => Some("ROMULUS_SANDBOX_URL_PG15"),
        16 => Some("ROMULUS_SANDBOX_URL_PG16"),
        18 => Some("ROMULUS_SANDBOX_URL_PG18"),
        _ => None,
    }
}

/// `localhost`, `*.localhost` or a loopback IP: a URL only this machine reaches.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&'static str, &str)]) -> Result<Config, ConfigError> {
        let vars: HashMap<_, _> = vars.iter().map(|&(k, v)| (k, v.to_owned())).collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    const BASE: [(&str, &str); 2] = [
        ("DATABASE_URL", "postgres://localhost/romulus"),
        ("ROMULUS_PUBLIC_URL", "http://localhost:41000/"),
    ];

    #[test]
    fn billing_is_off_unless_configured_and_then_needs_its_keys_and_terms() {
        assert!(config(&BASE).unwrap().billing.is_none());
        let with = |extra: &[(&'static str, &str)]| {
            let mut vars = BASE.to_vec();
            vars.extend_from_slice(extra);
            config(&vars)
        };
        assert!(matches!(
            with(&[("STRIPE_SECRET_KEY", "sk_test_x")]),
            Err(ConfigError::Invalid {
                var: "STRIPE_SECRET_KEY",
                ..
            })
        ));
        assert!(matches!(
            with(&[("ROMULUS_BILLING", "stripe")]),
            Err(ConfigError::Missing("STRIPE_SECRET_KEY"))
        ));
        let keys = [
            ("ROMULUS_BILLING", "stripe"),
            ("STRIPE_SECRET_KEY", "sk_test_abc"),
            ("STRIPE_WEBHOOK_SECRET", "whsec_abc"),
        ];
        assert!(matches!(
            with(&keys),
            Err(ConfigError::Missing("ROMULUS_TERMS_VERSION"))
        ));
        let mut full = keys.to_vec();
        full.extend([
            ("ROMULUS_SMTP_HOST", "smtp.example.com"),
            ("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"),
            ("ROMULUS_LEGAL_URL", "https://romulus.example"),
            ("ROMULUS_TERMS_VERSION", "1.0"),
            ("ROMULUS_LEGAL_CONTACT", "legal@romulus.example"),
            ("ROMULUS_INVOICE_FOOTER", "VAT not applicable"),
            (
                "ROMULUS_INVOICE_FIELDS",
                "Seller=Someone EI | Address=1 rue X, 75000 Paris",
            ),
        ]);
        let without_mail: Vec<_> = full
            .iter()
            .copied()
            .filter(|(var, _)| !var.starts_with("ROMULUS_SMTP") && *var != "ROMULUS_MAIL_FROM")
            .collect();
        assert!(matches!(
            with(&without_mail),
            Err(ConfigError::Invalid {
                var: "ROMULUS_BILLING",
                ..
            })
        ));
        let config = with(&full).unwrap();
        assert_eq!(config.legal.as_ref().unwrap().terms_version, "1.0");
        let billing = config.billing.unwrap();
        assert_eq!(
            billing.invoice_fields,
            vec![
                ("Seller".to_owned(), "Someone EI".to_owned()),
                ("Address".to_owned(), "1 rue X, 75000 Paris".to_owned())
            ]
        );
        assert!(!format!("{billing:?}").contains("sk_test_abc"));
        assert!(!format!("{billing:?}").contains("whsec_abc"));
        let mut wrong = full.clone();
        wrong[1] = ("STRIPE_SECRET_KEY", "pk_test_abc");
        assert!(matches!(
            with(&wrong),
            Err(ConfigError::Invalid {
                var: "STRIPE_SECRET_KEY",
                ..
            })
        ));
        let mut too_many = full.clone();
        for var in &mut too_many {
            if var.0 == "ROMULUS_INVOICE_FIELDS" {
                var.1 = "A=1|B=2|C=3|D=4|E=5";
            }
        }
        assert!(matches!(
            with(&too_many),
            Err(ConfigError::Invalid {
                var: "ROMULUS_INVOICE_FIELDS",
                ..
            })
        ));
    }

    #[test]
    fn self_hosted_installs_have_no_billing_and_the_fake_stays_local() {
        let mut vars = BASE.to_vec();
        vars.extend([("ROMULUS_BILLING", "fake"), ("ROMULUS_SINGLE_ORG", "true")]);
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_BILLING",
                ..
            })
        ));
        let fake = config(&[
            ("DATABASE_URL", "postgres://localhost/romulus"),
            ("ROMULUS_PUBLIC_URL", "http://localhost:41000/"),
            ("ROMULUS_BILLING", "fake"),
        ])
        .unwrap();
        let legal = fake.legal.unwrap();
        assert_eq!(
            legal.url("/legal/terms"),
            "http://localhost:41000/legal/terms"
        );
        assert_eq!(legal.terms_version, "test");
        assert!(matches!(
            fake.billing.unwrap().stripe,
            StripeKeys::Fake { .. }
        ));
        assert!(matches!(
            config(&[
                ("DATABASE_URL", "postgres://localhost/romulus"),
                ("ROMULUS_PUBLIC_URL", "https://romulus.example/"),
                ("ROMULUS_BILLING", "fake"),
            ]),
            Err(ConfigError::Invalid {
                var: "ROMULUS_BILLING",
                ..
            })
        ));
    }

    #[test]
    fn the_terms_are_off_unless_configured_and_then_need_their_site_and_version() {
        let with = |extra: &[(&'static str, &str)]| {
            let mut vars = BASE.to_vec();
            vars.extend_from_slice(extra);
            config(&vars)
        };
        assert!(with(&[]).unwrap().legal.is_none());
        let site = [
            ("ROMULUS_LEGAL_URL", "https://romulus.example/"),
            ("ROMULUS_TERMS_VERSION", " 1.0 "),
        ];
        assert!(matches!(
            with(&site),
            Err(ConfigError::Missing("ROMULUS_LEGAL_CONTACT"))
        ));
        let mut full = site.to_vec();
        full.push(("ROMULUS_LEGAL_CONTACT", "legal@romulus.example"));
        let legal = with(&full).unwrap().legal.unwrap();
        assert_eq!(legal.contact, "legal@romulus.example");
        // Businesses only until consumer sales are switched on.
        assert!(!legal.consumer_sales);
        assert_eq!(legal.edition(), crate::domain::TermsEdition::Professional);
        assert_eq!(legal.terms_version, "1.0");
        assert_eq!(
            legal.url("/fr/legal/conditions"),
            "https://romulus.example/fr/legal/conditions"
        );
        assert!(matches!(
            with(&[("ROMULUS_LEGAL_URL", "https://romulus.example")]),
            Err(ConfigError::Missing("ROMULUS_TERMS_VERSION"))
        ));
        assert!(matches!(
            with(&[("ROMULUS_TERMS_VERSION", "1.0")]),
            Err(ConfigError::Missing("ROMULUS_LEGAL_URL"))
        ));
        assert!(matches!(
            with(&[
                ("ROMULUS_LEGAL_URL", "romulus.example"),
                ("ROMULUS_TERMS_VERSION", "1.0")
            ]),
            Err(ConfigError::Invalid {
                var: "ROMULUS_LEGAL_URL",
                ..
            })
        ));
        // The old variable says what replaced it.
        assert!(matches!(
            with(&[("ROMULUS_TERMS_URL", "https://romulus.example/terms")]),
            Err(ConfigError::Invalid {
                var: "ROMULUS_TERMS_URL",
                ..
            })
        ));
    }

    #[test]
    fn consumer_sales_open_only_with_the_full_edition_and_a_new_version() {
        let with = |extra: &[(&'static str, &str)]| {
            let mut vars = BASE.to_vec();
            vars.extend_from_slice(&[
                ("ROMULUS_LEGAL_URL", "https://romulus.example"),
                ("ROMULUS_LEGAL_CONTACT", "legal@romulus.example"),
            ]);
            vars.extend_from_slice(extra);
            config(&vars)
        };
        let refused = |extra: &[(&'static str, &str)], var: &str| {
            assert!(
                matches!(with(extra), Err(ConfigError::Invalid { var: v, .. }) if v == var),
                "{extra:?}"
            );
        };
        // The switch alone, or with the edition but version 1.0: refused.
        refused(
            &[
                ("ROMULUS_TERMS_VERSION", "2.0"),
                ("ROMULUS_CONSUMER_SALES", "true"),
            ],
            "ROMULUS_CONSUMER_SALES",
        );
        refused(
            &[
                ("ROMULUS_TERMS_VERSION", "1.0"),
                ("ROMULUS_CONSUMER_SALES", "true"),
                ("ROMULUS_TERMS_EDITION", "full"),
            ],
            "ROMULUS_TERMS_VERSION",
        );
        // The full edition without consumer sales, or a made-up edition.
        refused(
            &[
                ("ROMULUS_TERMS_VERSION", "2.0"),
                ("ROMULUS_TERMS_EDITION", "full"),
            ],
            "ROMULUS_TERMS_EDITION",
        );
        refused(
            &[
                ("ROMULUS_TERMS_VERSION", "1.0"),
                ("ROMULUS_TERMS_EDITION", "consumers"),
            ],
            "ROMULUS_TERMS_EDITION",
        );
        // All three: the full edition.
        let legal = with(&[
            ("ROMULUS_TERMS_VERSION", "2.0"),
            ("ROMULUS_CONSUMER_SALES", "true"),
            ("ROMULUS_TERMS_EDITION", "full"),
        ])
        .unwrap()
        .legal
        .unwrap();
        assert_eq!(legal.edition(), crate::domain::TermsEdition::Full);
        // Saying the professional edition is saying what is already so.
        let legal = with(&[
            ("ROMULUS_TERMS_VERSION", "1.0"),
            ("ROMULUS_TERMS_EDITION", "professional"),
        ])
        .unwrap()
        .legal
        .unwrap();
        assert_eq!(legal.edition(), crate::domain::TermsEdition::Professional);
    }

    #[test]
    fn minimal_environment_is_enough() {
        let config = config(&BASE).unwrap();
        assert_eq!(config.api_addr.port(), 41001);
        assert_eq!(config.public_origin(), "http://localhost:41000");
        assert!(config.oidc.is_none());
    }

    #[test]
    fn playgrounds_need_both_their_url_and_a_long_key() {
        assert!(config(&BASE).unwrap().playground.is_none());
        let key = "k".repeat(32);
        let on = config(&[
            BASE[0],
            BASE[1],
            ("ROMULUS_PLAY_URL", "http://playground:8080"),
            ("ROMULUS_PLAY_KEY", &key),
        ])
        .unwrap();
        assert_eq!(
            on.playground.unwrap().url.as_str(),
            "http://playground:8080/"
        );
        assert!(matches!(
            config(&[
                BASE[0],
                BASE[1],
                ("ROMULUS_PLAY_URL", "http://playground:8080")
            ]),
            Err(ConfigError::Missing("ROMULUS_PLAY_KEY"))
        ));
        assert!(matches!(
            config(&[
                BASE[0],
                BASE[1],
                ("ROMULUS_PLAY_URL", "http://p"),
                ("ROMULUS_PLAY_KEY", "short")
            ]),
            Err(ConfigError::Invalid {
                var: "ROMULUS_PLAY_KEY",
                ..
            })
        ));
    }

    #[test]
    fn missing_database_url_is_named() {
        let err = config(&[BASE[1]]).unwrap_err();
        assert_eq!(err.to_string(), "DATABASE_URL is not set");
    }

    #[test]
    fn issuer_alone_means_not_provisioned_yet() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_OIDC_ISSUER", "http://localhost:41004"));
        assert!(config(&vars).unwrap().oidc.is_none());
    }

    #[test]
    fn half_an_oidc_client_is_an_error() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_OIDC_ISSUER", "http://localhost:41004"));
        vars.push(("ROMULUS_OIDC_CLIENT_ID", "client"));
        let err = config(&vars).unwrap_err();
        assert_eq!(err.to_string(), "ROMULUS_OIDC_CLIENT_SECRET is not set");
    }

    #[test]
    fn client_secret_does_not_leak_through_debug() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_OIDC_ISSUER", "http://localhost:41004"));
        vars.push(("ROMULUS_OIDC_CLIENT_ID", "client"));
        vars.push(("ROMULUS_OIDC_CLIENT_SECRET", "hunter2"));
        let config = config(&vars).unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
        assert_eq!(config.oidc.unwrap().client_secret.expose(), "hunter2");
    }

    #[test]
    fn dev_signin_is_off_unless_asked_for() {
        assert!(!config(&BASE).unwrap().dev_signin);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_DEV_SIGNIN", "true"));
        assert!(config(&vars).unwrap().dev_signin);
    }

    #[test]
    fn dev_signin_is_refused_on_a_public_url() {
        let vars = [
            BASE[0],
            ("ROMULUS_PUBLIC_URL", "https://app.romulus.dev"),
            ("ROMULUS_DEV_SIGNIN", "true"),
        ];
        assert!(matches!(
            config(&vars),
            Err(ConfigError::DevSigninNotLocal(_))
        ));
    }

    #[test]
    fn dev_signin_is_refused_when_the_api_listens_beyond_loopback() {
        for addr in ["0.0.0.0:41001", "192.168.1.20:41001", "[::]:41001"] {
            let mut vars = BASE.to_vec();
            vars.push(("ROMULUS_DEV_SIGNIN", "true"));
            vars.push(("ROMULUS_API_ADDR", addr));
            assert!(
                matches!(config(&vars), Err(ConfigError::DevSigninNotLoopbackAddr(_))),
                "{addr}"
            );
        }
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_DEV_SIGNIN", "true"));
        vars.push(("ROMULUS_API_ADDR", "[::1]:41001"));
        assert!(config(&vars).unwrap().dev_signin);
    }

    #[test]
    fn dev_signin_accepts_loopback_addresses() {
        for public in [
            "http://127.0.0.1:41000",
            "http://[::1]:41000",
            "http://r.localhost",
        ] {
            let vars = [
                BASE[0],
                ("ROMULUS_PUBLIC_URL", public),
                ("ROMULUS_DEV_SIGNIN", "1"),
            ];
            assert!(config(&vars).unwrap().dev_signin, "{public}");
        }
    }

    #[test]
    fn each_major_has_its_sandbox() {
        assert!(config(&BASE).unwrap().sandbox_urls.is_empty());
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SANDBOX_URL", "postgres://a@h:1/postgres"));
        vars.push(("ROMULUS_SANDBOX_URL_PG15", "postgres://a@h:2/postgres"));
        let urls = config(&vars).unwrap().sandbox_urls;
        assert_eq!(
            urls.iter()
                .map(|(major, url)| (major.get(), url.as_str()))
                .collect::<Vec<_>>(),
            [
                (15, "postgres://a@h:2/postgres"),
                (17, "postgres://a@h:1/postgres")
            ]
        );
        // Every supported major but the default has its variable.
        for major in PgMajor::SUPPORTED {
            assert_eq!(sandbox_var(major).is_none(), major == PgMajor::DEFAULT);
        }
    }

    #[test]
    fn the_sandbox_prefix_is_an_identifier() {
        assert_eq!(config(&BASE).unwrap().sandbox_prefix, "romulus_sandbox");
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SANDBOX_PREFIX", "romulus_e2e"));
        assert_eq!(config(&vars).unwrap().sandbox_prefix, "romulus_e2e");
        for bad in ["Romulus", "1st", "a-b", "x; drop", &"a".repeat(41)] {
            let mut vars = BASE.to_vec();
            vars.push(("ROMULUS_SANDBOX_PREFIX", bad));
            assert!(config(&vars).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_web_dir_must_hold_a_build() {
        assert!(config(&BASE).unwrap().web_dir.is_none());
        let dir = std::env::temp_dir().join(format!("romulus-web-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut vars = BASE.to_vec();
        let path = dir.to_str().unwrap().to_owned();
        vars.push(("ROMULUS_WEB_DIR", &path));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_WEB_DIR",
                ..
            })
        ));
        std::fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
        assert_eq!(
            config(&vars).unwrap().web_dir.as_deref(),
            Some(dir.as_path())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_edge_key_is_long_and_secret() {
        assert!(config(&BASE).unwrap().edge_key.is_none());
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_EDGE_KEY", "short"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_EDGE_KEY",
                ..
            })
        ));
        let key = "k".repeat(32);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_EDGE_KEY", &key));
        let config = config(&vars).unwrap();
        assert!(!format!("{config:?}").contains(&key));
        assert_eq!(config.edge_key.unwrap().expose(), key);
    }

    #[test]
    fn sign_in_methods_default_to_sso_and_take_a_list() {
        let auth = config(&BASE).unwrap().auth;
        assert!(auth.oidc && !auth.password);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_AUTH", "password"));
        let auth = config(&vars).unwrap().auth;
        assert!(!auth.oidc && auth.password);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_AUTH", " oidc , password "));
        let auth = config(&vars).unwrap().auth;
        assert!(auth.oidc && auth.password);
        for bad in ["ldap", ",", "password,saml"] {
            let mut vars = BASE.to_vec();
            vars.push(("ROMULUS_AUTH", bad));
            assert!(
                matches!(
                    config(&vars),
                    Err(ConfigError::Invalid {
                        var: "ROMULUS_AUTH",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_oidc_client_is_ignored_when_sso_is_off() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_AUTH", "password"));
        vars.push(("ROMULUS_OIDC_ISSUER", "http://localhost:41004"));
        vars.push(("ROMULUS_OIDC_CLIENT_ID", "client"));
        vars.push(("ROMULUS_OIDC_CLIENT_SECRET", "secret"));
        assert!(config(&vars).unwrap().oidc.is_none());
    }

    #[test]
    fn open_sign_up_needs_mail() {
        assert_eq!(config(&BASE).unwrap().signup, SignUpPolicy::Invite);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SIGNUP", "open"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_SIGNUP",
                ..
            })
        ));
        vars.push(("ROMULUS_SMTP_HOST", "smtp.resend.com"));
        vars.push(("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"));
        assert_eq!(config(&vars).unwrap().signup, SignUpPolicy::Open);
    }

    #[test]
    fn smtp_settings_default_to_starttls_and_need_a_sender() {
        assert!(config(&BASE).unwrap().mail.is_none());
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SMTP_HOST", "smtp.resend.com"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Missing("ROMULUS_MAIL_FROM"))
        ));
        vars.push(("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"));
        vars.push(("ROMULUS_SMTP_USERNAME", "resend"));
        vars.push(("ROMULUS_SMTP_PASSWORD", "re_secret"));
        let config_ = config(&vars).unwrap();
        let mail = config_.mail.as_ref().unwrap();
        assert_eq!((mail.tls, mail.port), (SmtpTls::StartTls, 587));
        assert_eq!(mail.from.email.to_string(), "romulus@example.com");
        assert!(!format!("{config_:?}").contains("re_secret"));

        let mut implicit = vars.clone();
        implicit.push(("ROMULUS_SMTP_TLS", "implicit"));
        let mail = config(&implicit).unwrap().mail.unwrap();
        assert_eq!((mail.tls, mail.port), (SmtpTls::Implicit, 465));
    }

    #[test]
    fn smtp_without_tls_is_for_a_relay_on_this_host_only() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SMTP_HOST", "mail.internal"));
        vars.push(("ROMULUS_MAIL_FROM", "romulus@example.com"));
        vars.push(("ROMULUS_SMTP_TLS", "none"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_SMTP_TLS",
                ..
            })
        ));
        for local in ["127.0.0.1", "localhost", "::1"] {
            let vars: Vec<_> = vars
                .iter()
                .map(|&(k, v)| (k, if k == "ROMULUS_SMTP_HOST" { local } else { v }))
                .collect();
            assert_eq!(config(&vars).unwrap().mail.unwrap().port, 25, "{local}");
        }
    }

    #[test]
    fn inactive_accounts_are_kept_unless_deleting_them_is_on_and_mail_can_warn() {
        let config_ = config(&BASE).unwrap();
        assert!(!config_.delete_inactive_accounts);
        assert!(!Config::for_tests().delete_inactive_accounts);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_DELETE_INACTIVE_ACCOUNTS", "true"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_DELETE_INACTIVE_ACCOUNTS",
                ..
            })
        ));
        vars.push(("ROMULUS_SMTP_HOST", "smtp.example.com"));
        vars.push(("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"));
        assert!(config(&vars).unwrap().delete_inactive_accounts);
    }

    #[test]
    fn analytics_are_off_unless_both_the_script_and_the_website_are_set() {
        assert_eq!(config(&BASE).unwrap().analytics, None);
        assert_eq!(Config::for_tests().analytics, None);
        let with = |extra: &[(&'static str, &str)]| {
            let mut vars = BASE.to_vec();
            vars.extend_from_slice(extra);
            config(&vars)
        };
        let id = "00e44160-7141-4b01-a9d2-4b4bf27d8236";
        let on = with(&[
            (
                "ROMULUS_ANALYTICS_SCRIPT_URL",
                "https://stats.example.com/script.js",
            ),
            ("ROMULUS_ANALYTICS_WEBSITE_ID", id),
        ])
        .unwrap()
        .analytics
        .unwrap();
        assert_eq!(
            on.script_url.as_str(),
            "https://stats.example.com/script.js"
        );
        assert_eq!(on.website_id, id);
        assert_eq!(on.origin(), "https://stats.example.com");

        assert!(matches!(
            with(&[(
                "ROMULUS_ANALYTICS_SCRIPT_URL",
                "https://stats.example.com/script.js"
            )]),
            Err(ConfigError::Missing("ROMULUS_ANALYTICS_WEBSITE_ID"))
        ));
        assert!(matches!(
            with(&[("ROMULUS_ANALYTICS_WEBSITE_ID", id)]),
            Err(ConfigError::Missing("ROMULUS_ANALYTICS_SCRIPT_URL"))
        ));
        for (script, ok) in [
            ("http://stats.example.com/script.js", false),
            ("http://localhost:3000/script.js", true),
            ("https://user:pw@stats.example.com/script.js", false),
            ("javascript:alert(1)", false),
            ("/script.js", false),
        ] {
            let result = with(&[
                ("ROMULUS_ANALYTICS_SCRIPT_URL", script),
                ("ROMULUS_ANALYTICS_WEBSITE_ID", id),
            ]);
            assert_eq!(result.is_ok(), ok, "{script}");
        }
        let long = "a".repeat(65);
        for (website, ok) in [
            ("abc-123", true),
            ("a\"><script>", false),
            (long.as_str(), false),
        ] {
            let result = with(&[
                (
                    "ROMULUS_ANALYTICS_SCRIPT_URL",
                    "https://stats.example.com/script.js",
                ),
                ("ROMULUS_ANALYTICS_WEBSITE_ID", website),
            ]);
            assert_eq!(result.is_ok(), ok, "{website}");
        }
    }

    #[test]
    fn operator_notifications_are_off_when_empty_and_need_mail_when_on() {
        assert_eq!(config(&BASE).unwrap().operator, None);
        assert_eq!(Config::for_tests().operators(), &[] as &[String]);
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_OPERATOR_EMAILS", " , "));
        assert_eq!(config(&vars).unwrap().operator, None, "empty is off");
        // An hour alone changes nothing.
        vars.push(("ROMULUS_OPERATOR_DIGEST_HOUR", "9"));
        assert_eq!(config(&vars).unwrap().operator, None);

        let mut vars = BASE.to_vec();
        vars.push((
            "ROMULUS_OPERATOR_EMAILS",
            "ops@example.com, Founder@Example.com,founder@example.com",
        ));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_OPERATOR_EMAILS",
                ..
            })
        ));
        vars.push(("ROMULUS_SMTP_HOST", "smtp.example.com"));
        vars.push(("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"));
        let on = config(&vars).unwrap();
        assert_eq!(
            on.operator,
            Some(OperatorConfig {
                emails: vec!["ops@example.com".into(), "Founder@Example.com".into()],
                digest_hour: 7,
                origin: "http://localhost:41000".into(),
                ignore_domains: Vec::new(),
            })
        );
        assert_eq!(on.operators().len(), 2);

        for (hour, ok) in [("0", true), ("23", true), ("24", false), ("7am", false)] {
            let mut vars = vars.clone();
            vars.push(("ROMULUS_OPERATOR_DIGEST_HOUR", hour));
            assert_eq!(config(&vars).is_ok(), ok, "{hour}");
        }
        // Later values win in `config`'s map.
        vars.push(("ROMULUS_OPERATOR_EMAILS", "ops@example.com, not an address"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_OPERATOR_EMAILS",
                ..
            })
        ));
    }

    #[test]
    fn ignored_domains_match_exactly_or_below_whatever_the_case() {
        let mut vars = BASE.to_vec();
        vars.extend([
            ("ROMULUS_OPERATOR_EMAILS", "ops@example.com"),
            ("ROMULUS_SMTP_HOST", "smtp.example.com"),
            ("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"),
        ]);
        let operator =
            |vars: &[(&'static str, &'static str)]| config(vars).unwrap().operator.unwrap();
        assert_eq!(operator(&vars).ignore_domains, Vec::<String>::new());
        assert!(
            !operator(&vars).ignores("ada@romulus.test"),
            "empty ignores nothing"
        );

        vars.push((
            "ROMULUS_OPERATOR_IGNORE_DOMAINS",
            " Romulus.Test, @example.com,romulus.test ,, ",
        ));
        let on = operator(&vars);
        assert_eq!(on.ignore_domains, ["romulus.test", "example.com"]);
        for (email, ignored) in [
            ("alice@romulus.test", true),
            ("Alice@ROMULUS.TEST", true),
            ("ops@eu.example.com", true),
            ("ops@EU.Example.Com", true),
            ("ops@notexample.com", false),
            ("ops@example.com.evil", false),
            ("example.com@gmail.com", false),
            ("not an address", false),
        ] {
            assert_eq!(on.ignores(email), ignored, "{email}");
        }

        for bad in [
            "a@b.com",
            ".com",
            "example..com",
            "exa mple.com",
            "example.com.",
        ] {
            let mut vars = vars.clone();
            vars.push(("ROMULUS_OPERATOR_IGNORE_DOMAINS", bad));
            assert!(
                matches!(
                    config(&vars),
                    Err(ConfigError::Invalid {
                        var: "ROMULUS_OPERATOR_IGNORE_DOMAINS",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn deleting_inactive_accounts_with_single_sign_on_needs_the_zitadel_token() {
        let mut vars = BASE.to_vec();
        vars.extend([
            ("ROMULUS_DELETE_INACTIVE_ACCOUNTS", "true"),
            ("ROMULUS_SMTP_HOST", "smtp.example.com"),
            ("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"),
            ("ROMULUS_OIDC_ISSUER", "http://localhost:41004"),
            ("ROMULUS_OIDC_CLIENT_ID", "client"),
            ("ROMULUS_OIDC_CLIENT_SECRET", "secret"),
        ]);
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_DELETE_INACTIVE_ACCOUNTS",
                ..
            })
        ));
        vars.push(("ROMULUS_ZITADEL_TOKEN", "pat"));
        assert!(config(&vars).unwrap().delete_inactive_accounts);
        // Password sign-in only: no identity provider to delete from.
        let mut password = BASE.to_vec();
        password.extend([
            ("ROMULUS_AUTH", "password"),
            ("ROMULUS_DELETE_INACTIVE_ACCOUNTS", "true"),
            ("ROMULUS_SMTP_HOST", "smtp.example.com"),
            ("ROMULUS_MAIL_FROM", "romulus <romulus@example.com>"),
            ("ROMULUS_OIDC_ISSUER", "http://localhost:41004"),
            ("ROMULUS_OIDC_CLIENT_ID", "client"),
            ("ROMULUS_OIDC_CLIENT_SECRET", "secret"),
        ]);
        assert!(config(&password).unwrap().delete_inactive_accounts);
    }

    #[test]
    fn the_zitadel_token_needs_the_oidc_client() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_ZITADEL_TOKEN", "pat"));
        assert!(config(&vars).unwrap().zitadel_token.is_none());
        vars.extend([
            ("ROMULUS_OIDC_ISSUER", "http://localhost:41004"),
            ("ROMULUS_OIDC_CLIENT_ID", "client"),
            ("ROMULUS_OIDC_CLIENT_SECRET", "secret"),
        ]);
        assert_eq!(
            config(&vars).unwrap().zitadel_token.unwrap().expose(),
            "pat"
        );
    }

    #[test]
    fn the_source_defaults_to_romulus_and_a_fork_names_its_own() {
        let default = config(&BASE).unwrap();
        assert_eq!(default.source_url.as_str(), DEFAULT_SOURCE_URL);
        let mut vars = BASE.to_vec();
        vars.push((
            "ROMULUS_SOURCE_URL",
            "https://git.example.com/acme/romulus/",
        ));
        let fork = config(&vars).unwrap();
        let at = fork.source_at_commit();
        match COMMIT {
            Some(commit) => assert_eq!(
                at,
                format!("https://git.example.com/acme/romulus/tree/{commit}")
            ),
            None => assert_eq!(at, "https://git.example.com/acme/romulus"),
        }
        vars.push(("ROMULUS_SOURCE_URL", "ftp://example.com"));
        let vars: Vec<_> = vars
            .into_iter()
            .filter(|&(k, v)| k != "ROMULUS_SOURCE_URL" || v.starts_with("ftp"))
            .collect();
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_SOURCE_URL",
                ..
            })
        ));
    }

    #[test]
    fn smtp_details_without_a_host_are_a_mistake() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SMTP_PASSWORD", "p"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_SMTP_PASSWORD",
                ..
            })
        ));
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SMTP_HOST", "h"));
        vars.push(("ROMULUS_MAIL_FROM", "not an address"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_MAIL_FROM",
                ..
            })
        ));
    }

    #[test]
    fn single_org_and_trusted_proxies_are_read() {
        let plain = config(&BASE).unwrap();
        assert!(!plain.single_org && plain.trusted_proxies.is_empty());
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SINGLE_ORG", "true"));
        vars.push(("ROMULUS_TRUSTED_PROXIES", "172.16.0.0/12, 127.0.0.1 ,::1"));
        let set = config(&vars).unwrap();
        assert!(set.single_org);
        assert_eq!(set.trusted_proxies.len(), 3);
        assert!(set.trusted_proxies[1].contains(&"127.0.0.1".parse::<std::net::IpAddr>().unwrap()));
        for bad in ["everyone", "0.0.0.0/0", "::/0"] {
            let mut vars = BASE.to_vec();
            vars.push(("ROMULUS_TRUSTED_PROXIES", bad));
            assert!(config(&vars).is_err(), "{bad}");
        }
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_SINGLE_ORG", "yes"));
        assert!(config(&vars).is_err());
    }

    #[test]
    fn malformed_address_is_rejected() {
        let mut vars = BASE.to_vec();
        vars.push(("ROMULUS_API_ADDR", "localhost"));
        assert!(matches!(
            config(&vars),
            Err(ConfigError::Invalid {
                var: "ROMULUS_API_ADDR",
                ..
            })
        ));
    }
}
