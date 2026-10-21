//! A fake OIDC provider on a random local port: discovery, keys, and a
//! token endpoint that checks the client secret and the PKCE verifier and
//! answers an ID token signed with a key generated for this test run. Lets
//! the sign-in tests run the API's real relying party end to end. Also
//! Zitadel's `DELETE /v2/users/{id}` (with [`ADMIN_TOKEN`]), for deleting
//! accounts.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use openidconnect::{
    AccessToken, Audience, AuthUrl, EmptyAdditionalClaims, EmptyAdditionalProviderMetadata,
    EmptyExtraTokenFields, EndUserEmail, EndUserName, IssuerUrl, JsonWebKeyId, JsonWebKeySetUrl,
    LocalizedClaim, Nonce, PrivateSigningKey, ResponseTypes, StandardClaims, SubjectIdentifier,
    TokenUrl,
    core::{
        CoreIdToken, CoreIdTokenClaims, CoreIdTokenFields, CoreJsonWebKeySet,
        CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey,
        CoreSubjectIdentifierType, CoreTokenResponse, CoreTokenType,
    },
};
use romulus_api::config::{Config, OidcConfig, Secret};
use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use url::Url;

pub const CLIENT_ID: &str = "romulus-test";
pub const CLIENT_SECRET: &str = "test-secret";
/// The personal access token Zitadel's user API takes here.
pub const ADMIN_TOKEN: &str = "test-zitadel-pat";

/// What the IdP says about whoever signs in with a code.
#[derive(Debug, Clone)]
pub struct Claims {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

impl Claims {
    pub fn verified(subject: &str, email: &str, name: &str) -> Self {
        Self {
            subject: subject.to_owned(),
            email: Some(email.to_owned()),
            email_verified: true,
            name: Some(name.to_owned()),
        }
    }
}

/// An authorization the fake IdP granted: the code's nonce and PKCE challenge
/// as the authorize request carried them, and who signed in.
struct Grant {
    nonce: String,
    challenge: String,
    claims: Claims,
}

#[derive(Clone)]
struct Idp {
    issuer: Url,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
    /// Tests can make the IdP misbehave.
    tamper: Arc<Mutex<Tamper>>,
    /// Users `DELETE /v2/users/{id}` deleted, in order.
    deleted: Arc<Mutex<Vec<String>>>,
}

#[derive(Default, Clone, Copy)]
pub struct Tamper {
    /// Sign the ID token with a nonce other than the one requested.
    pub wrong_nonce: bool,
    /// Issue the ID token for another client.
    pub wrong_audience: bool,
    /// Answer 500 to user deletions.
    pub refuse_deletions: bool,
}

pub struct FakeIdp {
    pub issuer: Url,
    idp: Idp,
}

fn signing_key() -> &'static CoreRsaPrivateSigningKey {
    static KEY: OnceLock<CoreRsaPrivateSigningKey> = OnceLock::new();
    KEY.get_or_init(|| {
        let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).expect("an RSA key");
        let pem = key.to_pkcs1_pem(LineEnding::LF).expect("PEM");
        CoreRsaPrivateSigningKey::from_pem(&pem, Some(JsonWebKeyId::new("test".to_owned())))
            .expect("a signing key")
    })
}

impl FakeIdp {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port");
        let issuer: Url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
            .parse()
            .unwrap();
        let idp = Idp {
            issuer: issuer.clone(),
            grants: Arc::default(),
            tamper: Arc::default(),
            deleted: Arc::default(),
        };
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/keys", get(keys))
            .route("/token", post(token))
            .route("/v2/users/{id}", delete(delete_user))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self { issuer, idp }
    }

    /// A config whose OIDC client is this IdP.
    pub fn config(&self) -> Config {
        let mut config = Config::for_tests();
        config.oidc = Some(OidcConfig {
            issuer: self.issuer.clone(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: Secret::new(CLIENT_SECRET),
        });
        config
    }

    /// Plays the user signing in at the IdP: takes the authorization URL the
    /// API redirected to and returns the code the IdP would send back, which
    /// signs `claims` in.
    pub fn authorize(&self, authorize_url: &str, claims: Claims) -> (String, Url) {
        let url: Url = authorize_url.parse().expect("an absolute authorize URL");
        let param = |name: &str| {
            url.query_pairs().find(|(key, _)| key == name).map_or_else(
                || panic!("the authorize URL has no {name}"),
                |(_, value)| value.into_owned(),
            )
        };
        assert_eq!(param("code_challenge_method"), "S256");
        assert_eq!(param("client_id"), CLIENT_ID);
        let code = romulus_api::token::generate();
        self.idp.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                nonce: param("nonce"),
                challenge: param("code_challenge"),
                claims,
            },
        );
        (code, url)
    }

    pub fn tamper(&self, tamper: Tamper) {
        *self.idp.tamper.lock().unwrap() = tamper;
    }

    /// [`Self::config`] with Zitadel's user API on (`ROMULUS_ZITADEL_TOKEN`).
    pub fn config_with_user_api(&self) -> Config {
        let mut config = self.config();
        config.zitadel_token = Some(Secret::new(ADMIN_TOKEN));
        config
    }

    /// The users deleted through the user API, in order.
    pub fn deleted_users(&self) -> Vec<String> {
        self.idp.deleted.lock().unwrap().clone()
    }
}

/// Zitadel's `DELETE /v2/users/{id}`: ids starting with `gone-` are 404s.
async fn delete_user(
    State(idp): State<Idp>,
    headers: HeaderMap,
    romulus_api::extract::Path(id): romulus_api::extract::Path<String>,
) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {ADMIN_TOKEN}"))
    {
        return axum_json(StatusCode::UNAUTHORIZED, &json!({ "code": 16 }));
    }
    if idp.tamper.lock().unwrap().refuse_deletions {
        return axum_json(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "code": 13 }));
    }
    if id.starts_with("gone-") {
        return axum_json(StatusCode::NOT_FOUND, &json!({ "code": 5 }));
    }
    idp.deleted.lock().unwrap().push(id);
    axum_json(StatusCode::OK, &json!({ "details": {} }))
}

async fn discovery(State(idp): State<Idp>) -> Response {
    let base = idp.issuer.as_str().trim_end_matches('/');
    let metadata = CoreProviderMetadata::new(
        IssuerUrl::new(base.to_owned()).unwrap(),
        AuthUrl::new(format!("{base}/authorize")).unwrap(),
        JsonWebKeySetUrl::new(format!("{base}/keys")).unwrap(),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(TokenUrl::new(format!("{base}/token")).unwrap()));
    let mut json = serde_json::to_value(metadata).unwrap();
    json["end_session_endpoint"] = Value::String(format!("{base}/logout"));
    axum_json(StatusCode::OK, &json)
}

async fn keys() -> Response {
    let jwks = CoreJsonWebKeySet::new(vec![signing_key().as_verification_key()]);
    axum_json(StatusCode::OK, &serde_json::to_value(jwks).unwrap())
}

async fn token(State(idp): State<Idp>, headers: HeaderMap, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&expected)
    {
        return axum_json(
            StatusCode::UNAUTHORIZED,
            &json!({ "error": "invalid_client" }),
        );
    }
    let Some(grant) = form
        .get("code")
        .and_then(|code| idp.grants.lock().unwrap().remove(code))
    else {
        return axum_json(
            StatusCode::BAD_REQUEST,
            &json!({ "error": "invalid_grant" }),
        );
    };
    let verifier = form
        .get("code_verifier")
        .map(String::as_str)
        .unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != grant.challenge {
        return axum_json(
            StatusCode::BAD_REQUEST,
            &json!({ "error": "invalid_grant", "error_description": "PKCE" }),
        );
    }

    let tamper = *idp.tamper.lock().unwrap();
    let mut standard = StandardClaims::new(SubjectIdentifier::new(grant.claims.subject.clone()))
        .set_email(grant.claims.email.clone().map(EndUserEmail::new))
        .set_email_verified(Some(grant.claims.email_verified));
    if let Some(name) = &grant.claims.name {
        standard = standard.set_name(Some(LocalizedClaim::from(EndUserName::new(name.clone()))));
    }
    let audience = if tamper.wrong_audience {
        "someone-else"
    } else {
        CLIENT_ID
    };
    let nonce = if tamper.wrong_nonce {
        "not-the-nonce".to_owned()
    } else {
        grant.nonce
    };
    let now = chrono::Utc::now();
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(idp.issuer.as_str().trim_end_matches('/').to_owned()).unwrap(),
        vec![Audience::new(audience.to_owned())],
        now + chrono::Duration::minutes(5),
        now,
        standard,
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let id_token = CoreIdToken::new(
        claims,
        signing_key(),
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .expect("a signed ID token");
    let response = CoreTokenResponse::new(
        AccessToken::new("access".to_owned()),
        CoreTokenType::Bearer,
        CoreIdTokenFields::new(Some(id_token), EmptyExtraTokenFields {}),
    );
    axum_json(StatusCode::OK, &serde_json::to_value(response).unwrap())
}

fn axum_json(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}
