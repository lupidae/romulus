//! Data export: a database's (any member) or the organization's (admins)
//! versions, migrations, comments and manifest, as a streamed ZIP; versions
//! hidden by a plan included, removed ones not.

use std::{collections::BTreeMap, io::Read};

use axum::http::{StatusCode, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    billing::{billed, old_version},
    harness::{Team, TestApp, TestResponse, database, model},
};

/// The archive's files: path → text.
fn unzip(response: &TestResponse) -> BTreeMap<String, String> {
    assert_eq!(
        response.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(response.headers[header::CONTENT_TYPE], "application/zip");
    assert!(response.body.starts_with(b"PK"));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(response.body.to_vec())).unwrap();
    let mut files = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        files.insert(file.name().unwrap().into_owned(), text);
    }
    files
}

async fn insert_version(
    db: &PgPool,
    database: Uuid,
    number: i32,
    tables: &[&str],
    source_kind: &str,
    renames: Value,
) -> Uuid {
    let schema = serde_json::to_value(model(tables)).unwrap();
    let source = if source_kind == "remus_json" {
        schema.to_string()
    } else {
        tables.iter().fold(String::new(), |mut sql, t| {
            sql.push_str("CREATE TABLE public.");
            sql.push_str(t);
            sql.push_str(" (id bigint NOT NULL);\n");
            sql
        })
    };
    sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout, renames)
         VALUES ($1, $2, $3, $4, $5::text::version_source_kind,
                 CASE WHEN $5 = 'remus_json' THEN 'push' ELSE 'upload' END::version_origin, '{}', $6)
         RETURNING id",
        database,
        number,
        schema,
        source,
        source_kind,
        renames,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_export_holds_every_version_its_migrations_and_comments(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    insert_version(&db, shop, 1, &["users"], "sql", json!({})).await;
    // v2 renames users to customers (chosen at publish) and was pushed as JSON.
    let v2 = insert_version(
        &db,
        shop,
        2,
        &["customers"],
        "remus_json",
        json!({"known": [{"kind": "table", "schema": "public", "parent": null, "from": "users", "to": "customers"}]}),
    )
    .await;
    insert_version(&db, shop, 3, &["customers", "orders"], "sql", json!({})).await;
    insert_version(
        &db,
        shop,
        4,
        &["customers", "orders", "items"],
        "sql",
        json!({}),
    )
    .await;
    sqlx::query!("UPDATE versions SET removed_at = now(), removed_by = $2, removal_reason = 'oops' WHERE database_id = $1 AND number = 3", shop, team.admin.user)
        .execute(&db)
        .await
        .unwrap();
    let thread = sqlx::query_scalar!(
        "INSERT INTO version_threads (version_id, frozen, anchor, created_by) VALUES ($1, false, '{\"table\": \"public.customers\"}', $2) RETURNING id",
        v2,
        team.developer.user,
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO version_comments (thread_id, author_id, author_name, body) VALUES ($1, $2, 'Developer', 'Why customers?')",
        thread,
        team.developer.user,
    )
    .execute(&db)
    .await
    .unwrap();

    // A reviewer may export.
    let response = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/export"))
        .await;
    let disposition = response.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        disposition.starts_with("attachment; filename=\"acme-shop-export-"),
        "{disposition}"
    );
    let files = unzip(&response);
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    for expected in [
        "shop/manifest.json",
        "shop/comments.json",
        "shop/versions/v0001.sql",
        "shop/versions/v0001.model.json",
        "shop/versions/v0002.sql",
        "shop/versions/v0002.source.json",
        "shop/versions/v0004.sql",
        "shop/migrations/v0001-v0002.up.sql",
        "shop/migrations/v0001-v0002.down.sql",
        "shop/migrations/v0002-v0004.up.sql",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.contains("v0003")),
        "removed versions are left out: {names:?}"
    );
    assert!(files["shop/versions/v0001.sql"].contains("CREATE TABLE public.users"));
    // A pushed version's DDL is rendered from its model.
    assert!(files["shop/versions/v0002.sql"].contains("customers"));
    // The rename chosen at the time: ALTER … RENAME, not a drop and an add.
    let up = &files["shop/migrations/v0001-v0002.up.sql"];
    assert!(up.contains("RENAME TO"), "{up}");
    assert!(!up.contains("DROP TABLE"), "{up}");
    assert!(files["shop/migrations/v0001-v0002.down.sql"].contains("RENAME TO"));
    assert!(files["shop/migrations/v0002-v0004.up.sql"].contains("items"));

    let manifest: Value = serde_json::from_str(&files["shop/manifest.json"]).unwrap();
    assert_eq!(manifest["format"], "romulus-export/1");
    assert_eq!(manifest["database"]["slug"], "shop");
    let numbers: Vec<i64> = manifest["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [1, 2, 4]);
    assert_eq!(manifest["removed_versions"], json!([3]));
    assert_eq!(manifest["migrations"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["migrations"][0]["renames"][0]["to"], "customers");
    let comments: Value = serde_json::from_str(&files["shop/comments.json"]).unwrap();
    assert_eq!(comments[0]["version"], 2);
    assert_eq!(comments[0]["comments"][0]["body"], "Why customers?");

    assert_eq!(
        app.as_user(&team.outsider)
            .get(&team.path("/databases/shop/export"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_organization_export_is_for_admins_and_holds_every_database(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let crm = database(&db, team.org, "crm").await;
    insert_version(&db, shop, 1, &["users"], "sql", json!({})).await;
    insert_version(&db, crm, 1, &["leads"], "sql", json!({})).await;
    insert_version(&db, crm, 2, &["leads", "deals"], "sql", json!({})).await;
    for who in [&team.developer, &team.reviewer] {
        assert_eq!(
            app.as_user(who).get(&team.path("/export")).await.status,
            StatusCode::FORBIDDEN
        );
    }
    let files = unzip(&app.as_user(&team.admin).get(&team.path("/export")).await);
    let manifest: Value = serde_json::from_str(&files["manifest.json"]).unwrap();
    assert_eq!(manifest["organization"]["slug"], "acme");
    assert_eq!(manifest["databases"].as_array().unwrap().len(), 2);
    assert!(files.contains_key("crm/migrations/v0001-v0002.up.sql"));
    assert!(files.contains_key("shop/versions/v0001.sql"));
    assert!(files.contains_key("crm/manifest.json"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn versions_hidden_by_the_plan_are_exported_and_export_is_never_capped(db: PgPool) {
    let b = billed(&db).await;
    let shop = database(&db, b.team.org, "shop").await;
    old_version(&db, shop, 1, 200).await;
    old_version(&db, shop, 2, 1).await;
    let hidden = b
        .app
        .as_user(&b.team.reviewer)
        .get(&b.team.path("/databases/shop/versions/1"))
        .await;
    assert_eq!(hidden.status, StatusCode::PAYMENT_REQUIRED);
    let files = unzip(
        &b.app
            .as_user(&b.team.reviewer)
            .get(&b.team.path("/databases/shop/export"))
            .await,
    );
    assert!(files.contains_key("shop/versions/v0001.sql"));
    assert!(files.contains_key("shop/migrations/v0001-v0002.up.sql"));
    let _ = &b.stripe;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_export_holds_layouts_renames_and_open_drafts_with_their_text(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    insert_version(&db, shop, 1, &["users"], "sql", json!({})).await;
    insert_version(
        &db,
        shop,
        2,
        &["customers"],
        "sql",
        json!({"known": [{"kind": "table", "schema": "public", "parent": null, "from": "users", "to": "customers"}]}),
    )
    .await;
    let layout = json!({"layout": {"public.customers": {"x": 10, "y": 20}}, "groups": {}});
    let schema = serde_json::to_value(model(&["customers", "orders"])).unwrap();
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, 3, $2, 'CREATE TABLE public.customers (id bigint NOT NULL);', 'sql', 'upload', $3)",
        shop,
        schema,
        layout,
    )
    .execute(&db)
    .await
    .unwrap();
    let draft = app
        .as_user(&team.developer)
        .post(&team.path("/databases/shop/drafts"), &json!({}))
        .await;
    assert_eq!(draft.status, StatusCode::CREATED, "{}", draft.json());

    let files = unzip(
        &app.as_user(&team.reviewer)
            .get(&team.path("/databases/shop/export"))
            .await,
    );
    let layout: Value = serde_json::from_str(&files["shop/versions/v0003.layout.json"]).unwrap();
    assert_eq!(
        layout["layout"]["public.customers"]["x"].as_f64(),
        Some(10.0)
    );
    assert!(files.contains_key("shop/versions/v0001.layout.json"));
    let renames: Value = serde_json::from_str(&files["shop/versions/v0002.renames.json"]).unwrap();
    assert_eq!(renames["known"][0]["to"], "customers");
    assert!(
        !files.contains_key("shop/versions/v0001.renames.json"),
        "none chosen"
    );
    let schema = files
        .iter()
        .find(|(name, _)| name.starts_with("shop/drafts/01-") && name.ends_with("/schema.sql"))
        .map(|(_, text)| text.clone())
        .expect("the open draft's SQL");
    assert!(schema.contains("customers"), "{schema}");
    assert!(
        files
            .keys()
            .any(|n| n.starts_with("shop/drafts/01-") && n.ends_with("/comments.json"))
    );
    assert!(
        files
            .keys()
            .any(|n| n.starts_with("shop/drafts/01-") && n.ends_with("/layout.json"))
    );
    let manifest: Value = serde_json::from_str(&files["shop/manifest.json"]).unwrap();
    assert_eq!(manifest["drafts"][0]["base_version"], 3);
    assert_eq!(manifest["drafts"][0]["mode"], "schema");
    app.state.collab.shutdown().await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn exports_are_limited_per_person(db: PgPool) {
    let app = TestApp::compiling(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    insert_version(&db, shop, 1, &["users"], "sql", json!({})).await;
    for _ in 0..5 {
        let ok = app
            .as_user(&team.reviewer)
            .get(&team.path("/databases/shop/export"))
            .await;
        assert_eq!(ok.status, StatusCode::OK);
    }
    let refused = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/shop/export"))
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(refused.headers.contains_key(header::RETRY_AFTER));
    // Someone else of the organization still may.
    let other = app
        .as_user(&team.developer)
        .get(&team.path("/databases/shop/export"))
        .await;
    assert_eq!(other.status, StatusCode::OK);
}
