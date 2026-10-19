//! Argon2id hashes of passwords, as PHC strings
//! (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`): the parameters and the
//! salt travel with the hash, so stronger parameters later only need
//! [`needs_rehash`] at the next sign-in.
//!
//! Parameters: OWASP's minimum for Argon2id (19 MiB, 2 passes, 1 lane),
//! a 16-byte random salt. Hashing is deliberately slow and memory-hungry, so
//! it runs on the blocking pool through [`Slots`], the one guard on that
//! cost: at most [`CONCURRENT`] at once (the permit travels with the work,
//! so a request that gives up never frees a slot still hashing), at most
//! [`QUEUE`] waiting, each for at most [`QUEUE_WAIT`]. Only a hashing pool
//! actually saturated answers `busy` (503); there is no requests-per-second
//! cap a few clients could fill for everyone (per-client and per-address
//! limits are `throttle`'s).

use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use argon2::{Argon2, Params, PasswordHasher, PasswordVerifier, password_hash::phc::PasswordHash};
use tokio::sync::Semaphore;
use unicode_normalization::UnicodeNormalization;

/// Hashes computed at the same time, at most (each holds 19 MiB).
pub const CONCURRENT: usize = 4;
/// Requests waiting for a slot, at most: past it, `busy` at once.
pub const QUEUE: usize = 64;
/// How long a request waits for a slot before answering `busy`.
pub const QUEUE_WAIT: Duration = Duration::from_secs(2);

/// A bounded queue in front of a few hashing slots.
pub struct Slots {
    permits: Arc<Semaphore>,
    waiting: AtomicUsize,
    queue: usize,
    wait: Duration,
}

impl Slots {
    pub fn new(concurrent: usize, queue: usize, wait: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrent)),
            waiting: AtomicUsize::new(0),
            queue,
            wait,
        }
    }

    /// Runs `work` on the blocking pool once a slot is free: `Busy` when
    /// the queue is full, or no slot came within the wait.
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, HashError> + Send + 'static,
    ) -> Result<T, HashError> {
        let permit = if let Ok(permit) = self.permits.clone().try_acquire_owned() {
            permit
        } else {
            if self.waiting.fetch_add(1, Ordering::SeqCst) >= self.queue {
                self.waiting.fetch_sub(1, Ordering::SeqCst);
                return Err(HashError::Busy);
            }
            let acquired =
                tokio::time::timeout(self.wait, self.permits.clone().acquire_owned()).await;
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            acquired
                .map_err(|_| HashError::Busy)?
                .map_err(|err| HashError::Failed(err.to_string()))?
        };
        tokio::task::spawn_blocking(move || {
            // Held until the hash is done, whether anyone still waits for it.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|err| HashError::Failed(err.to_string()))?
    }
}

static SLOTS: LazyLock<Slots> = LazyLock::new(|| Slots::new(CONCURRENT, QUEUE, QUEUE_WAIT));

/// What an unknown address's password is checked against, so a sign-in
/// takes as long whether the account exists or not.
static DECOY: LazyLock<String> =
    LazyLock::new(|| hash_now(&crate::token::generate()).expect("hashing a random password works"));

#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// Hashing is saturated: the queue was full, or no slot came within
    /// [`QUEUE_WAIT`].
    #[error("too many passwords are being checked at once")]
    Busy,
    #[error("password hashing failed: {0}")]
    Failed(String),
}

/// The form a password is hashed in: NFC, so the same characters typed on
/// two keyboards (é as one code point or two) are one password. Never
/// trimmed: spaces are characters.
pub fn normalize(password: &str) -> String {
    password.nfc().collect()
}

/// A new PHC string for `password`.
pub async fn hash(password: &str) -> Result<String, HashError> {
    let password = normalize(password);
    blocking(move || hash_now(&password)).await
}

/// Whether `password` matches `phc`; with `None` (no such account, or one
/// without a password yet) it checks against a decoy and answers false, in
/// about the same time.
pub async fn verify(password: &str, phc: Option<String>) -> Result<bool, HashError> {
    let password = normalize(password);
    blocking(move || {
        let (phc, real) = match phc {
            Some(phc) => (phc, true),
            None => (DECOY.clone(), false),
        };
        Ok(verify_now(&password, &phc) && real)
    })
    .await
}

/// Whether `phc` was made with other parameters than today's (it is then
/// hashed again after a successful sign-in).
pub fn needs_rehash(phc: &str) -> bool {
    let today = Params::default();
    PasswordHash::new(phc)
        .ok()
        .filter(|hash| hash.algorithm.as_str() == "argon2id")
        .and_then(|hash| Params::try_from(&hash).ok())
        .is_none_or(|params| {
            (params.m_cost(), params.t_cost(), params.p_cost())
                != (today.m_cost(), today.t_cost(), today.p_cost())
        })
}

fn hash_now(password: &str) -> Result<String, HashError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|err| HashError::Failed(err.to_string()))?;
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| HashError::Failed(err.to_string()))
}

fn verify_now(password: &str, phc: &str) -> bool {
    // The parameters come from the PHC string, whatever `default()` says.
    Argon2::default()
        .verify_password(password.as_bytes(), phc)
        .is_ok()
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, HashError> + Send + 'static,
) -> Result<T, HashError> {
    SLOTS.run(work).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hash_verifies_its_password_only() {
        let phc = hash("correct horse battery").await.unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        assert!(
            verify("correct horse battery", Some(phc.clone()))
                .await
                .unwrap()
        );
        assert!(
            !verify("correct horse batter", Some(phc.clone()))
                .await
                .unwrap()
        );
        assert!(!needs_rehash(&phc));
    }

    #[tokio::test]
    async fn salts_differ() {
        assert_ne!(
            hash("same password!").await.unwrap(),
            hash("same password!").await.unwrap()
        );
    }

    #[tokio::test]
    async fn both_spellings_of_an_accent_are_one_password() {
        let phc = hash("E\u{301}lodie's secret garden").await.unwrap();
        assert!(verify("Élodie's secret garden", Some(phc)).await.unwrap());
    }

    #[tokio::test]
    async fn only_a_saturated_pool_is_busy() {
        let slots = Arc::new(Slots::new(1, 1, Duration::from_millis(200)));
        let (release, held) = std::sync::mpsc::channel::<()>();
        // The one slot, busy until released.
        let running = {
            let slots = slots.clone();
            tokio::spawn(async move {
                slots
                    .run(move || {
                        held.recv().ok();
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        // One may wait (and gives up after the wait); a second waiter at
        // the same time finds the queue full at once.
        let waiter = {
            let slots = slots.clone();
            tokio::spawn(async move { slots.run(|| Ok(())).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(slots.run(|| Ok(())).await, Err(HashError::Busy)));
        assert!(matches!(waiter.await.unwrap(), Err(HashError::Busy)));
        release.send(()).unwrap();
        running.await.unwrap().unwrap();
        // Free again: no rate, only saturation, ever says busy.
        for _ in 0..10 {
            slots.run(|| Ok(())).await.unwrap();
        }
    }

    #[tokio::test]
    async fn no_account_never_verifies() {
        assert!(!verify("anything at all", None).await.unwrap());
    }

    #[test]
    fn weaker_or_foreign_hashes_are_rehashed() {
        assert!(needs_rehash(
            "$argon2id$v=19$m=4096,t=1,p=1$c29tZXNhbHQ$aGFzaGhhc2hoYXNo"
        ));
        assert!(needs_rehash("not a phc string"));
    }
}
