//! Onboarding: a database, its first version from SQL, and CI pushing the
//! next ones with remus.

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Audit, Ctx, Output, Tool, ToolError, args, read::diagnostics, schema};
use crate::{
    audit::AuditDetails,
    databases::{Database, tokens::CreatedToken},
    versions::VersionSummary,
};

pub const TOOLS: &[&Tool] = &[
    &CREATE_DATABASE,
    &PUSH_SCHEMA,
    &CREATE_PUSH_TOKEN,
    &CI_SETUP,
];

pub async fn call(ctx: &Ctx, name: &str, arguments: Value) -> Result<Output, ToolError> {
    match name {
        "create_database" => create_database(ctx, args(arguments)?).await,
        "push_schema" => push_schema(ctx, args(arguments)?).await,
        "create_push_token" => create_push_token(ctx, args(arguments)?).await,
        _ => ci_setup(ctx, args(arguments)?).await,
    }
}

#[derive(Deserialize)]
struct CreateArgs {
    #[serde(default)]
    org: Option<String>,
    name: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pg_major: Option<u16>,
}

static CREATE_DATABASE: Tool = Tool {
    name: "create_database",
    title: "Create a database",
    description: "Changes shared state: adds a database to an organization (developers and \
                  admins), with no version yet: push_schema gives it its first, from SQL. \
                  `pg_major` is the Postgres major its schema targets (15 to 18; 17 by \
                  default): its SQL compiles there and its migrations are written for it.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "name": { "type": "string", "maxLength": 100, "description": "\"Billing\"." },
                "slug": { "type": "string", "maxLength": 63, "description": "Its address: lowercase letters, digits and hyphens. From the name when absent." },
                "description": { "type": "string", "maxLength": 1000 },
                "pg_major": { "type": "integer", "enum": [15, 16, 17, 18], "default": 17 },
            }),
            &["name"],
        )
    },
};

/// A slug from a name: lowercase ASCII letters and digits (accents
/// dropped), single hyphens.
pub fn slug_of(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut slug = String::new();
    for c in name
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(63).collect();
    slug.trim_end_matches('-').to_owned()
}

async fn create_database(ctx: &Ctx, a: CreateArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let slug = a
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| slug_of(&a.name), str::to_owned);
    let mut body = json!({
        "name": a.name,
        "slug": slug,
        "description": a.description.unwrap_or_default(),
    });
    if let Some(major) = a.pg_major {
        body["pg_major"] = json!(major);
    }
    let database: Database = ctx.api.post(&["orgs", &org, "databases"], &body).await?;
    let text = format!(
        "Created database {org}/{} \"{}\" (Postgres {}, id {}). It has no version yet: \
         push_schema gives it its first from SQL (pg_dump --schema-only, or remus's SQL), and \
         ci_setup explains pushing from CI.\nPage: {}\n",
        database.slug,
        database.name,
        database.pg_major,
        database.id,
        ctx.link(&format!("{org}/{}", database.slug))
    );
    Ok(Output::text(text).audited(Audit {
        org,
        database: Some(database.id),
        version: None,
        outcome: format!("created database \"{}\"", database.name),
        details: AuditDetails::default(),
    }))
}

#[derive(Deserialize)]
struct PushArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    sql: String,
    #[serde(default)]
    message: Option<String>,
}

static PUSH_SCHEMA: Tool = Tool {
    name: "push_schema",
    title: "Push a schema",
    description: "Changes shared state: creates the database's next version from SQL DDL: \
                  the output of `pg_dump --schema-only` as it is, remus's SQL, or a \
                  hand-written schema (1 MiB at most). What a schema version does not hold \
                  (settings, ownership, grants, data, functions, triggers, psql commands) is \
                  left out and listed. It is compiled in a sandbox Postgres of the database's \
                  major first: when it does not compile, nothing is stored and each problem is \
                  answered with its line. For CI, use create_push_token and ci_setup instead.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "sql": { "type": "string", "description": "The whole schema as SQL DDL." },
                "message": { "type": "string", "maxLength": 1000, "description": "What it is, in a line: \"Initial schema from production\"." },
            }),
            &["database", "sql"],
        )
    },
};

async fn push_schema(ctx: &Ctx, a: PushArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    // A dump holds more than a version does: what is not schema is left
    // out (and said), lines kept, so errors point into the text as sent.
    let reduced = romulus_sandbox::import::schema_only(&a.sql);
    if reduced.sql.trim().is_empty() {
        return Err(ToolError::Invalid(
            "`sql` holds no schema: no table, type or view to keep.".into(),
        ));
    }
    let version: VersionSummary = ctx
        .api
        .post(
            &["orgs", &org, "databases", &a.database, "versions"],
            &json!({ "sql": reduced.sql, "message": a.message }),
        )
        .await
        .map_err(|failure| match failure.code() {
            "compile_failed" => {
                let problems = failure
                    .body
                    .details
                    .as_ref()
                    .and_then(|d| d.get("diagnostics"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                ToolError::Say(format!(
                    "The SQL does not compile, so no version was created. Fix these (lines are \
                     the text's own) and push again:\n{}",
                    diagnostics(&problems, 30)
                ))
            }
            _ => ToolError::Api(failure),
        })?;
    let database: Database = ctx
        .api
        .get(&["orgs", &org, "databases", &a.database], &[])
        .await?;
    let mut text = format!(
        "Created v{} of {org}/{}.\nPage: {}\n",
        version.number,
        a.database,
        ctx.link(&format!("{org}/{}/v/{}", a.database, version.number))
    );
    if !reduced.skipped.is_empty() {
        let _ = writeln!(
            text,
            "Left out, as a schema version does not hold them ({}):",
            reduced.skipped.len()
        );
        for skipped in reduced.skipped.iter().take(15) {
            let _ = writeln!(text, "- line {}: {}", skipped.line, skipped.text);
        }
        if reduced.skipped.len() > 15 {
            let _ = writeln!(text, "- … and {} more", reduced.skipped.len() - 15);
        }
    }
    Ok(Output::text(text).audited(Audit {
        org,
        database: Some(database.id),
        version: Some(version.number),
        outcome: format!("pushed v{}", version.number),
        details: AuditDetails::default(),
    }))
}

#[derive(Deserialize)]
struct TokenArgs {
    #[serde(default)]
    org: Option<String>,
    database: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

static CREATE_PUSH_TOKEN: Tool = Tool {
    name: "create_push_token",
    title: "Create a CI push token",
    description: "Changes shared state: creates an API token for one database, for CI to push \
                  versions with remus (`push`) or also read them back (`full`). Its secret is \
                  answered once: give it to your person to store as a CI secret \
                  (ROMULUS_TOKEN); never write it into a repository. Revoked in the \
                  database's settings.",
    writes: true,
    destructive: false,
    idempotent: false,
    input: || {
        schema::object(
            json!({
                "org": schema::org(),
                "database": schema::database(),
                "name": { "type": "string", "maxLength": 100, "description": "Where it is used: \"GitHub Actions\"." },
                "scope": { "type": "string", "enum": ["push", "full"], "default": "push" },
            }),
            &["database"],
        )
    },
};

async fn create_push_token(ctx: &Ctx, a: TokenArgs) -> Result<Output, ToolError> {
    let org = ctx.org(a.org.as_deref()).await?;
    let created: CreatedToken = ctx
        .api
        .post(
            &["orgs", &org, "databases", &a.database, "tokens"],
            &json!({
                "name": a.name.unwrap_or_else(|| "CI".into()),
                "scope": a.scope.unwrap_or_else(|| "push".into()),
            }),
        )
        .await?;
    let database: Database = ctx
        .api
        .get(&["orgs", &org, "databases", &a.database], &[])
        .await?;
    let text = format!(
        "Created the API token \"{}\" for {org}/{}. Its secret, shown this once:\n\n{}\n\n\
         Store it as the CI secret ROMULUS_TOKEN (never in the repository). Then:\n{}",
        created.token.name,
        a.database,
        created.secret,
        push_command(ctx, &database)
    );
    Ok(Output::text(text).audited(Audit {
        org,
        database: Some(database.id),
        version: None,
        outcome: format!("created the API token \"{}\"", created.token.name),
        details: AuditDetails::default(),
    }))
}

/// The command CI runs: remus's JSON, through curl (the web app shows the
/// same: `features/databases/push-command.ts`).
fn push_command(ctx: &Ctx, database: &Database) -> String {
    format!(
        "remus -f json | curl -fsS -H \"Authorization: Bearer $ROMULUS_TOKEN\" \\\n  \
         --data-binary @- {}\n",
        ctx.public_url
            .join(&format!("/api/databases/{}/versions", database.id))
            .map_or_else(|_| String::new(), String::from)
    )
}

#[derive(Deserialize)]
struct CiArgs {
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    database: Option<String>,
}

static CI_SETUP: Tool = Tool {
    name: "ci_setup",
    title: "How to push from CI",
    description: "How to wire remus into CI so that every change to a real database's schema \
                  becomes a version: the command, a GitHub Actions step, and the token it \
                  needs (with `database`, filled in for it).",
    writes: false,
    destructive: false,
    idempotent: true,
    input: || {
        schema::object(
            json!({ "org": schema::org(), "database": schema::database() }),
            &[],
        )
    },
};

async fn ci_setup(ctx: &Ctx, a: CiArgs) -> Result<Output, ToolError> {
    let command = match &a.database {
        Some(slug) => {
            let org = ctx.org(a.org.as_deref()).await?;
            let database: Database = ctx.api.get(&["orgs", &org, "databases", slug], &[]).await?;
            push_command(ctx, &database)
        }
        None => format!(
            "remus -f json | curl -fsS -H \"Authorization: Bearer $ROMULUS_TOKEN\" \\\n  \
             --data-binary @- {}\n",
            ctx.public_url
                .join("/api/databases/<database id>/versions")
                .map_or_else(|_| String::new(), String::from)
        ),
    };
    Ok(Output::text(ci_guide(&command)))
}

/// The CI guide (the `ci_setup` tool and the `romulus://guide/ci`
/// resource).
pub fn ci_guide(command: &str) -> String {
    let mut text = String::from(
        "Pushing from CI with remus (https://github.com/lupidae/remus)\n\n\
         remus reads a live database's schema and prints it; romulus keeps each new schema \
         as a version. Run it where your migrations have just run (a CI service database, \
         staging) on every build: an unchanged schema creates nothing.\n\n\
         1. A token: create_push_token (or the database's settings, API tokens), stored as \
         the CI secret ROMULUS_TOKEN.\n\
         2. The command, after the migrations:\n\n",
    );
    text.push_str(command);
    let _ = write!(
        text,
        "\n3. In GitHub Actions, as a step after the migrations:\n\n\
         - name: Push the schema to romulus\n  \
         env:\n    ROMULUS_TOKEN: ${{{{ secrets.ROMULUS_TOKEN }}}}\n  \
         run: |\n    {}\n\
         A migrations folder (sqlx, Flyway, Prisma, dbmate, golang-migrate…) can also become \
         the history at once: the database's Import page in the web app.\n",
        command.trim_end().replace('\n', "\n    ")
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_come_from_names() {
        assert_eq!(slug_of("Billing"), "billing");
        assert_eq!(slug_of("  Main DB (prod) "), "main-db-prod");
        assert_eq!(slug_of("Été 2026"), "ete-2026");
        assert!(slug_of(&"a".repeat(80)).len() <= 63);
    }

    #[test]
    fn the_ci_guide_has_a_runnable_step() {
        let guide = ci_guide("remus -f json | curl …\n");
        assert!(guide.contains("${{ secrets.ROMULUS_TOKEN }}"), "{guide}");
        assert!(guide.contains("    remus -f json"), "{guide}");
    }
}
