//! Random secrets handed to a browser or a person (session ids, invitation
//! links, OIDC state), and the hash that is stored instead of them.
//!
//! A token is 32 random bytes from the OS, base64url without padding (43
//! characters, safe in a cookie, a path segment or a query string). The
//! database only ever holds `sha256(token)`, so a leaked table yields nothing
//! usable; a lookup hashes what the client presents and compares hashes.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

/// A new random token.
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    // The OS random source failing leaves no safe way to continue.
    getrandom::fill(&mut bytes).expect("the OS random number generator is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What the database stores for `token`: 32 bytes, matching the
/// `octet_length(…) = 32` checks on every `*_hash` column.
pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Whether `value` has the shape [`generate`] produces. Lets handlers turn
/// garbage away before a query, and keeps it out of logs.
pub fn is_well_formed(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_well_formed() {
        let (a, b) = (generate(), generate());
        assert_ne!(a, b);
        assert!(is_well_formed(&a), "{a}");
    }

    #[test]
    fn hashes_fit_the_hash_columns() {
        assert_eq!(hash(&generate()).len(), 32);
        assert_eq!(hash("same"), hash("same"));
    }

    #[test]
    fn garbage_is_not_a_token() {
        for value in [
            "",
            "short",
            &"a".repeat(44),
            &format!("{}/", "a".repeat(42)),
        ] {
            assert!(!is_well_formed(value), "{value}");
        }
    }
}
