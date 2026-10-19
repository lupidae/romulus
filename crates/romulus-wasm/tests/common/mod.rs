//! What the tests share: parse trees exactly as the browser gets them, and
//! the sandbox Postgres as the oracle.
//!
//! In the browser, libpg-query's wasm (`libpg_query` 17) returns the parse
//! tree as JSON. Natively, `pg_query` (the same `libpg_query`, the one the
//! API parses with) returns it as protobuf; written back with the
//! protobuf's JSON mapping, it is byte for byte that JSON: the `json_name`
//! of every field in `pg_query.proto` is `libpg_query`'s, enums are their
//! names and defaults are left out on both sides.

#![allow(dead_code)] // each test binary uses its own part

use std::{future::Future, sync::LazyLock, time::Duration};

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, SerializeOptions};
use remus_core::Schema;
use romulus_sandbox::{Compiler, SandboxConfig};
use romulus_wasm::{Compiled, Diagnostic};
use tokio::{runtime::Runtime, sync::OnceCell};

static PARSE_RESULT: LazyLock<MessageDescriptor> = LazyLock::new(|| {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/common");
    let files = protox::compile(["pg_query.proto"], [dir]).expect("pg_query.proto compiles");
    DescriptorPool::from_file_descriptor_set(files)
        .expect("a descriptor pool")
        .get_message_by_name("pg_query.ParseResult")
        .expect("pg_query.ParseResult")
});

/// `sql`'s parse tree as `libpg_query`'s JSON, or Postgres's syntax error.
pub fn tree(sql: &str) -> Result<String, pg_query::Error> {
    let parsed = pg_query::parse(sql)?;
    let bytes = parsed.protobuf.encode_to_vec();
    let message =
        DynamicMessage::decode(PARSE_RESULT.clone(), bytes.as_slice()).expect("decodes again");
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut out);
    message
        .serialize_with_options(
            &mut serializer,
            &SerializeOptions::new().stringify_64_bit_integers(false),
        )
        .expect("serializes");
    Ok(String::from_utf8(out).expect("JSON is UTF-8"))
}

/// What the browser's engine makes of `sql`: its parse tree, or its syntax
/// error, compiled.
pub fn compile(sql: &str) -> Compiled {
    match tree(sql) {
        Ok(tree) => romulus_wasm::compile(sql, &tree),
        Err(pg_query::Error::Parse(message)) => {
            // libpg-query reports the error's position; pg_query keeps it in
            // the message only. Tests of syntax errors call `syntax_error`.
            romulus_wasm::syntax_error(sql, &message, 0)
        }
        Err(other) => panic!("pg_query failed: {other}"),
    }
}

/// The model the engine makes of `sql`, failing the test on any error.
#[track_caller]
pub fn model(sql: &str) -> Schema {
    let compiled = compile(sql);
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.clone())
        .collect();
    assert!(errors.is_empty(), "errors: {errors:?}\nSQL:\n{sql}");
    compiled.model.expect("a model")
}

/// The error messages the engine reports for `sql`.
pub fn errors(sql: &str) -> Vec<String> {
    compile(sql)
        .diagnostics
        .into_iter()
        .filter(Diagnostic::is_error)
        .map(|d| d.message)
        .collect()
}

// ---- The oracle ---------------------------------------------------------------

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
});

static COMPILER: OnceCell<Compiler> = OnceCell::const_new();

/// The sandbox's admin URL, from the environment or the worktree's `.env`.
fn admin_url() -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    std::env::var("ROMULUS_SANDBOX_URL").ok()
}

/// Run `body` with the sandbox compiler. Without `ROMULUS_SANDBOX_URL` it
/// is skipped, loudly, except in CI, where the oracle must run.
pub fn with_sandbox<F, Fut>(body: F)
where
    F: FnOnce(&'static Compiler) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = admin_url() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "ROMULUS_SANDBOX_URL is not set: CI must run the oracle"
        );
        #[allow(clippy::print_stderr)] // a skipped suite must say so
        {
            eprintln!("SKIPPED: ROMULUS_SANDBOX_URL is not set, the oracle did not run");
        }
        return;
    };
    RUNTIME.block_on(async {
        let compiler = COMPILER
            .get_or_init(|| async {
                Compiler::start(SandboxConfig {
                    pool_size: 2,
                    prefix: "romulus_wasm_test".to_owned(),
                    statement_timeout: Duration::from_secs(15),
                    compile_timeout: Duration::from_secs(60),
                    min_interval: Duration::from_millis(20),
                    ..SandboxConfig::new(url)
                })
                .await
                .expect("the sandbox compiler starts (is sandbox-postgres up?)")
            })
            .await;
        body(compiler).await;
    });
}

/// The model the sandbox Postgres makes of `sql`.
pub async fn sandbox_model(compiler: &Compiler, sql: &str) -> Schema {
    let compilation = compiler.compile(sql).await.expect("the sandbox answers");
    let errors: Vec<_> = compilation
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.clone())
        .collect();
    assert!(errors.is_empty(), "the sandbox refused it: {errors:?}");
    compilation.model.expect("a model")
}
