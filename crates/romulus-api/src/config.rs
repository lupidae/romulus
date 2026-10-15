//! Configuration, read from the environment only.
//!
//! In development `main` first loads `.env` (written per worktree by
//! `scripts/dev-env`) and `.dev/zitadel/oidc.env` (written by the Zitadel
//! provisioner); variables already set always win.

use std::{env, fmt, net::SocketAddr};

use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    /// `DATABASE_URL`: the application database.
    pub database_url: String,
    /// `ROMULUS_API_ADDR`: where the API listens.
    pub api_addr: SocketAddr,
    /// `ROMULUS_PUBLIC_URL`: the origin the browser uses (the web app, which
    /// proxies `/api`). Used for `Origin` checks and redirect URLs.
    pub public_url: Url,
    /// `ROMULUS_SANDBOX_URL`: an admin connection to the sandbox Postgres.
    pub sandbox_url: Option<String>,
    /// `ROMULUS_OIDC_*`: `None` until the Zitadel provisioner has run.
    pub oidc: Option<OidcConfig>,
    /// `ROMULUS_SESSION_COOKIE`: per stack, because cookies are not
    /// port-scoped and every worktree's stack lives on localhost.
    pub session_cookie: String,
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
            sandbox_url: None,
            oidc: None,
            session_cookie: "romulus_session".to_owned(),
        }
    }

    /// The browser origin, e.g. `http://localhost:41000`, as the `Origin`
    /// header spells it.
    pub fn public_origin(&self) -> String {
        self.public_url.origin().ascii_serialization()
    }

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

        Ok(Self {
            database_url: required("DATABASE_URL")?,
            api_addr,
            public_url: url("ROMULUS_PUBLIC_URL", &required("ROMULUS_PUBLIC_URL")?)?,
            sandbox_url: get("ROMULUS_SANDBOX_URL"),
            oidc,
            session_cookie: get("ROMULUS_SESSION_COOKIE")
                .unwrap_or_else(|| "romulus_session".to_owned()),
        })
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
