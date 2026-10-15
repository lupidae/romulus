//! The OIDC relying party: discovery (cached), the authorization request
//! (code flow, PKCE S256, state, nonce), the code exchange with ID token
//! verification, and RP-initiated logout.
//!
//! Only the API talks to the IdP; the browser just follows redirects. The
//! tokens the IdP returns are used once, to learn who signed in, and dropped:
//! romulus keeps its own session and never calls the IdP on the user's
//! behalf, so it keeps no refresh token (`sessions.oidc_refresh_token` stays
//! NULL) and asks for no `offline_access`.

use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use openidconnect::{
    AdditionalProviderMetadata, AuthenticationFlow, AuthorizationCode, ClaimsVerificationError,
    ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet, IssuerUrl,
    Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, ProviderMetadata, RedirectUrl,
    Scope, TokenResponse,
    core::{
        CoreAuthDisplay, CoreAuthPrompt, CoreClaimName, CoreClaimType, CoreClient,
        CoreClientAuthMethod, CoreGrantType, CoreIdTokenClaims, CoreJsonWebKey,
        CoreJweContentEncryptionAlgorithm, CoreJweKeyManagementAlgorithm, CoreResponseMode,
        CoreResponseType, CoreSubjectIdentifierType, CoreUserInfoClaims,
    },
    reqwest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use crate::{Config, error::Chain};

/// How long discovery (endpoints and signing keys) is trusted before it is
/// fetched again. A signature that fails to verify refetches at once, so a
/// key rotation never waits for this.
const DISCOVERY_TTL: Duration = Duration::from_mins(10);

/// Zitadel (and OIDC Session Management) advertise the logout endpoint
/// outside the core discovery fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct LogoutMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end_session_endpoint: Option<Url>,
}

impl AdditionalProviderMetadata for LogoutMetadata {}

type Metadata = ProviderMetadata<
    LogoutMetadata,
    CoreAuthDisplay,
    CoreClientAuthMethod,
    CoreClaimName,
    CoreClaimType,
    CoreGrantType,
    CoreJweContentEncryptionAlgorithm,
    CoreJweKeyManagementAlgorithm,
    CoreJsonWebKey,
    CoreResponseMode,
    CoreResponseType,
    CoreSubjectIdentifierType,
>;

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("no OIDC client is configured (run `just up` to provision Zitadel)")]
    NotConfigured,
    #[error("OIDC discovery failed")]
    Discovery(#[source] Box<dyn Error + Send + Sync>),
    #[error("the authorization code exchange failed")]
    Exchange(#[source] Box<dyn Error + Send + Sync>),
    #[error("the IdP returned no ID token")]
    NoIdToken,
    #[error("the ID token was rejected")]
    IdToken(#[source] ClaimsVerificationError),
    #[error("the userinfo request failed")]
    UserInfo(#[source] Box<dyn Error + Send + Sync>),
}

/// Who signed in, as the IdP vouches for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The `sub` claim: the only stable identity (emails change).
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

/// An authorization request: send the browser to `url`, keep the rest for
/// the callback.
pub struct Started {
    pub url: Url,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

struct Settings {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect_uri: RedirectUrl,
    post_logout_uri: Url,
}

struct Provider {
    client: Client,
    end_session: Option<Url>,
    fetched_at: Instant,
}

/// The relying party, in `AppState`. Holds the discovery cache.
pub struct Oidc {
    settings: Option<Settings>,
    http: reqwest::Client,
    provider: Mutex<Option<Arc<Provider>>>,
}

impl Oidc {
    pub fn new(config: &Config) -> Self {
        let settings = config.oidc.as_ref().map(|oidc| Settings {
            issuer: issuer(&oidc.issuer),
            client_id: ClientId::new(oidc.client_id.clone()),
            client_secret: ClientSecret::new(oidc.client_secret.expose().to_owned()),
            redirect_uri: RedirectUrl::from_url(
                config
                    .public_url
                    .join("/api/auth/callback")
                    .expect("a path joins any base URL"),
            ),
            post_logout_uri: config
                .public_url
                .join("/app/signin")
                .expect("a path joins any base URL"),
        });
        let http = reqwest::ClientBuilder::new()
            // Following redirects would let a hostile IdP response aim the
            // API at internal addresses (SSRF).
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("the HTTP client's TLS backend initialises");
        Self {
            settings,
            http,
            provider: Mutex::new(None),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.settings.is_some()
    }

    fn settings(&self) -> Result<&Settings, OidcError> {
        self.settings.as_ref().ok_or(OidcError::NotConfigured)
    }

    /// The provider's endpoints and keys: cached, refetched after
    /// [`DISCOVERY_TTL`] or when `refresh` is set. A failed refetch keeps
    /// serving the previous discovery rather than failing every sign-in.
    async fn provider(&self, refresh: bool) -> Result<Arc<Provider>, OidcError> {
        let settings = self.settings()?;
        let mut cached = self.provider.lock().await;
        if let Some(provider) = cached.as_ref()
            && !refresh
            && provider.fetched_at.elapsed() < DISCOVERY_TTL
        {
            return Ok(Arc::clone(provider));
        }
        match Metadata::discover_async(settings.issuer.clone(), &self.http).await {
            Ok(metadata) => {
                let end_session = metadata.additional_metadata().end_session_endpoint.clone();
                let client = CoreClient::from_provider_metadata(
                    metadata,
                    settings.client_id.clone(),
                    Some(settings.client_secret.clone()),
                )
                .set_redirect_uri(settings.redirect_uri.clone());
                let provider = Arc::new(Provider {
                    client,
                    end_session,
                    fetched_at: Instant::now(),
                });
                *cached = Some(Arc::clone(&provider));
                Ok(provider)
            }
            Err(err) => match cached.as_ref() {
                Some(stale) => {
                    tracing::warn!(error = %Chain(&err), "oidc: discovery failed, keeping the previous one");
                    Ok(Arc::clone(stale))
                }
                None => Err(OidcError::Discovery(Box::new(err))),
            },
        }
    }

    /// A new authorization request. `select_account` makes the IdP ask which
    /// account to use even when one is signed in ("switch account").
    pub async fn start(&self, select_account: bool) -> Result<Started, OidcError> {
        let provider = self.provider(false).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = provider
            .client
            .authorize_url(
                AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
                || CsrfToken::new(crate::token::generate()),
                || Nonce::new(crate::token::generate()),
            )
            .add_scope(Scope::new("email".to_owned()))
            .add_scope(Scope::new("profile".to_owned()))
            .set_pkce_challenge(challenge);
        if select_account {
            request = request.add_prompt(CoreAuthPrompt::SelectAccount);
        }
        let (url, state, nonce) = request.url();
        Ok(Started {
            url,
            state: state.into_secret(),
            nonce: nonce.secret().clone(),
            pkce_verifier: verifier.into_secret(),
        })
    }

    /// Exchanges the callback's code (with the PKCE verifier) and verifies the
    /// ID token: signature, issuer, audience and `azp`, expiry, nonce.
    pub async fn finish(
        &self,
        code: String,
        pkce_verifier: String,
        nonce: &str,
    ) -> Result<Identity, OidcError> {
        let provider = self.provider(false).await?;
        let tokens = provider
            .client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|err| OidcError::Exchange(Box::new(err)))?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
            .request_async(&self.http)
            .await
            .map_err(|err| OidcError::Exchange(Box::new(err)))?;
        let id_token = tokens.id_token().ok_or(OidcError::NoIdToken)?;
        let nonce = Nonce::new(nonce.to_owned());

        let claims = match id_token.claims(&verifier(&provider.client), &nonce) {
            Ok(claims) => claims.clone(),
            // Keys rotated since the last discovery: fetch them and try again.
            Err(ClaimsVerificationError::SignatureVerification(_)) => {
                let fresh = self.provider(true).await?;
                id_token
                    .claims(&verifier(&fresh.client), &nonce)
                    .map_err(OidcError::IdToken)?
                    .clone()
            }
            Err(err) => return Err(OidcError::IdToken(err)),
        };

        let mut identity = identity_from_id_token(&claims);
        // Zitadel puts the profile in the ID token (the provisioner sets
        // idTokenUserinfoAssertion); another IdP may only serve it here.
        if identity.email.is_none() {
            let info: CoreUserInfoClaims = provider
                .client
                .user_info(
                    tokens.access_token().clone(),
                    Some(claims.subject().clone()),
                )
                .map_err(|err| OidcError::UserInfo(Box::new(err)))?
                .request_async(&self.http)
                .await
                .map_err(|err| OidcError::UserInfo(Box::new(err)))?;
            identity.email = info.email().map(|email| email.to_string());
            identity.email_verified = info.email_verified().unwrap_or(false);
            if identity.name.is_none() {
                identity.name = info
                    .name()
                    .and_then(|name| name.get(None))
                    .map(|name| name.to_string());
            }
        }
        Ok(identity)
    }

    /// Where to send the browser to end its IdP session too, coming back to
    /// the sign-in page. `None` when there is no IdP (or it is unreachable):
    /// the romulus session is gone either way.
    pub async fn logout_url(&self) -> Option<Url> {
        let settings = self.settings.as_ref()?;
        let provider = self
            .provider(false)
            .await
            .inspect_err(|err| tracing::warn!(error = %Chain(err), "oidc: no logout endpoint"))
            .ok()?;
        let mut url = provider.end_session.clone()?;
        url.query_pairs_mut()
            .append_pair("client_id", settings.client_id.as_str())
            .append_pair(
                "post_logout_redirect_uri",
                settings.post_logout_uri.as_str(),
            );
        Some(url)
    }
}

/// The issuer as discovery spells it. Discovery and ID tokens are checked
/// against it character for character, and `Url` gives a bare origin a
/// trailing slash that Zitadel's issuer (`http://localhost:<port>`) lacks.
fn issuer(url: &Url) -> IssuerUrl {
    let text = if url.path() == "/" && url.query().is_none() {
        url.as_str().trim_end_matches('/')
    } else {
        url.as_str()
    };
    IssuerUrl::new(text.to_owned()).expect("a URL is a valid issuer")
}

/// Zitadel lists the project next to the client in `aud`; the verifier then
/// requires `azp` to be this client, which is what proves the token was
/// issued to us.
fn verifier(client: &Client) -> openidconnect::core::CoreIdTokenVerifier<'_> {
    client
        .id_token_verifier()
        .set_other_audience_verifier_fn(|_| true)
}

fn identity_from_id_token(claims: &CoreIdTokenClaims) -> Identity {
    let name = claims
        .name()
        .and_then(|name| name.get(None))
        .map(|name| name.to_string())
        .or_else(|| {
            let given = claims.given_name().and_then(|n| n.get(None))?;
            Some(match claims.family_name().and_then(|n| n.get(None)) {
                Some(family) => format!("{} {}", given.as_str(), family.as_str()),
                None => given.to_string(),
            })
        })
        .or_else(|| claims.preferred_username().map(|n| n.to_string()))
        .filter(|name| !name.trim().is_empty());
    Identity {
        subject: claims.subject().to_string(),
        email: claims.email().map(|email| email.to_string()),
        email_verified: claims.email_verified().unwrap_or(false),
        name,
    }
}
