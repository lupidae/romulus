//! What a new password must be (OWASP ASVS, chapter 2): long enough, not
//! too long, not one everybody guesses first. No composition rules (a digit,
//! a symbol…): length does more, and every character is allowed, spaces and
//! emoji included. Checked on the NFC form `hash::normalize` hashes.
//!
//! "Guessed first" is breach data (ASVS 2.1.7, offline): `common-passwords.txt.gz`
//! holds every password of the `SecLists` (MIT) `xato-net-10-million-passwords-1000000`
//! and `100k-most-used-passwords-NCSC` long enough to pass the length rule
//! (12 to 128 characters), NFC, lowercased, deduplicated and sorted: 46,825
//! of them, 332 KB gzipped, read once on first use. Besides it: repetitions,
//! runs along the alphabet or a keyboard, the account's own name and
//! address, and passwords built on a handful of words.

/// At least this many characters.
pub const MIN_CHARS: usize = 12;
/// At most this many: long enough for any passphrase or manager, short
/// enough that hashing one costs what hashing any costs.
pub const MAX_CHARS: usize = 128;

/// Why `password` will not do, in a sentence for the form; `None` if it
/// will. `email` and `name` are the account's: a password made of them is
/// the first thing tried.
pub fn problem(password: &str, email: &str, name: &str) -> Option<&'static str> {
    let password = super::hash::normalize(password);
    let length = password.chars().count();
    if length < MIN_CHARS {
        return Some("Use at least 12 characters. A few unrelated words make a strong password.");
    }
    if length > MAX_CHARS {
        return Some("Use at most 128 characters.");
    }
    let lower = password.to_lowercase();
    if is_repetition(&lower) || is_sequence(&lower) {
        return Some("Avoid repeated or sequential characters: pick something less predictable.");
    }
    let local = email.split('@').next().unwrap_or_default().to_lowercase();
    let mentions_email = lower.contains(&email.to_lowercase())
        || (local.chars().count() >= 4 && lower.contains(&local));
    let mentions_name = name
        .split_whitespace()
        .map(str::to_lowercase)
        .any(|word| word.chars().count() >= 4 && lower.contains(&word));
    if mentions_email || mentions_name {
        return Some("Leave your name and email address out of your password.");
    }
    if is_common(&lower) {
        return Some("This password is among the most common ones. Pick something less guessable.");
    }
    None
}

/// The same short unit over and over: `aaaaaaaaaaaa`, `abcabcabcabc`.
fn is_repetition(password: &str) -> bool {
    let chars: Vec<char> = password.chars().collect();
    (1..=4).any(|unit| {
        chars.len() > unit && chars.iter().enumerate().all(|(i, c)| *c == chars[i % unit])
    })
}

/// A run along the alphabet, the digits or a keyboard row, either way.
fn is_sequence(password: &str) -> bool {
    const RUNS: [&str; 6] = [
        "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
        "01234567890123456789012345678901234567890",
        "qwertyuiopasdfghjklzxcvbnmqwertyuiop",
        "1qaz2wsx3edc4rfv5tgb6yhn7ujm8ik9ol0p",
        "1q2w3e4r5t6y7u8i9o0p",
        "qazwsxedcrfvtgbyhnujmikolp",
    ];
    let reversed: String = password.chars().rev().collect();
    RUNS.iter()
        .any(|run| run.contains(password) || run.contains(reversed.as_str()))
}

/// The words people build passwords from.
const WORDS: [&str; 40] = [
    "password",
    "passw",
    "pass",
    "passwort",
    "motdepasse",
    "contrasena",
    "romulus",
    "remus",
    "qwerty",
    "azerty",
    "letmein",
    "welcome",
    "admin",
    "administrator",
    "iloveyou",
    "monkey",
    "dragon",
    "football",
    "baseball",
    "soccer",
    "sunshine",
    "princess",
    "starwars",
    "master",
    "shadow",
    "superman",
    "batman",
    "trustno",
    "changeme",
    "secret",
    "login",
    "abc",
    "qwe",
    "asdf",
    "zxcv",
    "test",
    "default",
    "postgres",
    "database",
    "root",
];
/// Whole passwords long enough to pass the length rule that still top the
/// breach lists.
const WHOLE: [&str; 16] = [
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123123123123",
    "1q2w3e4r5t6y",
    "q1w2e3r4t5y6",
    "1qaz2wsx3edc",
    "zaq12wsxcde3",
    "a1b2c3d4e5f6",
    "1234qwerasdf",
    "qwerty123456",
    "123456qwerty",
    "iloveyou1234",
    "passwordpassword",
    "correcthorsebatterystaple",
    "correct horse battery staple",
];

/// The breach list, as a set (see the module docs).
static BREACHED: std::sync::LazyLock<std::collections::HashSet<String>> =
    std::sync::LazyLock::new(|| {
        use std::io::Read as _;
        let mut text = String::new();
        flate2::read::GzDecoder::new(&include_bytes!("common-passwords.txt.gz")[..])
            .read_to_string(&mut text)
            .expect("the embedded password list is valid gzip and UTF-8");
        text.lines().map(str::to_owned).collect()
    });

/// In the breach list, built on one of [`WORDS`], or one of [`WHOLE`].
fn is_common(password: &str) -> bool {
    if BREACHED.contains(password) {
        return true;
    }
    // The letters alone: `Password2024!` is `password`.
    let letters: String = password.chars().filter(|c| c.is_alphabetic()).collect();

    WHOLE.contains(&password) || WORDS.iter().any(|word| letters == *word)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 128 characters.
    const LONGEST: &str = "the quick brown fox jumps over the lazy dog while seven \
        wizards quietly box jumping zebras near a frozen lake at dawn, ok then!!";

    fn ok(password: &str) -> bool {
        problem(password, "alice.martin@acme.io", "Alice Martin").is_none()
    }

    #[test]
    fn long_unpredictable_passwords_pass() {
        for password in [
            "violet tractor mango",
            "Gr4nite-Pelican-Sky",
            "élodie aime le café noir",
            "🦊 runs through 🌲 at night",
            "8472 9910 3355 7781",
            LONGEST,
        ] {
            assert!(ok(password), "{password}");
        }
    }

    #[test]
    fn length_is_counted_in_characters() {
        assert!(!ok("short pass"));
        // 12 characters, 24 bytes.
        assert!(ok("éàüöçñæøåßðþ"));
        assert_eq!(LONGEST.chars().count(), MAX_CHARS);
        assert!(!ok(&format!("{LONGEST}!")));
    }

    #[test]
    fn predictable_passwords_are_refused() {
        for password in [
            "aaaaaaaaaaaa",
            "abcabcabcabc",
            "123456789012",
            "abcdefghijklmn",
            "zyxwvutsrqpon",
            "qwertyuiopasd",
            "1q2w3e4r5t6y",
            "Password2024!",
            "romulus12345678",
            "000000000000",
            "correct horse battery staple",
        ] {
            assert!(!ok(password), "{password}");
        }
    }

    #[test]
    fn breached_passwords_are_refused() {
        assert!(BREACHED.len() > 40_000);
        // Long, unremarkable, and in the breach lists.
        for password in [
            "sojdlg123aljg",
            "startfinding",
            "Telechargement",
            "polniypizdec0211",
        ] {
            assert!(!ok(password), "{password}");
        }
    }

    #[test]
    fn the_account_s_own_name_and_address_are_refused() {
        assert!(!ok("alice.martin@acme.io"));
        assert!(!ok("alice.martin rocks!"));
        assert!(!ok("my name is Martin, ok"));
        // Short words of a name are fine inside a longer password.
        assert!(problem("ed loves green tea", "ed@x.io", "Ed Wu").is_none());
    }
}
