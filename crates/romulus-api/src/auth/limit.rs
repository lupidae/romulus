//! How fast one client may start sign-ins. `GET /api/auth/login` needs no
//! session and writes an `oidc_flows` row, so without a limit anyone could
//! fill the table. A token bucket per client IP: a burst of [`BURST`], then
//! one every [`REFILL`]. In memory, like everything per-instance in the MVP
//! (one API instance).
//!
//! The client IP is the TCP peer. Behind a reverse proxy every request has the
//! proxy's address and the limit becomes global, which is still a cap; trusting
//! `X-Forwarded-For` needs a configured list of trusted proxies (a follow-up).

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Sign-ins one client may start at once.
pub const BURST: u32 = 20;
/// Then one more per this long.
pub const REFILL: Duration = Duration::from_secs(3);
/// Expired flows are swept at most this often.
const SWEEP_EVERY: Duration = Duration::from_mins(1);
/// Past this many tracked clients, the ones back to a full bucket are forgotten.
const MAX_TRACKED: usize = 10_000;

struct Bucket {
    tokens: f64,
    at: Instant,
}

pub struct SignInLimiter {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    last_sweep: Mutex<Option<Instant>>,
}

impl Default for SignInLimiter {
    fn default() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(None),
        }
    }
}

impl SignInLimiter {
    /// Whether `client` may start one more sign-in now (and counts it). An
    /// unknown peer (no connection info, as in tests) shares one bucket.
    pub fn allow(&self, client: Option<IpAddr>) -> bool {
        self.allow_at(client, Instant::now())
    }

    fn allow_at(&self, client: Option<IpAddr>, now: Instant) -> bool {
        let key = client.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refill = |bucket: &Bucket| {
            let earned =
                now.saturating_duration_since(bucket.at).as_secs_f64() / REFILL.as_secs_f64();
            (bucket.tokens + earned).min(f64::from(BURST))
        };
        if buckets.len() >= MAX_TRACKED {
            buckets.retain(|_, bucket| refill(bucket) < f64::from(BURST));
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: f64::from(BURST),
            at: now,
        });
        bucket.tokens = refill(bucket);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// True at most once per [`SWEEP_EVERY`]: when this sign-in should also
    /// delete the flows nobody came back for.
    pub fn sweep_due(&self) -> bool {
        let mut last = self
            .last_sweep
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < SWEEP_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_gets_a_burst_then_one_per_refill() {
        let limiter = SignInLimiter::default();
        let ip = Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)));
        let start = Instant::now();
        for _ in 0..BURST {
            assert!(limiter.allow_at(ip, start));
        }
        assert!(!limiter.allow_at(ip, start));
        assert!(!limiter.allow_at(ip, start + REFILL / 2));
        assert!(limiter.allow_at(ip, start + REFILL));
        assert!(!limiter.allow_at(ip, start + REFILL));
    }

    #[test]
    fn clients_are_counted_apart() {
        let limiter = SignInLimiter::default();
        let (a, b) = (
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2))),
        );
        let now = Instant::now();
        for _ in 0..BURST {
            assert!(limiter.allow_at(a, now));
        }
        assert!(!limiter.allow_at(a, now));
        assert!(limiter.allow_at(b, now));
    }

    #[test]
    fn the_sweep_runs_once_a_minute() {
        let limiter = SignInLimiter::default();
        assert!(limiter.sweep_due());
        assert!(!limiter.sweep_due());
    }
}
