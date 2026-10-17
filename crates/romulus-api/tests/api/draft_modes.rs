//! Drafts in two modes (docs/MVP.md "Review round 1", item 2): switching
//! between the schema SQL and a migration as the source, compiling base +
//! migration, publishing both, updating by re-applying the migration.

use std::collections::HashSet;

use axum::http::StatusCode;
use romulus_api::{
    AppState, Config,
    collab::{
        Hub,
        compile::{Compiled, SqlCompiler, input_hash, sha256_hex},
    },
    domain::{DraftMode, PgMajor},
};
use romulus_sandbox::{Action, Code, Diagnostic, LineIndex, ObjectKind};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{Map, Text, Transact};

use super::{
    harness::{Team, TestApp, database},
    live::{Client, model_of, quick, serve, version},
};

const V1: &str = "CREATE TABLE users (id bigint PRIMARY KEY);\nCREATE TABLE orders (id bigint);\n";

/// The live tests' compiler (a table per `CREATE TABLE`, an error at
/// `ERROR`), Postgres's rule for a table created twice (`relation already
/// exists`, at the second one), and a foreign table per `CREATE FOREIGN
/// TABLE`: something canonical SQL does not write back (`render_sql` skips
/// foreign tables), like the gaps Tidy refuses over.
fn mode_compiler() -> SqlCompiler {
    SqlCompiler::from_fn(mode_compiler_fn)
}

/// [`mode_compiler`]'s compile, to build others on.
fn mode_compiler_fn(sql: &str) -> Compiled {
    {
        let lines = LineIndex::new(sql);
        let error = |message: String, range: std::ops::Range<usize>| Compiled {
            model: None,
            diagnostics: vec![
                Diagnostic::error(Code::Sql, message).with_span(lines.span(sql, range)),
            ],
        };
        if let Some(at) = sql.find("ERROR") {
            return error("syntax error at or near \"ERROR\"".into(), at..at + 5);
        }
        let mut seen = HashSet::new();
        for statement in romulus_sandbox::parse(sql).statements {
            if statement.key.action() != Action::Create || statement.key.kind() != ObjectKind::Table
            {
                continue;
            }
            let name = statement.key.path().join(".");
            if !seen.insert(name.clone()) {
                return error(
                    format!("relation \"{name}\" already exists"),
                    statement.span.range(),
                );
            }
        }
        Compiled {
            model: Some(mode_model(sql)),
            diagnostics: Vec::new(),
        }
    }
}

/// [`model_of`], foreign tables included as such.
fn mode_model(sql: &str) -> romulus_api::domain::Schema {
    let mut model = serde_json::to_value(model_of(sql)).unwrap();
    let foreign: Vec<String> = romulus_sandbox::parse(sql)
        .statements
        .iter()
        .filter(|s| s.key.action() == Action::Create && s.key.kind() == ObjectKind::ForeignTable)
        .map(|s| s.key.path().last().unwrap().clone())
        .collect();
    for entity in model["entities"].as_array_mut().unwrap() {
        if foreign.iter().any(|name| entity["name"] == name.as_str()) {
            entity["kind"] = json!("f");
        }
    }
    <romulus_api::domain::Schema as serde::Deserialize>::deserialize(model)
        .unwrap()
        .canonical()
}

struct Shop {
    app: TestApp,
    team: Team,
    database: Uuid,
}

async fn shop(db: &PgPool) -> Shop {
    let hub = Hub::with_settings(db.clone(), mode_compiler(), quick());
    let app = TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_collab(hub));
    let team = Team::new(db, "acme").await;
    let database = database(db, team.org, "shop").await;
    version(
        db,
        database,
        1,
        V1,
        json!({"layout": {"public.users": {"x": 0.0, "y": 0.0}}}),
    )
    .await;
    Shop {
        app,
        team,
        database,
    }
}

impl Shop {
    async fn create(&self, body: Option<Value>) -> Value {
        let path = self.team.path("/databases/shop/drafts");
        let as_dev = self.app.as_user(&self.team.developer);
        let response = match body {
            Some(body) => as_dev.post(&path, &body).await,
            None => as_dev.post_empty(&path).await,
        };
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
        response.json()
    }

    fn path(&self, draft: &Value, rest: &str) -> String {
        self.team.path(&format!(
            "/databases/shop/drafts/{}{rest}",
            draft["id"].as_str().expect("id")
        ))
    }

    fn ws(draft: &Value) -> String {
        format!("/api/orgs/acme/drafts/{}/ws", draft["id"].as_str().unwrap())
    }

    async fn switch(&self, draft: &Value, mode: &str) -> super::harness::TestResponse {
        self.app
            .as_user(&self.team.developer)
            .post(&self.path(draft, "/mode"), &json!({ "mode": mode }))
            .await
    }
}

fn id_of(draft: &Value) -> Uuid {
    draft["id"].as_str().unwrap().parse().unwrap()
}

fn names(event: &Value) -> Vec<String> {
    event["model"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect()
}

fn append_to_migration(text: &str) -> impl FnOnce(&yrs::TextRef, &mut yrs::TransactionMut) {
    let text = text.to_owned();
    move |migration, txn| {
        let len = migration.len(txn);
        migration.insert(txn, len, &text);
    }
}

/// Waits for an `ok` compile of exactly the client's migration.
async fn compiled_migration(client: &mut Client) -> Value {
    client
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "ok" && e["sha256"] == sha256_hex(&c.migration()))
        })
        .await;
    client.last_event("compile").unwrap().clone()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn edit_migration_seeds_the_generated_migration_and_makes_it_the_source(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    assert_eq!(draft["mode"], "schema");
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    let mut bob = Client::join(addr, &Shop::ws(&draft), &shop.team.reviewer).await;
    alice
        .edit(|roots, txn| {
            let len = roots.sql.len(txn);
            roots
                .sql
                .insert(txn, len, "CREATE TABLE coupons (id bigint);\n");
        })
        .await;
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "ok" && names(e).contains(&"coupons".to_owned()))
        })
        .await;

    let switched = shop.switch(&draft, "migration").await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.json());
    assert_eq!(switched.json()["mode"], "migration");

    // Everyone gets the migration, the mode and who switched, at once.
    for client in [&mut alice, &mut bob] {
        client
            .until(|c| c.mode() == DraftMode::Migration && c.last_event("mode").is_some())
            .await;
        let migration = client.migration();
        assert!(
            migration.starts_with("-- acme/shop: migration from v1, written by hand."),
            "{migration}"
        );
        assert!(
            migration.contains("CREATE TABLE public.coupons"),
            "{migration}"
        );
        assert!(!migration.contains("BEGIN;"), "{migration}");
        let event = client.last_event("mode").unwrap();
        assert_eq!(
            (event["mode"].clone(), event["by"].clone()),
            (json!("migration"), json!("Developer-acme"))
        );
    }
    // The migration compiles on top of v1: the schema is the same, with
    // its SQL derived.
    let compiled = compiled_migration(&mut bob).await;
    assert_eq!(names(&compiled), ["coupons", "orders", "users"]);
    let schema_sql = compiled["schema_sql"].as_str().expect("schema_sql");
    assert!(schema_sql.contains("coupons"), "{schema_sql}");
    // The column follows the doc.
    let mode = sqlx::query_scalar!(
        r#"SELECT mode AS "mode: DraftMode" FROM drafts WHERE id = $1"#,
        id_of(&draft)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(mode, DraftMode::Migration);
    // Switching again is nothing.
    let again = shop.switch(&draft, "migration").await;
    assert_eq!(
        (again.status, again.json()["mode"].clone()),
        (StatusCode::OK, json!("migration"))
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_editors_switch_modes(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let path = shop.path(&draft, "/mode");
    let body = json!({"mode": "migration"});
    let reviewer = shop
        .app
        .as_user(&shop.team.reviewer)
        .post(&path, &body)
        .await;
    assert_eq!(
        (reviewer.status, reviewer.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let outsider = shop
        .app
        .as_user(&shop.team.outsider)
        .post(&path, &body)
        .await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    let invalid = shop
        .app
        .as_user(&shop.team.developer)
        .post(&path, &json!({"mode": "graph"}))
        .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    let admin = shop.app.as_user(&shop.team.admin).post(&path, &body).await;
    assert_eq!(admin.status, StatusCode::OK, "{}", admin.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn sql_with_errors_has_no_migration_to_start_from(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            let len = roots.sql.len(txn);
            roots.sql.insert(txn, len, "CREATE TABLE ERROR;\n");
        })
        .await;
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;
    let refused = shop.switch(&draft, "migration").await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "compile_failed")
    );
    assert_eq!(
        refused.json()["details"]["diagnostics"][0]["code"],
        json!("sql")
    );
    assert_eq!(alice.mode(), DraftMode::Schema);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_new_migration_compiles_on_its_base_with_errors_located_in_it(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    assert_eq!(
        (draft["name"].clone(), draft["mode"].clone()),
        (json!("Migration from v1"), json!("migration"))
    );
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    assert_eq!(alice.mode(), DraftMode::Migration);
    assert!(
        alice.migration().starts_with(
            "-- acme/shop: migration from v1, written by hand.\n-- Write what changes v1"
        ),
        "{}",
        alice.migration()
    );
    // The schema SQL is the base's, waiting.
    assert_eq!(alice.sql(), V1);
    // Nothing written yet: the base.
    let first = compiled_migration(&mut alice).await;
    assert_eq!(names(&first), ["orders", "users"]);

    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    let compiled = compiled_migration(&mut alice).await;
    assert_eq!(names(&compiled), ["invoices", "orders", "users"]);

    // An error is where it is in the migration, not in the text compiled.
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE orders (id int);\n")(&roots.migration, txn);
        })
        .await;
    alice
        .until(|c| {
            c.last_event("compile").is_some_and(|e| {
                e["state"] == "failed" && e["sha256"] == sha256_hex(&c.migration())
            })
        })
        .await;
    let failed = alice.last_event("compile").unwrap();
    let migration = alice.migration();
    let at = migration.find("CREATE TABLE orders").unwrap();
    let span = &failed["diagnostics"][0]["span"];
    assert_eq!(
        failed["diagnostics"][0]["message"],
        "relation \"public.orders\" already exists"
    );
    assert_eq!(span["start"], at);
    assert_eq!(span["utf16_start"], migration[..at].encode_utf16().count());
    assert_eq!(span["from"]["line"], migration[..at].lines().count() + 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_a_migration_stores_its_schema_and_the_migration(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\nUPDATE users SET id = id;\n")(
                &roots.migration,
                txn,
            );
        })
        .await;
    let compiled = compiled_migration(&mut alice).await;
    assert_eq!(names(&compiled), ["invoices", "orders", "users"]);
    let migration = alice.migration();

    let dev = shop.app.as_user(&shop.team.developer);
    let preview = dev.get(&shop.path(&draft, "/publish")).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json());
    let preview = preview.json();
    assert_eq!(preview["mode"], "migration");
    // What is reviewed is the migration on its base.
    assert_eq!(
        preview["sha256"],
        input_hash(DraftMode::Migration, V1, &migration)
    );
    assert_eq!(preview["migration"], migration.as_str());
    assert_eq!(preview["number"], 2);

    let published = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": preview["sha256"], "message": "Add invoices"}),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );

    let v2 = dev
        .get(&shop.team.path("/databases/shop/versions/2"))
        .await
        .json();
    assert_eq!(v2["origin"], "draft");
    assert_eq!(v2["source_kind"], "sql");
    assert_eq!(v2["written_migration"], true);
    let names: Vec<_> = v2["model"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].clone())
        .collect();
    assert_eq!(names, [json!("invoices"), json!("orders"), json!("users")]);
    // Its source is the derived schema, not the migration.
    let source = v2["source"].as_str().unwrap();
    assert!(
        source.contains("invoices") && !source.contains("UPDATE"),
        "{source}"
    );
    // The layout went with it, as today.
    assert_eq!(v2["layout"]["layout"]["public.users"]["x"], 0.0);

    // The migration, as written, downloadable later.
    let written = dev
        .get(&shop.team.path("/databases/shop/versions/2/migration"))
        .await;
    assert_eq!(written.status, StatusCode::OK, "{}", written.json());
    assert_eq!(
        written.json(),
        json!({
            "number": 2,
            "from": 1,
            "sql": migration,
            "file_name": "v2_add_invoices.sql",
        })
    );
    let reviewer = shop
        .app
        .as_user(&shop.team.reviewer)
        .get(&shop.team.path("/databases/shop/versions/2/migration"))
        .await;
    assert_eq!(reviewer.status, StatusCode::OK);
    let none = dev
        .get(&shop.team.path("/databases/shop/versions/1/migration"))
        .await;
    assert_eq!(
        (none.status, none.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let history = dev
        .get(&shop.team.path("/databases/shop/versions"))
        .await
        .json();
    assert_eq!(history["versions"][0]["written_migration"], true);
    assert_eq!(history["versions"][1]["written_migration"], false);
    let stored = sqlx::query_scalar!(
        "SELECT migration_sql FROM versions WHERE database_id = $1 AND number = 2",
        shop.database
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(stored.as_deref(), Some(migration.as_str()));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn discarding_the_migration_writes_the_derived_schema_as_sql(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    compiled_migration(&mut alice).await;

    let back = shop.switch(&draft, "schema").await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.json());
    assert_eq!(back.json()["mode"], "schema");
    alice
        .until(|c| c.mode() == DraftMode::Schema && c.migration().is_empty())
        .await;
    let sql = alice.sql();
    assert!(sql.contains("invoices") && sql.contains("users"), "{sql}");
    // The schema SQL is the source again.
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "ok" && e["sha256"] == sha256_hex(&c.sql()))
        })
        .await;
    let compiled = alice.last_event("compile").unwrap();
    assert_eq!(names(compiled), ["invoices", "orders", "users"]);
    assert_eq!(compiled.get("schema_sql"), None);
    // Updates merge like with like: the base, as canonical SQL.
    let base_sql = sqlx::query_scalar!("SELECT base_sql FROM drafts WHERE id = $1", id_of(&draft))
        .fetch_one(&db)
        .await
        .unwrap()
        .unwrap();
    assert!(
        base_sql.contains("users") && !base_sql.contains("invoices"),
        "{base_sql}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn updating_reapplies_the_migration_on_the_latest_version(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    compiled_migration(&mut alice).await;
    let migration = alice.migration();

    // Someone published v2 meanwhile: payments.
    version(
        &db,
        shop.database,
        2,
        &format!("{V1}CREATE TABLE payments (id bigint);\n"),
        json!({"layout": {"public.payments": {"x": 9.0, "y": 9.0}}}),
    )
    .await;
    let path = shop.path(&draft, "/update");
    let updated = shop
        .app
        .as_user(&shop.team.developer)
        .post_empty(&path)
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    assert_eq!(
        updated.json(),
        json!({"base_version": 2, "conflicts": 0, "changed": true})
    );
    // The migration is the same text, now on v2.
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "ok" && names(e).contains(&"payments".to_owned()))
        })
        .await;
    assert_eq!(alice.migration(), migration);
    assert_eq!(
        names(alice.last_event("compile").unwrap()),
        ["invoices", "orders", "payments", "users"]
    );
    // v2's layout came in.
    let positions = alice
        .roots
        .layout
        .get(&alice.doc.transact(), "public.payments");
    assert!(positions.is_some());

    // v3 adds invoices itself: the migration no longer applies, and says where.
    version(
        &db,
        shop.database,
        3,
        &format!("{V1}CREATE TABLE payments (id bigint);\nCREATE TABLE invoices (id bigint);\n"),
        json!({}),
    )
    .await;
    let updated = shop
        .app
        .as_user(&shop.team.developer)
        .post_empty(&path)
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    let body = updated.json();
    assert_eq!(body["base_version"], 3);
    let at = migration.find("CREATE TABLE invoices").unwrap();
    assert_eq!(body["diagnostics"][0]["span"]["start"], at);
    assert_eq!(
        body["diagnostics"][0]["message"],
        "relation \"public.invoices\" already exists"
    );
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;
    // Publishing waits until the migration is fixed.
    let preview = shop
        .app
        .as_user(&shop.team.developer)
        .get(&shop.path(&draft, "/publish"))
        .await
        .json();
    assert_eq!(preview["diff"], Value::Null);
    assert_eq!(preview["diagnostics"][0]["code"], "sql");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_the_server_writes_the_mode(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let addr = serve(&shop.app).await;
    let mut mallory = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    mallory
        .edit(|roots, txn| {
            roots.meta.insert(txn, "mode", "migration");
            roots
                .migration
                .insert(txn, 0, "CREATE TABLE sneaky (id int);");
        })
        .await;
    // Written back for everyone; the SQL stays the source.
    mallory.until(|c| c.mode() == DraftMode::Schema).await;
    let mut bob = Client::join(addr, &Shop::ws(&draft), &shop.team.admin).await;
    assert_eq!(bob.mode(), DraftMode::Schema);
    bob.until(|c| c.last_event("compile").is_some()).await;
    assert!(!names(bob.last_event("compile").unwrap()).contains(&"sneaky".to_owned()));
    let content = shop.app.state.collab.content(id_of(&draft)).await.unwrap();
    assert_eq!(content.mode, DraftMode::Schema);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn tidy_has_nothing_to_do_in_migration_mode(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let tidy = shop
        .app
        .as_user(&shop.team.developer)
        .post_empty(&shop.path(&draft, "/tidy"))
        .await;
    assert_eq!(
        (tidy.status, tidy.code().as_str()),
        (StatusCode::CONFLICT, "migration_mode")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_draft_survives_a_restart(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let switched = shop.switch(&draft, "migration").await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.json());
    shop.app.state.collab.shutdown().await;

    let hub = Hub::with_settings(db.clone(), mode_compiler(), quick());
    let restarted =
        TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_collab(hub));
    let addr = serve(&restarted).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    assert_eq!(alice.mode(), DraftMode::Migration);
    assert!(
        alice
            .migration()
            .starts_with("-- acme/shop: migration from v1")
    );
    assert_eq!(alice.sql(), V1);
    let compiled = compiled_migration(&mut alice).await;
    assert_eq!(names(&compiled), ["orders", "users"]);
    assert!(compiled["schema_sql"].as_str().is_some());
}

// ---- Review fixes ------------------------------------------------------------

/// The model `sql` compiles to with [`mode_compiler`], named like a version's.
async fn compiled_as(sql: &str) -> Option<Value> {
    let mut model = mode_compiler()
        .compile(PgMajor::DEFAULT, sql)
        .await
        .unwrap()
        .model?;
    "shop".clone_into(&mut model.database);
    Some(serde_json::to_value(model).unwrap())
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_published_migration_s_source_compiles_back_to_its_model(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE FOREIGN TABLE remote (id int) SERVER elsewhere;\n")(
                &roots.migration,
                txn,
            );
        })
        .await;
    let compiled = compiled_migration(&mut alice).await;
    assert_eq!(names(&compiled), ["orders", "remote", "users"]);

    let dev = shop.app.as_user(&shop.team.developer);
    let preview = dev.get(&shop.path(&draft, "/publish")).await.json();
    let published = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": preview["sha256"], "message": "Remote"}),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
    let v2 = dev
        .get(&shop.team.path("/databases/shop/versions/2"))
        .await
        .json();
    // Canonical SQL would drop the foreign table: the source is one that
    // says what the model says.
    let source = v2["source"].as_str().unwrap();
    assert_eq!(
        compiled_as(source).await,
        Some(v2["model"].clone()),
        "{source}"
    );

    // A draft from v2 starts with everything v2 has.
    let next = shop.create(None).await;
    let content = shop.app.state.collab.content(id_of(&next)).await.unwrap();
    assert_eq!(compiled_as(&content.sql).await, Some(v2["model"].clone()));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn discarding_keeps_what_canonical_sql_cannot_write(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE FOREIGN TABLE remote (id int) SERVER elsewhere;\n")(
                &roots.migration,
                txn,
            );
        })
        .await;
    let compiled = compiled_migration(&mut alice).await;
    let back = shop.switch(&draft, "schema").await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.json());
    alice.until(|c| c.mode() == DraftMode::Schema).await;
    let mut model = mode_compiler()
        .compile(PgMajor::DEFAULT, &alice.sql())
        .await
        .unwrap()
        .model
        .unwrap();
    "sandbox".clone_into(&mut model.database);
    assert_eq!(
        serde_json::to_value(model).unwrap(),
        compiled["model"],
        "{}",
        alice.sql()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_what_was_reviewed_on_another_base_is_refused(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    compiled_migration(&mut alice).await;
    let dev = shop.app.as_user(&shop.team.developer);
    let reviewed = dev.get(&shop.path(&draft, "/publish")).await.json();
    assert_eq!(reviewed["base_version"], 1);

    // v2 comes out and the draft is updated onto it: the same text, another base.
    version(
        &db,
        shop.database,
        2,
        &format!("{V1}CREATE TABLE payments (id bigint);\n"),
        json!({}),
    )
    .await;
    let updated = dev.post_empty(&shop.path(&draft, "/update")).await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());

    let published = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": reviewed["sha256"]}),
        )
        .await;
    assert_eq!(
        (published.status, published.code().as_str()),
        (StatusCode::CONFLICT, "draft_changed"),
        "{}",
        published.json()
    );
    // Reviewed again, it publishes.
    let again = dev.get(&shop.path(&draft, "/publish")).await.json();
    assert_ne!(again["sha256"], reviewed["sha256"]);
    let published = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": again["sha256"]}),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_switch_computed_on_an_older_base_is_refused(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let id = id_of(&draft);
    let v1 = sqlx::query_scalar!(
        "SELECT id FROM versions WHERE database_id = $1 AND number = 1",
        shop.database
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let before = shop.app.state.collab.content(id).await.unwrap();
    assert_eq!(before.base_version, Some(v1));
    version(
        &db,
        shop.database,
        2,
        &format!("{V1}CREATE TABLE payments (id bigint);\n"),
        json!({}),
    )
    .await;
    let dev = shop.app.as_user(&shop.team.developer);
    let updated = dev.post_empty(&shop.path(&draft, "/update")).await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    // The room's base moved with the update.
    let after = shop.app.state.collab.content(id).await.unwrap();
    assert_ne!(after.base_version, Some(v1));
    // A seed computed against v1 never lands on v2.
    let stale = shop
        .app
        .state
        .collab
        .switch_mode(
            id,
            DraftMode::Migration,
            sha256_hex(&after.sql),
            before.base_version,
            "-- from v1".into(),
            romulus_api::collab::store::BaseSql::Keep,
            "Developer-acme",
        )
        .await
        .unwrap();
    assert!(
        matches!(stale, Err(romulus_api::collab::room::RewriteError::Changed)),
        "{stale:?}"
    );
    // The endpoint seeds from v2.
    let switched = shop.switch(&draft, "migration").await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.json());
    let content = shop.app.state.collab.content(id).await.unwrap();
    assert!(
        content.migration.contains("migration from v2"),
        "{}",
        content.migration
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_migration_that_no_longer_applies_cannot_be_discarded(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    compiled_migration(&mut alice).await;
    // v2 already has invoices: on it, the migration fails.
    version(
        &db,
        shop.database,
        2,
        &format!("{V1}CREATE TABLE invoices (id bigint);\nCREATE TABLE payments (id bigint);\n"),
        json!({}),
    )
    .await;
    let dev = shop.app.as_user(&shop.team.developer);
    let updated = dev.post_empty(&shop.path(&draft, "/update")).await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;

    // Discarding now would write v1 + the migration as the schema, on v2.
    let refused = shop.switch(&draft, "schema").await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "compile_failed"),
        "{}",
        refused.json()
    );
    assert_eq!(alice.mode(), DraftMode::Migration);
    // Emptied, the migration applies (it is v2): then it can go.
    alice
        .edit(|roots, txn| {
            let len = roots.migration.len(txn);
            roots.migration.remove_range(txn, 0, len);
        })
        .await;
    compiled_migration(&mut alice).await;
    let back = shop.switch(&draft, "schema").await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.json());
    alice.until(|c| c.mode() == DraftMode::Schema).await;
    assert!(alice.sql().contains("payments"), "{}", alice.sql());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn edits_to_the_text_that_is_not_the_source_are_undone(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut late = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    // Typed in the SQL, as a tab that had not seen the switch would.
    late.edit(|roots, txn| roots.sql.insert(txn, 0, "CREATE TABLE lost (id int);\n"))
        .await;
    late.until(|c| c.sql() == V1 && c.last_event("stale_edit").is_some())
        .await;
    assert_eq!(late.last_event("stale_edit").unwrap()["mode"], "migration");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_lost_write_of_the_mode_is_repaired_from_the_doc(db: PgPool) {
    let shop = shop(&db).await;
    let draft = shop.create(None).await;
    let switched = shop.switch(&draft, "migration").await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.json());
    shop.app.state.collab.shutdown().await;
    // As if `drafts.mode` had not been written.
    sqlx::query!(
        "UPDATE drafts SET mode = 'schema' WHERE id = $1",
        id_of(&draft)
    )
    .execute(&db)
    .await
    .unwrap();

    let hub = Hub::with_settings(db.clone(), mode_compiler(), quick());
    let restarted =
        TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_collab(hub));
    let addr = serve(&restarted).await;
    let alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    assert_eq!(alice.mode(), DraftMode::Migration);
    restarted.state.collab.shutdown().await;
    let mode = sqlx::query_scalar!(
        r#"SELECT mode AS "mode: DraftMode" FROM drafts WHERE id = $1"#,
        id_of(&draft)
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(mode, DraftMode::Migration);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_switch_reaching_a_room_as_it_unloads_is_answered(db: PgPool) {
    // Rooms unload as soon as they are idle: a switch often arrives while
    // one writes everything before leaving, and must not be lost there.
    let settings = romulus_api::collab::room::Settings {
        idle_unload: std::time::Duration::ZERO,
        ..quick()
    };
    let hub = Hub::with_settings(db.clone(), mode_compiler(), settings);
    let mut state = AppState::new(db.clone(), Config::for_tests()).with_collab(hub);
    // Each switch back compiles its checks: roomy limits, this is about
    // unloading.
    state.limits = std::sync::Arc::new(romulus_api::rate::Limits {
        compile: romulus_api::rate::Limiter::new(1000, std::time::Duration::from_secs(1)),
        database_compiles: romulus_api::rate::Limiter::new(1000, std::time::Duration::from_secs(1)),
        ..romulus_api::rate::Limits::default()
    });
    let app = TestApp::with_state(state);
    let team = Team::new(&db, "acme").await;
    let shop_id = database(&db, team.org, "shop").await;
    version(&db, shop_id, 1, V1, json!({})).await;
    let shop = Shop {
        app,
        team,
        database: shop_id,
    };
    let draft = shop.create(None).await;
    for round in 0..15 {
        for mode in ["migration", "schema"] {
            let switched = shop.switch(&draft, mode).await;
            assert_eq!(
                switched.status,
                StatusCode::OK,
                "round {round}, to {mode}: {}",
                switched.json()
            );
            assert_eq!(switched.json()["mode"], mode);
        }
    }
}

/// A shop whose drafts compile with `compiler`, and whose limits are `limits`.
async fn shop_with(
    db: &PgPool,
    compiler: SqlCompiler,
    limits: Option<romulus_api::rate::Limits>,
) -> Shop {
    let hub = Hub::with_settings(db.clone(), compiler, quick());
    let mut state = AppState::new(db.clone(), Config::for_tests()).with_collab(hub);
    if let Some(limits) = limits {
        state.limits = std::sync::Arc::new(limits);
    }
    let app = TestApp::with_state(state);
    let team = Team::new(db, "acme").await;
    let database = database(db, team.org, "shop").await;
    version(db, database, 1, V1, json!({})).await;
    Shop {
        app,
        team,
        database,
    }
}

/// A migration draft whose migration adds a foreign table (canonical SQL
/// cannot write it), compiled.
async fn foreign_table_draft(shop: &Shop) -> (Value, Client) {
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE FOREIGN TABLE remote (id int) SERVER elsewhere;\n")(
                &roots.migration,
                txn,
            );
        })
        .await;
    let compiled = compiled_migration(&mut alice).await;
    assert_eq!(names(&compiled), ["orders", "remote", "users"]);
    (draft, alice)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_schema_no_sql_reproduces_is_refused_not_stored(db: PgPool) {
    // As if the base + migration form compiled to another schema too (a
    // stub guessed differently, say): here it loses the foreign table.
    let perturbed = SqlCompiler::from_fn(|sql| {
        let mut compiled = mode_compiler_fn(sql);
        if sql.contains("-- Then the migration that made this schema") {
            compiled.model = compiled.model.map(|mut model| {
                model.entities.retain(|e| e.name != "remote");
                model
            });
        }
        compiled
    });
    let shop = shop_with(&db, perturbed, None).await;
    let (draft, mut alice) = foreign_table_draft(&shop).await;
    let dev = shop.app.as_user(&shop.team.developer);

    let preview = dev.get(&shop.path(&draft, "/publish")).await.json();
    let refused = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "unrepresentable"),
        "{}",
        refused.json()
    );
    assert_eq!(
        refused.json()["details"]["objects"],
        json!(["public.remote"])
    );
    assert!(
        refused.json()["message"]
            .as_str()
            .unwrap()
            .contains("public.remote")
    );
    // Nothing was stored, and the draft is still open.
    let versions = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM versions WHERE database_id = $1"#,
        shop.database
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(versions, 1);
    let open = dev.get(&shop.path(&draft, "")).await.json();
    assert_eq!(open["status"], "open");

    // Discarding refuses for the same reason.
    let discard = shop.switch(&draft, "schema").await;
    assert_eq!(
        (discard.status, discard.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "unrepresentable"),
        "{}",
        discard.json()
    );
    alice.settle(std::time::Duration::from_millis(100)).await;
    assert_eq!(alice.mode(), DraftMode::Migration);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_checks_publishing_runs_count_against_the_compile_limit(db: PgPool) {
    // One compile per person: checking the canonical form takes it, the
    // base + migration form would be a second.
    let limits = romulus_api::rate::Limits {
        compile: romulus_api::rate::Limiter::new(1, std::time::Duration::from_secs(60)),
        ..romulus_api::rate::Limits::default()
    };
    let shop = shop_with(&db, mode_compiler(), Some(limits)).await;
    let (draft, _alice) = foreign_table_draft(&shop).await;
    let dev = shop.app.as_user(&shop.team.developer);
    let preview = dev.get(&shop.path(&draft, "/publish")).await.json();
    let limited = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({"sha256": preview["sha256"]}),
        )
        .await;
    assert_eq!(
        (limited.status, limited.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        "{}",
        limited.json()
    );
    assert!(limited.headers.contains_key("retry-after"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_approval_of_a_migration_is_of_it_on_its_base(db: PgPool) {
    let shop = shop(&db).await;
    let set = shop
        .app
        .as_user(&shop.team.admin)
        .patch(&shop.team.path(""), &json!({ "approvals_required": 1 }))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    let draft = shop.create(Some(json!({"mode": "migration"}))).await;
    let addr = serve(&shop.app).await;
    let mut alice = Client::join(addr, &Shop::ws(&draft), &shop.team.developer).await;
    alice
        .edit(|roots, txn| {
            append_to_migration("CREATE TABLE invoices (id bigint);\n")(&roots.migration, txn);
        })
        .await;
    compiled_migration(&mut alice).await;
    let migration = alice.migration();
    let reviewer = shop.app.as_user(&shop.team.reviewer);
    let dev = shop.app.as_user(&shop.team.developer);

    // The reviewer approves the migration as they see it.
    let approved = reviewer
        .post(
            &shop.path(&draft, "/approvals"),
            &json!({ "sha256": sha256_hex(&migration) }),
        )
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.json());
    assert_eq!(approved.json()["approvals"], 1);

    // v2 comes out and the draft is updated: the same text on another base
    // is another review, the approval no longer counts.
    version(
        &db,
        shop.database,
        2,
        &format!("{V1}CREATE TABLE payments (id bigint);\n"),
        json!({}),
    )
    .await;
    let updated = dev.post_empty(&shop.path(&draft, "/update")).await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json());
    let preview = dev.get(&shop.path(&draft, "/publish")).await.json();
    assert_eq!(preview["approvals"]["approved_by"], json!([]), "{preview}");
    let refused = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({ "sha256": preview["sha256"] }),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "approvals_required")
    );

    // Approved again on v2, it publishes, the approval kept with it.
    let again = reviewer
        .post(
            &shop.path(&draft, "/approvals"),
            &json!({ "sha256": sha256_hex(&migration) }),
        )
        .await;
    assert_eq!(again.json()["approvals"], 1, "{}", again.json());
    let preview = dev.get(&shop.path(&draft, "/publish")).await.json();
    let published = dev
        .post(
            &shop.path(&draft, "/publish"),
            &json!({ "sha256": preview["sha256"] }),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
    let v3 = dev
        .get(&shop.team.path("/databases/shop/versions/3"))
        .await
        .json();
    assert_eq!(v3["written_migration"], true);
    assert_eq!(v3["approvals"][0]["name"], "Reviewer-acme", "{v3}");
}
