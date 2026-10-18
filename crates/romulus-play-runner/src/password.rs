//! A playground role's password: HMAC(secret, playground, role), as the
//! sandbox derives its pool's, so nothing is stored. The runner hands them
//! to the container's init (from its own secret, not from a request); the
//! service, which holds the same secret, logs in with them.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// The roles a playground has (deploy/playground/play-init.sql).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Owns the schema: building, seeding, migration runs.
    Owner,
    /// The read-only console.
    Read,
    /// The service's control session (cancel, terminate).
    Service,
}

impl Role {
    pub const ALL: [Self; 3] = [Self::Owner, Self::Read, Self::Service];

    /// Its name in Postgres.
    pub fn name(self) -> &'static str {
        match self {
            Self::Owner => "play_owner",
            Self::Read => "play_read",
            Self::Service => "play_service",
        }
    }
}

/// The password of `role` in playground `id`.
pub fn derive(secret: &[u8], id: Uuid, role: Role) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(b"romulus-playground-password\0");
    mac.update(id.as_bytes());
    mac.update(role.name().as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_differ_per_playground_role_and_secret() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let p = derive(b"secret", a, Role::Read);
        assert_eq!(p, derive(b"secret", a, Role::Read));
        assert_ne!(p, derive(b"secret", b, Role::Read));
        assert_ne!(p, derive(b"secret", a, Role::Owner));
        assert_ne!(p, derive(b"other", a, Role::Read));
        assert_eq!(p.len(), 43);
    }
}
