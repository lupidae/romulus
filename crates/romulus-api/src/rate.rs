//! Rate limits for endpoints that cost real work: pushes (per API token), SQL
//! compiles (per person, and per database). A token bucket per key: a burst, then one more
//! request per refill period. In memory, like everything per-instance in the
//! MVP (one API instance); a restart forgets the counts, which only ever
//! errs on the generous side.
//!
//! (`auth::limit` is the same idea for sign-ins, keyed by IP, with its own
//! sweep; it can move onto this one.)

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex, PoisonError},
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

/// Work running at once (exports streaming): at most `per_key` per key and
/// `total` together; a slot is held until its guard drops.
pub struct Slots {
    per_key: usize,
    total: usize,
    running: Arc<Mutex<HashMap<Uuid, usize>>>,
}

/// A slot of [`Slots`], given back when dropped.
pub struct Slot {
    running: Arc<Mutex<HashMap<Uuid, usize>>>,
    key: Uuid,
}

impl Slots {
    pub fn new(per_key: usize, total: usize) -> Self {
        assert!(per_key > 0 && total > 0, "slots must allow something");
        Self {
            per_key,
            total,
            running: Arc::default(),
        }
    }

    /// A slot for `key`, or 429 `rate_limited` (try again in a little while)
    /// when `key` or everyone has as many running as allowed.
    pub fn take(&self, key: Uuid) -> Result<Slot, ApiError> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let all: usize = running.values().sum();
        let mine = running.get(&key).copied().unwrap_or(0);
        if mine >= self.per_key || all >= self.total {
            return Err(ApiError::RateLimited {
                retry_after: Duration::from_secs(15),
            });
        }
        *running.entry(key).or_default() += 1;
        Ok(Slot {
            running: Arc::clone(&self.running),
            key,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = running.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                running.remove(&self.key);
            }
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
    /// SQL compiles per database, whoever asks (people, and every API token
    /// of it, which cost nothing to create).
    pub database_compiles: Limiter<Uuid>,
    /// Migration imports per database: each replays a whole history in
    /// the sandbox.
    pub imports: Limiter<Uuid>,
    /// Import previews per database: each reads, unzips and parses up to
    /// 64 MiB on the API. Apart from `imports`, so that looking at the
    /// files a few times never costs the import itself.
    pub import_previews: Limiter<Uuid>,
    /// Changes to versions' discussions per person (a draft's are budgeted
    /// by its room): someone commenting writes a few at a time.
    pub version_comments: Limiter<Uuid>,
    /// Playgrounds created per person: each builds a cluster on the host.
    pub playgrounds: Limiter<Uuid>,
    /// Resets and migration runs per playground: each copies its cluster
    /// back and restarts it.
    pub playground_actions: Limiter<Uuid>,
    /// Console runs per person, across playgrounds (each takes a statement
    /// slot on the host and a history row).
    pub playground_console: Limiter<Uuid>,
    /// Exports per person, and per organization (each reads every version
    /// and plans every migration of what it exports).
    pub exports: Limiter<Uuid>,
    pub org_exports: Limiter<Uuid>,
    /// Exports streaming at once, per organization and in all.
    pub export_slots: Slots,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            push: Limiter::new(20, Duration::from_secs(3)),
            compile: Limiter::new(30, Duration::from_secs(2)),
            database_compiles: Limiter::new(60, Duration::from_secs(1)),
            imports: Limiter::new(6, Duration::from_secs(30)),
            import_previews: Limiter::new(6, Duration::from_secs(5)),
            version_comments: Limiter::new(30, Duration::from_millis(500)),
            playgrounds: Limiter::new(5, Duration::from_mins(2)),
            playground_actions: Limiter::new(10, Duration::from_secs(6)),
            playground_console: Limiter::new(20, Duration::from_secs(1)),
            exports: Limiter::new(5, Duration::from_secs(30)),
            org_exports: Limiter::new(10, Duration::from_secs(15)),
            export_slots: Slots::new(2, 6),
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
    fn slots_are_held_until_dropped_per_key_and_in_all() {
        let slots = Slots::new(1, 2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let first = slots.take(a).unwrap();
        assert!(slots.take(a).is_err(), "one per key");
        let second = slots.take(b).unwrap();
        assert!(slots.take(c).is_err(), "two in all");
        drop(first);
        assert!(slots.take(a).is_ok());
        drop(second);
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
