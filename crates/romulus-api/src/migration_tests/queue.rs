//! Whose migration test runs next. Each test builds a playground of its
//! own on the host, which runs a few at a time: tests wait here for a turn
//! ([`SLOTS`] at once, [`PER_ORG`] per organization), in the order they
//! came, an organization at its share passed over until one of its tests
//! ends. A test's place in line is live ([`Queue::position`]): the page
//! says "2nd in line".
//!
//! A place is held by a [`Place`] and a turn by a [`Turn`]: dropping
//! either (the test cancelled, its task aborted) gives it up, and the next
//! test in line is woken. At most [`WAITING_PER_ORG`] tests of an
//! organization wait, [`WAITING`] in all; more are refused (409
//! `test_queue_full`).

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
};

use tokio::sync::Notify;
use uuid::Uuid;

use crate::ApiError;

/// Tests running at once on the host.
pub const SLOTS: usize = 2;
/// Tests of one organization running at once.
pub const PER_ORG: usize = 1;
/// Tests of one organization waiting, at most.
pub const WAITING_PER_ORG: usize = 5;
/// Tests waiting, at most.
pub const WAITING: usize = 50;

#[derive(Default)]
struct State {
    running: Vec<(Uuid, Uuid)>,
    /// `(test, organization)`, in the order they came.
    waiting: VecDeque<(Uuid, Uuid)>,
}

impl State {
    fn org_running(&self, org: Uuid) -> usize {
        self.running.iter().filter(|(_, o)| *o == org).count()
    }

    /// The test whose turn it is now, if any.
    fn next(&self) -> Option<Uuid> {
        if self.running.len() >= SLOTS {
            return None;
        }
        self.waiting
            .iter()
            .find(|(_, org)| self.org_running(*org) < PER_ORG)
            .map(|(test, _)| *test)
    }
}

/// The queue (one per API). Cheap to clone.
#[derive(Clone, Default)]
pub struct Queue {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

impl Queue {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes a place in line for `test` of `org`, or refuses when the line
    /// is full.
    pub fn enqueue(&self, test: Uuid, org: Uuid) -> Result<Place, ApiError> {
        let mut state = self.state();
        let of_org = state.waiting.iter().filter(|(_, o)| *o == org).count();
        if of_org >= WAITING_PER_ORG || state.waiting.len() >= WAITING {
            return Err(ApiError::Conflict {
                code: "test_queue_full",
                message: if of_org >= WAITING_PER_ORG {
                    format!(
                        "Your organization has {WAITING_PER_ORG} migration tests waiting already. Try again when one has run."
                    )
                } else {
                    "Too many migration tests are waiting. Try again in a few minutes.".to_owned()
                },
            });
        }
        state.waiting.push_back((test, org));
        Ok(Place {
            queue: self.clone(),
            test,
            taken: false,
        })
    }

    /// `test`'s place in line, from 1, while it waits.
    pub fn position(&self, test: Uuid) -> Option<usize> {
        let state = self.state();
        state
            .waiting
            .iter()
            .position(|(t, _)| *t == test)
            .map(|i| i + 1)
    }

    /// Tests running now.
    pub fn running(&self) -> usize {
        self.state().running.len()
    }

    fn leave(&self, test: Uuid) {
        let mut state = self.state();
        state.waiting.retain(|(t, _)| *t != test);
        state.running.retain(|(t, _)| *t != test);
        drop(state);
        self.changed.notify_waiters();
    }
}

/// A place in line; given up when dropped.
pub struct Place {
    queue: Queue,
    test: Uuid,
    taken: bool,
}

impl Place {
    /// Waits for this test's turn.
    pub async fn turn(mut self) -> Turn {
        loop {
            let changed = self.queue.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.queue.state();
                if state.next() == Some(self.test)
                    && let Some(at) = state.waiting.iter().position(|(t, _)| *t == self.test)
                    && let Some(entry) = state.waiting.remove(at)
                {
                    state.running.push(entry);
                    drop(state);
                    // Places behind it moved up.
                    self.queue.changed.notify_waiters();
                    self.taken = true;
                    return Turn {
                        queue: self.queue.clone(),
                        test: self.test,
                    };
                }
            }
            changed.await;
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        if !self.taken {
            self.queue.leave(self.test);
        }
    }
}

/// A test's turn on the host; the next test's when dropped.
pub struct Turn {
    queue: Queue,
    test: Uuid,
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.queue.leave(self.test);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[tokio::test]
    async fn tests_take_turns_two_at_a_time_one_per_organization() {
        let queue = Queue::default();
        let (a, b) = (id(100), id(200));
        let a1 = queue.enqueue(id(1), a).unwrap();
        let a2 = queue.enqueue(id(2), a).unwrap();
        let b1 = queue.enqueue(id(3), b).unwrap();
        assert_eq!(queue.position(id(2)), Some(2));
        let first = a1.turn().await;
        // a's second waits for a's first; b's goes past it.
        let second = tokio::time::timeout(Duration::from_secs(1), b1.turn())
            .await
            .expect("b's test runs beside a's");
        assert_eq!(queue.running(), 2);
        assert_eq!(queue.position(id(2)), Some(1));
        let waiting = tokio::spawn(a2.turn());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "a has its one running");
        drop(first);
        let third = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("a's next runs once its first ends")
            .unwrap();
        drop((second, third));
        assert_eq!(queue.running(), 0);
    }

    #[tokio::test]
    async fn a_place_given_up_moves_the_others_up_and_the_line_has_an_end() {
        let queue = Queue::default();
        let org = id(100);
        let places: Vec<Place> = (0..WAITING_PER_ORG as u128)
            .map(|n| queue.enqueue(id(n), org).unwrap())
            .collect();
        assert!(matches!(
            queue.enqueue(id(99), org),
            Err(ApiError::Conflict {
                code: "test_queue_full",
                ..
            })
        ));
        assert_eq!(queue.position(id(3)), Some(4));
        drop(places);
        assert_eq!(queue.position(id(3)), None);
        assert!(queue.enqueue(id(99), org).is_ok());
    }
}
