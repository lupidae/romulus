//! What the service may ask the runner, over HTTP on the runner's socket:
//!
//! | request | what |
//! |---|---|
//! | `POST /v1/playgrounds/{id}` [`Create`] | its directories, its quota, its container (stopped) |
//! | `POST /v1/playgrounds/{id}/start` → [`Started`] | start it (init runs on the first start) |
//! | `POST /v1/playgrounds/{id}/stop` | a clean stop (Postgres's fast shutdown) |
//! | `POST /v1/playgrounds/{id}/snapshot` | stopped: copy its data as the generated copy |
//! | `POST /v1/playgrounds/{id}/restore` | stop, then put the generated copy back |
//! | `DELETE /v1/playgrounds/{id}` | the container, its directories, its quota |
//! | `GET /v1/playgrounds/{id}` → [`State`] | exists, running, killed for memory |
//! | `GET /v1/playgrounds` → [`Listing`] | every playground of this runner's scope |
//! | `GET /v1/health` → [`Health`] | Docker, the runtime, the image, quotas |
//!
//! Errors are JSON [`ErrorBody`]s with a stable `code`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The extensions a playground may have: romulus's allowlist
/// (`romulus_sandbox::catalog::ALLOWED_EXTENSIONS`), the control files the
/// playground image keeps (deploy/playground/extensions.txt; a test keeps
/// the three in agreement). Installed at init, by the superuser, before
/// anyone connects.
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "btree_gin",
    "btree_gist",
    "citext",
    "cube",
    "earthdistance",
    "fuzzystrmatch",
    "hstore",
    "intarray",
    "isn",
    "ltree",
    "pg_trgm",
    "pgcrypto",
    "postgis",
    "postgis_topology",
    "seg",
    "tsm_system_rows",
    "tsm_system_time",
    "unaccent",
    "uuid-ossp",
    "vector",
];

/// A playground's resources. Memory is the same for both: Postgres's
/// shared buffers and per-process caps are; the disk quota (data and the
/// generated copy, which reflinks share on XFS but the quota counts twice)
/// grows with the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// ~50 k rows.
    Small,
    /// ~1 M rows, ~300 MB.
    Medium,
}

impl Size {
    /// The container's memory limit (cgroup), bytes.
    pub fn memory_bytes(self) -> i64 {
        512 * 1024 * 1024
    }

    /// The XFS project quota on its directory, bytes.
    pub fn disk_bytes(self) -> u64 {
        match self {
            Self::Small => 1024 * 1024 * 1024,
            Self::Medium => 2 * 1024 * 1024 * 1024,
        }
    }
}

/// The XFS project quota on a playground's socket directory (`run/<id>`):
/// a socket and a lock file need a few bytes.
pub const RUN_DIR_BYTES: u64 = 16 * 1024 * 1024;

/// An extension to install at init.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extension {
    /// One of [`ALLOWED_EXTENSIONS`].
    pub name: String,
    /// Where: a plain lowercase identifier ([`is_plain_identifier`]).
    pub schema: String,
}

impl Extension {
    /// Its name as the allowlist spells it, if it is on it: what reaches the
    /// container is the constant, never the request's string.
    pub fn allowed_name(&self) -> Option<&'static str> {
        ALLOWED_EXTENSIONS
            .iter()
            .copied()
            .find(|name| *name == self.name)
    }
}

/// `[a-z_][a-z0-9_]*`, at most 63 bytes, and not a `pg_` name: a schema
/// the init may create without quoting surprises.
pub fn is_plain_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
    first
        && name.len() <= 63
        && !name.starts_with("pg_")
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Create {
    pub size: Size,
    #[serde(default)]
    pub extensions: Vec<Extension>,
}

/// Where the service reaches a started playground's Postgres.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    /// Its socket directory, under the service's own mount of the
    /// playground volume's `run/` (`<sockets>/<id>/.s.PGSQL.5432`).
    Socket,
    /// Local development only (no gVisor, so no host Unix sockets through
    /// Docker Desktop or OrbStack): a port published on loopback.
    Tcp { host: String, port: u16 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Started {
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub exists: bool,
    pub running: bool,
    /// The last stop was the memory limit.
    #[serde(default)]
    pub oom_killed: bool,
    #[serde(default)]
    pub endpoint: Option<Endpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listed {
    pub id: Uuid,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Listing {
    pub playgrounds: Vec<Listed>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a health report: one flag per check"
)]
pub struct Health {
    pub docker: bool,
    /// The configured runtime, and whether Docker has it.
    pub runtime: String,
    pub runtime_available: bool,
    pub image: bool,
    /// The playground volume's filesystem (`xfs` for quotas), once known.
    pub filesystem: Option<String>,
    pub quotas: bool,
}

/// The playground volume's space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl Space {
    /// `df`'s "<available> <size>" in bytes.
    pub fn parse(text: &str) -> Option<Self> {
        let mut numbers = text.split_whitespace().map(str::parse::<u64>);
        Some(Self {
            free_bytes: numbers.next()?.ok()?,
            total_bytes: numbers.next()?.ok()?,
        })
    }
}

/// Every error the runner answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `not_found`, `exists`, `running`, `no_snapshot`, `bad_request`,
    /// `docker` (Docker failed: see the runner's log).
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_is_the_images_and_the_sandboxs() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy");
        for file in [
            "playground/extensions.txt",
            "sandbox/trusted-extensions.txt",
        ] {
            let text = std::fs::read_to_string(format!("{root}/{file}")).expect("read the list");
            let listed: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
            assert_eq!(listed, ALLOWED_EXTENSIONS, "{file}");
        }
    }

    #[test]
    fn only_plain_identifiers_name_an_extensions_schema() {
        for ok in ["public", "extensions", "_x", "gis2"] {
            assert!(is_plain_identifier(ok), "{ok}");
        }
        for bad in [
            "",
            "Public",
            "2x",
            "a-b",
            "a\"b",
            "pg_catalog",
            "a b",
            &"x".repeat(64),
        ] {
            assert!(!is_plain_identifier(bad), "{bad}");
        }
    }

    #[test]
    fn an_extension_off_the_allowlist_has_no_name() {
        let ext = |name: &str| Extension {
            name: name.to_owned(),
            schema: "public".to_owned(),
        };
        assert_eq!(ext("vector").allowed_name(), Some("vector"));
        assert_eq!(ext("dblink").allowed_name(), None);
        assert_eq!(ext("vector; DROP").allowed_name(), None);
    }
}
