//! Rate limits for endpoints that cost real work: pushes (per API token) and
//! SQL compiles (per person). A token bucket per key: a burst, then one more
//! request per refill period. In memory, like everything per-instance in the
//! MVP (one API instance); a restart forgets the counts, which only ever
//! errs on the generous side.
//!
//! (`auth::limit` is the same idea for sign-ins, keyed by IP, with its own
//! sweep; it can move onto this one.)

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use uuid::Uuid;

use crate::ApiError;

/// Past this many tracked keys, the ones back to a full bucket are forgotten.
const MAX_TRACKED: usize = 10_000;

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// A token bucket per key.
pub struct Limiter<K> {
    burst: u32,
    refill: Duration,
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K: Hash + Eq> Limiter<K> {
    /// `burst` requests at once, then one per `refill`.
    pub fn new(burst: u32, refill: Duration) -> Self {
        assert!(
            burst > 0 && !refill.is_zero(),
            "a limiter must allow something"
        );
        Self {
            burst,
            refill,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one request for `key`, or answers 429 `rate_limited` with how
    /// long until the next one is allowed.
    pub fn check(&self, key: K) -> Result<(), ApiError> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: K, now: Instant) -> Result<(), ApiError> {
        let burst = f64::from(self.burst);
        let refill = self.refill.as_secs_f64();
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let level = |b: &Bucket| {
            (b.tokens + now.saturating_duration_since(b.at).as_secs_f64() / refill).min(burst)
        };
        if buckets.len() >= MAX_TRACKED {
            buckets.retain(|_, bucket| level(bucket) < burst);
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        bucket.tokens = level(bucket);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(ApiError::RateLimited {
                retry_after: self.refill.mul_f64(1.0 - bucket.tokens),
            })
        }
    }
}

/// The limits this instance enforces, in `AppState`.
pub struct Limits {
    /// Pushes per API token: CI on every commit of a busy repository fits,
    /// a loop does not.
    pub push: Limiter<Uuid>,
    /// SQL compiles (checks and uploads) per person: the sandbox is shared.
    pub compile: Limiter<Uuid>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            push: Limiter::new(20, Duration::from_secs(3)),
            compile: Limiter::new(30, Duration::from_secs(2)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_gets_a_burst_then_one_per_refill() {
        let limiter = Limiter::new(3, Duration::from_secs(2));
        let start = Instant::now();
        for _ in 0..3 {
            assert!(limiter.check_at(1, start).is_ok());
        }
        let Err(ApiError::RateLimited { retry_after }) = limiter.check_at(1, start) else {
            panic!("the fourth request is refused");
        };
        assert_eq!(retry_after, Duration::from_secs(2));
        assert!(limiter.check_at(1, start + Duration::from_secs(1)).is_err());
        assert!(limiter.check_at(1, start + Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn keys_are_counted_apart() {
        let limiter = Limiter::new(1, Duration::from_mins(1));
        let now = Instant::now();
        assert!(limiter.check_at("a", now).is_ok());
        assert!(limiter.check_at("a", now).is_err());
        assert!(limiter.check_at("b", now).is_ok());
    }
}
