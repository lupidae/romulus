//! What the time machine keeps between requests, and whose turn it is to
//! build.
//!
//! - **Steps** (romulus-diff between two consecutive versions), by the ids
//!   of the two versions: they never change. Past the budget, the database
//!   holding the most gives back first, its least recently used steps
//!   first: one database's long history cannot evict everyone else's.
//! - **Histories**, built (folded and serialised), by the database and its
//!   ordered list of version ids: the same versions are the same history,
//!   a version created, removed or restored is another list. The authors'
//!   names are not in them (they can change); the response joins them in.
//! - **Turns**: a build that misses the cache takes its organization's
//!   turn (one at a time per organization), then one of the process's two
//!   (builds are CPU-bound), so one organization's long history never
//!   holds up the others.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
};

use axum::body::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::history::Step;

/// The versions a step goes from (none, for a first version) and to.
pub type Pair = (Option<Uuid>, Uuid);

/// A database's history over one list of versions: the database, and a
/// digest of its version ids in order.
pub type HistoryKey = (Uuid, [u8; 32]);

/// The key of `database`'s history over `versions` (ids, oldest first).
pub fn history_key(database: Uuid, versions: &[Uuid]) -> HistoryKey {
    let mut hash = Sha256::new();
    for id in versions {
        hash.update(id.as_bytes());
    }
    (database, hash.finalize().into())
}

pub struct TimelineCache {
    steps: Mutex<Steps>,
    step_budget: usize,
    histories: Mutex<VecDeque<(HistoryKey, Bytes)>>,
    history_budget: usize,
    builds: Arc<Semaphore>,
    orgs: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Default)]
struct Steps {
    map: HashMap<Pair, StepEntry>,
    /// What each database's steps weigh.
    by_database: HashMap<Uuid, usize>,
    clock: u64,
    weight: usize,
}

struct StepEntry {
    step: Arc<Step>,
    used: u64,
    database: Uuid,
    weight: usize,
}

/// A build's turn: its organization's, then one of the process's.
pub struct BuildTurn {
    _org: OwnedMutexGuard<()>,
    _process: OwnedSemaphorePermit,
}

impl Default for TimelineCache {
    fn default() -> Self {
        Self::with_budgets(96 * 1024 * 1024, 32 * 1024 * 1024)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Plain data, valid at every step: a panic elsewhere leaves it usable.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TimelineCache {
    /// Keeps at most about `steps` bytes of steps and `histories` bytes of
    /// built histories (and always the last of each).
    pub fn with_budgets(steps: usize, histories: usize) -> Self {
        Self {
            steps: Mutex::new(Steps::default()),
            step_budget: steps,
            histories: Mutex::new(VecDeque::new()),
            history_budget: histories,
            builds: Arc::new(Semaphore::new(2)),
            orgs: Mutex::new(HashMap::new()),
        }
    }

    /// The cached steps among `pairs`, marked as just used.
    pub fn steps(&self, pairs: &[Pair]) -> HashMap<Pair, Arc<Step>> {
        let mut steps = lock(&self.steps);
        steps.clock += 1;
        let now = steps.clock;
        pairs
            .iter()
            .filter_map(|pair| {
                let entry = steps.map.get_mut(pair)?;
                entry.used = now;
                Some((*pair, entry.step.clone()))
            })
            .collect()
    }

    /// Keeps `database`'s step `pair`. Past the budget, steps go down to 90%
    /// of it: the least recently used of the database weighing the most,
    /// then of the next, never the step just kept.
    pub fn insert_step(&self, database: Uuid, pair: Pair, step: Arc<Step>) {
        let mut steps = lock(&self.steps);
        steps.clock += 1;
        let weight = step.weight();
        let entry = StepEntry {
            step,
            used: steps.clock,
            database,
            weight,
        };
        if let Some(old) = steps.map.insert(pair, entry) {
            steps.remove_weight(old.database, old.weight);
        }
        steps.weight += weight;
        *steps.by_database.entry(database).or_default() += weight;
        if steps.weight <= self.step_budget {
            return;
        }
        let target = self.step_budget / 10 * 9;
        while steps.weight > target {
            // The heaviest; between equals, the one keeping a step now.
            let Some(victim) = steps
                .by_database
                .iter()
                .max_by_key(|(d, w)| (**w, **d == database))
                .map(|(d, _)| *d)
            else {
                break;
            };
            let oldest = steps
                .map
                .iter()
                .filter(|(p, e)| e.database == victim && **p != pair)
                .min_by_key(|(_, e)| e.used)
                .map(|(p, _)| *p);
            let Some(oldest) = oldest else {
                // The heaviest database is down to the step just kept.
                break;
            };
            if let Some(old) = steps.map.remove(&oldest) {
                steps.remove_weight(old.database, old.weight);
            }
        }
    }

    /// A built history, marked as just used.
    pub fn history(&self, key: &HistoryKey) -> Option<Bytes> {
        let mut histories = lock(&self.histories);
        let at = histories.iter().position(|(k, _)| k == key)?;
        let entry = histories.remove(at)?;
        let body = entry.1.clone();
        histories.push_front(entry);
        Some(body)
    }

    /// Keeps a built history; past the budget, the least recently used go.
    pub fn insert_history(&self, key: HistoryKey, body: Bytes) {
        let mut histories = lock(&self.histories);
        histories.retain(|(k, _)| *k != key);
        histories.push_front((key, body));
        let mut total: usize = histories.iter().map(|(_, b)| b.len()).sum();
        while total > self.history_budget && histories.len() > 1 {
            if let Some((_, evicted)) = histories.pop_back() {
                total -= evicted.len();
            }
        }
    }

    /// Waits for `org`'s turn to build, then for one of the process's.
    pub async fn build_turn(&self, org: Uuid) -> BuildTurn {
        let mutex = {
            let mut orgs = lock(&self.orgs);
            // Organizations no build is waiting for are forgotten.
            if orgs.len() > 256 {
                orgs.retain(|_, m| Arc::strong_count(m) > 1);
            }
            orgs.entry(org).or_default().clone()
        };
        let org = mutex.lock_owned().await;
        let process = self
            .builds
            .clone()
            .acquire_owned()
            .await
            .expect("the build semaphore is never closed");
        BuildTurn {
            _org: org,
            _process: process,
        }
    }

    /// How many steps are kept.
    pub fn len(&self) -> usize {
        lock(&self.steps).map.len()
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.steps).map.is_empty()
    }

    /// How many built histories are kept.
    pub fn histories_len(&self) -> usize {
        lock(&self.histories).len()
    }
}

impl Steps {
    fn remove_weight(&mut self, database: Uuid, weight: usize) {
        self.weight -= weight;
        if let Some(w) = self.by_database.get_mut(&database) {
            *w -= weight;
            if *w == 0 {
                self.by_database.remove(&database);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versions::timeline::history::Event;

    fn step_of(weight: usize) -> Arc<Step> {
        let value: super::super::history::Json =
            Arc::from(serde_json::value::to_raw_value(&"x".repeat(weight)).unwrap());
        Arc::new(Step {
            extensions: vec![Event {
                old: None,
                new: None,
                value: Some(value),
                changes: Vec::new(),
            }],
            ..Step::default()
        })
    }

    fn pair(n: u128) -> Pair {
        (None, Uuid::from_u128(n))
    }

    const A: Uuid = Uuid::from_u128(0xA);
    const B: Uuid = Uuid::from_u128(0xB);

    #[test]
    fn the_cache_forgets_the_least_recently_used_steps_past_its_budget() {
        let one = step_of(1000).weight();
        let cache = TimelineCache::with_budgets(one * 3, 1024);
        cache.insert_step(A, pair(1), step_of(1000));
        cache.insert_step(A, pair(2), step_of(1000));
        cache.insert_step(A, pair(3), step_of(1000));
        // Reading the first makes the second the oldest.
        assert_eq!(cache.steps(&[pair(1)]).len(), 1);
        cache.insert_step(A, pair(4), step_of(1000));
        assert!(cache.steps(&[pair(2)]).is_empty());
        assert_eq!(cache.steps(&[pair(1), pair(4)]).len(), 2);
    }

    #[test]
    fn a_database_with_a_long_history_gives_back_its_own_steps_first() {
        let one = step_of(1000).weight();
        let cache = TimelineCache::with_budgets(one * 4, 1024);
        cache.insert_step(B, pair(100), step_of(1000));
        cache.insert_step(B, pair(101), step_of(1000));
        // A builds a long history: it outweighs B, so its own steps go.
        for n in 1..=10 {
            cache.insert_step(A, pair(n), step_of(1000));
        }
        assert_eq!(cache.steps(&[pair(100), pair(101)]).len(), 2);
        assert_eq!(cache.steps(&[pair(10)]).len(), 1);
        assert!(cache.steps(&[pair(1)]).is_empty());
    }

    #[test]
    fn the_cache_keeps_the_step_just_computed_even_past_its_budget() {
        let cache = TimelineCache::with_budgets(10, 10);
        cache.insert_step(A, pair(1), step_of(1000));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn histories_are_kept_by_their_versions_least_recently_used_first() {
        let cache = TimelineCache::with_budgets(1024, 10);
        let ids = [Uuid::from_u128(1), Uuid::from_u128(2)];
        let key = history_key(A, &ids);
        assert_ne!(key, history_key(A, &ids[..1]));
        assert_ne!(key, history_key(B, &ids));
        assert_ne!(key, history_key(A, &[ids[1], ids[0]]));
        cache.insert_history(key, Bytes::from_static(b"aaaa"));
        cache.insert_history(history_key(B, &ids), Bytes::from_static(b"bbbb"));
        // Reading A's makes B's the oldest.
        assert!(cache.history(&key).is_some());
        cache.insert_history(history_key(A, &ids[..1]), Bytes::from_static(b"cccc"));
        assert_eq!(cache.histories_len(), 2);
        assert!(cache.history(&history_key(B, &ids)).is_none());
        assert!(cache.history(&key).is_some());
    }

    #[tokio::test]
    async fn an_organization_builds_one_history_at_a_time_others_go_on() {
        let cache = Arc::new(TimelineCache::default());
        let first = cache.build_turn(A).await;
        // Another organization is not held up.
        let other = tokio::time::timeout(std::time::Duration::from_secs(1), cache.build_turn(B))
            .await
            .expect("another organization's turn");
        drop(other);
        // The same organization waits for the first build.
        let waiting = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.build_turn(A).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("its turn once the first is done")
            .unwrap();
    }
}
