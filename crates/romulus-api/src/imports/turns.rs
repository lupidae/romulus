//! Whose import replays next. The sandbox replays one import at a time
//! (`romulus_sandbox::Compiler::replay`), for up to minutes each: imports
//! wait here for their turn, organizations taking turns (round-robin, the
//! one that just ran last), each one's imports in the order they came. An
//! organization importing over and over holds the others back by one
//! import at most.
//!
//! At most [`PER_ORG`] imports of an organization wait, one once half of
//! the [`WAITING`] places are taken, and [`WAITING`] in all; more are
//! refused (503 `unavailable`), and so is a wait past [`MAX_WAIT`].
//!
//! An import keeps its place when its client leaves: the job runs to the
//! end on its own (`run::start` spawns it), like one already replaying, so
//! leaving the page never loses an import. Only a place given up (the wait
//! timed out) stops counting; a turn handed to it passes on.
//!
//! [`Turns::running`] also keeps one import per database, waiting or
//! replaying, taken by `run::start` before any answer goes out: another
//! one (a CI retry after its proxy gave up waiting, say) gets a 409 saying
//! it still runs. It names the import's rows in the staging table, which
//! the sweeper leaves alone. [`Turns::model_budget`] is how many bytes of
//! models one import may store.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::oneshot;
use uuid::Uuid;

use crate::ApiError;

/// Imports of one organization waiting, at most (one once the queue is
/// half full, so that a few organizations cannot take every place).
const PER_ORG: usize = 2;
/// Imports waiting, at most.
const WAITING: usize = 16;
/// How long an import waits for its turn, at most.
const MAX_WAIT: Duration = Duration::from_mins(10);

/// The models one import may store, all versions together, by default:
/// each version keeps its whole model (about 2 MB for 1 000 tables).
pub const MODEL_BUDGET: usize = 256 << 20;

/// The queue (one per API). Cheap to clone.
#[derive(Clone, Default)]
pub struct Turns(Arc<Inner>);

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    /// Imports in flight, by database.
    running: Mutex<HashMap<Uuid, InFlight>>,
    /// [`MODEL_BUDGET`] unless set (0).
    model_budget: AtomicUsize,
}

/// An import in flight: when it started, its fingerprint
/// (`plan::fingerprint`), its id in the staging table.
type InFlight = (Instant, [u8; 32], Uuid);

/// An import of a database in flight; it ends when this is dropped.
pub struct Running {
    inner: Arc<Inner>,
    database: Uuid,
    staging: Uuid,
}

impl Running {
    /// The import's id in the staging table (`staging`).
    pub fn staging(&self) -> Uuid {
        self.staging
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        lock(&self.inner.running).remove(&self.database);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct State {
    /// A turn is out.
    running: bool,
    /// The organization whose import has (or last had) the turn.
    last: Option<Uuid>,
    /// Organizations with imports waiting, the next one first.
    ring: VecDeque<Uuid>,
    waiting: HashMap<Uuid, VecDeque<oneshot::Sender<Turn>>>,
}

/// The right to replay: the next import gets it when this is dropped.
pub struct Turn(Option<Arc<Inner>>);

/// Where an import stands.
pub enum Place {
    /// Nobody was waiting: its turn.
    Now(Turn),
    /// Behind other imports.
    Waiting(Wait),
}

/// A place in the queue: [`Wait::turn`] waits for it.
pub struct Wait(oneshot::Receiver<Turn>);

impl Turns {
    /// The turn now, or a place in the queue; refused when too many wait.
    pub fn take(&self, org: Uuid) -> Result<Place, ApiError> {
        let mut guard = self.0.lock();
        let state = &mut *guard;
        state.forget_gone();
        if !state.running {
            state.running = true;
            state.last = Some(org);
            return Ok(Place::Now(Turn(Some(Arc::clone(&self.0)))));
        }
        let mine = state.waiting.get(&org).map_or(0, VecDeque::len);
        let all: usize = state.waiting.values().map(VecDeque::len).sum();
        let share = if all >= WAITING / 2 { 1 } else { PER_ORG };
        if all >= WAITING {
            return Err(ApiError::Unavailable(
                "Too many imports are waiting to run. Try again in a few minutes.".into(),
            ));
        }
        if mine >= share {
            return Err(ApiError::Unavailable(
                "Another import of this organization is waiting to run. Try again when it is done."
                    .into(),
            ));
        }
        let (send, receive) = oneshot::channel();
        let queue = state.waiting.entry(org).or_default();
        if queue.is_empty() {
            state.ring.push_back(org);
        }
        queue.push_back(send);
        Ok(Place::Waiting(Wait(receive)))
    }
}

impl Turns {
    /// Marks an import of `database` (`fingerprint`: `plan::fingerprint`)
    /// as in flight until the guard is dropped; 409 `concurrent_import` if
    /// one already is, saying whether it is this very import (a retry).
    pub fn running(&self, database: Uuid, fingerprint: [u8; 32]) -> Result<Running, ApiError> {
        let mut running = lock(&self.0.running);
        if let Some((started, theirs, _)) = running.get(&database) {
            let ago = ago(started.elapsed());
            return Err(ApiError::Conflict {
                code: "concurrent_import",
                message: if *theirs == fingerprint {
                    format!(
                        "These migrations are still being imported (started {ago} ago): the import finishes on the server. Send the same request again in a minute: once the versions are stored, it answers that nothing is left to import."
                    )
                } else {
                    format!(
                        "Another import of this database is running (started {ago} ago). Send this one again when it is done."
                    )
                },
            });
        }
        let staging = super::staging::new_id();
        running.insert(database, (Instant::now(), fingerprint, staging));
        Ok(Running {
            inner: Arc::clone(&self.0),
            database,
            staging,
        })
    }

    /// The staging ids of the imports in flight: never swept.
    pub fn in_flight(&self) -> Vec<Uuid> {
        lock(&self.0.running)
            .values()
            .map(|(_, _, staging)| *staging)
            .collect()
    }

    /// The databases with an import in flight in this process.
    pub fn databases_in_flight(&self) -> Vec<Uuid> {
        lock(&self.0.running).keys().copied().collect()
    }

    /// How many bytes of models one import may store ([`MODEL_BUDGET`]).
    pub fn model_budget(&self) -> usize {
        match self.0.model_budget.load(Ordering::Relaxed) {
            0 => MODEL_BUDGET,
            set => set,
        }
    }

    /// Another budget (tests).
    pub fn set_model_budget(&self, bytes: usize) {
        self.0.model_budget.store(bytes, Ordering::Relaxed);
    }

    /// Imports waiting for their turn.
    pub fn waiting(&self) -> usize {
        let mut state = self.0.lock();
        state.forget_gone();
        state.waiting.values().map(VecDeque::len).sum()
    }
}

impl Wait {
    /// Waits for the turn, at most [`MAX_WAIT`].
    pub async fn turn(self) -> Result<Turn, ApiError> {
        let busy = || {
            ApiError::Unavailable(
                "The SQL compiler is busy with other imports. Try again in a few minutes.".into(),
            )
        };
        tokio::time::timeout(MAX_WAIT, self.0)
            .await
            .map_err(|_| busy())?
            .map_err(|_| busy())
    }
}

/// `42 s`, `3 min`.
fn ago(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 120 {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds / 60)
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Hands the turn to the next waiter still there, or frees it.
    fn pass(self: &Arc<Self>) {
        loop {
            let next = {
                let mut state = self.lock();
                let Some(next) = state.next() else {
                    state.running = false;
                    return;
                };
                next
            };
            // Outside the lock: a turn that comes back is disarmed (its
            // waiter left), and the loop tries the next one.
            match next.send(Turn(Some(Arc::clone(self)))) {
                Ok(()) => return,
                Err(mut back) => back.0 = None,
            }
        }
    }
}

impl State {
    /// The next waiter: the next organization's oldest import, the one
    /// that just ran last when others wait.
    fn next(&mut self) -> Option<oneshot::Sender<Turn>> {
        if self.ring.len() > 1 && self.ring.front() == self.last.as_ref() {
            self.ring.rotate_left(1);
        }
        while let Some(org) = self.ring.pop_front() {
            let Some(queue) = self.waiting.get_mut(&org) else {
                continue;
            };
            let next = std::iter::from_fn(|| queue.pop_front()).find(|s| !s.is_closed());
            if queue.is_empty() {
                self.waiting.remove(&org);
            } else {
                self.ring.push_back(org);
            }
            if next.is_some() {
                self.last = Some(org);
                return next;
            }
        }
        None
    }

    /// Waiters that gave up no longer count.
    fn forget_gone(&mut self) {
        for queue in self.waiting.values_mut() {
            queue.retain(|s| !s.is_closed());
        }
        self.waiting.retain(|_, queue| !queue.is_empty());
        let waiting = &self.waiting;
        self.ring.retain(|org| waiting.contains_key(org));
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            inner.pass();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(turns: &Turns, org: Uuid) -> Turn {
        match turns.take(org) {
            Ok(Place::Now(turn)) => turn,
            _ => panic!("the turn was free"),
        }
    }

    fn wait(turns: &Turns, org: Uuid) -> Wait {
        match turns.take(org) {
            Ok(Place::Waiting(wait)) => wait,
            _ => panic!("the turn was out"),
        }
    }

    /// Whose turn it is among `waits`, by index (it is taken).
    fn whose(waits: &mut [Wait]) -> Vec<(usize, Turn)> {
        waits
            .iter_mut()
            .enumerate()
            .filter_map(|(k, w)| w.0.try_recv().ok().map(|turn| (k, turn)))
            .collect()
    }

    #[test]
    fn one_import_per_database_is_in_flight_and_a_retry_is_told_so() {
        let turns = Turns::default();
        let (database, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let running = turns.running(database, [1; 32]).unwrap();
        let Err(ApiError::Conflict { code, message }) = turns.running(database, [1; 32]) else {
            panic!("one at a time");
        };
        assert_eq!(code, "concurrent_import");
        assert!(message.contains("still being imported"), "{message}");
        let Err(ApiError::Conflict { message, .. }) = turns.running(database, [2; 32]) else {
            panic!("one at a time");
        };
        assert!(message.contains("Another import"), "{message}");
        // Other databases are not concerned; done, the database is free.
        let _other = turns.running(other, [1; 32]).unwrap();
        drop(running);
        let _again = turns.running(database, [2; 32]).unwrap();
    }

    #[test]
    fn organizations_take_turns_whoever_queued_first() {
        let turns = Turns::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let running = now(&turns, a);
        let mut waits = vec![wait(&turns, a), wait(&turns, a), wait(&turns, b)];
        drop(running);
        let mut order = Vec::new();
        for _ in 0..3 {
            let mut turn = whose(&mut waits);
            assert_eq!(turn.len(), 1, "one turn at a time");
            let (k, turn) = turn.pop().unwrap();
            order.push(k);
            drop(turn);
        }
        // b's import goes before a's second: a just ran.
        assert_eq!(order, [2, 0, 1]);
        // Free again.
        drop(now(&turns, b));
    }

    #[test]
    fn an_organization_cannot_queue_past_its_share() {
        let turns = Turns::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let _running = now(&turns, a);
        let _waits = [wait(&turns, a), wait(&turns, a)];
        assert!(matches!(turns.take(a), Err(ApiError::Unavailable(_))));
        assert!(matches!(turns.take(b), Ok(Place::Waiting(_))));
    }

    #[test]
    fn once_half_full_an_organization_waits_with_one_import_at_most() {
        let turns = Turns::default();
        let _running = now(&turns, Uuid::from_u128(100));
        // Four organizations take two places each: half the queue.
        let _waits: Vec<Wait> = (1..=4)
            .flat_map(|org| [Uuid::from_u128(org); 2])
            .map(|org| wait(&turns, org))
            .collect();
        assert_eq!(turns.waiting(), WAITING / 2);
        let late = Uuid::from_u128(5);
        let _one = wait(&turns, late);
        assert!(matches!(turns.take(late), Err(ApiError::Unavailable(_))));
        // The other places go one per organization.
        let _others: Vec<Wait> = (6..=12)
            .map(|org| wait(&turns, Uuid::from_u128(org)))
            .collect();
        assert_eq!(turns.waiting(), WAITING);
        assert!(matches!(
            turns.take(Uuid::from_u128(13)),
            Err(ApiError::Unavailable(_))
        ));
    }

    /// A place given up (its wait timed out) no longer counts.
    #[test]
    fn a_waiter_that_left_loses_its_place() {
        let turns = Turns::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let running = now(&turns, a);
        let gone = wait(&turns, b);
        let mut waits = vec![wait(&turns, a)];
        drop(gone);
        // Its place no longer counts against b's share.
        waits.extend([wait(&turns, b), wait(&turns, b)]);
        drop(running);
        let mut order = Vec::new();
        while let Some((k, turn)) = whose(&mut waits).pop() {
            order.push(k);
            drop(turn);
        }
        assert_eq!(order, [1, 0, 2]);
    }

    #[tokio::test]
    async fn a_turn_handed_to_a_waiter_that_never_took_it_passes_on() {
        let turns = Turns::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let running = now(&turns, a);
        let first = wait(&turns, b);
        let second = wait(&turns, a);
        drop(running);
        // `first` holds the turn, unread: dropping it hands it on.
        drop(first);
        let turn = second.turn().await.expect("the turn passed on");
        drop(turn);
        drop(now(&turns, b));
    }
}
