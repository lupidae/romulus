//! The tools an agent calls: what each is ([`all`]: names, descriptions,
//! JSON schemas, hints), and calling one ([`call`]).
//!
//! Every tool is a thin adapter over the REST API ([`super::api::Api`]: the
//! web app's handlers, as the token's person): it resolves names to what the
//! routes take, calls them, and answers compact text an agent reads well
//! (ids, slugs and links to carry on with; no models or layouts). Refusals
//! come back as tool errors with the API's message and what to do about it
//! ([`ToolError`]): a role refused, a cap or plan limit reached, a name that
//! does not exist.
//!
//! Write tools say so in their description (they change what others see),
//! are hidden from read-only tokens, and what they change is recorded in the
//! Activity of the database it touched (`agent_action`, with the token)
//! right after the write succeeds ([`Ctx::audit`]), whatever the tool does
//! next. A write a tool did not describe (it failed after it) is recorded
//! anyway, from the API calls it made ([`call`]).

mod drafts;
mod files;
mod onboarding;
mod playgrounds;
mod read;
mod testing;

use std::{
    fmt::Write as _,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

pub use self::onboarding::ci_guide;
use super::api::{Api, Failure};
use crate::{
    audit::{self, AuditDetails},
    domain::AuditAction,
};

/// What a tool is, for `tools/list`.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// It changes shared state: hidden from read-only tokens, audited.
    pub writes: bool,
    /// It may delete or overwrite something (a hint for the client to ask
    /// first).
    pub destructive: bool,
    /// Calling it again with the same arguments changes nothing more.
    pub idempotent: bool,
    pub input: fn() -> Value,
}

impl Tool {
    /// Its `tools/list` entry.
    pub fn listing(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": (self.input)(),
            "annotations": {
                "title": self.title,
                "readOnlyHint": !self.writes,
                "destructiveHint": self.destructive,
                "idempotentHint": self.idempotent,
                "openWorldHint": false,
            },
        })
    }
}

/// Every tool, in the order `tools/list` gives them (stable: clients cache
/// it, and models read it).
pub fn all() -> Vec<&'static Tool> {
    let mut tools: Vec<&'static Tool> = Vec::new();
    tools.extend(read::TOOLS);
    tools.extend(drafts::TOOLS);
    tools.extend(testing::TOOLS);
    tools.extend(playgrounds::TOOLS);
    tools.extend(onboarding::TOOLS);
    tools.extend(files::TOOLS);
    tools
}

pub fn find(name: &str) -> Option<&'static Tool> {
    all().into_iter().find(|tool| tool.name == name)
}

/// What a tool call has to work with.
pub struct Ctx {
    pub api: Api,
    /// Where romulus is: links the agent can hand its person.
    pub public_url: Url,
    /// The tool called (its writes are audited under its name).
    tool: OnceLock<&'static str>,
    /// An audit was recorded for this call.
    audited: AtomicBool,
}

/// What a wait leaves of the request's time for the answer.
const ANSWER_MARGIN: Duration = Duration::from_secs(5);

impl Ctx {
    pub fn new(api: Api, public_url: Url) -> Self {
        Self {
            api,
            public_url,
            tool: OnceLock::new(),
            audited: AtomicBool::new(false),
        }
    }

    /// How long a tool may wait for something (a test, a build): what it
    /// asked for, at most `max`, and never past the request's deadline
    /// (less a margin to answer).
    pub fn wait_budget(&self, asked: Duration, max: Duration) -> Duration {
        budget(asked, max, self.api.time_left())
    }

    /// Records a write in the Activity, with the token, right after it
    /// succeeded: what the tool does next cannot lose it.
    pub async fn audit(&self, audit: Audit) {
        self.audited.store(true, Ordering::Relaxed);
        record(self, &audit).await;
    }

    /// The call's writes so far changed nothing anyone else sees (files
    /// staged for an import, the person's alone): nothing for the Activity
    /// unless [`Ctx::audit`] records something after.
    pub fn nothing_to_audit(&self) {
        self.audited.store(true, Ordering::Relaxed);
    }

    /// The page at `path` (under `/app`), as a link.
    pub fn link(&self, path: &str) -> String {
        self.public_url
            .join(&format!("/app/{}", path.trim_start_matches('/')))
            .map_or_else(|_| path.to_owned(), String::from)
    }

    /// The organization a tool is about: the one given, or the caller's only
    /// one.
    pub async fn org(&self, given: Option<&str>) -> Result<String, ToolError> {
        if let Some(given) = given.map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(given.to_owned());
        }
        let orgs: Vec<crate::orgs::Org> = self.api.get(&["orgs"], &[]).await?;
        match orgs.as_slice() {
            [only] => Ok(only.slug.clone()),
            [] => Err(ToolError::Say(
                "You are not a member of any organization yet: create one in romulus, or ask \
                 an admin for an invitation."
                    .into(),
            )),
            many => Err(ToolError::Invalid(format!(
                "Say which organization (`org`): you belong to {}.",
                many.iter()
                    .map(|o| format!("`{}`", o.slug))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

/// The wait [`Ctx::wait_budget`] allows, `left` before the deadline.
fn budget(asked: Duration, max: Duration, left: Duration) -> Duration {
    asked.min(max).min(left.saturating_sub(ANSWER_MARGIN))
}

/// A tool's answer: text. Clients hand a model `structuredContent` instead
/// of the text when both are there (Claude Code does), and the text is the
/// whole answer: ids, links, what to do next. Only `whoami` adds structured
/// facts, for programs checking a connection (the web app's Test
/// connection).
///
/// A program driving the tools (the romulus CLI) asks for `data` too, the
/// API's own JSON of what a tool did, by putting [`DATA_META`]`: true` in
/// the request's `_meta`: only that request gets it, as
/// `structuredContent`, so agents keep reading the text.
#[derive(Default)]
pub struct Output {
    pub text: String,
    pub structured: Option<Value>,
    pub data: Option<Value>,
}

/// The `_meta` key a `tools/call` sets (to `true`) to get [`Output::data`].
pub const DATA_META: &str = "romulus/data";

impl Output {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// With `data`, for programs that ask for it.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// The database a write touched: its id when the answer gave it, else its
/// slug in the organization.
pub enum Target {
    Id(Uuid),
    Slug(String),
}

/// What a write did, for the Activity.
pub struct Audit {
    /// The organization's slug.
    pub org: String,
    pub database: Option<Target>,
    pub version: Option<i32>,
    /// In a few words: `created draft "Add invoices"`.
    pub outcome: String,
    pub details: AuditDetails,
}

/// Why a tool did not do what was asked: said to the agent as a tool error
/// (`isError`), which it can act on.
#[derive(Debug)]
pub enum ToolError {
    /// The arguments are wrong: what to send instead.
    Invalid(String),
    /// The API refused or failed.
    Api(Failure),
    /// Anything else to say.
    Say(String),
}

impl From<Failure> for ToolError {
    fn from(failure: Failure) -> Self {
        Self::Api(failure)
    }
}

impl ToolError {
    /// The text the agent reads.
    pub fn text(&self) -> String {
        match self {
            Self::Invalid(message) => format!("Invalid arguments: {message}"),
            Self::Say(message) => message.clone(),
            Self::Api(failure) => explain(failure),
        }
    }

    /// The error as a program reads it ([`Output::data`]): the API's own
    /// (`code`, `message`, `details`, and its HTTP `status`), or `invalid`
    /// for arguments and `refused` for anything else the tool says.
    pub fn data(&self) -> Value {
        match self {
            Self::Api(failure) => {
                let mut body = json!(failure.body);
                body["status"] = json!(failure.status.as_u16());
                body
            }
            Self::Invalid(message) => json!({ "code": "invalid", "message": message }),
            Self::Say(message) => json!({ "code": "refused", "message": message }),
        }
    }
}

/// A refusal of the API, with what to do about it.
fn explain(failure: &Failure) -> String {
    let body = &failure.body;
    let mut text = format!("{} ({})", body.message, body.code);
    let hint = match body.code.as_str() {
        "forbidden" => Some(
            "Your role in this organization does not allow it: reviewers read and comment, \
             developers change schemas, admins manage members and settings.",
        ),
        "read_only_token" => Some(
            "This agent's access token is read-only: its person can create one with write \
             access in romulus (account menu, Access tokens).",
        ),
        "not_found" => Some(
            "Check the names: whoami lists organizations, list_databases a database's slug, \
             get_database its drafts.",
        ),
        "plan_limit" => Some(
            "Nothing was changed: the organization's plan does not allow more of this. An \
             admin can change the plan on the organization's Billing page (Settings, then \
             Billing); tell your person rather than retrying.",
        ),
        "terms_required" => Some(
            "Nothing was done: your person has not accepted romulus's Terms of Service yet. Ask \
             them to sign in to romulus once in their browser and accept them; then try again.",
        ),
        "rate_limited" => Some("Wait the time it says, then try again."),
        "playground_limit" | "playground_host_full" | "too_many_tokens" => {
            Some("Delete one that is no longer needed, then try again.")
        }
        "host_unavailable" => Some("Playgrounds are not available right now on this server."),
        "stale_base" => Some(
            "Someone published a version since the draft started: update the draft from the \
             web app (Update draft), then try again.",
        ),
        "draft_changed" => Some("Read the draft again (get_draft), then try again."),
        "concurrent_import" => Some(
            "Another import of this database is running (maybe this one, sent twice): wait for \
             it (get_import), then try again if it is still needed.",
        ),
        "review_required" => Some(
            "Nothing was imported. This organization reviews changes, so an agent's import \
             only brings a history into a database without versions. Write the change as a \
             draft instead: create_draft in migration mode with the new migration's SQL, then \
             test_migration and request_review; publishing it makes the version once approved.",
        ),
        "upload_expired" => {
            Some("Send every file again with a new import_migrations call (without `upload`).")
        }
        "upload_busy" => Some("Follow the import with get_import; do not send it again."),
        "too_many_uploads" => Some(
            "Commit one of your uploads (import_migrations with its `upload`), or wait an hour \
             for it to expire.",
        ),
        "payload_too_large" => Some(
            "Send fewer files per call (commit: false, then `upload`), or a large file in parts.",
        ),
        "timeout" => Some(
            "It may still finish: read the state again (get_draft, get_database, \
             list_playgrounds) before trying again.",
        ),
        _ => None,
    };
    if let Some(details) = &body.details {
        if let Some(diagnostics) = details.get("diagnostics").and_then(Value::as_array) {
            let _ = write!(text, "\n{}", read::diagnostics(diagnostics, 20));
        } else if body.code == "validation" {
            let _ = write!(text, "\nDetails: {details}");
        } else if let Some(retry) = details.get("retry_after") {
            let _ = write!(text, " Retry after {retry} s.");
        }
    }
    if let Some(hint) = hint {
        let _ = write!(text, "\n{hint}");
    }
    text
}

/// A tool's arguments, read into what it takes.
pub fn args<T: DeserializeOwned>(arguments: Value) -> Result<T, ToolError> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    serde_json::from_value(arguments).map_err(|err| ToolError::Invalid(err.to_string()))
}

/// An id given as text.
pub fn uuid(value: &str, what: &str) -> Result<Uuid, ToolError> {
    Uuid::parse_str(value.trim())
        .map_err(|_| ToolError::Invalid(format!("`{what}` must be an id like {}.", Uuid::nil())))
}

/// Calls the tool `name` with `arguments`.
pub async fn call(ctx: &Ctx, tool: &Tool, arguments: Value) -> Result<Output, ToolError> {
    if tool.writes && !ctx.api.agent().token.can_write() {
        return Err(ToolError::Api(super::api::failure_of(
            &crate::auth::access_tokens::read_only(),
        )));
    }
    let _ = ctx.tool.set(tool.name);
    let result = match tool.name {
        name if read::handles(name) => read::call(ctx, name, arguments).await,
        name if drafts::handles(name) => drafts::call(ctx, name, arguments).await,
        name if testing::handles(name) => testing::call(ctx, name, arguments).await,
        name if playgrounds::handles(name) => playgrounds::call(ctx, name, arguments).await,
        name if files::handles(name) => files::call(ctx, name, arguments).await,
        name => onboarding::call(ctx, name, arguments).await,
    };
    // A write the tool did not get to describe (it failed after it): in the
    // Activity all the same.
    if !ctx.audited.load(Ordering::Relaxed)
        && let Some(wrote) = ctx.api.first_write()
    {
        let Some(org) = wrote.org else {
            tracing::warn!(
                tool = tool.name,
                "mcp: a write outside an organization, not audited"
            );
            return result;
        };
        let outcome = match &result {
            Ok(_) => format!("used {}", tool.name),
            Err(_) => format!("used {}, which then failed", tool.name),
        };
        ctx.audit(Audit {
            org,
            database: wrote.database.map(Target::Slug),
            version: None,
            outcome,
            details: AuditDetails::default(),
        })
        .await;
    }
    result
}

/// Records a write in the Activity, with the token. A failure is logged,
/// not answered: the write happened, and its own handler kept what the
/// organization must account for.
async fn record(ctx: &Ctx, audit: &Audit) {
    let agent = ctx.api.agent();
    let tool = ctx.tool.get().copied().unwrap_or("mcp");
    let db = &ctx.api.state().db;
    let place = sqlx::query!(
        r#"SELECT o.id AS org_id,
                  (SELECT d.id FROM databases d
                   WHERE d.org_id = o.id AND d.slug = $3 AND d.deleted_at IS NULL) AS database_id
           FROM organizations o JOIN memberships m ON m.org_id = o.id
           WHERE o.slug = $1 AND m.user_id = $2"#,
        audit.org,
        agent.user.id,
        match &audit.database {
            Some(Target::Slug(slug)) => Some(slug.as_str()),
            _ => None,
        },
    )
    .fetch_optional(db)
    .await;
    let Ok(Some(place)) = place else {
        tracing::warn!(tool, org = %audit.org, "mcp: cannot find the organization to audit");
        return;
    };
    let database_id = match &audit.database {
        Some(Target::Id(id)) => Some(*id),
        Some(Target::Slug(_)) => place.database_id,
        None => None,
    };
    let app = agent.token.kind == crate::auth::access_tokens::TokenKind::App;
    let details = AuditDetails {
        token_id: Some(agent.token.id),
        token_name: (!app).then(|| agent.token.name.clone()),
        app_name: app.then(|| agent.token.name.clone()),
        tool: Some(tool.to_owned()),
        outcome: Some(audit.outcome.clone()),
        ..audit.details.clone()
    };
    let recorded = audit::record(
        db,
        &audit::Entry {
            org_id: place.org_id,
            database_id,
            action: AuditAction::AgentAction,
            actor_id: Some(agent.user.id),
            actor_name: Some(agent.user.name.clone()),
            version_number: audit.version,
            reason: None,
            details,
        },
    )
    .await;
    if let Err(err) = recorded {
        tracing::warn!(tool, error = %err, "mcp: cannot audit an agent's write");
    }
}

/// Where a draft, test or playground is, for the caller: its organization's
/// and its database's slugs. `None` when it does not exist or the caller is
/// not a member of its organization (the API says the same: not found).
pub struct Place {
    pub org: String,
    pub database: String,
    pub database_id: Uuid,
}

pub async fn draft_place(ctx: &Ctx, draft: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database, d.id AS database_id
           FROM drafts dr
           JOIN databases d ON d.id = dr.database_id
           JOIN organizations o ON o.id = d.org_id
           JOIN memberships m ON m.org_id = o.id AND m.user_id = $2
           WHERE dr.id = $1 AND dr.status <> 'deleted' AND d.deleted_at IS NULL"#,
        draft,
        ctx.api.agent().user.id,
    )
    .fetch_optional(&ctx.api.state().db)
    .await
    .map_err(|err| ToolError::Api(super::api::failure_of(&err.into())))?
    .ok_or_else(|| {
        ToolError::Say(format!(
            "No draft {draft} in your organizations: it may have been deleted. \
             get_database lists a database's open drafts."
        ))
    })
}

pub async fn test_place(ctx: &Ctx, test: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database, d.id AS database_id
           FROM migration_tests t
           JOIN databases d ON d.id = t.database_id
           JOIN organizations o ON o.id = d.org_id
           JOIN memberships m ON m.org_id = o.id AND m.user_id = $2
           WHERE t.id = $1 AND d.deleted_at IS NULL"#,
        test,
        ctx.api.agent().user.id,
    )
    .fetch_optional(&ctx.api.state().db)
    .await
    .map_err(|err| ToolError::Api(super::api::failure_of(&err.into())))?
    .ok_or_else(|| ToolError::Say(format!("No migration test {test} in your organizations.")))
}

/// An upload of migration files (and its import) the caller started.
pub async fn upload_place(ctx: &Ctx, upload: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database, d.id AS database_id
           FROM import_uploads u
           JOIN databases d ON d.id = u.database_id
           JOIN organizations o ON o.id = d.org_id
           JOIN memberships m ON m.org_id = o.id AND m.user_id = $2
           WHERE u.id = $1 AND u.created_by = $2 AND d.deleted_at IS NULL"#,
        upload,
        ctx.api.agent().user.id,
    )
    .fetch_optional(&ctx.api.state().db)
    .await
    .map_err(|err| ToolError::Api(super::api::failure_of(&err.into())))?
    .ok_or_else(|| {
        ToolError::Say(format!(
            "No upload or import {upload} of yours: it may have expired (a day after it ended). \
             Send the files again with import_migrations (without `upload`)."
        ))
    })
}

pub async fn playground_place(ctx: &Ctx, playground: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database, d.id AS database_id
           FROM playgrounds p
           JOIN databases d ON d.id = p.database_id
           JOIN organizations o ON o.id = d.org_id
           JOIN memberships m ON m.org_id = o.id AND m.user_id = $2
           WHERE p.id = $1 AND d.deleted_at IS NULL"#,
        playground,
        ctx.api.agent().user.id,
    )
    .fetch_optional(&ctx.api.state().db)
    .await
    .map_err(|err| ToolError::Api(super::api::failure_of(&err.into())))?
    .ok_or_else(|| {
        ToolError::Say(format!(
            "No playground {playground} in your organizations: it may have expired. \
             list_playgrounds lists a database's."
        ))
    })
}

/// Schema fragments the tools share.
pub mod schema {
    use serde_json::{Value, json};

    pub fn org() -> Value {
        json!({
            "type": "string",
            "description": "The organization's slug (whoami lists yours). Optional when you belong to one organization.",
        })
    }

    pub fn database() -> Value {
        json!({
            "type": "string",
            "description": "The database's slug (list_databases lists them).",
        })
    }

    pub fn draft() -> Value {
        json!({
            "type": "string",
            "format": "uuid",
            "description": "The draft's id (from create_draft, or get_database's open drafts).",
        })
    }

    pub fn version(what: &str) -> Value {
        json!({ "type": "integer", "minimum": 1, "description": what })
    }

    /// An object schema: `properties`, the `required` ones, nothing else.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "every tool builds its properties in place"
    )]
    pub fn object(properties: Value, required: &[&str]) -> Value {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn waits_end_before_the_request_does() {
        let s = Duration::from_secs;
        // What was asked, within the tool's own cap…
        assert_eq!(budget(s(10), s(40), s(55)), s(10));
        assert_eq!(budget(s(90), s(40), s(55)), s(40));
        // …and never past the deadline, less the time to answer.
        assert_eq!(budget(s(40), s(40), s(30)), s(25));
        assert_eq!(budget(s(40), s(40), s(3)), s(0));
    }

    #[test]
    fn tool_names_are_unique_and_well_formed() {
        let tools = all();
        let mut names: Vec<_> = tools.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for tool in tools {
            assert!(
                tool.name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{}",
                tool.name
            );
            let input = (tool.input)();
            assert_eq!(input["type"], "object", "{}", tool.name);
            // Every write says so, for the model and its person.
            assert_eq!(
                tool.writes,
                tool.description.contains("Changes shared state"),
                "{}",
                tool.name
            );
            assert!(tool.description.len() < 1200, "{}", tool.name);
        }
    }
}
