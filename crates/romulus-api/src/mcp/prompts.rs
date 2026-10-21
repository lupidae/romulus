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
    Prompt {
        name: "import_migrations_folder",
        title: "Import my migrations folder",
        description: "Bring this repository's migration history into romulus: find the \
                      migrations folder, detect its tool, read every file, import them as one \
                      version per migration, and summarize what came in.",
        arguments: &[
            (
                "database",
                "The romulus database to import into (its slug).",
                false,
            ),
            ("folder", "Where the migrations are, if you know.", false),
            (
                "org",
                "The organization's slug, if you have several.",
                false,
            ),
        ],
    },
    Prompt {
        name: "write_draft_migration",
        title: "Write a draft's migration into my repository",
        description: "Write a romulus draft's migration into this repository, as the files its \
                      migration tool expects, named and numbered like the ones already there.",
        arguments: &[("draft", "The draft's id.", true)],
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
        "import_migrations_folder" => import_folder(arg("database"), arg("folder"), arg("org")),
        "write_draft_migration" => write_migration(arg("draft").unwrap_or_default()),
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
         push_schema with the schema's SQL (its first version; later changes are drafts). If \
         it does not compile, fix only what is not schema (data, ownership, settings) and say \
         what you removed.\n\
         4. get_schema with format `summary`: tell me in a few lines what romulus now holds.\n\
         5. ci_setup, and propose the CI change for this repository: I create the token on the \
         page it links and store it in the CI's secrets (never in a file)."
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

fn import_folder(database: Option<&str>, folder: Option<&str>, org: Option<&str>) -> String {
    let org_text = org
        .map(|o| format!(" (organization {o})"))
        .unwrap_or_default();
    let target = database.map_or_else(
        || {
            "ask me which database to import into, or whether to create one (create_database; \
             ask me its Postgres major). A database with versions already only takes an \
             import appended after them."
                .to_owned()
        },
        |d| {
            format!(
                "check that {d} exists; if it does not, ask me before creating it \
                 (create_database; ask me its Postgres major)."
            )
        },
    );
    let folder = folder.map_or_else(String::new, |f| format!(" Start with {f}."));
    format!(
        "Import this repository's database migrations into romulus with the romulus tools.\n\
         1. Find the migrations folder.{folder} Look for Flyway's V<n>__<name>.sql (often \
         src/main/resources/db/migration), Prisma's prisma/migrations/<timestamp>_<name>/\
         migration.sql, sqlx's or dbmate's timestamped .sql files (migrations/, \
         db/migrations/), golang-migrate's .up.sql and .down.sql pairs, node-pg-migrate's \
         migrations/, or numbered .sql files. Tell me the tool and the folder you found; if \
         there are several, ask me which.\n\
         2. Rails (db/migrate/*.rb), Django (<app>/migrations/*.py) and Alembic \
         (alembic/versions/*.py) migrations are code: romulus imports the SQL their tool \
         prints. Read the exporter from the resource romulus://export-migrations/rails.sh \
         (django.sh, alembic.sh) and ask me before running it: Django's and Rails' apply each \
         migration to an empty scratch database I give you, never a real one. Then import its \
         output as one file, with format rails, django or alembic.\n\
         3. whoami (my role must be developer or admin), then list_databases{org_text}: \
         {target}\n\
         4. Read every file of the folder yourself (romulus cannot read this disk): the SQL \
         files and the tool's own (migration_lock.toml, down files: romulus ignores what is \
         not a migration). Never send secrets (.env and the like). Send them with \
         import_migrations: each file's `path` relative to the repository, its `content` \
         exactly as on disk. Over about 3 MB in all, send them in several calls (commit: \
         false, then the `upload` it answers, commit: true on the last).\n\
         5. If it answers has_versions, tell me, and import with append: true only if I \
         agree. If it answers review_required, the organization reviews changes: import \
         nothing more; offer to write the new migrations as a draft instead (create_draft in \
         migration mode). If a migration fails, show me its file, line and Postgres's message; \
         change nothing in the repository without asking.\n\
         6. When it is done (get_import while it runs), summarize: the versions created, the \
         statements skipped in each file (data, settings…) and whether that matters, the \
         files ignored, and the link to the history."
    )
}

fn write_migration(draft: &str) -> String {
    format!(
        "Write the migration of the romulus draft {draft} into this repository.\n\
         1. get_draft {draft}: check that it compiles and is not stale (if it is, stop and \
         tell me). Before writing anything, tell me every statement of its migration flagged \
         DELETES DATA, CAN FAIL or WIDENS ACCESS.\n\
         2. Find how this repository migrates its database: the tool (Flyway, Prisma, sqlx, \
         dbmate, golang-migrate, plain SQL files; or Rails, Django, Alembic), its migrations \
         folder, and how the files there are named and numbered.\n\
         3. get_migration_files with draft {draft} and that tool's `format` (plain for Rails, \
         Django, Alembic or a folder of plain SQL files). Pass `version` when the repository \
         numbers its files its own way (Flyway's next V<n>, a golang-migrate sequence), so the \
         new files sort after the existing ones.\n\
         4. Write every file at its path under the migrations folder, with exactly the content \
         between its markers (the lines carrying the answer's marker code; any other line is \
         content), and check each file's length in bytes is the one its header gives; when the \
         answer is paged, call it again with its cursor until the last page. Refuse a path that \
         is not a plain relative path under the migrations folder (absolute, with `..`, into \
         another folder): write nothing then, and tell me. For Rails, Django or Alembic, write \
         a new migration of that tool around the SQL (without BEGIN and COMMIT), named like the \
         existing ones.\n\
         5. Do not run the migration against any database, and do not commit, unless I ask. \
         Show me the files written and what to review in them."
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
        assert_eq!(list()["prompts"].as_array().unwrap().len(), 5);
        let write = get("write_draft_migration", &json!({ "draft": "0193" })).unwrap();
        let text = write["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(
            text.contains("get_migration_files with draft 0193"),
            "{text}"
        );
        assert!(text.contains("length in bytes"), "{text}");
        assert!(text.contains("Refuse a path"), "{text}");
        let import = get(
            "import_migrations_folder",
            &json!({ "database": "shop", "folder": "db/migration" }),
        )
        .unwrap();
        let text = import["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("check that shop exists"), "{text}");
        assert!(text.contains("Start with db/migration."), "{text}");
        assert!(
            text.contains("romulus://export-migrations/rails.sh"),
            "{text}"
        );
        assert!(text.contains("review_required"), "{text}");
    }
}
