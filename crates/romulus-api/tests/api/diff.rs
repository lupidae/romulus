//! Two versions compared: the semantic diff, both models, their SQL rendered
//! alike whatever the source, the layout of the union, and who may read it.

use axum::http::StatusCode;
use romulus_api::{
    domain::{SourceKind, TokenScope, VersionOrigin},
    versions::{
        layout::VersionLayout,
        store::{NewVersion, store},
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::harness::{Team, TestApp, api_token, database, model, model_json};

/// Uploaded in this order: the diff's SQL puts them in canonical order.
const BILLING: &str = "CREATE TABLE plans (id bigint);\nCREATE TABLE invoices (id bigint);\n";

/// A version of `tables` stored as a push of remus JSON, as remus would send
/// it from a production server.
fn pushed(database_id: Uuid, tables: &[&str]) -> NewVersion {
    let mut model = model(tables).canonical();
    "prod".clone_into(&mut model.database);
    "16.4 (Debian 16.4-1.pgdg120+2)".clone_into(&mut model.server_version);
    NewVersion {
        database_id,
        source: serde_json::to_string(&model).unwrap(),
        model,
        source_kind: SourceKind::RemusJson,
        origin: VersionOrigin::Push,
        message: Some("From CI".into()),
        created_by: None,
        created_by_token: None,
        layout: None,
        expected_base: None,
        publishes_draft: None,
        approvals: None,
        migration_sql: None,
    }
}

fn entity_status<'a>(diff: &'a Value, name: &str) -> &'a Value {
    &diff["diff"]["status"]["entities"]["public"][name]["status"]
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_diff_says_what_changed_three_ways(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db);
    let dev = app.as_user(&team.developer);
    let upload = dev
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": BILLING, "message": "First cut" }),
        )
        .await;
    assert_eq!(upload.status, StatusCode::CREATED);
    let push = app
        .push(
            id,
            &token,
            Some("application/json"),
            model_json(&["invoices", "refunds"]),
        )
        .await;
    assert_eq!(push.status, StatusCode::CREATED, "{}", push.json());

    let response = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing/diff?from=1&to=2"))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
    let body = response.json();
    assert_eq!(body["from"]["number"], 1);
    assert_eq!(body["from"]["message"], "First cut");
    assert_eq!(body["from"]["source_kind"], "sql");
    assert_eq!(body["to"]["number"], 2);
    assert_eq!(body["to"]["origin"], "push");

    // The change list and the graph's statuses.
    assert_eq!(body["diff"]["summary"]["text"], "1 table added, 1 removed");
    let changed: Vec<(&str, &str)> = body["diff"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["status"].as_str().unwrap()))
        .collect();
    assert_eq!(changed, [("plans", "removed"), ("refunds", "added")]);
    assert_eq!(entity_status(&body, "invoices"), "unchanged");
    assert_eq!(entity_status(&body, "plans"), "removed");
    assert_eq!(entity_status(&body, "refunds"), "added");

    // Both models, for the graph's union.
    let tables = |side: &str| -> Vec<String> {
        body["models"][side]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(tables("before"), ["plans", "invoices"]);
    assert_eq!(tables("after"), ["invoices", "refunds"]);

    // Both as SQL, the uploaded side rendered from its model too: same
    // header, canonical order, so the unchanged table lines up.
    let before = body["sql"]["before"].as_str().unwrap();
    let after = body["sql"]["after"].as_str().unwrap();
    assert_ne!(before, BILLING);
    assert_eq!(before.lines().next(), after.lines().next());
    assert!(before.find("public.invoices").unwrap() < before.find("public.plans").unwrap());
    assert!(after.contains("CREATE TABLE public.refunds"));
    assert!(!after.contains("public.plans"));

    // Backwards, it reads the other way.
    let back = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing/diff?from=2&to=1"))
        .await
        .json();
    assert_eq!(back["diff"]["summary"]["text"], "1 table added, 1 removed");
    assert_eq!(entity_status(&back, "plans"), "added");
    assert_eq!(entity_status(&back, "refunds"), "removed");
    assert_eq!(back["sql"]["before"], body["sql"]["after"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_upload_and_a_push_of_the_same_schema_differ_in_nothing(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let app = TestApp::compiling(db.clone());
    let dev = app.as_user(&team.developer);
    dev.post(
        &team.path("/databases/billing/versions"),
        &json!({ "sql": BILLING }),
    )
    .await;
    // The same tables in another order, from another server: stored even
    // though nothing changed, as an upload of the same schema would be.
    store(&db, pushed(id, &["invoices", "plans"]), false)
        .await
        .unwrap();

    let body = dev
        .get(&team.path("/databases/billing/diff?from=1&to=2"))
        .await
        .json();
    assert_eq!(body["diff"]["summary"]["text"], "no changes");
    assert_eq!(body["diff"]["summary"]["counts"], json!([]));
    assert_eq!(body["diff"]["entities"], json!([]));
    assert_eq!(entity_status(&body, "plans"), "unchanged");
    // Line for line the same text: the SQL side was rendered from its model,
    // with the pushed side's header.
    assert_eq!(body["sql"]["before"], body["sql"]["after"]);
    assert!(
        body["sql"]["after"]
            .as_str()
            .unwrap()
            .contains("from prod (PostgreSQL 16.4")
    );

    // A version compared with itself, too.
    let itself = dev
        .get(&team.path("/databases/billing/diff?from=2&to=2"))
        .await;
    assert_eq!(itself.status, StatusCode::OK);
    let itself = itself.json();
    assert_eq!(itself["diff"]["summary"]["text"], "no changes");
    assert_eq!(itself["from"]["id"], itself["to"]["id"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_graph_gets_the_newer_layout_and_the_older_one_for_removed_tables(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let mut first = pushed(id, &["invoices", "plans"]);
    first.layout = Some(VersionLayout::from_value(&json!({
        "layout": {
            "public.invoices": { "x": 0, "y": 0 },
            "public.plans": { "x": 400, "y": 0 }
        },
        "groups": { "g1": { "name": "Billing", "color": "amber",
                            "members": ["public.invoices", "public.plans"] } }
    })));
    store(&db, first, false).await.unwrap();
    let mut second = pushed(id, &["invoices", "refunds"]);
    second.layout = Some(VersionLayout::from_value(&json!({
        "layout": { "public.invoices": { "x": 50, "y": 60 } },
        "groups": { "g1": { "name": "Money", "color": "green",
                            "members": ["public.invoices"] } }
    })));
    store(&db, second, false).await.unwrap();
    let app = TestApp::new(db);

    let expected = json!({
        "layout": {
            "public.invoices": { "x": 50.0, "y": 60.0 },
            "public.plans": { "x": 400.0, "y": 0.0 }
        },
        "groups": { "g1": { "name": "Money", "color": "green",
                            "members": ["public.invoices", "public.plans"] } }
    });
    // Whichever way it is read, the newer version decides.
    for query in ["from=1&to=2", "from=2&to=1"] {
        let body = app
            .as_user(&team.reviewer)
            .get(&team.path(&format!("/databases/billing/diff?{query}")))
            .await
            .json();
        assert_eq!(body["layout"], expected, "{query}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_diff_is_computed_once_per_pair_of_versions(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    store(&db, pushed(id, &["invoices"]), false).await.unwrap();
    store(&db, pushed(id, &["invoices", "plans"]), false)
        .await
        .unwrap();
    let app = TestApp::new(db);
    let path = team.path("/databases/billing/diff?from=1&to=2");

    let first = app.as_user(&team.reviewer).get(&path).await;
    assert_eq!(app.state.diffs.len(), 1);
    let again = app.as_user(&team.admin).get(&path).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json(), first.json());
    assert_eq!(app.state.diffs.len(), 1);
    app.as_user(&team.admin)
        .get(&team.path("/databases/billing/diff?from=2&to=1"))
        .await;
    assert_eq!(app.state.diffs.len(), 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_cached_diff_reads_no_full_version(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    store(&db, pushed(id, &["invoices"]), false).await.unwrap();
    store(&db, pushed(id, &["invoices", "plans"]), false)
        .await
        .unwrap();
    let app = TestApp::new(db.clone());
    let reviewer = app.as_user(&team.reviewer);
    let path = team.path("/databases/billing/diff?from=1&to=2");
    let first = reviewer.get(&path).await.json();

    // Without the models' column, loading a full version fails: a hit
    // still answers, from the summaries and the cache alone.
    sqlx::query("ALTER TABLE versions RENAME COLUMN model TO model_elsewhere")
        .execute(&db)
        .await
        .unwrap();
    let again = reviewer.get(&path).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json(), first);
    let miss = reviewer
        .get(&team.path("/databases/billing/diff?from=2&to=1"))
        .await;
    assert_eq!(miss.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_cached_diff_names_authors_as_they_are_now(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::compiling(db.clone());
    app.as_user(&team.developer)
        .post(
            &team.path("/databases/billing/versions"),
            &json!({ "sql": BILLING }),
        )
        .await;
    app.push(
        id,
        &token,
        Some("application/json"),
        model_json(&["refunds"]),
    )
    .await;
    let reviewer = app.as_user(&team.reviewer);
    let path = team.path("/databases/billing/diff?from=1&to=2");
    let first = reviewer.get(&path).await.json();
    assert_eq!(first["from"]["author"]["name"], "Developer-acme");
    assert_eq!(first["to"]["author"]["kind"], "token");

    sqlx::query("UPDATE users SET name = 'Dev Renamed' WHERE id = $1")
        .bind(team.developer.user)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE api_tokens SET name = 'Deploy bot' WHERE database_id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    let again = reviewer.get(&path).await.json();
    assert_eq!(app.state.diffs.len(), 1);
    assert_eq!(again["from"]["author"]["name"], "Dev Renamed");
    assert_eq!(again["to"]["author"]["name"], "Deploy bot");
    assert_eq!(again["diff"], first["diff"]);
    assert_eq!(again["sql"], first["sql"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn diffs_are_for_members_of_the_organization(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let other = Team::new(&db, "globex").await;
    let id = database(&db, team.org, "billing").await;
    database(&db, other.org, "billing").await;
    store(&db, pushed(id, &["invoices"]), false).await.unwrap();
    store(&db, pushed(id, &["invoices", "plans"]), false)
        .await
        .unwrap();
    let app = TestApp::new(db);
    let path = team.path("/databases/billing/diff?from=1&to=2");

    for member in [&team.reviewer, &team.developer, &team.admin] {
        assert_eq!(app.as_user(member).get(&path).await.status, StatusCode::OK);
    }
    // Someone else's organization, even an admin there with a database of
    // the same name, does not exist for them.
    for outsider in [&team.outsider, &other.admin] {
        let response = app.as_user(outsider).get(&path).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
    }
    // Their own database of that name has no versions.
    let theirs = app
        .as_user(&other.admin)
        .get(&other.path("/databases/billing/diff?from=1&to=2"))
        .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get(&path).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn version_numbers_are_checked(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    store(&db, pushed(id, &["invoices"]), false).await.unwrap();
    let app = TestApp::new(db);
    let reviewer = app.as_user(&team.reviewer);
    let diff = |query: &str| team.path(&format!("/databases/billing/diff?{query}"));

    let unknown = reviewer.get(&diff("from=1&to=9")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(unknown.json()["message"].as_str().unwrap().contains('9'));
    let unknown_from = reviewer.get(&diff("from=7&to=1")).await;
    assert_eq!(unknown_from.status, StatusCode::NOT_FOUND);

    let zero = reviewer.get(&diff("from=0&to=1")).await;
    assert_eq!(zero.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(zero.code(), "validation");
    assert!(zero.json()["details"]["from"].is_string());
    let negative = reviewer.get(&diff("from=1&to=-2")).await;
    assert!(negative.json()["details"]["to"].is_string());

    for query in ["from=one&to=1", "from=1", "to=1", "from=1&to=99999999999"] {
        let response = reviewer.get(&diff(query)).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(response.code(), "bad_request", "{query}");
    }
    let missing = reviewer
        .get(&team.path("/databases/nope/diff?from=1&to=1"))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}
