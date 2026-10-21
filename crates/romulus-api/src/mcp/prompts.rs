//! Prompts: the common flows, as instructions a client can offer its person
//! (`/` commands in Claude Code). Each names the tools in the order that
//! works, and the moments to ask the person before going on.

use serde_json::{Value, json};

struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// (name, description, required)
    arguments: &'static [(&'static str, &'static str, bool)],
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "onboard_database",
        title: "Onboard a database",
        description: "Bring a Postgres database into romulus: create it, push its schema as the \
                      first version, then set CI up to push every change.",
        arguments: &[
            ("name", "What to call the database in romulus.", false),
            (
                "source",
                "Where the schema comes from: a pg_dump --schema-only file, a SQL file in the \
                 repository, or a live database through remus.",
                false,
            ),
        ],
    },
    Prompt {
        name: "review_draft",
        title: "Review a draft",
        description: "Review a draft as a careful DBA: what it changes, the migration it \
                      generates and its hazards, renames, and a test on data.",
        arguments: &[("draft", "The draft's id.", true)],
    },
    Prompt {
        name: "plan_safe_migration",
        title: "Plan a safe migration",
        description: "Turn a change you describe into a draft whose migration is safe on \
                      existing data, tested on rows before anyone publishes it.",
        arguments: &[
            ("database", "The database's slug.", true),
            (
                "change",
                "The change, in words: \"add a refunds table\".",
                true,
            ),
            (
                "org",
                "The organization's slug, if you have several.",
                false,
            ),
        ],
    },
];

pub fn list() -> Value {
    json!({
        "prompts": PROMPTS.iter().map(|p| json!({
            "name": p.name,
            "title": p.title,
            "description": p.description,
            "arguments": p.arguments.iter().map(|(name, description, required)| json!({
                "name": name,
                "description": description,
                "required": required,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The prompt `name` with `arguments` filled in, or what is wrong.
pub fn get(name: &str, arguments: &Value) -> Result<Value, String> {
    let prompt = PROMPTS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Unknown prompt: {name}"))?;
    let arg = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    for (key, _, required) in prompt.arguments {
        if *required && arg(key).is_none() {
            return Err(format!("The prompt {name} needs `{key}`."));
        }
    }
    let text = match name {
        "onboard_database" => onboard(arg("name"), arg("source")),
        "review_draft" => review(arg("draft").unwrap_or_default()),
        _ => plan(
            arg("database").unwrap_or_default(),
            arg("change").unwrap_or_default(),
            arg("org"),
        ),
    };
    Ok(json!({
        "description": prompt.description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

fn onboard(name: Option<&str>, source: Option<&str>) -> String {
    let name = name.map_or_else(
        || "the database (ask me what to call it if it is not obvious)".to_owned(),
        |n| format!("\"{n}\""),
    );
    let source = source.map_or_else(
        || {
            "Find where its schema is: a schema dump in this repository, migrations, or ask me \
             for a `pg_dump --schema-only` (never a dump with data)."
                .to_owned()
        },
        |s| format!("Its schema comes from: {s}."),
    );
    format!(
        "Bring {name} into romulus with the romulus tools.\n\
         1. whoami: check my organizations and that my role there is developer or admin.\n\
         2. {source}\n\
         3. create_database (ask me which Postgres major it runs if you cannot tell), then \
         push_schema with the schema's SQL. If it does not compile, fix only what is not \
         schema (data, ownership, settings) and say what you removed.\n\
         4. get_schema with format `summary`: tell me in a few lines what romulus now holds.\n\
         5. Ask me before create_push_token: it makes a secret. Then ci_setup, and propose the \
         CI change for this repository (the secret goes in the CI's secrets, never in a file)."
    )
}

fn review(draft: &str) -> String {
    format!(
        "Review the romulus draft {draft} as a careful DBA would.\n\
         1. get_draft: what it changes, whether it compiles, its review state.\n\
         2. Read its migration: for every statement flagged DELETES DATA, CAN FAIL or WIDENS \
         ACCESS, say whether it is intended and what could go wrong on production rows \
         (locks, rewrites of large tables, NOT NULL on existing rows).\n\
         3. Renames: for each one considered, say whether it really is a rename. Do not change \
         them yourself: suggest confirm_renames and let me decide.\n\
         4. test_migration on the draft; if it is not finished, poll get_migration_test. \
         Report the verdict and any data lost that the migration does not declare.\n\
         5. Summarize: approve, approve with changes (which), or not yet (why). Do not \
         publish, and do not edit the draft unless I ask."
    )
}

fn plan(database: &str, change: &str, org: Option<&str>) -> String {
    let org = org
        .map(|o| format!(" (organization {o})"))
        .unwrap_or_default();
    format!(
        "Plan this change to the romulus database {database}{org}: {change}\n\
         1. get_schema with format `summary`, then `sql` for the tables involved.\n\
         2. create_draft with a name saying what it does, then update_draft with `edits` \
         (exact replacements) rather than rewriting the whole schema. Fix compile errors.\n\
         3. get_draft: read the migration. Prefer what keeps data and avoids long locks: \
         renames over drop and add (confirm_renames), new columns nullable or with a \
         constant default, NOT NULL added after a backfill, indexes concurrently on big \
         tables. If a step cannot be safe in one migration, say how to split it.\n\
         4. test_migration on the draft, then get_migration_test until it is final. Fix and \
         test again until it passes.\n\
         5. Show me the draft's link, the migration, the test's verdict and what to watch \
         when it runs. request_review only if I ask; never publish_draft without my explicit \
         go-ahead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_fill_in_their_arguments_and_refuse_missing_ones() {
        let got = get("review_draft", &json!({ "draft": "0193" })).unwrap();
        let text = got["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("draft 0193"));
        assert!(get("review_draft", &json!({})).is_err());
        assert!(get("nope", &json!({})).is_err());
        assert_eq!(list()["prompts"].as_array().unwrap().len(), 3);
    }
}
