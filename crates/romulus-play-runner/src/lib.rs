//! The playground runner (docs/playground.md, section 4): the only process
//! that holds the Docker socket, which is root on its host, so it stays
//! tiny and fixed.
//!
//! It does a handful of operations on one playground's container, named
//! by the playground's id and nothing else: [`protocol`] lists them.
//! **Nothing from a request becomes a Docker argument**: the image, the
//! runtime, the flags, the mounts and the limits come from this crate's code
//! and the runner's own configuration ([`server::Config`]); a request picks
//! a playground (a UUID, parsed), a [`protocol::Size`] (an enum) and
//! extensions (names matched against [`protocol::ALLOWED_EXTENSIONS`],
//! schemas against a strict identifier pattern).
//!
//! It listens on a Unix socket that only the playground service mounts
//! (`ROMULUS_PLAY_RUNNER_SOCKET`): the runner accepts only the service.
//! [`client::Runner`] is that service's side.
//!
//! Every playground container runs with the configured runtime (`runsc-uds`,
//! gVisor with host Unix sockets, in the preview; `runc` in local
//! development, where gVisor is absent), `--network none`, a read-only root
//! filesystem, no capabilities, `no-new-privileges`, pids, memory and CPU
//! limits, its data on the playground volume (an XFS project quota per
//! playground when the volume is XFS) and its Postgres socket in a directory
//! of that volume. Files are touched only by `play-files` (deploy/playground)
//! in short-lived helper containers; quotas by `xfs_quota` in a short-lived
//! privileged one.

pub mod client;
pub mod password;
pub mod protocol;
mod unix;

#[cfg(feature = "server")]
mod docker;
#[cfg(feature = "server")]
pub mod server;

pub use client::{Runner, RunnerError};
