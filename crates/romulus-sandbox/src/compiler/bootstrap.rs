//! The privileged start: one role and one database per pool slot, and the
//! catalog of what the sandbox provides.
//!
//! Runs as the superuser of the sandbox cluster (`SandboxConfig::admin_url`).
//! Compiles never do: each slot connects as its own role, which owns nothing
//! but its database's `public` schema and the objects a compile creates.
//!
//! - **Roles** `<prefix>_<n>`: `LOGIN`, nothing else (no superuser, no
//!   `CREATEDB`/`CREATEROLE`, no `BYPASSRLS`, `NOINHERIT`), with role-level
//!   defaults the text cannot change (`SET` is refused but for
//!   `search_path`): timeouts, `temp_file_limit`, UTC, and logging turned
//!   down so the sandbox's logs never hold a customer's schema. A password
//!   derived from the admin URL, so restarts agree on it without storing it.
//!   One role per slot: Postgres lets a role signal its own backends, so a
//!   shared role would let one compile cancel another.
//! - **Databases** `<prefix>_<n>`: created from `template0`, owned by the
//!   admin, `CONNECT`/`CREATE`/`TEMPORARY` granted to their role only. Kept
//!   across restarts (nothing is ever committed in them) unless their pool
//!   version comment is stale.
//! - **Catalog**: built-in function names, and what each allowlisted
//!   extension creates, measured by installing them all into one pool
//!   database in a transaction that is rolled back.
//!
//! A session advisory lock on the prefix, held by the admin connection for
//! the compiler's lifetime, stops two processes from sharing a pool.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Mutex,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Connection, PgConnection, Row, postgres::PgConnectOptions, raw_sql};

use super::{SandboxConfig, SandboxError};
use crate::catalog::{ALLOWED_EXTENSIONS, Catalog, Extension, ProvidedKind, ProvidedObject};

/// Bump to recreate every pool database on the next start (changes to how
/// they are set up).
const POOL_VERSION: &str = "romulus sandbox pool v1";

/// What a slot needs to connect.
#[derive(Debug, Clone)]
pub(crate) struct SlotSpec {
    pub database: String,
    pub role: String,
    pub password: String,
}

pub(crate) struct Bootstrapped {
    pub admin: PgConnection,
    pub slots: Vec<SlotSpec>,
    pub catalog: Catalog,
    pub server_version: String,
}

pub(crate) async fn bootstrap(
    config: &SandboxConfig,
    options: &PgConnectOptions,
) -> Result<Bootstrapped, SandboxError> {
    validate(config)?;
    let mut admin = PgConnection::connect_with(options)
        .await
        .map_err(|e| SandboxError::Unavailable(e.to_string()))?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext($1))")
        .bind(format!("romulus_sandbox:{}", config.prefix))
        .fetch_one(&mut admin)
        .await
        .map_err(bootstrap_error)?;
    if !locked {
        return Err(SandboxError::PrefixInUse(config.prefix.clone()));
    }

    let server_version: String = sqlx::query_scalar("SELECT current_setting('server_version')")
        .fetch_one(&mut admin)
        .await
        .map_err(bootstrap_error)?;
    let server_num: String = sqlx::query_scalar("SELECT current_setting('server_version_num')")
        .fetch_one(&mut admin)
        .await
        .map_err(bootstrap_error)?;
    let server_major = server_num.parse::<u32>().unwrap_or(0) / 10_000;
    let parser_major = parser_major();
    if server_major != parser_major {
        return Err(SandboxError::VersionMismatch {
            server: server_version,
            parser: parser_major,
        });
    }

    let mut slots = Vec::with_capacity(config.pool_size);
    for index in 0..config.pool_size {
        let name = format!("{}_{index}", config.prefix);
        let password = password(&config.admin_url, &name);
        ensure_role(&mut admin, config, &name, &password).await?;
        ensure_database(&mut admin, options, &name).await?;
        slots.push(SlotSpec {
            database: name.clone(),
            role: name,
            password,
        });
    }

    let first = slots
        .first()
        .ok_or_else(|| SandboxError::Bootstrap("the pool needs at least one database".into()))?;
    // The catalog depends only on the sandbox image: measured once per
    // process and server (installing PostGIS takes a while, and memory).
    let cache_key = format!("{}\0{server_version}", config.admin_url);
    let cached = lock_catalogs().get(&cache_key).cloned();
    let catalog = if let Some(catalog) = cached {
        catalog
    } else {
        let catalog = probe_catalog(options, &first.database).await?;
        lock_catalogs().insert(cache_key, catalog.clone());
        catalog
    };

    Ok(Bootstrapped {
        admin,
        slots,
        catalog,
        server_version,
    })
}

static CATALOGS: Mutex<BTreeMap<String, Catalog>> = Mutex::new(BTreeMap::new());

fn lock_catalogs() -> std::sync::MutexGuard<'static, BTreeMap<String, Catalog>> {
    CATALOGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn validate(config: &SandboxConfig) -> Result<(), SandboxError> {
    let prefix_ok = config.prefix.len() <= 40
        && config
            .prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && config
            .prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !prefix_ok {
        return Err(SandboxError::Bootstrap(format!(
            "prefix {:?} must be a lowercase identifier of at most 40 characters",
            config.prefix
        )));
    }
    if !(1..=64).contains(&config.pool_size) {
        return Err(SandboxError::Bootstrap(format!(
            "pool size {} must be between 1 and 64",
            config.pool_size
        )));
    }
    Ok(())
}

/// The major version of the Postgres grammar `pg_query` parses (17 for
/// `libpg_query` 17.x): the sandbox must run the same.
pub(crate) fn parser_major() -> u32 {
    pg_query::parse("SELECT 1")
        .ok()
        .and_then(|r| u32::try_from(r.protobuf.version).ok())
        .map_or(0, |v| v / 10_000)
}

/// A slot role's password: derived from the admin URL (which holds the
/// superuser's password), so every start computes the same one.
fn password(admin_url: &str, role: &str) -> String {
    let digest = Sha256::digest(format!("romulus-sandbox\0{admin_url}\0{role}"));
    hex(&digest)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[allow(clippy::needless_pass_by_value)] // used as `map_err(bootstrap_error)`
fn bootstrap_error(err: sqlx::Error) -> SandboxError {
    SandboxError::Bootstrap(err.to_string())
}

async fn exec(conn: &mut PgConnection, sql: String) -> Result<(), SandboxError> {
    raw_sql(AssertSqlSafe(sql))
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(bootstrap_error)
}

/// Create (or reset) a slot's role. `name` is a validated identifier and
/// `password` hex, so both are safe to interpolate.
async fn ensure_role(
    admin: &mut PgConnection,
    config: &SandboxConfig,
    name: &str,
    password: &str,
) -> Result<(), SandboxError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(name)
            .fetch_one(&mut *admin)
            .await
            .map_err(bootstrap_error)?;
    if !exists {
        exec(admin, format!("CREATE ROLE {name}")).await?;
    }
    // A backstop for what runs outside a compile's transaction (session
    // setup, rollbacks): each compile sets its own, stricter, limits.
    let statement_ms = config
        .compile_timeout
        .max(config.statement_timeout)
        .as_millis();
    let lock_ms = config.lock_timeout.as_millis();
    exec(
        admin,
        format!(
            "ALTER ROLE {name} WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT \
             NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 4 PASSWORD '{password}';
             ALTER ROLE {name} SET statement_timeout = '{statement_ms}ms';
             ALTER ROLE {name} SET lock_timeout = '{lock_ms}ms';
             ALTER ROLE {name} SET idle_in_transaction_session_timeout = '60s';
             ALTER ROLE {name} SET temp_file_limit = '64MB';
             ALTER ROLE {name} SET work_mem = '4MB';
             ALTER ROLE {name} SET maintenance_work_mem = '16MB';
             ALTER ROLE {name} SET timezone = 'UTC';
             ALTER ROLE {name} SET datestyle = 'ISO, MDY';
             ALTER ROLE {name} SET intervalstyle = 'postgres';
             ALTER ROLE {name} SET search_path = '\"$user\", public';
             ALTER ROLE {name} SET client_min_messages = 'warning';
             ALTER ROLE {name} SET log_min_messages = 'fatal';
             ALTER ROLE {name} SET log_min_error_statement = 'panic';"
        ),
    )
    .await
}

/// Create a slot's database unless an up-to-date one exists, and grant it to
/// the slot's role only.
async fn ensure_database(
    admin: &mut PgConnection,
    options: &PgConnectOptions,
    name: &str,
) -> Result<(), SandboxError> {
    let marker: Option<Option<String>> = sqlx::query_scalar(
        "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = $1",
    )
    .bind(name)
    .fetch_optional(&mut *admin)
    .await
    .map_err(bootstrap_error)?;
    if marker.flatten().as_deref() != Some(POOL_VERSION) {
        exec(
            admin,
            format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        )
        .await?;
        exec(
            admin,
            format!("CREATE DATABASE {name} TEMPLATE template0 ENCODING 'UTF8'"),
        )
        .await?;
        exec(
            admin,
            format!("COMMENT ON DATABASE {name} IS '{POOL_VERSION}'"),
        )
        .await?;
    }
    exec(
        admin,
        format!(
            "REVOKE ALL ON DATABASE {name} FROM PUBLIC;
             GRANT CONNECT, CREATE, TEMPORARY ON DATABASE {name} TO {name};"
        ),
    )
    .await?;
    let mut db = PgConnection::connect_with(&options.clone().database(name))
        .await
        .map_err(bootstrap_error)?;
    exec(
        &mut db,
        format!(
            "ALTER SCHEMA public OWNER TO {name};
             REVOKE ALL ON SCHEMA public FROM PUBLIC;"
        ),
    )
    .await?;
    db.close().await.map_err(bootstrap_error)
}

#[derive(Deserialize)]
struct ProbedExtension {
    name: String,
    relocatable: bool,
    schema: Option<String>,
    requires: Option<Vec<String>>,
    installed_in: Option<String>,
    #[serde(default)]
    functions: Vec<String>,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    objects: Vec<ProbedObject>,
}

#[derive(Deserialize)]
struct ProbedObject {
    schema: String,
    name: String,
    kind: String,
}

/// Install every available allowlisted extension in a rolled-back
/// transaction and record what each one creates.
async fn probe_catalog(
    options: &PgConnectOptions,
    database: &str,
) -> Result<Catalog, SandboxError> {
    let mut conn = PgConnection::connect_with(&options.clone().database(database))
        .await
        .map_err(bootstrap_error)?;
    let builtins: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT proname::text FROM pg_proc WHERE pronamespace = 'pg_catalog'::regnamespace",
    )
    .fetch_all(&mut conn)
    .await
    .map_err(bootstrap_error)?;
    let available: Vec<String> =
        sqlx::query_scalar("SELECT name::text FROM pg_available_extensions WHERE name = ANY($1)")
            .bind(ALLOWED_EXTENSIONS)
            .fetch_all(&mut conn)
            .await
            .map_err(bootstrap_error)?;
    let missing: Vec<&&str> = ALLOWED_EXTENSIONS
        .iter()
        .filter(|e| !available.iter().any(|a| a == *e))
        .collect();
    if !missing.is_empty() {
        tracing::warn!(
            ?missing,
            "allowlisted extensions missing from the sandbox image"
        );
    }

    exec(&mut conn, "BEGIN".to_owned()).await?;
    let mut installed = HashSet::new();
    for name in &available {
        exec(&mut conn, "SAVEPOINT probe".to_owned()).await?;
        let created = raw_sql(AssertSqlSafe(format!(
            "CREATE EXTENSION IF NOT EXISTS \"{}\" CASCADE",
            name.replace('"', "\"\"")
        )))
        .execute(&mut conn)
        .await;
        match created {
            Ok(_) => {
                exec(&mut conn, "RELEASE SAVEPOINT probe".to_owned()).await?;
                installed.insert(name.clone());
            }
            Err(err) => {
                tracing::warn!(extension = %name, error = %err, "extension cannot be installed in the sandbox");
                exec(&mut conn, "ROLLBACK TO SAVEPOINT probe".to_owned()).await?;
            }
        }
    }
    let probed: String = raw_sql(PROBE_SQL)
        .fetch_one(&mut conn)
        .await
        .map_err(bootstrap_error)?
        .try_get(0)
        .map_err(bootstrap_error)?;
    exec(&mut conn, "ROLLBACK".to_owned()).await?;
    conn.close().await.map_err(bootstrap_error)?;

    let probed: Vec<ProbedExtension> = serde_json::from_str(&probed)
        .map_err(|e| SandboxError::Bootstrap(format!("extension probe: {e}")))?;
    let mut extensions = BTreeMap::new();
    for e in probed.into_iter().filter(|e| installed.contains(&e.name)) {
        let home = e.installed_in.clone();
        let objects = e
            .objects
            .into_iter()
            .map(|o| ProvidedObject {
                schema: (Some(&o.schema) != home.as_ref()).then_some(o.schema),
                name: o.name,
                kind: if o.kind == "relation" {
                    ProvidedKind::Relation
                } else {
                    ProvidedKind::Type
                },
            })
            .collect();
        extensions.insert(
            e.name,
            Extension {
                requires: e.requires.unwrap_or_default(),
                relocatable: e.relocatable,
                fixed_schema: e.schema,
                functions: e.functions.into_iter().collect(),
                types: e.types.into_iter().collect(),
                objects,
            },
        );
    }
    Ok(Catalog::new(builtins, extensions))
}

/// One JSON document describing every extension available, and for the
/// installed ones what they created (their `pg_depend` 'e' members).
const PROBE_SQL: &str = r"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'name', v.name,
    'relocatable', v.relocatable,
    'schema', v.schema,
    'requires', v.requires,
    'installed_in', (SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
                     WHERE e.extname = v.name),
    'functions', (SELECT coalesce(jsonb_agg(DISTINCT p.proname), '[]')
                  FROM pg_extension e
                  JOIN pg_depend d ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid
                                  AND d.deptype = 'e' AND d.classid = 'pg_proc'::regclass
                  JOIN pg_proc p ON p.oid = d.objid
                  WHERE e.extname = v.name),
    'types', (SELECT coalesce(jsonb_agg(DISTINCT t.typname), '[]')
              FROM pg_extension e
              JOIN pg_depend d ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid
                              AND d.deptype = 'e' AND d.classid = 'pg_type'::regclass
              JOIN pg_type t ON t.oid = d.objid
              WHERE e.extname = v.name),
    'objects', (SELECT coalesce(jsonb_agg(o), '[]') FROM (
                  SELECT jsonb_build_object('schema', n.nspname, 'name', c.relname, 'kind', 'relation') AS o
                  FROM pg_extension e
                  JOIN pg_depend d ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid
                                  AND d.deptype = 'e' AND d.classid = 'pg_class'::regclass
                  JOIN pg_class c ON c.oid = d.objid AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
                  JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE e.extname = v.name
                  UNION ALL
                  SELECT jsonb_build_object('schema', n.nspname, 'name', t.typname, 'kind', 'type')
                  FROM pg_extension e
                  JOIN pg_depend d ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid
                                  AND d.deptype = 'e' AND d.classid = 'pg_type'::regclass
                  JOIN pg_type t ON t.oid = d.objid
                  JOIN pg_namespace n ON n.oid = t.typnamespace
                  WHERE e.extname = v.name
                    AND (t.typtype IN ('e', 'd', 'r')
                         OR (t.typtype = 'c' AND (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c'))
                ) objects)
)), '[]')::text
FROM pg_available_extension_versions v
JOIN pg_available_extensions a ON a.name = v.name AND a.default_version = v.version";

#[cfg(test)]
mod tests {
    use super::{hex, parser_major, password};

    #[test]
    fn the_parser_speaks_postgres_17() {
        assert_eq!(parser_major(), 17);
    }

    #[test]
    fn passwords_are_stable_and_per_role() {
        let a = password("postgres://admin:secret@host/db", "romulus_sandbox_0");
        assert_eq!(
            a,
            password("postgres://admin:secret@host/db", "romulus_sandbox_0")
        );
        assert_ne!(
            a,
            password("postgres://admin:secret@host/db", "romulus_sandbox_1")
        );
        assert_ne!(
            a,
            password("postgres://admin:other@host/db", "romulus_sandbox_0")
        );
        assert_eq!(a.len(), 64);
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
