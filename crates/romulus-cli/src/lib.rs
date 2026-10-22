//! romulus from a terminal or CI. `romulus test <migrations-dir> --database
//! <org>/<db>` runs a repository's new migrations on rows before they
//! merge, in a playground holding the database's latest version and
//! generated rows, and fails the job when one fails, locks dangerously or
//! loses data (docs/cli.md).
//!
//! It is a client of romulus's MCP endpoint with a personal access token,
//! doing what an agent does there (the server's logic stays the server's):
//! the folder's files sent as an upload (`import_migrations`, `commit:
//! false`), then `test_migration` with that upload (the migrations the
//! database has not imported yet, written into a migration-mode draft named
//! after the pull request, tested), then `get_migration_test` until the
//! verdict. [`report`] turns the answers into the outcome, its text, its
//! JSON and its exit status.

pub mod args;
pub mod ci;
pub mod files;
pub mod mcp;
pub mod report;

use std::{collections::BTreeMap, time::Duration};

use serde_json::{Value, json};
use tokio::time::Instant;

use self::{
    args::TestOptions,
    mcp::{CallError, Client},
    report::{Outcome, Reporting, RunResult, Taken},
};

/// The hosted service, unless `--url` or `ROMULUS_URL` says otherwise.
pub const DEFAULT_URL: &str = "https://romulus.lupidae.com";

/// The release this binary is (`ROMULUS_CLI_VERSION` at build time: the
/// release workflow sets the tag's), else the crate's version.
pub const VERSION: &str = match option_env!("ROMULUS_CLI_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// How long a tool call may wait for a test (romulus's cap).
const WAIT: Duration = Duration::from_secs(40);

/// Refusals that mean the migrations themselves are wrong: a failed check.
const FAILED_CHECKS: &[&str] = &[
    "import_failed",
    "migration_changed",
    "migration_out_of_order",
];

/// Runs `romulus test`, telling `progress` what it does (a line at a time);
/// never fails: what went wrong is in the outcome.
pub async fn test(
    options: &TestOptions,
    url: &str,
    token: &str,
    context: &ci::Context,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Outcome {
    let deadline = Instant::now() + options.timeout;
    let label = options
        .label
        .as_deref()
        .map(ci::clip)
        .or_else(|| context.label());
    let dir = options
        .dir
        .to_string_lossy()
        .trim_end_matches('/')
        .to_owned();
    let read = files::read(&options.dir);
    let (files, skipped) = match read {
        Ok(read) => read,
        Err(err) => (Vec::new(), vec![err.to_string()]),
    };
    let contents: BTreeMap<String, String> = files
        .iter()
        .map(|f| (f.path.clone(), f.content.clone()))
        .collect();
    let reporting = Reporting {
        database: format!("{}/{}", options.org, options.database),
        label: label.clone(),
        commit: context.commit.clone(),
        dir,
        files: &contents,
        allow_locks: options.allow_locks,
        skipped_files: if files.is_empty() {
            Vec::new()
        } else {
            skipped.clone()
        },
    };
    if files.is_empty() {
        let why = skipped.first().cloned().unwrap_or_default();
        return reporting.error(RunResult::Error, "files", &why);
    }
    let client = match Client::new(url, token) {
        Ok(client) => client,
        Err(err) => return failed(&reporting, &err),
    };

    let mut answer: Option<Taken> = None;
    let steps = run(
        &client,
        options,
        label.as_deref(),
        &files,
        deadline,
        &mut answer,
        progress,
    );
    match tokio::time::timeout_at(deadline, steps).await {
        Ok(Ok(())) => answer.map_or_else(
            || reporting.error(RunResult::Error, "internal", "romulus answered nothing"),
            |taken| reporting.taken(&taken),
        ),
        Ok(Err(err)) => failed(&reporting, &err),
        Err(_) => {
            let started = answer.as_ref().is_some_and(|a| a.test.is_some());
            let message = format!(
                "gave up after {} s (--timeout){}",
                options.timeout.as_secs(),
                if started {
                    "; the test goes on in romulus"
                } else {
                    ""
                }
            );
            answer
                .map_or_else(
                    || reporting.error(RunResult::Timeout, "timeout", &message),
                    |taken| reporting.taken(&taken),
                )
                .timed_out(message)
        }
    }
}

/// The outcome of a call that failed.
fn failed(reporting: &Reporting<'_>, err: &CallError) -> Outcome {
    match err {
        CallError::Tool {
            code,
            message,
            details,
        } if FAILED_CHECKS.contains(&code.as_str()) => {
            reporting.refused(code, message, details.as_ref())
        }
        CallError::Tool { code, message, .. } => {
            reporting.error(RunResult::Error, code, &explain(code, message))
        }
        other => reporting.error(RunResult::Error, other.code(), &other.to_string()),
    }
}

/// romulus's refusal, with what to do about it.
fn explain(code: &str, message: &str) -> String {
    let hint = match code {
        "not_found" => {
            " Check --database (the organization's slug and the database's, as in the app's \
             address) and that the token's person is a member."
        }
        "forbidden" => {
            " Testing writes a draft and runs a test: the token's person needs the developer \
             role (or admin) in this organization."
        }
        "history_not_imported" | "invalid_migrations" => " Check the folder given.",
        "playground_limit" | "playground_host_full" | "host_unavailable" | "tests_unavailable" => {
            " Tests run on playgrounds, which this romulus cannot give right now."
        }
        "plan_limit" => " The organization's plan does not allow it: an admin can change it.",
        "too_many_uploads" => {
            " Three uploads of this person are in progress (running tests in parallel?): wait \
             for one to end, or an hour for them to expire."
        }
        _ => "",
    };
    format!("{message}{hint}")
}

async fn run(
    client: &Client,
    options: &TestOptions,
    label: Option<&str>,
    files: &[files::File],
    deadline: Instant,
    taken: &mut Option<Taken>,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Result<(), CallError> {
    client.initialize().await?;
    let batches = files::batches(files, files::BATCH_BYTES);
    progress(&format!(
        "Sending {} file{} of {} to romulus…",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        options.dir.display()
    ));
    let mut upload: Option<String> = None;
    for batch in &batches {
        let mut arguments = json!({ "files": batch, "commit": false });
        if let Some(id) = &upload {
            arguments["upload"] = json!(id);
        } else {
            arguments["org"] = json!(options.org);
            arguments["database"] = json!(options.database);
        }
        let sent = client.call("import_migrations", arguments, true).await?;
        upload = sent["upload"]["id"].as_str().map(str::to_owned);
        if upload.is_none() {
            return Err(CallError::Connection(
                "romulus did not say where the files went".into(),
            ));
        }
    }

    progress("Testing the new migrations on a playground…");
    let mut arguments = json!({
        "upload": upload,
        "fresh": options.fresh,
        "wait_seconds": seconds_left(deadline),
    });
    for (key, value) in [
        ("name", label),
        ("format", options.format.as_deref()),
        ("size", options.size.as_deref()),
        ("down", options.down.as_deref()),
    ] {
        if let Some(value) = value {
            arguments[key] = json!(value);
        }
    }
    let answer = client.call("test_migration", arguments, true).await?;
    let mut current: Taken = read(answer)?;
    if let Some(draft) = &current.draft {
        progress(&format!(
            "{} new migration{} in draft \"{}\"{}.",
            current.migrations.len(),
            if current.migrations.len() == 1 {
                ""
            } else {
                "s"
            },
            draft.name,
            if draft.reused { " (reused)" } else { "" }
        ));
    }
    let test_id = current.test.as_ref().map(|t| t.id.clone());
    *taken = Some(current.clone());
    let Some(test_id) = test_id else {
        return Ok(());
    };
    let mut said = String::new();
    while current.test.as_ref().is_some_and(|t| !t.is_final()) {
        let test = current.test.as_ref().expect("checked above");
        let now = match test.position {
            Some(n) => format!("Queued: number {n} in line…"),
            None => "Running…".to_owned(),
        };
        if now != said {
            progress(&now);
            said = now;
        }
        let answer = client
            .call(
                "get_migration_test",
                json!({ "test": test_id, "wait_seconds": seconds_left(deadline) }),
                false,
            )
            .await?;
        current.test = Some(read(answer["test"].clone())?);
        if let Some(page) = answer["page"].as_str() {
            current.page = Some(page.to_owned());
        }
        *taken = Some(current.clone());
    }
    Ok(())
}

/// What a tool may wait, within the time left.
fn seconds_left(deadline: Instant) -> u64 {
    deadline
        .saturating_duration_since(Instant::now())
        .min(WAIT)
        .as_secs()
}

fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, CallError> {
    serde_json::from_value(value)
        .map_err(|err| CallError::Connection(format!("romulus's answer did not read: {err}")))
}
