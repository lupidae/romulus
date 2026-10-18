//! The API's signature on every call to the playground service
//! (docs/playground.md, "Forged calls to the service or runner"): an HMAC
//! over what the call may do (the operation, the playground, its
//! organization, who asks and with which rights, the body's hash) and when
//! it expires. The service trusts nothing else: the API checked the session,
//! CSRF and the membership before signing, and the key (`ROMULUS_PLAY_KEY`)
//! lives only in the two processes.
//!
//! The header is `Authorization: RomulusPlay <claims>.<signature>`, both
//! base64url: the claims as JSON, the HMAC-SHA256 of the claims' encoding.
//! A signature lives [`LIFETIME`]; the service refuses one expiring further
//! out, so a captured header is useless within minutes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::protocol::Access;

pub const SCHEME: &str = "RomulusPlay";
/// How long a signature is valid (seconds).
pub const LIFETIME: i64 = 60;

/// What a call may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Build,
    Migrate,
    Reset,
    Stop,
    Destroy,
    Status,
    List,
    Console,
}

/// What the API vouches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub op: Op,
    /// `None` for [`Op::List`] only.
    pub playground: Option<Uuid>,
    /// The playground's organization (caps on running playgrounds).
    pub org: Option<Uuid>,
    /// Who asks (`None`: romulus itself, the expiry sweep), and their name
    /// (what the people in the playground are told: "Alice is resetting it").
    pub user: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    pub access: Access,
    /// Unix seconds.
    pub exp: i64,
    /// Hex SHA-256 of the body, for calls that have one.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("no RomulusPlay authorization")]
    Missing,
    #[error("malformed authorization")]
    Malformed,
    #[error("bad signature")]
    Bad,
    #[error("expired, or valid for too long")]
    Expired,
    #[error("signed for another operation, playground or body")]
    Mismatch,
}

fn mac(key: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(b"romulus-playground-call\0");
    mac
}

/// Hex SHA-256 of a body.
pub fn body_hash(body: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(body)
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

impl Claims {
    /// Claims for `op` on `playground`, expiring [`LIFETIME`] after `now`.
    pub fn new(op: Op, playground: Option<Uuid>, access: Access, now: i64) -> Self {
        Self {
            op,
            playground,
            org: None,
            user: None,
            name: None,
            access,
            exp: now + LIFETIME,
            body: None,
        }
    }

    #[must_use]
    pub fn org(mut self, org: Uuid) -> Self {
        self.org = Some(org);
        self
    }

    #[must_use]
    pub fn user(mut self, user: Uuid) -> Self {
        self.user = Some(user);
        self
    }

    #[must_use]
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }

    #[must_use]
    pub fn body(mut self, body: &[u8]) -> Self {
        self.body = Some(body_hash(body));
        self
    }

    /// The `Authorization` header's value.
    pub fn sign(&self, key: &[u8]) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("claims serialize"));
        let mut mac = mac(key);
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{SCHEME} {payload}.{signature}")
    }

    /// The claims of a header the API signed with `key`, still valid at
    /// `now`. Compares in constant time.
    pub fn verify(key: &[u8], header: Option<&str>, now: i64) -> Result<Self, SignatureError> {
        let header = header.ok_or(SignatureError::Missing)?;
        let token = header
            .strip_prefix(SCHEME)
            .and_then(|rest| rest.strip_prefix(' '))
            .ok_or(SignatureError::Missing)?;
        let (payload, signature) = token.split_once('.').ok_or(SignatureError::Malformed)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| SignatureError::Malformed)?;
        let mut mac = mac(key);
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| SignatureError::Bad)?;
        let json = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| SignatureError::Malformed)?;
        let claims: Self = serde_json::from_slice(&json).map_err(|_| SignatureError::Malformed)?;
        if claims.exp < now || claims.exp > now + LIFETIME + 5 {
            return Err(SignatureError::Expired);
        }
        Ok(claims)
    }

    /// The claims allow `op` on `playground`, with exactly `body`.
    pub fn allow(
        &self,
        op: Op,
        playground: Option<Uuid>,
        body: Option<&[u8]>,
    ) -> Result<(), SignatureError> {
        let body_ok = match (&self.body, body) {
            (None, None) => true,
            (Some(hash), Some(body)) => {
                use subtle::ConstantTimeEq as _;
                bool::from(hash.as_bytes().ct_eq(body_hash(body).as_bytes()))
            }
            (None, Some(body)) => body.is_empty(),
            (Some(_), None) => false,
        };
        if self.op == op && self.playground == playground && body_ok {
            Ok(())
        } else {
            Err(SignatureError::Mismatch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn claims() -> Claims {
        Claims::new(Op::Reset, Some(Uuid::from_u128(1)), Access::Develop, 1_000)
            .org(Uuid::from_u128(2))
            .user(Uuid::from_u128(3))
    }

    #[test]
    fn a_signed_call_verifies_and_allows_what_it_was_signed_for() {
        let header = claims().sign(KEY);
        let verified = Claims::verify(KEY, Some(&header), 1_010).unwrap();
        assert_eq!(verified, claims());
        assert!(
            verified
                .allow(Op::Reset, Some(Uuid::from_u128(1)), None)
                .is_ok()
        );
        assert_eq!(
            verified.allow(Op::Destroy, Some(Uuid::from_u128(1)), None),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verified.allow(Op::Reset, Some(Uuid::from_u128(9)), None),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn another_key_a_changed_claim_or_an_old_signature_is_refused() {
        let header = claims().sign(KEY);
        assert_eq!(
            Claims::verify(b"another key, 32 bytes long......", Some(&header), 1_010),
            Err(SignatureError::Bad)
        );
        // Promote the call to Develop on another playground: the MAC breaks.
        let (payload, signature) = header
            .strip_prefix("RomulusPlay ")
            .unwrap()
            .split_once('.')
            .unwrap();
        let mut forged: Claims =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        forged.playground = Some(Uuid::from_u128(9));
        let forged = format!(
            "RomulusPlay {}.{signature}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap())
        );
        assert_eq!(
            Claims::verify(KEY, Some(&forged), 1_010),
            Err(SignatureError::Bad)
        );
        assert_eq!(
            Claims::verify(KEY, Some(&header), 1_000 + LIFETIME + 1),
            Err(SignatureError::Expired)
        );
        // Signed to last a day: refused, whatever the key.
        let mut long = claims();
        long.exp = 1_000 + 86_400;
        assert_eq!(
            Claims::verify(KEY, Some(&long.sign(KEY)), 1_000),
            Err(SignatureError::Expired)
        );
        assert_eq!(
            Claims::verify(KEY, None, 1_000),
            Err(SignatureError::Missing)
        );
        assert_eq!(
            Claims::verify(KEY, Some("Bearer x"), 1_000),
            Err(SignatureError::Missing)
        );
    }

    #[test]
    fn the_body_is_part_of_the_signature() {
        let signed = claims().body(b"{\"a\":1}");
        assert!(
            signed
                .allow(Op::Reset, Some(Uuid::from_u128(1)), Some(b"{\"a\":1}"))
                .is_ok()
        );
        assert_eq!(
            signed.allow(Op::Reset, Some(Uuid::from_u128(1)), Some(b"{\"a\":2}")),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            claims().allow(Op::Reset, Some(Uuid::from_u128(1)), Some(b"x")),
            Err(SignatureError::Mismatch)
        );
    }
}
