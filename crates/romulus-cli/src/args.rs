//! The command line: `romulus test <migrations-dir> --database <org/db>`
//! and its options. No interactive prompt, ever: CI has no one to answer.
//! The token is never an argument (a process list would show it): it comes
//! from `ROMULUS_TOKEN` only.

use std::{path::PathBuf, time::Duration};

/// What to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Test(Box<TestOptions>),
    Help,
    Version,
}

/// `romulus test`'s options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOptions {
    /// The migrations folder.
    pub dir: PathBuf,
    /// The organization's slug and the database's.
    pub org: String,
    pub database: String,
    /// The migration tool (`auto`: detected from the files).
    pub format: Option<String>,
    /// The draft's name; from the CI's context when absent.
    pub label: Option<String>,
    /// `small` or `medium` generated rows.
    pub size: Option<String>,
    /// `none` (the default for written migrations) or `generate`.
    pub down: Option<String>,
    /// Table rewrites and index builds that block writes do not fail the
    /// job.
    pub allow_locks: bool,
    /// Test again even when this text was tested already.
    pub fresh: bool,
    /// How long everything may take, uploads included.
    pub timeout: Duration,
    /// Print the outcome as JSON.
    pub json: bool,
    /// romulus's address (`--url`, else `ROMULUS_URL`).
    pub url: Option<String>,
}

/// The default `--timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(15);

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::new(),
            org: String::new(),
            database: String::new(),
            format: None,
            label: None,
            size: None,
            down: None,
            allow_locks: false,
            fresh: false,
            timeout: DEFAULT_TIMEOUT,
            json: false,
            url: None,
        }
    }
}

const SIZES: [&str; 2] = ["small", "medium"];
const DOWNS: [&str; 2] = ["none", "generate"];

pub const USAGE: &str = "\
romulus: test a repository's new migrations on rows in a romulus playground.

Usage:
  romulus test <migrations-dir> --database <org>/<db> [options]

Uploads the folder's migrations (plain SQL, sqlx, Flyway, Prisma, dbmate,
golang-migrate, node-pg-migrate, or the SQL Rails, Django and Alembic
exporters print), takes the ones the database has not imported yet, and runs
them on a fresh playground holding its latest version and generated rows.

Options:
  -d, --database <org>/<db>  The database in romulus (or ROMULUS_DATABASE)
      --format <tool>        auto (default), plain, sqlx, flyway, prisma, dbmate,
                             golang_migrate, node_pg_migrate, alembic, django, rails
      --label <text>         The draft's name (default: the CI's pull request
                             and branch); a later run with the same label reuses it
      --size <small|medium>  How many generated rows (default: small)
      --down <none|generate> Also run a generated down migration (default: none)
      --allow-locks          Do not fail on table rewrites or index builds that
                             block writes (still reported)
      --fresh                Test again even if this exact text was tested
      --timeout <duration>   Give up after this long: 90s, 15m (default), 1h
      --json                 Print the outcome as JSON
      --url <url>            romulus's address (or ROMULUS_URL; default
                             https://romulus.lupidae.com)
  -h, --help                 This help
  -V, --version              The version

Environment:
  ROMULUS_TOKEN   A personal access token with write access (account menu,
                  Access tokens), of someone with the developer role or above

Exit status: 0 the migrations passed (or none is new), 1 a check failed,
2 a usage, token, connection or timeout error.
";

/// Why the command line is wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

fn usage(message: impl Into<String>) -> UsageError {
    UsageError(message.into())
}

/// Reads the arguments (without the program's name); `database` is
/// `ROMULUS_DATABASE` when `--database` is absent.
pub fn parse(
    args: impl IntoIterator<Item = String>,
    database: Option<String>,
) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };
    match command.as_str() {
        "-h" | "--help" | "help" => return Ok(Command::Help),
        "-V" | "--version" | "version" => return Ok(Command::Version),
        "test" => {}
        other => {
            return Err(usage(format!(
                "unknown command `{other}`: the command is `romulus test <migrations-dir> \
                 --database <org>/<db>`"
            )));
        }
    }
    let mut dir = None;
    let mut database = database.filter(|d| !d.trim().is_empty());
    let mut options = TestOptions::default();
    while let Some(arg) = args.next() {
        // `--name=value` as well as `--name value`.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_owned(), Some(value)),
            _ => (arg.clone(), None),
        };
        let mut value = |what: &str| -> Result<String, UsageError> {
            match inline {
                Some(value) => Ok(value.to_owned()),
                None => args
                    .next()
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| usage(format!("`{name}` needs {what}"))),
            }
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-d" | "--database" => database = Some(value("<org>/<db>")?),
            "--format" => options.format = Some(choice(&value("a tool")?, &FORMATS, "--format")?),
            "--label" => options.label = Some(value("a text")?),
            "--size" => options.size = Some(choice(&value("a size")?, &SIZES, "--size")?),
            "--down" => options.down = Some(choice(&value("none or generate")?, &DOWNS, "--down")?),
            "--timeout" => options.timeout = duration(&value("a duration")?)?,
            "--url" => options.url = Some(value("an address")?),
            "--allow-locks" | "--fresh" | "--json" if inline.is_some() => {
                return Err(usage(format!("`{name}` takes no value")));
            }
            "--allow-locks" => options.allow_locks = true,
            "--fresh" => options.fresh = true,
            "--json" => options.json = true,
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(usage(format!(
                    "unknown option `{flag}` (romulus --help lists them)"
                )));
            }
            _ if dir.is_none() => dir = Some(PathBuf::from(arg.clone())),
            _ => {
                return Err(usage(format!(
                    "one migrations folder only, not also `{arg}`"
                )));
            }
        }
    }
    options.dir = dir.ok_or_else(|| {
        usage("give the migrations folder: romulus test <migrations-dir> --database <org>/<db>")
    })?;
    let database = database
        .ok_or_else(|| usage("give the database: --database <org>/<db> (or ROMULUS_DATABASE)"))?;
    let (org, db) = database
        .trim()
        .split_once('/')
        .filter(|(org, db)| slug(org) && slug(db))
        .ok_or_else(|| {
            usage(format!(
                "--database is the organization's slug and the database's, as in the app's \
                 address: acme/shop, not `{database}`"
            ))
        })?;
    org.clone_into(&mut options.org);
    db.clone_into(&mut options.database);
    if let Some(label) = &options.label
        && label.trim().is_empty()
    {
        options.label = None;
    }
    Ok(Command::Test(Box::new(options)))
}

/// The tools an import reads (romulus's names).
pub const FORMATS: [&str; 11] = [
    "auto",
    "plain",
    "sqlx",
    "flyway",
    "prisma",
    "dbmate",
    "golang_migrate",
    "node_pg_migrate",
    "alembic",
    "django",
    "rails",
];

fn choice(value: &str, allowed: &[&str], name: &str) -> Result<String, UsageError> {
    let value = value.trim().to_ascii_lowercase().replace('-', "_");
    if allowed.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(usage(format!("`{name}` is one of {}", allowed.join(", "))))
    }
}

/// A slug as romulus makes them: lowercase letters, digits and dashes.
fn slug(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `90`, `90s`, `15m`, `1h`.
pub fn duration(text: &str) -> Result<Duration, UsageError> {
    let text = text.trim();
    let (number, unit) = text
        .find(|c: char| !c.is_ascii_digit())
        .map_or((text, ""), |at| text.split_at(at));
    let seconds = match (number.parse::<u64>(), unit) {
        (Ok(n), "" | "s") => Some(n),
        (Ok(n), "m") => n.checked_mul(60),
        (Ok(n), "h") => n.checked_mul(3600),
        _ => None,
    };
    match seconds {
        Some(s) if s > 0 => Ok(Duration::from_secs(s)),
        _ => Err(usage(format!(
            "--timeout is a duration like 90s, 15m or 1h, not `{text}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn test(list: &[&str]) -> TestOptions {
        match parse(args(list), None).unwrap() {
            Command::Test(options) => *options,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_folder_and_a_database_are_enough() {
        let options = test(&["test", "db/migrations", "--database", "acme/shop"]);
        assert_eq!(options.dir, PathBuf::from("db/migrations"));
        assert_eq!(
            (options.org.as_str(), options.database.as_str()),
            ("acme", "shop")
        );
        assert_eq!(options.timeout, DEFAULT_TIMEOUT);
        assert!(!options.json && !options.allow_locks && !options.fresh);
    }

    #[test]
    fn options_come_in_either_form_and_any_order() {
        let options = test(&[
            "test",
            "--json",
            "--database=acme/shop",
            "--format",
            "golang-migrate",
            "--timeout=90s",
            "--label",
            "PR #3",
            "migrations",
            "--allow-locks",
            "--down=generate",
            "--size",
            "medium",
        ]);
        assert!(options.json && options.allow_locks);
        assert_eq!(options.format.as_deref(), Some("golang_migrate"));
        assert_eq!(options.timeout, Duration::from_secs(90));
        assert_eq!(options.label.as_deref(), Some("PR #3"));
        assert_eq!(options.down.as_deref(), Some("generate"));
        assert_eq!(options.size.as_deref(), Some("medium"));
        assert_eq!(options.dir, PathBuf::from("migrations"));
    }

    #[test]
    fn the_database_may_come_from_the_environment() {
        let Command::Test(options) = parse(args(&["test", "m"]), Some("acme/shop".into())).unwrap()
        else {
            panic!()
        };
        assert_eq!(options.database, "shop");
        // The flag wins.
        let Command::Test(options) = parse(
            args(&["test", "m", "--database", "acme/other"]),
            Some("acme/shop".into()),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(options.database, "other");
    }

    #[test]
    fn mistakes_are_usage_errors_that_say_what_to_write() {
        for (list, says) in [
            (
                &["test", "--database", "acme/shop"][..],
                "migrations folder",
            ),
            (&["test", "m"][..], "--database"),
            (&["test", "m", "--database", "shop"][..], "acme/shop"),
            (&["test", "m", "--database", "Acme/Shop"][..], "acme/shop"),
            (
                &["test", "m", "-d", "a/b", "--format", "liquibase"][..],
                "--format",
            ),
            (
                &["test", "m", "-d", "a/b", "--timeout", "soon"][..],
                "--timeout",
            ),
            (
                &["test", "m", "-d", "a/b", "--timeout", "0"][..],
                "--timeout",
            ),
            (&["test", "m", "-d", "a/b", "--wat"][..], "unknown option"),
            (
                &["test", "m", "n", "-d", "a/b"][..],
                "one migrations folder",
            ),
            (&["test", "m", "-d"][..], "needs"),
            (
                &["test", "m", "-d", "a/b", "--json=yes"][..],
                "takes no value",
            ),
            (&["deploy"][..], "unknown command"),
        ] {
            let err = parse(args(list), None).unwrap_err();
            assert!(err.0.contains(says), "{list:?}: {err}");
        }
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(args(&[]), None).unwrap(), Command::Help);
        assert_eq!(parse(args(&["--help"]), None).unwrap(), Command::Help);
        assert_eq!(parse(args(&["test", "-h"]), None).unwrap(), Command::Help);
        assert_eq!(parse(args(&["-V"]), None).unwrap(), Command::Version);
    }

    #[test]
    fn durations() {
        assert_eq!(duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(duration("15m").unwrap(), Duration::from_mins(15));
        assert_eq!(duration("1h").unwrap(), Duration::from_hours(1));
        assert!(duration("1d").is_err());
        assert!(duration("").is_err());
    }
}
