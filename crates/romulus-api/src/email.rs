//! Email addresses: how they are stored and how two of them are compared.
//!
//! Invitations are matched to accounts by address, so "the same address" must
//! mean one thing everywhere. Unicode spells "é" two ways (one code point, or
//! "e" plus a combining accent) and has case beyond ASCII. romulus:
//! - stores every address in NFC ([`normalize`]; the database refuses others:
//!   `users_email_nfc`, `invitations_email_nfc`), keeping its case for display;
//! - compares in Postgres with `lower(email COLLATE pg_unicode_fast)`: Unicode
//!   case mapping that does not depend on the database's locale (a `C`-locale
//!   database would otherwise only fold ASCII). [`key`] is the same comparison
//!   in Rust, and a test keeps the two in agreement.

use unicode_normalization::UnicodeNormalization;

/// The stored form of an address: trimmed, NFC, case kept.
pub fn normalize(email: &str) -> String {
    email.trim().nfc().collect()
}

/// What two addresses are equal by: NFC, then Unicode lowercase.
pub fn key(email: &str) -> String {
    normalize(email).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_spellings_of_an_accent_are_one_address() {
        assert_eq!(normalize("E\u{301}lodie@x.io"), "Élodie@x.io");
        assert_eq!(key("E\u{301}LODIE@X.IO"), key(" élodie@x.io "));
        assert_ne!(key("elodie@x.io"), key("élodie@x.io"));
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn rust_and_postgres_agree_on_equal_addresses(db: sqlx::PgPool) {
        let samples = [
            "alice@romulus.test",
            "ALICE@Romulus.Test",
            "Élodie@x.io",
            "élodie@x.io",
            "E\u{301}lodie@x.io",
            "ÆSIR@x.io",
            "æsir@x.io",
            "Ørsted@x.dk",
            "øRSTED@X.DK",
            "МАША@x.ru",
            "маша@x.ru",
            "Straße@x.de",
        ];
        for a in samples {
            for b in samples {
                let postgres = sqlx::query_scalar!(
                    r#"SELECT lower(normalize($1, NFC) COLLATE pg_unicode_fast)
                            = lower(normalize($2, NFC) COLLATE pg_unicode_fast) AS "same!""#,
                    a.trim(),
                    b.trim(),
                )
                .fetch_one(&db)
                .await
                .unwrap();
                assert_eq!(key(a) == key(b), postgres, "{a} vs {b}");
            }
        }
    }
}
