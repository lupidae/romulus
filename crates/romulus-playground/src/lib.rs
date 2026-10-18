//! The playground service (docs/playground.md, section 4): builds
//! playgrounds, runs their console sessions and migration runs, and stops
//! the idle ones. Unprivileged: it reaches each playground's Postgres over
//! its socket, as the playground's own roles, and asks the runner
//! (`romulus-play-runner`, over the runner's socket) for everything that
//! touches a container or a file.
//!
//! It trusts only calls the API signed ([`signing`]): the API owns who may
//! do what (the role matrix, the caps on playgrounds existing, the tables),
//! the service enforces what only it can see (statements running at once,
//! deadlines, playgrounds running per organization and per host, idle stop).
//!
//! Without the `server` feature this crate is the [`protocol`] and the
//! [`signing`] the API shares.

pub mod protocol;
pub mod signing;

#[cfg(feature = "server")]
pub mod server;
