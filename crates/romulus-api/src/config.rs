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
        }
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
        })
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
