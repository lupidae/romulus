//! `romulus`: see the library's documentation and `romulus --help`.

use std::{io::Write as _, process::ExitCode};

use romulus_cli::{
    DEFAULT_URL, VERSION,
    args::{self, Command, USAGE},
    ci::Context,
};

fn main() -> ExitCode {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let command = match args::parse(std::env::args().skip(1), env("ROMULUS_DATABASE")) {
        Ok(command) => command,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "romulus: {err}\nSee romulus --help.");
            return ExitCode::from(2);
        }
    };
    let options = match command {
        Command::Help => {
            let _ = write!(std::io::stdout(), "{USAGE}");
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            let _ = writeln!(std::io::stdout(), "romulus {VERSION}");
            return ExitCode::SUCCESS;
        }
        Command::Test(options) => options,
    };
    let url = options
        .url
        .clone()
        .or_else(|| env("ROMULUS_URL"))
        .unwrap_or_else(|| DEFAULT_URL.to_owned());
    let token = env("ROMULUS_TOKEN").unwrap_or_default();
    // GitHub's event payload: a pull request's number and head commit.
    let event = env("GITHUB_EVENT_PATH")
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let context = Context::detect(&env, event.as_ref());
    let github = env("GITHUB_ACTIONS").as_deref() == Some("true");

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "romulus: {err}");
            return ExitCode::from(2);
        }
    };
    let quiet = options.json;
    let mut progress = |line: &str| {
        if !quiet {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
    };
    let outcome = runtime.block_on(romulus_cli::test(
        &options,
        &url,
        &token,
        &context,
        &mut progress,
    ));

    let mut stdout = std::io::stdout();
    if options.json {
        let _ = writeln!(stdout, "{}", outcome.json());
    } else {
        let _ = write!(stdout, "{}", outcome.text());
    }
    if github {
        let _ = write!(stdout, "{}", outcome.annotations());
        // The step's outputs (the action's `result` and `report`).
        if let Some(output) = env("GITHUB_OUTPUT")
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(output)
        {
            let result = serde_json::to_value(outcome.result).unwrap_or_default();
            let _ = writeln!(file, "result={}", result.as_str().unwrap_or("error"));
            let _ = writeln!(
                file,
                "report={}",
                outcome.test_page.as_deref().unwrap_or("")
            );
        }
        if let Some(summary) = env("GITHUB_STEP_SUMMARY")
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(summary)
        {
            let _ = writeln!(file, "### romulus test\n\n```\n{}```\n", outcome.text());
        }
    }
    ExitCode::from(outcome.exit_code)
}
