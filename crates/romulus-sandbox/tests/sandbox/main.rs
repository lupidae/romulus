//! Integration tests against the sandbox Postgres of the compose stack.
//!
//! They need `ROMULUS_SANDBOX_URL` (a superuser URL; `.env` from
//! `scripts/dev-env` has it) and the sandbox running: `docker compose -p
//! "$(scripts/dev-env --project-name)" up -d --wait sandbox-postgres` (or
//! `just up`). Without the variable they are skipped, loudly. Every test
//! shares one compiler (one pool, prefix `romulus_test`) on one runtime, as
//! the API would.

mod concurrency;
mod corpus;
mod errors;
mod malicious;

use std::{future::Future, sync::LazyLock, time::Duration};

use romulus_sandbox::{Compilation, Compiler, SandboxConfig};
use tokio::{runtime::Runtime, sync::OnceCell};

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a tokio runtime")
});

static COMPILER: OnceCell<Compiler> = OnceCell::const_new();

/// The sandbox's admin URL, from the environment or the worktree's `.env`.
pub fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL").ok()
}

pub fn config(admin_url: String, prefix: &str) -> SandboxConfig {
    SandboxConfig {
        pool_size: 4,
        prefix: prefix.to_owned(),
        // CI machines are slow; the limits themselves are tested on their own.
        statement_timeout: Duration::from_secs(15),
        compile_timeout: Duration::from_secs(60),
        min_interval: Duration::from_millis(20),
        ..SandboxConfig::new(admin_url)
    }
}

#[allow(clippy::print_stderr)] // a skipped suite must say so
fn skip() {
    eprintln!("SKIPPED: ROMULUS_SANDBOX_URL is not set, the sandbox tests did not run");
}

/// Run a test body on the shared compiler, or skip without a sandbox.
pub fn with_compiler<F, Fut>(body: F)
where
    F: FnOnce(&'static Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        skip();
        return;
    };
    RUNTIME.block_on(async {
        let compiler = COMPILER
            .get_or_init(|| async {
                Compiler::start(config(url, "romulus_test"))
                    .await
                    .expect("the sandbox compiler starts (is sandbox-postgres up?)")
            })
            .await;
        body(compiler).await;
    });
}

/// Run a test body on the shared runtime with a compiler of its own.
pub fn with_own_compiler<F, Fut>(config: impl FnOnce(String) -> SandboxConfig, body: F)
where
    F: FnOnce(Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        skip();
        return;
    };
    RUNTIME.block_on(async {
        let compiler = Compiler::start(config(url))
            .await
            .expect("the sandbox compiler starts");
        body(compiler.clone()).await;
        compiler.close().await;
    });
}

/// Every diagnostic, one per line, with its location and text.
pub fn explain(sql: &str, compilation: &Compilation) -> String {
    compilation
        .diagnostics
        .iter()
        .map(|d| match d.span {
            Some(span) => format!(
                "{:?} {:?} {}:{} {} [{}]{}",
                d.severity,
                d.code,
                span.from.line,
                span.from.column,
                d.message,
                &sql[span.range()],
                d.hint
                    .as_ref()
                    .map(|h| format!(" hint: {h}"))
                    .unwrap_or_default()
            ),
            None => format!("{:?} {:?} {}", d.severity, d.code, d.message),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compile, and fail the test with every diagnostic unless it compiles.
pub async fn compile_ok(compiler: &Compiler, sql: &str) -> Compilation {
    let compilation = compiler.compile(sql).await.expect("the sandbox runs");
    assert!(
        compilation.is_ok(),
        "expected a model, got:\n{}",
        explain(sql, &compilation)
    );
    compilation
}

#[test]
fn a_table_with_a_function_default_compiles_with_a_stub() {
    with_compiler(|compiler| async move {
        let compiled = compile_ok(
            compiler,
            "CREATE TABLE users (id bigint PRIMARY KEY, org uuid DEFAULT auth.org())",
        )
        .await;
        let model = compiled.model.unwrap();
        assert_eq!(model.entities[0].name, "users");
        assert_eq!(
            model.entities[0].columns[1].default.as_deref(),
            Some("auth.org()")
        );
        assert_eq!(model.database, romulus_sandbox::MODEL_DATABASE);
        assert_eq!(compiled.stubs[0].signature(), "auth.org() → uuid");
        assert_eq!(compiled.created_schemas, ["auth"]);
    });
}
