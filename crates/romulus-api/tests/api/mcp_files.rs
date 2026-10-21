//! Migration files over MCP: `import_migrations` and `get_import` (every
//! import format through an upload, chunks and parts, expiry, refusals that
//! keep the files, caps and roles, read-only tokens) and
//! `get_migration_files` (the web app's download, paged), with the prompts
//! and the exporters' resources.

use std::{fmt::Write as _, path::Path};

use axum::http::StatusCode;
use romulus_api::domain::AccessTokenScope;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    billing,
    harness::{Team, TestApp, database},
    live::version,
    mcp::{Mcp, audited, id_after, pat},
    migration_tests,
};

/// The fixture histories, one folder per format (`tests/sandbox/fixtures.rs`).
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../romulus-sandbox/tests/migrations"
);

/// Every file of the fixture history `name`, by its path in it, as an agent
/// reads a repository.
fn fixture(name: &str) -> Vec<(String, String)> {
    let root = Path::new(FIXTURES).join(name);
    let mut files = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("a fixture folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let relative = path.strip_prefix(&root).unwrap().to_string_lossy();
                files.push((
                    relative.replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

fn files_json(files: &[(String, String)]) -> Value {
    json!(
        files
            .iter()
            .map(|(path, content)| json!({ "path": path, "content": content }))
            .collect::<Vec<_>>()
    )
}

/// The database's imported versions: number, format, source.
async fn imported(db: &PgPool, database: Uuid) -> Vec<(i32, String, String)> {
    sqlx::query!(
        r#"SELECT v.number, m.format::text AS "format!", v.source AS "source!"
           FROM versions v JOIN version_migrations m ON m.version_id = v.id
           WHERE v.database_id = $1 ORDER BY v.number"#,
        database
    )
    .fetch_all(db)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.number, r.format, r.source))
    .collect()
}

/// `text` in three parts, cut between characters.
fn thirds(text: &str) -> [&str; 3] {
    let count = text.chars().count();
    let at = |k: usize| {
        text.char_indices()
            .nth(count * k / 3)
            .map_or(text.len(), |(at, _)| at)
    };
    let (a, b) = (at(1), at(2));
    [&text[..a], &text[a..b], &text[b..]]
}

async fn uploads(db: &PgPool) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM import_uploads"#)
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_format_imports_through_the_tool_detected_from_its_files(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    for format in [
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
    ] {
        let slug = format.replace('_', "-");
        let id = database(&db, team.org, &slug).await;
        let text = mcp
            .ok(
                "import_migrations",
                json!({ "database": slug, "files": files_json(&fixture(format)) }),
            )
            .await;
        assert!(text.contains(&format!("({format})")), "{format}: {text}");
        let versions = imported(&db, id).await;
        assert!(versions.len() > 1, "{format}: {versions:?}");
        assert!(
            versions.iter().all(|(_, f, _)| f == format),
            "{format}: {versions:?}"
        );
        assert!(
            text.contains(&format!("new as versions v1–v{}", versions.len())),
            "{format}: {text}"
        );
        assert!(text.contains("/app/acme/"), "the history's link: {text}");
    }
    // Files that are not migrations are said so.
    let upload = sqlx::query_scalar!(
        "SELECT u.id FROM import_uploads u JOIN databases d ON d.id = u.database_id
         WHERE d.slug = 'flyway'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let flyway = mcp
        .ok("get_import", json!({ "import": upload.to_string() }))
        .await;
    assert!(flyway.contains("Ignored files:"), "{flyway}");
    assert!(
        flyway.contains("R__issue_counts.sql (repeatable)"),
        "{flyway}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_chunked_upload_with_a_file_in_parts_imports_once_committed(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let whole = database(&db, team.org, "whole").await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let files = fixture("flyway");
    let (first, rest) = files.split_at(5);

    let started = mcp
        .ok(
            "import_migrations",
            json!({ "database": "shop", "files": files_json(first), "commit": false, "format": "flyway" }),
        )
        .await;
    assert!(started.contains("Nothing is imported yet"), "{started}");
    assert!(started.contains("5 files ("), "{started}");
    assert!(started.contains("they apply when it imports"), "{started}");
    let upload = id_after(&started, "Upload ").to_string();
    assert_eq!(imported(&db, shop).await, []);
    // The same chunk again (a retry): it replaces what it sent.
    let again = mcp
        .ok(
            "import_migrations",
            json!({ "upload": upload, "files": files_json(first), "commit": false }),
        )
        .await;
    assert!(again.contains("5 files ("), "{again}");

    // A file in three parts, sent out of order over several calls: until
    // part 2 arrives, it cannot be imported.
    let (path, content) = &rest[0];
    let parts = thirds(content);
    for part in [1, 3] {
        let sent = mcp
            .ok(
                "import_migrations",
                json!({ "upload": upload, "commit": false,
                        "files": [{ "path": path, "content": parts[part - 1], "part": part }] }),
            )
            .await;
        assert!(sent.contains("6 files ("), "{sent}");
    }
    let incomplete = mcp
        .refused(
            "import_migrations",
            json!({ "upload": upload, "format": "flyway" }),
        )
        .await;
    assert!(incomplete.contains("(incomplete_upload)"), "{incomplete}");
    assert!(incomplete.contains("part 2 is missing"), "{incomplete}");
    assert!(
        incomplete.contains("The files stay uploaded"),
        "{incomplete}"
    );
    let mut last = vec![json!({ "path": path, "content": parts[1], "part": 2 })];
    last.extend(
        rest[1..]
            .iter()
            .map(|(path, content)| json!({ "path": path, "content": content })),
    );
    let done = mcp
        .ok(
            "import_migrations",
            json!({ "upload": upload, "files": last, "format": "flyway" }),
        )
        .await;
    assert!(done.contains("(flyway)"), "{done}");
    assert!(done.contains("new as versions v1–v12"), "{done}");

    // The same files in one call make the same history.
    mcp.ok(
        "import_migrations",
        json!({ "database": "whole", "files": files_json(&files) }),
    )
    .await;
    assert_eq!(imported(&db, shop).await, imported(&db, whole).await);
    // Once imported, the upload is closed and its files gone.
    let closed = mcp
        .refused(
            "import_migrations",
            json!({ "upload": upload, "files": files_json(first) }),
        )
        .await;
    assert!(closed.contains("(upload_closed)"), "{closed}");
    let left: i64 = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM import_upload_files"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);

    // The Activity has each import, not the files staged before.
    let events = audited(&db).await;
    assert_eq!(events.len(), 2, "{events:?}");
    for event in &events {
        assert_eq!(event["tool"], "import_migrations");
        assert!(
            event["outcome"]
                .as_str()
                .unwrap()
                .starts_with("started importing"),
            "{event}"
        );
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_chunk_too_big_for_a_message_or_a_file_too_big_is_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "shop").await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    // Over the 4 MiB a message carries: refused before any tool runs.
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "import_migrations", "arguments": {
            "database": "shop",
            "files": [{ "path": "V1__a.sql", "content": "x".repeat(4 * 1024 * 1024) }],
        } },
    });
    let refused = mcp.post(&call, &[]).await;
    assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(refused.json()["code"], "payload_too_large");
    assert_eq!(uploads(&db).await, 0);

    // A file sent in parts, until it passes one file's 8 MiB.
    let part = "x".repeat(3_500_000);
    let started = mcp
        .ok(
            "import_migrations",
            json!({ "database": "shop", "commit": false,
                    "files": [{ "path": "V1__big.sql", "content": part, "part": 1 }] }),
        )
        .await;
    let upload = id_after(&started, "Upload ").to_string();
    mcp.ok(
        "import_migrations",
        json!({ "upload": upload, "commit": false,
                "files": [{ "path": "V1__big.sql", "content": part, "part": 2 }] }),
    )
    .await;
    let too_big = mcp
        .refused(
            "import_migrations",
            json!({ "upload": upload, "commit": false,
                    "files": [{ "path": "V1__big.sql", "content": part, "part": 3 }] }),
        )
        .await;
    assert!(too_big.contains("at most 8 MiB"), "{too_big}");
    // Paths must say where a file is in the repository, not on a disk.
    let absolute = mcp
        .refused(
            "import_migrations",
            json!({ "upload": upload, "commit": false,
                    "files": [{ "path": "/home/ada/app/V2__b.sql", "content": "" }] }),
        )
        .await;
    assert!(absolute.contains("is absolute"), "{absolute}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_expired_upload_is_refused_then_swept(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "shop").await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let started = mcp
        .ok(
            "import_migrations",
            json!({ "database": "shop", "commit": false,
                    "files": files_json(&fixture("sqlx")) }),
        )
        .await;
    let upload = id_after(&started, "Upload ");
    sqlx::query!(
        "UPDATE import_uploads SET created_at = now() - interval '2 hours',
                expires_at = now() - interval '1 minute' WHERE id = $1",
        upload
    )
    .execute(&db)
    .await
    .unwrap();

    let commit = mcp
        .refused("import_migrations", json!({ "upload": upload.to_string() }))
        .await;
    assert!(commit.contains("(upload_expired)"), "{commit}");
    assert!(commit.contains("Send every file again"), "{commit}");
    let more = mcp
        .refused(
            "import_migrations",
            json!({ "upload": upload.to_string(), "commit": false,
                    "files": [{ "path": "x.sql", "content": "" }] }),
        )
        .await;
    assert!(more.contains("(upload_expired)"), "{more}");
    let read = mcp
        .ok("get_import", json!({ "import": upload.to_string() }))
        .await;
    assert!(read.contains("expired"), "{read}");

    // Swept: the files at once, the upload a day later.
    romulus_api::imports::uploads::sweep(&db).await.unwrap();
    let files: i64 = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM import_upload_files"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(files, 0);
    assert_eq!(uploads(&db).await, 1);
    sqlx::query!("UPDATE import_uploads SET expires_at = now() - interval '2 days' + interval '1 second', created_at = now() - interval '3 days'")
        .execute(&db)
        .await
        .unwrap();
    romulus_api::imports::uploads::sweep(&db).await.unwrap();
    assert_eq!(uploads(&db).await, 0);
    let gone = mcp
        .refused("get_import", json!({ "import": upload.to_string() }))
        .await;
    assert!(gone.contains("No upload or import"), "{gone}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn data_statements_are_skipped_and_reported_and_failures_keep_the_files(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let mut files = fixture("sqlx");
    // The fake sandbox stops at `oops`: the last migration fails.
    files.push((
        "migrations/20240401000000_broken.sql".into(),
        "CREATE TABLE oops (id int);\n".into(),
    ));

    let failed = mcp
        .ok(
            "import_migrations",
            json!({ "database": "shop", "files": files_json(&files) }),
        )
        .await;
    assert!(failed.contains("stopped:"), "{failed}");
    assert!(failed.contains("(import_failed)"), "{failed}");
    assert!(
        failed.contains("In 20240401000000_broken.sql:\n- line 1: syntax error"),
        "{failed}"
    );
    assert!(failed.contains("The files stay uploaded"), "{failed}");
    assert_eq!(imported(&db, shop).await, []);
    let upload = id_after(&failed, "Import ").to_string();

    // The broken file fixed (sent again under its path), then imported.
    let fixed = mcp
        .ok(
            "import_migrations",
            json!({ "upload": upload, "commit": false, "files": [{
                "path": "migrations/20240401000000_broken.sql",
                "content": "CREATE TABLE fine (id int);\n",
            }] }),
        )
        .await;
    assert!(fixed.contains("received so far"), "{fixed}");
    let done = mcp
        .ok("import_migrations", json!({ "upload": upload }))
        .await;
    assert!(done.contains("new as versions v1–v13"), "{done}");
    assert!(
        done.contains("Skipped statements (reported, not replayed"),
        "{done}"
    );
    assert!(
        done.contains(
            "20240126100900_seed_acme.sql line 2: INSERT (data): INSERT INTO organizations"
        ),
        "{done}"
    );
    // get_import says the same, later.
    let read = mcp.ok("get_import", json!({ "import": upload })).await;
    assert!(read.contains("INSERT (data)"), "{read}");
    assert!(read.contains("new as versions v1–v13"), "{read}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_with_other_versions_takes_the_files_once_appending(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(
        &db,
        shop,
        1,
        "CREATE TABLE public.legacy (id int);\n",
        json!({}),
    )
    .await;
    let app = TestApp::compiling(db.clone());
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let refused = mcp
        .refused(
            "import_migrations",
            json!({ "database": "shop", "files": files_json(&fixture("dbmate")) }),
        )
        .await;
    assert!(refused.contains("(has_versions)"), "{refused}");
    assert!(
        refused.contains("Ask your person before appending"),
        "{refused}"
    );
    let upload = refused
        .split("upload=")
        .nth(1)
        .and_then(|rest| rest.get(..36))
        .expect("the upload's id")
        .to_owned();
    // A dry run first: nothing stored, the files kept.
    let checked = mcp
        .ok(
            "import_migrations",
            json!({ "upload": upload, "append": true, "dry_run": true }),
        )
        .await;
    assert!(checked.contains("Dry run of import"), "{checked}");
    assert!(checked.contains("nothing was stored"), "{checked}");
    assert_eq!(imported(&db, shop).await, []);
    let appended = mcp
        .ok(
            "import_migrations",
            json!({ "upload": upload, "append": true }),
        )
        .await;
    assert!(appended.contains("new as versions v2–v13"), "{appended}");
    // A dry run is not in the Activity; the import is.
    assert_eq!(audited(&db).await.len(), 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn plan_limits_and_roles_hold_for_imports(db: PgPool) {
    // Hosted billing on, acme on the Free plan: 50 versions per database.
    let billed = billing::billed(&db).await;
    let team = &billed.team;
    let shop = database(&db, team.org, "shop").await;
    let files: Vec<(String, String)> = (1..=51)
        .map(|k| {
            (
                format!("migrations/2024010100{k:04}_t{k}.sql"),
                format!("CREATE TABLE public.t{k} (id int);\n"),
            )
        })
        .collect();
    let developer = Mcp::new(
        &billed.app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let capped = developer
        .refused(
            "import_migrations",
            json!({ "database": "shop", "files": files_json(&files) }),
        )
        .await;
    assert!(capped.contains("(plan_limit)"), "{capped}");
    assert!(capped.contains("Billing page"), "{capped}");
    assert!(capped.contains("The files stay uploaded"), "{capped}");
    assert_eq!(imported(&db, shop).await, []);
    let upload = capped
        .split("upload=")
        .nth(1)
        .and_then(|rest| rest.get(..36))
        .expect("the upload's id")
        .to_owned();

    // Reviewers read; they do not import.
    let reviewer = Mcp::new(
        &billed.app,
        pat(&db, &team.reviewer, AccessTokenScope::Write).await,
    );
    let forbidden = reviewer
        .refused(
            "import_migrations",
            json!({ "database": "shop", "files": files_json(&files[..1]) }),
        )
        .await;
    assert!(forbidden.contains("(forbidden)"), "{forbidden}");
    // Someone else's upload is not theirs to read or commit.
    let other = reviewer
        .refused("get_import", json!({ "import": upload }))
        .await;
    assert!(other.contains("No upload or import"), "{other}");
    let admin = Mcp::new(
        &billed.app,
        pat(&db, &team.admin, AccessTokenScope::Write).await,
    );
    let not_theirs = admin
        .refused("import_migrations", json!({ "upload": upload }))
        .await;
    assert!(not_theirs.contains("No upload or import"), "{not_theirs}");
    // Outsiders do not see the database.
    let outsider = Mcp::new(
        &billed.app,
        pat(&db, &team.outsider, AccessTokenScope::Write).await,
    );
    let unknown = outsider
        .refused(
            "import_migrations",
            json!({ "org": "acme", "database": "shop", "files": files_json(&files[..1]) }),
        )
        .await;
    assert!(unknown.contains("(not_found)"), "{unknown}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn read_only_tokens_read_files_and_imports_but_never_import(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let reader = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Read).await,
    );
    let tools = reader.rpc("tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"import_migrations"), "{names:?}");
    assert!(names.contains(&"get_import"), "{names:?}");
    assert!(names.contains(&"get_migration_files"), "{names:?}");
    let refused = reader
        .refused(
            "import_migrations",
            json!({ "database": "shop", "files": [{ "path": "V1__a.sql", "content": "" }] }),
        )
        .await;
    assert!(refused.contains("(read_only_token)"), "{refused}");
    assert_eq!(uploads(&db).await, 0);
    let files = reader
        .ok(
            "get_migration_files",
            json!({ "database": "shop", "from": 1, "to": 2, "format": "flyway" }),
        )
        .await;
    assert!(files.contains("=== file 1/1: V2__"), "{files}");

    // The prompts and the exporters an import of code migrations needs.
    let prompts = reader.rpc("prompts/list", json!({})).await;
    let prompt_names: Vec<&str> = prompts["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        prompt_names.contains(&"import_migrations_folder")
            && prompt_names.contains(&"write_draft_migration"),
        "{prompt_names:?}"
    );
    let prompt = reader
        .rpc(
            "prompts/get",
            json!({ "name": "write_draft_migration", "arguments": { "draft": "0193" } }),
        )
        .await;
    assert!(
        prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("get_migration_files with draft 0193")
    );
    let resources = reader.rpc("resources/list", json!({})).await;
    assert!(
        resources["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "romulus://export-migrations/rails.sh"),
        "{resources}"
    );
    let script = reader
        .rpc(
            "resources/read",
            json!({ "uri": "romulus://export-migrations/rails.sh" }),
        )
        .await;
    assert_eq!(
        script["result"]["contents"][0]["text"],
        include_str!("../../../../scripts/export-migrations/rails.sh")
    );
}

/// The files in a `get_migration_files` answer, parts joined: (path,
/// content), in order.
fn files_in(answers: &[String]) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for text in answers {
        let mut rest = text.as_str();
        while let Some(at) = rest.find("\n=== file ") {
            let header_end = rest[at + 1..].find('\n').unwrap() + at + 1;
            let header = &rest[at + 1 + "=== file ".len()..header_end];
            let header = header.strip_suffix(" ===").expect("a header");
            let (number, described) = header.split_once(": ").unwrap();
            let path = described.split(", part ").next().unwrap();
            let marker = format!("=== end of file {number} ===\n");
            let body = &rest[header_end + 1..];
            let end = body.find(&marker).expect("the file's end");
            let content = &body[..end];
            let later_part = described.contains(", part ") && !described.contains("part 1 of");
            match files.last_mut() {
                Some((last, joined)) if last == path && later_part => joined.push_str(content),
                _ => files.push((path.to_owned(), content.to_owned())),
            }
            rest = &body[end + marker.len() - 1..];
        }
    }
    files
}

fn web_files(migration: &Value) -> Vec<(String, String)> {
    migration["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["content"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn migration_files_are_the_web_download(db: PgPool) {
    let (app, team, _host) = migration_tests::setup(&db).await;
    let mcp = Mcp::new(
        &app,
        pat(&db, &team.developer, AccessTokenScope::Write).await,
    );
    let web = app.as_user(&team.developer);
    for format in [
        "plain",
        "sqlx",
        "flyway",
        "prisma",
        "dbmate",
        "golang_migrate",
    ] {
        let version = if format == "flyway" { "7.1" } else { "000042" };
        let text = mcp
            .ok(
                "get_migration_files",
                json!({ "database": "shop", "from": 1, "to": 2, "format": format, "version": version }),
            )
            .await;
        let download = web
            .get(&team.path(&format!(
                "/databases/shop/migration?from=1&to=2&format={format}&version={version}"
            )))
            .await
            .json();
        assert_eq!(
            files_in(std::slice::from_ref(&text)),
            web_files(&download),
            "{format}: {text}"
        );
        assert!(text.contains("Where they go:"), "{text}");
        assert!(text.contains("That is every file."), "{text}");
    }
    // Without a version, every file is named for the time of the first page.
    let sqlx = mcp
        .ok(
            "get_migration_files",
            json!({ "database": "shop", "from": 1, "to": 2, "format": "sqlx" }),
        )
        .await;
    let files = files_in(std::slice::from_ref(&sqlx));
    assert_eq!(files.len(), 1, "{sqlx}");
    assert!(
        files[0].0.as_bytes()[..14].iter().all(u8::is_ascii_digit),
        "{sqlx}"
    );
    assert_eq!(files[0].0.as_bytes()[14], b'_', "{sqlx}");
    assert!(sqlx.contains("migrations/ next to Cargo.toml"), "{sqlx}");

    // A draft's: from its base to the text it has, as reviewed.
    let created = mcp
        .ok(
            "create_draft",
            json!({ "database": "shop", "name": "Add refunds",
                    "edits": [{ "old": "", "new": "CREATE TABLE public.refunds (id bigint PRIMARY KEY);\n" }] }),
        )
        .await;
    let draft = id_after(&created, "Created draft ");
    let text = mcp
        .ok(
            "get_migration_files",
            json!({ "draft": draft.to_string(), "format": "prisma", "version": "20261021093000" }),
        )
        .await;
    let reviewed = web
        .get(&team.path(&format!("/databases/shop/drafts/{draft}/text?wait=true")))
        .await
        .json()["reviewed_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let download = web
        .get(&team.path(&format!(
            "/databases/shop/drafts/{draft}/migration?format=prisma&version=20261021093000&sha256={reviewed}"
        )))
        .await
        .json();
    assert_eq!(
        files_in(std::slice::from_ref(&text)),
        web_files(&download),
        "{text}"
    );
    assert!(
        text.contains("=== file 1/1: 20261021093000_add_refunds/migration.sql ==="),
        "{text}"
    );
    assert!(text.contains("prisma/migrations/"), "{text}");

    // Longer than a page: pages, each with the cursor to the next, that
    // joined are the download.
    let big = (0..3000).fold(String::new(), |mut sql, k| {
        writeln!(
            sql,
            "CREATE TABLE public.a_table_with_a_rather_long_name_{k:05} (id int);"
        )
        .unwrap();
        sql
    });
    let shop = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'shop'")
        .fetch_one(&db)
        .await
        .unwrap();
    version(&db, shop, 3, &big, json!({})).await;
    let mut pages = vec![
        mcp.ok(
            "get_migration_files",
            json!({ "database": "shop", "from": 2, "to": 3, "format": "golang_migrate" }),
        )
        .await,
    ];
    while let Some(cursor) = pages
        .last()
        .unwrap()
        .split("with only cursor=")
        .nth(1)
        .map(|rest| rest.trim().to_owned())
    {
        assert!(pages.len() < 20, "pages never end");
        pages.push(
            mcp.ok("get_migration_files", json!({ "cursor": cursor }))
                .await,
        );
    }
    assert!(pages.len() > 2, "{} pages", pages.len());
    assert!(
        pages[0].contains(&format!("Page 1 of {}", pages.len())),
        "{}",
        pages[0]
    );
    assert!(pages[1].contains("part 2 of"), "{}", pages[1]);
    // The pages named their files with the first page's time.
    let joined = files_in(&pages);
    let version = joined[0].0.split('_').next().unwrap().to_owned();
    let download = web
        .get(&team.path(&format!(
            "/databases/shop/migration?from=2&to=3&format=golang_migrate&version={version}"
        )))
        .await
        .json();
    assert_eq!(joined, web_files(&download));
    let bad = mcp
        .refused("get_migration_files", json!({ "cursor": "nope" }))
        .await;
    assert!(bad.contains("start again from the first page"), "{bad}");
}
