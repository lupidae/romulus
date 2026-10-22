//! Deleting a single sign-on account at the identity provider too: Zitadel's
//! user API (`DELETE /v2/users/{id}`, the id being the OIDC subject), on the
//! issuer's origin, with `ROMULUS_ZITADEL_TOKEN` (a machine user allowed to
//! delete users: the provisioner makes `romulus-api`, `IAM_USER_MANAGER`).
//!
//! Without the token the operator deletes the user in Zitadel's console
//! (Users → the user → Delete): `delete-user` prints the id.
//!
//! Either way Zitadel's event store keeps what the user's events carried
//! (email, names, password hash, sign-ins' IP addresses): it has no API to
//! erase them, docs/self-host/privacy.md has the operator's SQL.

use std::time::Duration;

use url::Url;

use crate::{Config, config::Secret, error::Chain};

/// How a `users.oidc_subject` signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// romulus's own password (`password|<uuid>`).
    Password,
    /// The dev sign-in (`dev|<email>`): no identity provider holds it.
    Dev,
    /// The identity provider's user, by its id.
    SingleSignOn(String),
}

impl SignIn {
    pub fn of(subject: &str) -> Self {
        if subject.starts_with("password|") {
            Self::Password
        } else if subject.starts_with("dev|") {
            Self::Dev
        } else {
            Self::SingleSignOn(subject.to_owned())
        }
    }
}

/// What the identity provider did with an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdpOutcome {
    /// Not a single sign-on account: nothing there.
    NotThere,
    Deleted,
    /// It had no such user any more (deleted by hand, or by an earlier run).
    AlreadyGone,
    /// No `ROMULUS_ZITADEL_TOKEN`: the operator deletes this user in
    /// Zitadel's console.
    Manual {
        user_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IdpError {
    #[error("reaching Zitadel")]
    Http(#[from] reqwest::Error),
    #[error("Zitadel answered {status}: {body}")]
    Refused { status: u16, body: String },
}

/// Zitadel's user API, when configured.
#[derive(Clone)]
pub struct Zitadel {
    http: reqwest::Client,
    base: Url,
    token: Secret,
}

impl Zitadel {
    /// With the OIDC client and `ROMULUS_ZITADEL_TOKEN`, else `None`.
    pub fn from_config(config: &Config) -> Option<Self> {
        let oidc = config.oidc.as_ref()?;
        let token = config.zitadel_token.clone()?;
        let http = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("the HTTP client's TLS backend initialises");
        Some(Self {
            http,
            base: oidc.issuer.clone(),
            token,
        })
    }

    /// Deletes the user `id` (an OIDC subject). A user already gone counts.
    pub async fn delete_user(&self, id: &str) -> Result<IdpOutcome, IdpError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| IdpError::Refused {
                status: 0,
                body: "the issuer URL cannot have a path".to_owned(),
            })?
            .pop_if_empty()
            .extend(["v2", "users", id]);
        let response = self
            .http
            .delete(url)
            .bearer_auth(self.token.expose())
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(IdpOutcome::Deleted);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(IdpOutcome::AlreadyGone);
        }
        let body: String = response
            .text()
            .await
            .unwrap_or_else(|err| Chain(&err).to_string())
            .chars()
            .take(500)
            .collect();
        Err(IdpError::Refused {
            status: status.as_u16(),
            body,
        })
    }
}

/// What deleting `subject` takes at the identity provider: done through
/// `zitadel`, or left to the operator.
pub async fn delete(zitadel: Option<&Zitadel>, subject: &str) -> Result<IdpOutcome, IdpError> {
    match (SignIn::of(subject), zitadel) {
        (SignIn::Password | SignIn::Dev, _) => Ok(IdpOutcome::NotThere),
        (SignIn::SingleSignOn(id), None) => Ok(IdpOutcome::Manual { user_id: id }),
        (SignIn::SingleSignOn(id), Some(zitadel)) => zitadel.delete_user(&id).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_say_how_an_account_signs_in() {
        assert_eq!(SignIn::of("password|0192"), SignIn::Password);
        assert_eq!(SignIn::of("dev|alice@romulus.test"), SignIn::Dev);
        assert_eq!(
            SignIn::of("287346283746"),
            SignIn::SingleSignOn("287346283746".to_owned())
        );
    }
}
