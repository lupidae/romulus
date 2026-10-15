//! The machine API's push: remus JSON or SQL with an API token, unchanged
//! pushes, limits.

use axum::http::StatusCode;
use remus_core::fixtures::SHOWCASE;
use romulus_api::{domain::TokenScope, versions::push::PUSH_BODY_LIMIT};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::harness::{Team, TestApp, api_token, database, model_json};

/// A database with a push token: (database id, token).
async fn pushable(db: &PgPool) -> (Team, Uuid, String) {
    let team = Team::new(db, "acme").await;
    let id = database(db, team.org, "billing").await;
    let token = api_token(db, id, TokenScope::Push).await;
    (team, id, token)
}

async fn count(db: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        id
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unchanged_push_creates_nothing(db: PgPool) {
    let (team, id, token) = pushable(&db).await;
    let app = TestApp::new(db.clone());

    let first = app
        .push(id, &token, Some("application/json"), model_json(&["users"]))
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json());
    assert_eq!(first.json()["status"], "created");
    assert_eq!(first.json()["version"]["number"], 1);
    assert_eq!(first.json()["version"]["origin"], "push");
    assert_eq!(first.json()["version"]["source_kind"], "remus_json");
    assert_eq!(first.json()["version"]["author"]["kind"], "token");

    // The same schema, even pretty-printed and from another server: nothing new.
    let mut again: Value = serde_json::from_str(&model_json(&["users"])).unwrap();
    again["server_version"] = json!("18.1");
    again["generated_at"] = json!("2026-10-15T12:00:00Z");
    let same = app
        .push(
            id,
            &token,
            Some("application/json"),
            serde_json::to_string_pretty(&again).unwrap(),
        )
        .await;
    assert_eq!(same.status, StatusCode::OK);
    assert_eq!(same.json()["status"], "unchanged");
    assert_eq!(same.json()["version"]["number"], 1);
    assert_eq!(count(&db, id).await, 1);

    let changed = app
        .push(
            id,
            &token,
            Some("application/json"),
            model_json(&["users", "orders"]),
        )
        .await;
    assert_eq!(changed.status, StatusCode::CREATED);
    assert_eq!(changed.json()["version"]["number"], 2);

    // The source is kept verbatim.
    let v2 = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing/versions/2"))
        .await
        .json();
    assert_eq!(v2["source"], model_json(&["users", "orders"]));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_remus_showcase_round_trips(db: PgPool) {
    let (team, id, token) = pushable(&db).await;
    let app = TestApp::new(db);
    let response = app
        .push(id, &token, Some("application/json"), SHOWCASE)
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let again = app
        .push(id, &token, Some("application/json"), SHOWCASE)
        .await;
    assert_eq!(again.json()["status"], "unchanged");

    let reviewer = app.as_user(&team.reviewer);
    let version = reviewer
        .get(&team.path("/databases/billing/versions/1"))
        .await
        .json();
    // Domains and partitions come through as remus captured them.
    assert_ne!(version["model"]["domains"], json!([]));
    assert_ne!(version["model"]["partitions"], json!([]));
    let texts = reviewer
        .get(&team.path("/databases/billing/versions/1/texts"))
        .await
        .json();
    // A JSON version's SQL is rendered from its model.
    let sql = texts["sql"].as_str().unwrap();
    assert!(sql.contains("CREATE DOMAIN"), "{sql}");
    assert!(sql.contains("PARTITION OF"), "{sql}");
    assert!(texts["dbml"].as_str().unwrap().contains("Table"));
}

/// Two levels of partitions (remus 0.2 models a partition's own key).
const MULTI_LEVEL: &str = r#"{
  "remus_version": "0.2", "database": "app", "server_version": "17",
  "entities": [
    {"schema": "public", "name": "events", "kind": "p", "comment": null,
     "partition_key": "RANGE (at)", "view_definition": null,
     "columns": [
       {"name": "at", "type": "timestamp with time zone", "type_schema": "pg_catalog",
        "type_name": "timestamptz", "type_kind": "b", "not_null": true,
        "default": null, "identity": null, "generated": null, "comment": null},
       {"name": "kind", "type": "text", "type_schema": "pg_catalog", "type_name": "text",
        "type_kind": "b", "not_null": true, "default": null, "identity": null,
        "generated": null, "comment": null}]},
    {"schema": "public", "name": "events_2026", "kind": "p", "comment": null,
     "partition_key": "LIST (kind)", "view_definition": null,
     "columns": [
       {"name": "at", "type": "timestamp with time zone", "type_schema": "pg_catalog",
        "type_name": "timestamptz", "type_kind": "b", "not_null": true,
        "default": null, "identity": null, "generated": null, "comment": null},
       {"name": "kind", "type": "text", "type_schema": "pg_catalog", "type_name": "text",
        "type_kind": "b", "not_null": true, "default": null, "identity": null,
        "generated": null, "comment": null}]},
    {"schema": "public", "name": "events_2026_click", "kind": "r", "comment": null,
     "partition_key": null, "view_definition": null,
     "columns": [
       {"name": "at", "type": "timestamp with time zone", "type_schema": "pg_catalog",
        "type_name": "timestamptz", "type_kind": "b", "not_null": true,
        "default": null, "identity": null, "generated": null, "comment": null},
       {"name": "kind", "type": "text", "type_schema": "pg_catalog", "type_name": "text",
        "type_kind": "b", "not_null": true, "default": null, "identity": null,
        "generated": null, "comment": null}]}
  ],
  "partitions": [
    {"parent_schema": "public", "parent": "events", "child_schema": "public",
     "child": "events_2026", "partition_key": "LIST (kind)",
     "bound": "FOR VALUES FROM ('2026-01-01 00:00:00+00') TO ('2027-01-01 00:00:00+00')"},
    {"parent_schema": "public", "parent": "events_2026", "child_schema": "public",
     "child": "events_2026_click", "partition_key": null,
     "bound": "FOR VALUES IN ('click')"}
  ]
}"#;

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn multi_level_partitions_are_kept(db: PgPool) {
    let (team, id, token) = pushable(&db).await;
    let app = TestApp::new(db);
    let response = app
        .push(id, &token, Some("application/json"), MULTI_LEVEL)
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let version = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing/versions/1"))
        .await
        .json();
    assert_eq!(
        version["model"]["partitions"][0]["partition_key"],
        "LIST (kind)"
    );
    assert_eq!(version["model"]["partitions"][1]["parent"], "events_2026");

    // A sub-partition key that changes is a change.
    let rekeyed = MULTI_LEVEL.replacen(
        r#""child": "events_2026", "partition_key": "LIST (kind)""#,
        r#""child": "events_2026", "partition_key": "HASH (kind)""#,
        1,
    );
    let response = app
        .push(id, &token, Some("application/json"), rekeyed)
        .await;
    assert_eq!(response.json()["status"], "created");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn curl_as_documented_just_works(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::compiling(db);
    // `curl --data-binary @-` says application/x-www-form-urlencoded.
    let curl = Some("application/x-www-form-urlencoded");
    let json = app.push(id, &token, curl, model_json(&["users"])).await;
    assert_eq!(json.status, StatusCode::CREATED, "{}", json.json());
    assert_eq!(json.json()["version"]["source_kind"], "remus_json");
    let sql = app
        .push(
            id,
            &token,
            curl,
            "CREATE TABLE users (id bigint);\nCREATE TABLE b (id bigint);",
        )
        .await;
    assert_eq!(sql.status, StatusCode::CREATED, "{}", sql.json());
    assert_eq!(sql.json()["version"]["source_kind"], "sql");
    let none = app.push(id, &token, None, model_json(&["users"])).await;
    assert_eq!(none.json()["status"], "created");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_message_comes_with_the_query(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::new(db);
    let response = app
        .send(
            axum::http::Request::post(format!(
                "/api/databases/{id}/versions?message=Add%20orders%20(%23412)"
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(model_json(&["orders"]).into())
            .unwrap(),
        )
        .await;
    assert_eq!(response.json()["version"]["message"], "Add orders (#412)");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn bad_pushes_are_refused_and_store_nothing(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::compiling(db.clone());

    let not_remus = app
        .push(id, &token, Some("application/json"), r#"{"tables": []}"#)
        .await;
    assert_eq!(
        (not_remus.status, not_remus.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_schema")
    );
    let not_json = app
        .push(id, &token, Some("application/json"), "{ nope")
        .await;
    assert_eq!(not_json.code(), "invalid_schema");
    let html = app.push(id, &token, Some("text/html"), "<p>").await;
    assert_eq!(
        (html.status, html.code().as_str()),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
    );
    let empty = app.push(id, &token, Some("text/plain"), "  ").await;
    assert_eq!(empty.code(), "validation");
    let broken = app
        .push(id, &token, Some("application/sql"), "CREATE oops;")
        .await;
    assert_eq!(broken.code(), "compile_failed");
    assert_eq!(
        broken.json()["details"]["diagnostics"][0]["code"],
        "syntax_error"
    );
    let binary = app
        .push(id, &token, Some("text/plain"), vec![0xff, 0xfe, 0x00])
        .await;
    assert_eq!(binary.code(), "validation");
    assert_eq!(count(&db, id).await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_sql_push_without_a_compiler_is_a_503(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::new(db.clone());
    let response = app
        .push(id, &token, Some("text/plain"), "CREATE TABLE a (id int);")
        .await;
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(count(&db, id).await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn pushes_have_a_size_limit(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::new(db);
    // Above the default body limit, below the push limit: accepted (and
    // refused only for not being remus JSON).
    let big = format!("{{\"padding\": \"{}\"}}", "x".repeat(4 * 1024 * 1024));
    let response = app.push(id, &token, Some("application/json"), big).await;
    assert_eq!(response.code(), "invalid_schema");
    let huge = "x".repeat(PUSH_BODY_LIMIT + 1);
    let response = app.push(id, &token, Some("text/plain"), huge).await;
    assert_eq!(
        (response.status, response.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_token_that_pushes_in_a_loop_is_slowed_down(db: PgPool) {
    let (_, id, token) = pushable(&db).await;
    let app = TestApp::new(db);
    let mut statuses = Vec::new();
    for i in 0..25 {
        statuses.push(
            app.push(
                id,
                &token,
                Some("application/json"),
                model_json(&[&format!("t{i}")]),
            )
            .await,
        );
    }
    let limited = statuses
        .iter()
        .find(|r| r.status == StatusCode::TOO_MANY_REQUESTS)
        .expect("the burst runs out");
    assert_eq!(limited.code(), "rate_limited");
    assert!(limited.headers.contains_key("retry-after"));
    assert_eq!(
        statuses
            .iter()
            .filter(|r| r.status == StatusCode::CREATED)
            .count(),
        20
    );
}
