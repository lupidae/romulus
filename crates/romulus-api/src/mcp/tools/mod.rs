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
//! are hidden from read-only tokens, and each successful call is recorded in
//! the Activity of the database it touched (`agent_action`, with the token).

mod drafts;
mod onboarding;
mod playgrounds;
mod read;
mod testing;

use std::fmt::Write as _;

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
}

impl Ctx {
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

/// A tool's answer: text only. Clients hand a model `structuredContent`
/// instead of the text when both are there (Claude Code does), and the text
/// is the whole answer: ids, links, what to do next.
#[derive(Default)]
pub struct Output {
    pub text: String,
    /// What to record in the Activity (write tools).
    pub audit: Option<Audit>,
}

impl Output {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn audited(mut self, audit: Audit) -> Self {
        self.audit = Some(audit);
        self
    }
}

/// What a write did, for the Activity.
pub struct Audit {
    /// The organization's slug.
    pub org: String,
    pub database: Option<Uuid>,
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
            "The organization's plan does not allow more of this. An admin can change the \
             plan in the organization's settings; tell your person.",
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
    let output = match tool.name {
        name if read::handles(name) => read::call(ctx, name, arguments).await,
        name if drafts::handles(name) => drafts::call(ctx, name, arguments).await,
        name if testing::handles(name) => testing::call(ctx, name, arguments).await,
        name if playgrounds::handles(name) => playgrounds::call(ctx, name, arguments).await,
        name => onboarding::call(ctx, name, arguments).await,
    }?;
    if let Some(audit) = &output.audit {
        record(ctx, tool, audit).await;
    }
    Ok(output)
}

/// Records a write in the Activity, with the token. Best effort: the write
/// happened, and its own handler kept what the organization must account
/// for.
async fn record(ctx: &Ctx, tool: &Tool, audit: &Audit) {
    let agent = ctx.api.agent();
    let db = &ctx.api.state().db;
    let org = sqlx::query_scalar!(
        "SELECT o.id FROM organizations o JOIN memberships m ON m.org_id = o.id
         WHERE o.slug = $1 AND m.user_id = $2",
        audit.org,
        agent.user.id,
    )
    .fetch_optional(db)
    .await;
    let Ok(Some(org_id)) = org else {
        tracing::warn!(tool = tool.name, org = %audit.org, "mcp: cannot find the organization to audit");
        return;
    };
    let details = AuditDetails {
        token_id: Some(agent.token.id),
        token_name: Some(agent.token.name.clone()),
        tool: Some(tool.name.to_owned()),
        outcome: Some(audit.outcome.clone()),
        ..audit.details.clone()
    };
    let recorded = audit::record(
        db,
        &audit::Entry {
            org_id,
            database_id: audit.database,
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
        tracing::warn!(tool = tool.name, error = %err, "mcp: cannot audit an agent's write");
    }
}

/// Where a draft, test or playground is, for the caller: its organization's
/// and its database's slugs. `None` when it does not exist or the caller is
/// not a member of its organization (the API says the same: not found).
pub struct Place {
    pub org: String,
    pub database: String,
}

pub async fn draft_place(ctx: &Ctx, draft: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database
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
        r#"SELECT o.slug AS org, d.slug AS database
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

pub async fn playground_place(ctx: &Ctx, playground: Uuid) -> Result<Place, ToolError> {
    sqlx::query_as!(
        Place,
        r#"SELECT o.slug AS org, d.slug AS database
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
