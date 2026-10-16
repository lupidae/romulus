//! Migration imports: the preview, the import from the web and from CI,
//! idempotency, appending, refusals, failures that store nothing, the
//! streamed progress. The fake compiler stands in for the sandbox
//! (`CREATE TABLE x` makes a table, `oops` is a syntax error).

use std::io::Write;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use romulus_api::{csrf, domain::TokenScope};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::harness::{
    ORIGIN, Session, Team, TestApp, TestResponse, api_token, database, model_json,
};

const BOUNDARY: &str = "romulus-test-boundary";

/// A multipart body of files named by their paths.
fn multipart(files: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (path, text) in files {
        write!(
            body,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{path}\"\r\nContent-Type: application/sql\r\n\r\n{text}\r\n"
        )
        .unwrap();
    }
    write!(body, "--{BOUNDARY}--\r\n").unwrap();
    body
}

fn zip(files: &[(&str, &str)]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    for (path, text) in files {
        zip.start_file(*path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    out.into_inner()
}

/// An org-scoped request from the web app, as `session`.
async fn web(
    app: &TestApp,
    session: &Session,
    path: &str,
    files: &[(&str, &str)],
    accept: Option<&str>,
) -> TestResponse {
    let mut request = Request::post(path)
        .header(header::ORIGIN, ORIGIN)
        .header(csrf::HEADER, "1")
        .header(header::COOKIE, session.cookie())
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        );
    if let Some(accept) = accept {
        request = request.header(header::ACCEPT, accept);
    }
    app.send(request.body(Body::from(multipart(files))).unwrap())
        .await
}

/// A CI request with an API token.
async fn ci(
    app: &TestApp,
    database: Uuid,
    token: &str,
    query: &str,
    content_type: &str,
    body: Vec<u8>,
) -> TestResponse {
    app.send(
        Request::post(format!("/api/databases/{database}/imports{query}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

const HISTORY: &[(&str, &str)] = &[
    (
        "migrations/20240101000000_users.sql",
        "CREATE TABLE users (id bigint);\n",
    ),
    (
        "migrations/20240102000000_seed.sql",
        "BEGIN;\nINSERT INTO users VALUES (1);\nCOMMIT;\n",
    ),
    (
        "migrations/20240103000000_posts.sql",
        "CREATE TABLE posts (id bigint);\nCREATE INDEX CONCURRENTLY posts_id ON posts (id);\n",
    ),
    ("migrations/README.md", "# How we migrate"),
];

async fn versions(db: &PgPool, database: Uuid) -> Vec<(i32, String, String, Option<String>)> {
    sqlx::query!(
        r#"SELECT number, origin::text AS "origin!", source_kind::text AS "kind!", message
           FROM versions WHERE database_id = $1 ORDER BY number"#,
        database
    )
    .fetch_all(db)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.number, r.origin, r.kind, r.message))
    .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_developer_imports_a_folder_as_one_version_per_migration(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());

    let imported = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        HISTORY,
        None,
    )
    .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    let report = imported.json();
    assert_eq!(report["status"], "created");
    assert_eq!(report["format"], "sqlx");
    assert_eq!(report["created"], 3);
    assert_eq!(report["ignored"][0]["path"], "README.md");
    assert_eq!(report["migrations"][0]["version"], 1);
    assert_eq!(report["migrations"][0]["changes"], "1 table added");
    assert_eq!(report["migrations"][1]["changes"], "no changes");
    let skipped: Vec<&str> = report["migrations"][1]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert_eq!(skipped, ["BEGIN", "INSERT", "COMMIT"]);
    assert_eq!(report["migrations"][1]["skipped"][1]["line"], 2);
    assert_eq!(report["migrations"][1]["skipped"][1]["reason"], "data");

    assert_eq!(
        versions(&db, id).await,
        [
            (
                1,
                "import".into(),
                "migration".into(),
                Some("20240101000000_users.sql".into())
            ),
            (
                2,
                "import".into(),
                "migration".into(),
                Some("20240102000000_seed.sql".into())
            ),
            (
                3,
                "import".into(),
                "migration".into(),
                Some("20240103000000_posts.sql".into())
            ),
        ]
    );

    // A version keeps its migration: the file, its source, what it skipped.
    let v2 = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/app/versions/2"))
        .await
        .json();
    assert_eq!(v2["origin"], "import");
    assert_eq!(v2["source"], HISTORY[1].1);
    assert_eq!(v2["author"]["kind"], "user");
    assert_eq!(v2["migration"]["id"], "20240102000000");
    assert_eq!(v2["migration"]["path"], "20240102000000_seed.sql");
    assert_eq!(v2["migration"]["format"], "sqlx");
    assert_eq!(v2["migration"]["skipped"][1]["kind"], "INSERT");
    // Its SQL is the schema, not the migration.
    let texts = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/app/versions/3/texts"))
        .await
        .json();
    assert!(
        texts["sql"].as_str().unwrap().contains("CREATE TABLE"),
        "{texts}"
    );
    assert!(!texts["sql"].as_str().unwrap().contains("CONCURRENTLY"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_preview_plans_without_running_or_storing(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());

    let preview = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports/preview"),
        HISTORY,
        None,
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json());
    let preview = preview.json();
    assert_eq!(preview["format"], "sqlx");
    assert_eq!(preview["detected"], "sqlx");
    assert_eq!(preview["new"], 3);
    assert_eq!(preview["first_number"], 1);
    assert_eq!(preview["needs_append"], false);
    assert_eq!(preview["problem"], Value::Null);
    let statuses: Vec<&str> = preview["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["new", "new", "new"]);
    assert_eq!(
        preview["migrations"][1]["skipped"][1]["text"],
        "INSERT INTO users VALUES (1)"
    );
    assert_eq!(versions(&db, id).await, []);

    // Another format, asked for.
    let plain = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports/preview?format=plain"),
        HISTORY,
        None,
    )
    .await
    .json();
    assert_eq!(plain["format"], "plain");
    assert_eq!(plain["detected"], "sqlx");
    assert_eq!(plain["migrations"][0]["id"], "20240101000000_users");

    // A syntax error is a problem before anything runs.
    let broken = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports/preview"),
        &[
            ("1_a.sql", "CREATE TABLE a (id int);"),
            ("2_b.sql", "\n\nCREATE TABLE (;"),
        ],
        None,
    )
    .await
    .json();
    assert_eq!(broken["problem"]["code"], "import_failed");
    assert_eq!(broken["migrations"][1]["errors"][0]["line"], 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn importing_again_creates_nothing_and_new_migrations_append(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    let path = team.path("/databases/app/imports");

    let first = web(&app, &team.developer, &path, HISTORY, None).await;
    assert_eq!(first.status, StatusCode::CREATED);

    let again = web(&app, &team.developer, &path, HISTORY, None).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json());
    assert_eq!(again.json()["status"], "unchanged");
    assert_eq!(again.json()["created"], 0);
    assert_eq!(again.json()["migrations"][2]["status"], "imported");
    assert_eq!(again.json()["migrations"][2]["version"], 3);
    assert_eq!(versions(&db, id).await.len(), 3);

    // One more migration: only it is added, after the others.
    let mut more = HISTORY.to_vec();
    more.push((
        "migrations/20240104000000_tags.sql",
        "CREATE TABLE tags (id bigint);",
    ));
    let appended = web(&app, &team.developer, &path, &more, None).await;
    assert_eq!(appended.status, StatusCode::CREATED, "{}", appended.json());
    assert_eq!(appended.json()["created"], 1);
    assert_eq!(appended.json()["migrations"][3]["version"], 4);
    assert_eq!(appended.json()["migrations"][3]["changes"], "1 table added");
    // The new version's model holds the whole history.
    let v4 = app
        .as_user(&team.developer)
        .get(&team.path("/databases/app/versions/4"))
        .await
        .json();
    let mut tables: Vec<&str> = v4["model"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    tables.sort_unstable();
    assert_eq!(tables, ["posts", "tags", "users"]);

    // Only the new file is enough too: the rest is replayed from the database.
    let alone = web(
        &app,
        &team.developer,
        &path,
        &[(
            "20240105000000_labels.sql",
            "CREATE TABLE labels (id bigint);",
        )],
        None,
    )
    .await;
    assert_eq!(alone.status, StatusCode::CREATED, "{}", alone.json());
    assert_eq!(versions(&db, id).await.len(), 5);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn changed_and_out_of_order_migrations_are_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    let path = team.path("/databases/app/imports");
    web(&app, &team.developer, &path, &HISTORY[2..3], None).await;

    let mut edited = HISTORY[2..3].to_vec();
    edited[0].1 = "CREATE TABLE posts (id bigint, title text);";
    let changed = web(&app, &team.developer, &path, &edited, None).await;
    assert_eq!(changed.status, StatusCode::CONFLICT);
    assert_eq!(changed.code(), "migration_changed");
    assert!(changed.json()["message"].as_str().unwrap().contains("v1"));

    let late = web(&app, &team.developer, &path, HISTORY, None).await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(late.code(), "migration_out_of_order");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_with_other_versions_imports_only_when_appending(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    let token = api_token(&db, id, TokenScope::Push).await;
    let pushed = app
        .push(
            id,
            &token,
            Some("application/json"),
            model_json(&["legacy"]),
        )
        .await;
    assert_eq!(pushed.status, StatusCode::CREATED);

    let path = team.path("/databases/app/imports");
    let refused = web(&app, &team.developer, &path, HISTORY, None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "has_versions");
    let preview = web(
        &app,
        &team.developer,
        &format!("{path}/preview"),
        HISTORY,
        None,
    )
    .await
    .json();
    assert_eq!(preview["needs_append"], true);
    assert_eq!(preview["first_number"], 2);

    let appended = web(
        &app,
        &team.developer,
        &format!("{path}?append=true"),
        HISTORY,
        None,
    )
    .await;
    assert_eq!(appended.status, StatusCode::CREATED, "{}", appended.json());
    let numbers: Vec<(i32, String)> = versions(&db, id)
        .await
        .into_iter()
        .map(|(n, origin, _, _)| (n, origin))
        .collect();
    assert_eq!(
        numbers,
        [
            (1, "push".into()),
            (2, "import".into()),
            (3, "import".into()),
            (4, "import".into())
        ]
    );
    // Compared with the version before it (the push), the first one changes.
    assert_eq!(
        appended.json()["migrations"][0]["changes"],
        "1 table added, 1 removed"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_failing_migration_stops_the_import_and_stores_nothing(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());

    let failed = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        &[
            ("1_users.sql", "CREATE TABLE users (id int);"),
            (
                "2_posts.sql",
                "CREATE TABLE posts (id int);\n\nALTER TABLE posts oops;",
            ),
            ("3_tags.sql", "CREATE TABLE tags (id int);"),
        ],
        None,
    )
    .await;
    assert_eq!(
        failed.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        failed.json()
    );
    assert_eq!(failed.code(), "import_failed");
    let details = &failed.json()["details"];
    assert_eq!(details["index"], 1);
    assert_eq!(details["path"], "2_posts.sql");
    assert_eq!(details["errors"][0]["line"], 3);
    assert_eq!(details["errors"][0]["column"], 19);
    assert!(
        failed.json()["message"]
            .as_str()
            .unwrap()
            .starts_with("2_posts.sql:3: ")
    );
    assert_eq!(versions(&db, id).await, []);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reviewers_and_outsiders_cannot_import(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    for path in ["/databases/app/imports", "/databases/app/imports/preview"] {
        let reviewer = web(&app, &team.reviewer, &team.path(path), HISTORY, None).await;
        assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
        let outsider = web(&app, &team.outsider, &team.path(path), HISTORY, None).await;
        assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    }
    let anonymous = app
        .send(
            Request::post(team.path("/databases/app/imports"))
                .header(header::ORIGIN, ORIGIN)
                .header(csrf::HEADER, "1")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(Body::from(multipart(HISTORY)))
                .unwrap(),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn ci_imports_a_zip_with_its_token(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let other = database(&db, team.org, "other").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());
    let archive = zip(&[
        (
            "prisma/migrations/migration_lock.toml",
            "provider = \"postgresql\"",
        ),
        (
            "prisma/migrations/20240101000000_init/migration.sql",
            "-- CreateTable\nCREATE TABLE \"User\" (id int);",
        ),
        (
            "prisma/migrations/20240102000000_post/migration.sql",
            "-- CreateTable\nCREATE TABLE \"Post\" (id int);",
        ),
    ]);

    let imported = ci(&app, id, &token, "", "application/zip", archive.clone()).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    assert_eq!(imported.json()["format"], "prisma");
    assert_eq!(
        imported.json()["migrations"][0]["name"],
        "20240101000000_init"
    );
    let v1 = app
        .get_with_token(
            &format!("/api/databases/{id}/versions/1"),
            &api_token(&db, id, TokenScope::Full).await,
        )
        .await
        .json();
    assert_eq!(v1["author"]["kind"], "token");
    assert_eq!(v1["origin"], "import");

    // Idempotent from CI too.
    let again = ci(&app, id, &token, "", "application/zip", archive.clone()).await;
    assert_eq!(again.json()["status"], "unchanged");

    // Another database's token, or none.
    let wrong = ci(&app, other, &token, "", "application/zip", archive.clone()).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.code(), "wrong_database");
    let none = app
        .send(
            Request::post(format!("/api/databases/{id}/imports"))
                .header(header::CONTENT_TYPE, "application/zip")
                .body(Body::from(archive))
                .unwrap(),
        )
        .await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(none.code(), "invalid_token");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_exported_file_imports_as_sql_and_a_dry_run_stores_nothing(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());
    let alembic = "BEGIN;\n\nCREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\n-- Running upgrade  -> 1975ea83b712, create users\n\nCREATE TABLE users (id int);\n\nINSERT INTO alembic_version (version_num) VALUES ('1975ea83b712') RETURNING alembic_version.version_num;\n\n-- Running upgrade 1975ea83b712 -> ae1027a6acf0, create posts\n\nCREATE TABLE posts (id int);\n\nUPDATE alembic_version SET version_num='ae1027a6acf0' WHERE alembic_version.version_num = '1975ea83b712';\n\nCOMMIT;\n";

    let checked = ci(
        &app,
        id,
        &token,
        "?dry_run=true&filename=alembic.sql",
        "application/sql",
        alembic.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(checked.status, StatusCode::OK, "{}", checked.json());
    assert_eq!(checked.json()["status"], "checked");
    assert_eq!(checked.json()["format"], "alembic");
    assert_eq!(checked.json()["migrations"][1]["changes"], "1 table added");
    assert_eq!(checked.json()["migrations"][1]["version"], Value::Null);
    assert_eq!(versions(&db, id).await, []);

    let imported = ci(
        &app,
        id,
        &token,
        "?filename=alembic.sql",
        "application/sql",
        alembic.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    assert_eq!(
        versions(&db, id)
            .await
            .iter()
            .map(|v| v.3.clone().unwrap())
            .collect::<Vec<_>>(),
        ["1975ea83b712 create users", "ae1027a6acf0 create posts"]
    );
    // The bookkeeping of Alembic's version table is not the project's data.
    assert_eq!(
        imported.json()["migrations"][0]["skipped"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_progress_streams_one_event_per_line(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());

    let streamed = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        HISTORY,
        Some("application/x-ndjson"),
    )
    .await;
    assert_eq!(streamed.status, StatusCode::OK);
    assert_eq!(
        streamed.headers[header::CONTENT_TYPE],
        "application/x-ndjson"
    );
    let events: Vec<Value> = std::str::from_utf8(&streamed.body)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "planned", "checked", "replayed", "replayed", "replayed", "done"
        ]
    );
    assert_eq!(events[0]["preview"]["new"], 3);
    assert_eq!(events[2]["index"], 0);
    assert_eq!(events[4]["number"], 3);
    assert_eq!(events[5]["report"]["created"], 3);

    // A failure in the sandbox is the last line, and nothing is stored.
    // (What fails before anything runs, such as a syntax error, is an
    // ordinary error answer.)
    let failed = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        &[("20240109000000_bad.sql", "CREATE TABLE oops (id int);")],
        Some("application/x-ndjson"),
    )
    .await;
    assert_eq!(failed.status, StatusCode::OK);
    let last: Value = serde_json::from_str(
        std::str::from_utf8(&failed.body)
            .unwrap()
            .lines()
            .rfind(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["event"], "failed");
    assert_eq!(last["error"]["code"], "import_failed");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn bodies_that_are_not_migrations_are_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());

    let image = ci(&app, id, &token, "", "image/png", b"\x89PNG".to_vec()).await;
    assert_eq!(image.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let nothing = ci(
        &app,
        id,
        &token,
        "",
        "application/zip",
        zip(&[("notes.txt", "hello")]),
    )
    .await;
    assert_eq!(nothing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(nothing.code(), "invalid_migrations");
    let broken = ci(
        &app,
        id,
        &token,
        "",
        "application/zip",
        b"PK\x03\x04garbage".to_vec(),
    )
    .await;
    assert_eq!(broken.code(), "invalid_file");
    let latin1 = ci(
        &app,
        id,
        &token,
        "?filename=x.sql",
        "application/sql",
        b"CREATE TABLE caf\xe9 ();".to_vec(),
    )
    .await;
    assert_eq!(latin1.code(), "invalid_file");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_imported_migration_is_as_immutable_as_its_version(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        HISTORY,
        None,
    )
    .await;
    let err = sqlx::query!(
        "UPDATE version_migrations SET path = 'elsewhere.sql' WHERE database_id = $1",
        id
    )
    .execute(&db)
    .await
    .unwrap_err();
    assert_eq!(
        romulus_api::db::constraint(&err),
        Some("versions_are_immutable")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_zip_dropped_in_the_browser_is_unpacked(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    let archive = zip(&[
        (
            "db/migrations/V1__users.sql",
            "CREATE TABLE users (id int);",
        ),
        (
            "db/migrations/V2__posts.sql",
            "CREATE TABLE posts (id int);",
        ),
        ("db/migrations/R__views.sql", "CREATE VIEW v AS SELECT 1;"),
    ]);
    let mut body = Vec::new();
    write!(
        body,
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"migrations.zip\"\r\nContent-Type: application/zip\r\n\r\n"
    )
    .unwrap();
    body.extend_from_slice(&archive);
    write!(body, "\r\n--{BOUNDARY}--\r\n").unwrap();
    let imported = app
        .send(
            Request::post(team.path("/databases/app/imports"))
                .header(header::ORIGIN, ORIGIN)
                .header(csrf::HEADER, "1")
                .header(header::COOKIE, team.developer.cookie())
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
    assert_eq!(imported.json()["format"], "flyway");
    assert_eq!(imported.json()["ignored"][0]["reason"], "repeatable");
    assert_eq!(versions(&db, id).await.len(), 2);
}

/// Many tiny zips that each inflate to 8 MiB: refused at the request's
/// budget (64 MiB unpacked), never inflated past it.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn zip_bombs_spread_over_many_zips_are_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());
    let spaces = " ".repeat(8 << 20);
    let bomb = zip(&[("a.sql", spaces.as_str())]);
    let mut body = Vec::new();
    for k in 0..500 {
        write!(
            body,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"m{k}.zip\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .unwrap();
        body.extend_from_slice(&bomb);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{BOUNDARY}--\r\n").unwrap();
    let started = std::time::Instant::now();
    let refused = ci(
        &app,
        id,
        &token,
        "",
        &format!("multipart/form-data; boundary={BOUNDARY}"),
        body,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "validation");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(versions(&db, id).await, []);
}

/// What the database stores with limits (ids, paths, names) is checked
/// before anything replays, in the preview and the import.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn names_too_long_to_store_are_refused_before_the_replay(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());

    let long_name = format!("{}.sql", "a".repeat(501));
    let files = [(long_name.as_str(), "CREATE TABLE a (id int);")];
    let preview = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports/preview"),
        &files,
        None,
    )
    .await
    .json();
    assert_eq!(preview["problem"]["code"], "invalid_migrations");
    let message = preview["problem"]["message"].as_str().unwrap();
    assert!(
        message.contains("migration id is longer than 500 characters"),
        "{message}"
    );
    let refused = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        &files,
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "invalid_migrations");

    // A deep path, a short id.
    let deep = format!("{}/1_a.sql", "d".repeat(1000));
    let refused = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        &[
            (deep.as_str(), "CREATE TABLE a (id int);"),
            ("elsewhere/2_b.sql", "CREATE TABLE b (id int);"),
        ],
        None,
    )
    .await;
    assert_eq!(refused.code(), "invalid_migrations");
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("path is longer than 1000 characters")
    );

    // A single SQL text named by `filename`.
    let refused = ci(
        &app,
        id,
        &token,
        &format!("?filename={}.sql", "b".repeat(600)),
        "application/sql",
        b"CREATE TABLE b (id int);".to_vec(),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "invalid_migrations");
    assert_eq!(versions(&db, id).await, []);
}

/// Previews read and parse everything sent: limited per database, apart
/// from the imports themselves.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn previews_are_rate_limited_apart_from_imports(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    let path = team.path("/databases/app/imports/preview");
    let preview = || web(&app, &team.developer, &path, HISTORY, None);
    for _ in 0..6 {
        assert_eq!(preview().await.status, StatusCode::OK);
    }
    let limited = preview().await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "rate_limited");
    let imported = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        HISTORY,
        None,
    )
    .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json());
}

/// One import replays at a time: another one waits for its turn, and its
/// progress says so.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_import_waits_for_its_turn_and_says_so(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    // Another organization's import has the turn.
    let held = app.state.imports.take(Uuid::from_u128(7)).ok();
    let turns = &app.state.imports;
    let release = async move {
        while turns.waiting() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(held);
    };
    let path = team.path("/databases/app/imports");
    let request = web(
        &app,
        &team.developer,
        &path,
        HISTORY,
        Some("application/x-ndjson"),
    );
    let (streamed, ()) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(request, release)
    })
    .await
    .expect("the import ran once the turn was free");
    let kinds: Vec<String> = std::str::from_utf8(&streamed.body)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["event"].to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "\"planned\"",
            "\"queued\"",
            "\"checked\"",
            "\"replayed\"",
            "\"replayed\"",
            "\"replayed\"",
            "\"done\""
        ]
    );
}

/// Past the models' budget the import stops, nothing stored or staged.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_import_whose_models_outgrow_the_budget_is_refused(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let app = TestApp::compiling(db.clone());
    app.state.imports.set_model_budget(20_000);
    let files: Vec<(String, String)> = (0..10)
        .map(|k| {
            let tables = (0..20)
                .map(|t| format!("CREATE TABLE t_{k}_{t} (id bigint);"))
                .collect::<Vec<_>>()
                .join("\n");
            (format!("{:04}_tables.sql", k + 1), tables)
        })
        .collect();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let refused = web(
        &app,
        &team.developer,
        &team.path("/databases/app/imports"),
        &files,
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "models_too_large");
    assert_eq!(versions(&db, id).await, []);
    let staged = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM import_staging"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(staged, 0);
}

/// A retry of an import still running (a proxy gave up waiting for the
/// answer) is told so; once it is done, the same request answers that
/// nothing is left.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_retry_while_the_import_runs_is_told_it_still_runs(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "app").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());
    let archive = zip(HISTORY);
    // Another organization's import has the turn: this one waits.
    let held = app.state.imports.take(Uuid::from_u128(7)).ok();
    let first = ci(&app, id, &token, "", "application/zip", archive.clone());
    let retry = async {
        while app.state.imports.waiting() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let retried = ci(&app, id, &token, "", "application/zip", archive.clone()).await;
        drop(held);
        retried
    };
    let (first, retried) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(first, retry)
    })
    .await
    .expect("both answered");
    assert_eq!(retried.status, StatusCode::CONFLICT);
    assert_eq!(retried.code(), "concurrent_import");
    assert!(
        retried.json()["message"]
            .as_str()
            .unwrap()
            .contains("still being imported"),
        "{}",
        retried.json()
    );
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json());
    let again = ci(&app, id, &token, "", "application/zip", archive).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["status"], "unchanged");
}
