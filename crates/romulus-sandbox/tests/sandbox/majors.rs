//! The other Postgres majors (docs/MVP.md, "Several Postgres majors"): every
//! corpus schema compiles on each major's sandbox and survives the round
//! trip a draft takes, as on the default one (the rest of this suite).
//!
//! Each runs when its URL is set (`ROMULUS_SANDBOX_URL_PG15`, `…_PG16`,
//! `…_PG18`, in `.env`) and its sandbox answers: `just sandbox 15`, or
//! `just sandbox all`. In CI (`CI` set) a major whose URL is set must run.

use std::time::Duration;

use romulus_sandbox::{Code, Compiler, SandboxConfig, SandboxError};
use tokio::sync::OnceCell;

use crate::{RUNTIME, config, corpus::round_trip, explain, major_url};

/// Every schema of tests/corpus, by file name.
const CORPUS: &[(&str, &str)] = &[
    ("01_saas_rls.sql", include_str!("../corpus/01_saas_rls.sql")),
    (
        "02_ecommerce.sql",
        include_str!("../corpus/02_ecommerce.sql"),
    ),
    (
        "03_partitioned_events.sql",
        include_str!("../corpus/03_partitioned_events.sql"),
    ),
    (
        "04_generated_identity.sql",
        include_str!("../corpus/04_generated_identity.sql"),
    ),
    ("05_views.sql", include_str!("../corpus/05_views.sql")),
    ("06_postgis.sql", include_str!("../corpus/06_postgis.sql")),
    ("07_pgvector.sql", include_str!("../corpus/07_pgvector.sql")),
    (
        "08_custom_functions.sql",
        include_str!("../corpus/08_custom_functions.sql"),
    ),
    (
        "09_circular_fks.sql",
        include_str!("../corpus/09_circular_fks.sql"),
    ),
    (
        "10_multi_schema.sql",
        include_str!("../corpus/10_multi_schema.sql"),
    ),
    (
        "11_extensions.sql",
        include_str!("../corpus/11_extensions.sql"),
    ),
    (
        "12_supabase_like.sql",
        include_str!("../corpus/12_supabase_like.sql"),
    ),
    (
        "13_graph_edits.sql",
        include_str!("../corpus/13_graph_edits.sql"),
    ),
    (
        "14_enum_evolution.sql",
        include_str!("../corpus/14_enum_evolution.sql"),
    ),
    (
        "15_types_and_storage.sql",
        include_str!("../corpus/15_types_and_storage.sql"),
    ),
];

/// One compiler per major, shared by its tests (one prefix per process).
static COMPILERS: [OnceCell<Option<Compiler>>; 3] = [
    OnceCell::const_new(),
    OnceCell::const_new(),
    OnceCell::const_new(),
];

#[allow(clippy::print_stderr)] // a skipped major must say so
fn on_major<F, Fut>(major: u32, body: F)
where
    F: FnOnce(&'static Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = major_url(major) else {
        eprintln!(
            "SKIPPED: ROMULUS_SANDBOX_URL_PG{major} is not set, PostgreSQL {major} was not tested"
        );
        return;
    };
    let cell = match major {
        15 => &COMPILERS[0],
        16 => &COMPILERS[1],
        18 => &COMPILERS[2],
        _ => panic!("no other major to test: {major}"),
    };
    RUNTIME.block_on(async {
        let compiler = cell
            .get_or_init(|| async {
                let config = SandboxConfig {
                    major: Some(major),
                    // Its own prefix, on its own cluster.
                    ..config(url, &format!("romulus_test_pg{major}"))
                };
                match Compiler::start(config).await {
                    Ok(compiler) => Some(compiler),
                    Err(SandboxError::Unavailable(err)) if std::env::var_os("CI").is_none() => {
                        eprintln!(
                            "SKIPPED: PostgreSQL {major}'s sandbox is not running (`just sandbox {major}`): {err}"
                        );
                        None
                    }
                    Err(err) => panic!("PostgreSQL {major}'s sandbox compiler starts: {err}"),
                }
            })
            .await;
        let Some(compiler) = compiler else { return };
        assert_eq!(compiler.major(), major);
        assert!(
            compiler.server_version().starts_with(&format!("{major}.")),
            "{}",
            compiler.server_version()
        );
        body(compiler).await;
    });
}

/// The corpus on one major: each schema compiles and round-trips; the
/// failures all listed at once.
async fn corpus_compiles(compiler: &'static Compiler) {
    let major = compiler.major();
    for (name, sql) in CORPUS {
        if *name == "14_enum_evolution.sql" && major < 17 {
            enum_values_wait_for_a_commit(compiler, sql).await;
            continue;
        }
        let (compilation, model) = round_trip(compiler, name, sql).await;
        assert!(
            model.server_version.starts_with(&format!("{major}.")),
            "{name}: compiled on {}",
            model.server_version
        );
        assert!(
            compilation.is_ok(),
            "{name}: {}",
            explain(sql, &compilation)
        );
    }
}

/// Before 17, an enum value added in a transaction cannot be used in it,
/// even when the transaction created the type: the corpus schema growing its
/// enums in place fails, and says why.
async fn enum_values_wait_for_a_commit(compiler: &Compiler, sql: &str) {
    let outcome = compiler.compile(sql).await.expect("the sandbox runs");
    let error = outcome.errors().next().unwrap_or_else(|| {
        panic!(
            "PostgreSQL {} used an enum value in the transaction adding it",
            compiler.major()
        )
    });
    assert_eq!(
        error.sqlstate.as_deref(),
        Some("55P04"),
        "{}",
        explain(sql, &outcome)
    );
    assert!(
        error
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("Before PostgreSQL 17")),
        "{:?}",
        error.hint
    );
}

#[test]
fn the_corpus_compiles_on_postgres_15() {
    on_major(15, corpus_compiles);
}

#[test]
fn the_corpus_compiles_on_postgres_16() {
    on_major(16, corpus_compiles);
}

#[test]
fn the_corpus_compiles_on_postgres_18() {
    on_major(18, corpus_compiles);
}

/// What only a newer major has is refused by an older one, with Postgres's
/// own error, located: an exclusion constraint on a partitioned table is
/// 17+ (the default major compiles it, `errors.rs`).
#[test]
fn an_older_major_refuses_what_it_lacks() {
    on_major(15, |compiler| async move {
        let compiled = compiler
            .compile(PARTITIONED_EXCLUSION)
            .await
            .expect("the sandbox runs");
        assert!(!compiled.is_ok(), "PostgreSQL 15 compiled it");
        let error = compiled.errors().next().expect("an error");
        assert_eq!(error.code, Code::Sql);
        assert_eq!(
            error.message,
            "exclusion constraints are not supported on partitioned tables"
        );
        assert_eq!(error.span.expect("located").from.line, 5);
    });
}

/// Compiles on 17 and 18, not on 15 or 16.
pub const PARTITIONED_EXCLUSION: &str = "CREATE EXTENSION btree_gist;
CREATE TABLE bookings (
  room int NOT NULL,
  during tstzrange NOT NULL,
  EXCLUDE USING gist (room WITH =, during WITH &&)
) PARTITION BY LIST (room);";

/// A sandbox on another major than configured does not start.
#[test]
fn a_sandbox_on_another_major_is_refused() {
    on_major(15, |compiler| async move {
        let url = major_url(15).expect("set");
        let wrong = SandboxConfig {
            major: Some(16),
            acquire_timeout: Duration::from_secs(5),
            ..config(url, "romulus_test_pg15_wrong")
        };
        match Compiler::start(wrong).await {
            Err(SandboxError::VersionMismatch { expected, .. }) => assert_eq!(expected, 16),
            Err(err) => panic!("expected a version mismatch, got {err}"),
            Ok(_) => panic!("a PostgreSQL 15 sandbox started as 16"),
        }
        assert_eq!(compiler.major(), 15);
    });
}

/// The default major (17) compiles what 15 refuses.
#[test]
fn the_default_major_compiles_what_15_lacks() {
    crate::with_compiler(|compiler| async move {
        assert_eq!(compiler.major(), 17);
        crate::compile_ok(compiler, PARTITIONED_EXCLUSION).await;
    });
}

/// Syntax new in 18 is beyond the parser (17's grammar): refused before the
/// sandbox runs, with a hint saying why.
#[test]
fn syntax_new_in_18_is_refused_with_a_reason() {
    on_major(18, |compiler| async move {
        let sql = "CREATE TABLE t (a int, b int GENERATED ALWAYS AS (a * 2) VIRTUAL);";
        let compiled = compiler.compile(sql).await.expect("the sandbox runs");
        let error = compiled.errors().next().expect("a syntax error");
        assert_eq!(error.code, Code::SyntaxError, "{}", explain(sql, &compiled));
        assert!(
            error
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("PostgreSQL 18")),
            "{:?}",
            error.hint
        );
    });
}
