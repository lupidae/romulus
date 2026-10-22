//! `romulus test` (crates/romulus-cli) against the API on a real socket:
//! a folder whose first migration the database imported, then a good new
//! migration and a bad one, through the MCP endpoint with a personal
//! access token, as CI runs it. The playground host is the migration
//! tests' fake ([`TestHost`]), failing the statement a test names.

use std::{path::PathBuf, time::Duration};

use axum::http::StatusCode;
use romulus_api::{
    AppState, Config,
    collab::{Hub, room::Settings},
    domain::AccessTokenScope,
    playgrounds::host::Host,
};
use romulus_cli::{
    args::TestOptions,
    ci::Context,
    report::{Outcome, RunResult, Status},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    draft_modes::mode_compiler,
    harness::{Team, TestApp, database},
    live::{quick, serve, version},
    mcp::{Mcp, pat},
    migration_tests::TestHost,
};

const CUSTOMERS: &str = "CREATE TABLE public.customers (id bigint PRIMARY KEY);\n";
const ORDERS: &str = "CREATE TABLE public.orders (id bigint PRIMARY KEY);\n";
const EMAIL: &str = "-- every customer has an email\nALTER TABLE public.customers ADD COLUMN email text;\n\nALTER TABLE public.customers ALTER COLUMN email SET NOT NULL;\n";

struct Shop {
    app: TestApp,
    team: Team,
    host: TestHost,
    url: String,
    database: Uuid,
}

/// Team `acme`, database `shop` at v1, imported from `1_customers.sql`
/// (sqlx's naming: version 1); the API served, its tests on the fake host.
async fn shop(db: &PgPool) -> Shop {
    let host = TestHost::default();
    // No one has the draft open: its room must stay loaded through a
    // compile (quick()'s 50 ms would unload it first).
    let settings = Settings {
        idle_unload: Duration::from_secs(30),
        ..quick()
    };
    let hub = Hub::with_settings(db.clone(), mode_compiler(), settings);
    let mut state = AppState::new(db.clone(), Config::for_tests())
        .with_collab(hub)
        .with_playgrounds(Host::new(host.clone()));
    state.compiler = state.collab.compiler().clone();
    let app = TestApp::with_state(state);
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    let v1 = version(db, shop, 1, CUSTOMERS, json!({})).await;
    sqlx::query!(
        "INSERT INTO version_migrations (version_id, database_id, migration_id, path, format, checksum)
         VALUES ($1, $2, '1', '1_customers.sql', 'sqlx', $3)",
        v1,
        shop,
        Sha256::digest(CUSTOMERS.as_bytes()).to_vec(),
    )
    .execute(db)
    .await
    .expect("imported");
    let addr = serve(&app).await;
    Shop {
        app,
        team,
        host,
        url: format!("http://{addr}"),
        database: shop,
    }
}

/// A migrations folder of its own.
fn folder(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("romulus-cli-{name}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("folder");
    for (path, content) in files {
        std::fs::write(dir.join(path), content).expect("file");
    }
    dir
}

fn options(dir: PathBuf) -> TestOptions {
    TestOptions {
        dir,
        org: "acme".into(),
        database: "shop".into(),
        timeout: Duration::from_secs(60),
        ..TestOptions::default()
    }
}

fn pull_request() -> Context {
    Context {
        ci: Some("GitHub Actions"),
        pr: Some(42),
        branch: Some("customer-emails".into()),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        run_url: None,
    }
}

async fn run(shop: &Shop, token: &str, options: &TestOptions) -> Outcome {
    romulus_cli::test(options, &shop.url, token, &pull_request(), &mut |_| {}).await
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn romulus_test_passes_a_good_migration_and_fails_a_bad_one(db: PgPool) {
    let shop = shop(&db).await;
    shop.host.fail_on("SET NOT NULL");
    let token = pat(&db, &shop.team.developer, AccessTokenScope::Write).await;

    // The imported migration and a good new one: it passes.
    let good = folder(
        "good",
        &[
            ("1_customers.sql", CUSTOMERS),
            ("2_orders.sql", ORDERS),
            ("README.md", "# db"),
        ],
    );
    let outcome = run(&shop, &token, &options(good.clone())).await;
    assert_eq!(outcome.result, RunResult::Passed, "{}", outcome.text());
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.imported, 1);
    let shown: Vec<_> = outcome
        .migrations
        .iter()
        .map(|m| (m.path.rsplit('/').next().unwrap().to_owned(), m.status))
        .collect();
    assert_eq!(shown, [("2_orders.sql".to_owned(), Status::Passed)]);
    let draft = outcome.draft.clone().expect("a draft");
    assert_eq!(draft.name, "PR #42: customer-emails");
    assert!(!draft.reused);
    assert_eq!(draft.base_version, Some(1));
    let page = outcome.test_page.clone().expect("the report's page");
    assert!(page.contains("/app/acme/shop/tests/"), "{page}");
    assert!(outcome.text().contains("PASSED"), "{}", outcome.text());
    // The draft holds the new migration, in migration mode; the upload is
    // gone, and only the new migration was tested.
    let row = sqlx::query!(
        r#"SELECT name, mode::text AS "mode!" FROM drafts WHERE database_id = $1 AND status = 'open'"#,
        shop.database
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (row.name.as_str(), row.mode.as_str()),
        ("PR #42: customer-emails", "migration")
    );
    let uploads = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM import_uploads"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(uploads, 0);
    assert_eq!(
        shop.host.tested(),
        [vec![
            "CREATE TABLE public.orders (id bigint PRIMARY KEY);".to_owned()
        ]]
    );

    // A bad migration after it: the job fails, on that file and line, and
    // the pull request's draft is reused.
    std::fs::write(good.join("3_email.sql"), EMAIL).unwrap();
    let outcome = run(&shop, &token, &options(good.clone())).await;
    assert_eq!(outcome.result, RunResult::Failed, "{}", outcome.text());
    assert_eq!(outcome.exit_code, 1);
    let statuses: Vec<_> = outcome.migrations.iter().map(|m| m.status).collect();
    assert_eq!(statuses, [Status::Passed, Status::Failed]);
    let failure = outcome.migrations[1].failure.as_ref().expect("its failure");
    assert_eq!(failure.statement, 2);
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.kind == "statement_failed")
        .expect("the failed statement");
    assert!(finding.path.as_deref().unwrap().ends_with("/3_email.sql"));
    assert_eq!(finding.line, Some(4));
    assert_eq!(
        outcome.draft.as_ref().map(|d| (d.id.clone(), d.reused)),
        Some((draft.id.clone(), true))
    );
    let text = outcome.text();
    assert!(text.contains("FAIL  "), "{text}");
    assert!(text.contains("ERROR 23502"), "{text}");
    assert!(
        outcome.annotations().contains("3_email.sql,line=4::"),
        "{}",
        outcome.annotations()
    );

    // The same files again: the test of that exact text is the answer, no
    // second playground.
    let tests_before = shop.host.tested().len();
    let again = run(&shop, &token, &options(good.clone())).await;
    assert_eq!(again.exit_code, 1);
    assert!(again.reused_test);
    assert_eq!(again.test_id, outcome.test_id);
    assert_eq!(shop.host.tested().len(), tests_before);
    // Unless asked to test again.
    let fresh = run(
        &shop,
        &token,
        &TestOptions {
            fresh: true,
            ..options(good.clone())
        },
    )
    .await;
    assert_eq!(fresh.exit_code, 1);
    assert!(!fresh.reused_test);
    assert_eq!(shop.host.tested().len(), tests_before + 1);
    std::fs::remove_dir_all(good).unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn nothing_new_passes_and_a_changed_or_inapplicable_migration_fails(db: PgPool) {
    let shop = shop(&db).await;
    let token = pat(&db, &shop.team.developer, AccessTokenScope::Write).await;

    let imported = folder("imported", &[("1_customers.sql", CUSTOMERS)]);
    let outcome = run(&shop, &token, &options(imported.clone())).await;
    assert_eq!(outcome.result, RunResult::NothingNew, "{}", outcome.text());
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(shop.host.tested().len(), 0);

    // An imported migration edited since: refused, a failed check.
    std::fs::write(
        imported.join("1_customers.sql"),
        "CREATE TABLE public.customers (id int);\n",
    )
    .unwrap();
    std::fs::write(imported.join("2_orders.sql"), ORDERS).unwrap();
    let outcome = run(&shop, &token, &options(imported.clone())).await;
    assert_eq!(outcome.exit_code, 1, "{}", outcome.text());
    assert!(
        outcome.text().contains("1_customers.sql changed"),
        "{}",
        outcome.text()
    );

    // Back as imported, and a new migration that does not apply to the
    // latest version: the compile's error, on its file and line.
    std::fs::write(imported.join("1_customers.sql"), CUSTOMERS).unwrap();
    std::fs::write(
        imported.join("3_broken.sql"),
        "-- the fake compiler fails at its marker\nCREATE TABLE public.\"ERROR\" (id int);\n",
    )
    .unwrap();
    let outcome = run(&shop, &token, &options(imported.clone())).await;
    assert_eq!(outcome.exit_code, 1, "{}", outcome.text());
    assert_eq!(outcome.migrations[1].status, Status::Invalid);
    let finding = &outcome.findings[0];
    assert_eq!(finding.kind, "does_not_apply");
    assert!(finding.path.as_deref().unwrap().ends_with("/3_broken.sql"));
    assert_eq!(finding.line, Some(2));
    assert_eq!(shop.host.tested().len(), 0);
    std::fs::remove_dir_all(imported).unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tokens_that_cannot_test_are_errors_that_say_why(db: PgPool) {
    let shop = shop(&db).await;
    let dir = folder(
        "tokens",
        &[("1_customers.sql", CUSTOMERS), ("2_orders.sql", ORDERS)],
    );

    // A token romulus never made.
    let outcome = run(&shop, "rmls_pat_not-a-real-token", &options(dir.clone())).await;
    assert_eq!(outcome.exit_code, 2);
    assert_eq!(outcome.error.as_ref().unwrap().code, "token");
    assert!(
        outcome
            .text()
            .contains("revoked, has expired or is mistyped"),
        "{}",
        outcome.text()
    );

    // None at all.
    let outcome = run(&shop, "", &options(dir.clone())).await;
    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome.text().contains("ROMULUS_TOKEN is not set"),
        "{}",
        outcome.text()
    );

    // A revoked one.
    let token = pat(&db, &shop.team.developer, AccessTokenScope::Write).await;
    sqlx::query!("UPDATE personal_access_tokens SET revoked_at = now()")
        .execute(&db)
        .await
        .unwrap();
    let outcome = run(&shop, &token, &options(dir.clone())).await;
    assert_eq!(outcome.exit_code, 2);
    assert_eq!(outcome.error.as_ref().unwrap().code, "token");

    // A read-only one: testing writes.
    let token = pat(&db, &shop.team.developer, AccessTokenScope::Read).await;
    let outcome = run(&shop, &token, &options(dir.clone())).await;
    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome.text().contains("write access"),
        "{}",
        outcome.text()
    );

    // A reviewer's: they read tests, developers run them.
    let token = pat(&db, &shop.team.reviewer, AccessTokenScope::Write).await;
    let outcome = run(&shop, &token, &options(dir.clone())).await;
    assert_eq!(outcome.exit_code, 2, "{}", outcome.text());
    assert!(
        outcome.text().contains("developer role"),
        "{}",
        outcome.text()
    );

    // Nobody at that address.
    let token = pat(&db, &shop.team.developer, AccessTokenScope::Write).await;
    let outcome = romulus_cli::test(
        &options(dir.clone()),
        "http://127.0.0.1:9",
        &token,
        &Context::default(),
        &mut |_| {},
    )
    .await;
    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome.text().contains("cannot reach romulus"),
        "{}",
        outcome.text()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn new_migrations_are_taken_out_of_their_starters_upload(db: PgPool) {
    let shop = shop(&db).await;
    let (team, app) = (&shop.team, &shop.app);
    let uploads = team.path("/databases/shop/imports/uploads");
    let files = json!({ "files": [
        { "path": "1_customers.sql", "content": CUSTOMERS },
        { "path": "2_orders.sql", "content": ORDERS },
        { "path": "3_email.sql", "content": EMAIL },
    ]});
    let refused = app.as_user(&team.reviewer).post(&uploads, &files).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let started = app.as_user(&team.developer).post(&uploads, &files).await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.json());
    let take = format!(
        "{uploads}/{}/new-migrations",
        started.json()["id"].as_str().unwrap()
    );
    // An upload is its starter's alone.
    let theirs = app.as_user(&team.admin).post(&take, &json!({})).await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);

    let taken = app.as_user(&team.developer).post(&take, &json!({})).await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.json());
    let taken = taken.json();
    assert_eq!(taken["format"], "sqlx");
    assert_eq!(taken["imported"], 1);
    let sql = taken["sql"].as_str().unwrap();
    assert_eq!(
        sql,
        format!("-- 2_orders.sql\n{ORDERS}\n-- 3_email.sql\n{EMAIL}")
    );
    let laid: Vec<(String, u64, u64, u64)> = taken["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["path"].as_str().unwrap().to_owned(),
                m["sql_line"].as_u64().unwrap(),
                m["first_statement"].as_u64().unwrap(),
                m["statements"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        laid,
        [
            ("2_orders.sql".to_owned(), 2, 0, 1),
            ("3_email.sql".to_owned(), 5, 1, 2)
        ]
    );
    // Line 8 of the text is line 4 of 3_email.sql.
    assert_eq!(
        sql.lines().nth(7),
        Some("ALTER TABLE public.customers ALTER COLUMN email SET NOT NULL;")
    );
    // Used up.
    let again = app.as_user(&team.developer).post(&take, &json!({})).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    // A database that imported none of them: nothing tells which are new.
    let other = database(&db, team.org, "other").await;
    version(&db, other, 1, CUSTOMERS, json!({})).await;
    let uploads = team.path("/databases/other/imports/uploads");
    let started = app.as_user(&team.developer).post(&uploads, &files).await;
    let take = format!(
        "{uploads}/{}/new-migrations",
        started.json()["id"].as_str().unwrap()
    );
    let refused = app.as_user(&team.developer).post(&take, &json!({})).await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "history_not_imported")
    );
    // Refused, the files stay: the upload can still be imported.
    let still = app
        .as_user(&team.developer)
        .get(&take.replace("/new-migrations", ""))
        .await;
    assert_eq!(still.status, StatusCode::OK);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tool_data_goes_only_to_programs_that_ask_for_it(db: PgPool) {
    let shop = shop(&db).await;
    let mcp = Mcp::new(
        &shop.app,
        pat(&db, &shop.team.developer, AccessTokenScope::Write).await,
    );
    let call = |arguments: Value, data: bool| {
        let mut params = json!({ "name": "import_migrations", "arguments": arguments });
        if data {
            params["_meta"] = json!({ "romulus/data": true });
        }
        params
    };
    let files = json!([{ "path": "1_customers.sql", "content": CUSTOMERS }]);
    let plain = mcp
        .rpc(
            "tools/call",
            call(
                json!({ "org": "acme", "database": "shop", "files": files, "commit": false }),
                false,
            ),
        )
        .await;
    assert!(
        plain["result"].get("structuredContent").is_none(),
        "{plain}"
    );
    let asked = mcp
        .rpc(
            "tools/call",
            call(
                json!({ "org": "acme", "database": "shop", "files": files, "commit": false }),
                true,
            ),
        )
        .await;
    let data = &asked["result"]["structuredContent"];
    assert_eq!(data["upload"]["files"], 1, "{asked}");
    assert!(data["upload"]["id"].is_string());
    // A refusal reads the same way: the API's code and status.
    let refused = mcp
        .rpc(
            "tools/call",
            call(
                json!({ "org": "acme", "database": "nowhere", "files": files, "commit": false }),
                true,
            ),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true);
    let error = &refused["result"]["structuredContent"]["error"];
    assert_eq!(
        (error["code"].as_str(), error["status"].as_u64()),
        (Some("not_found"), Some(404))
    );
    // whoami keeps its facts for everyone.
    let whoami = mcp
        .rpc("tools/call", json!({ "name": "whoami", "arguments": {} }))
        .await;
    assert!(
        whoami["result"]["structuredContent"]["user"].is_object(),
        "{whoami}"
    );
}
