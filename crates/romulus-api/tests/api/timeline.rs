//! The time machine's data: every version and the history of every object
//! across them, who may read it, removed versions, and the cache of steps.

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

use crate::harness::{Team, TestApp, api_token, database, model_json};

/// A model of `tables`, each a list of `(column, type)`.
fn model(tables: &[(&str, &[(&str, &str)])]) -> String {
    let entities: Vec<Value> = tables
        .iter()
        .map(|(name, columns)| {
            json!({
                "schema": "public", "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null,
                "columns": columns.iter().map(|(c, t)| json!({
                    "name": c, "type": t, "type_schema": "pg_catalog", "type_name": t,
                    "type_kind": "b", "not_null": false, "default": null, "identity": null,
                    "generated": null, "comment": null
                })).collect::<Vec<_>>()
            })
        })
        .collect();
    json!({ "remus_version": "0.2", "database": "shop", "server_version": "17",
            "entities": entities })
    .to_string()
}

struct Shop {
    app: TestApp,
    team: Team,
    id: Uuid,
    token: String,
}

impl Shop {
    async fn new(db: PgPool) -> Self {
        let team = Team::new(&db, "acme").await;
        let id = database(&db, team.org, "shop").await;
        let token = api_token(&db, id, TokenScope::Push).await;
        Self {
            app: TestApp::new(db),
            team,
            id,
            token,
        }
    }

    async fn push(&self, body: String) {
        let response = self
            .app
            .push(self.id, &self.token, Some("application/json"), body)
            .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    }

    async fn timeline(&self, path: &str) -> Value {
        let response = self
            .app
            .as_user(&self.team.reviewer)
            .get(&self.team.path(&format!("/databases/shop/timeline{path}")))
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        response.json()
    }
}

fn spans(states: &Value) -> Vec<(i64, Option<i64>)> {
    states
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["from"].as_i64().unwrap(), s["to"].as_i64()))
        .collect()
}

fn table<'a>(timeline: &'a Value, id: &str) -> &'a Value {
    timeline["history"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("no table {id} in {timeline}"))
}

fn part<'a>(table: &'a Value, kind: &str, id: &str) -> &'a Value {
    table[kind]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

fn numbers(timeline: &Value) -> Vec<i64> {
    timeline["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["number"].as_i64().unwrap())
        .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_timeline_has_every_version_and_the_history_of_each_object(db: PgPool) {
    let shop = Shop::new(db).await;
    shop.push(model(&[("users", &[("id", "bigint"), ("email", "text")])]))
        .await;
    shop.push(model(&[
        ("users", &[("id", "bigint"), ("email", "text")]),
        ("orders", &[("id", "bigint")]),
    ]))
    .await;
    shop.push(model(&[
        ("users", &[("id", "bigint"), ("email", "citext")]),
        ("orders", &[("id", "bigint")]),
    ]))
    .await;
    shop.push(model(&[(
        "users",
        &[("id", "bigint"), ("email", "citext")],
    )]))
    .await;

    let timeline = shop.timeline("").await;
    assert_eq!(numbers(&timeline), [1, 2, 3, 4]);
    assert_eq!(timeline["earlier"], 0);
    let v = &timeline["versions"][0];
    assert_eq!(v["origin"], "push");
    assert_eq!(v["author"]["kind"], "token");
    assert_eq!(v["removed_at"], Value::Null);

    let users = table(&timeline, "public.users");
    assert_eq!(users["schema"], "public");
    assert_eq!(spans(&users["states"]), [(0, None)]);
    let email = part(users, "columns", "email");
    assert_eq!(spans(&email["states"]), [(0, Some(2)), (2, None)]);
    assert_eq!(email["states"][1]["value"]["type"], "citext");
    assert_eq!(
        email["states"][1]["changes"],
        json!([{ "field": "type", "before": "text", "after": "citext" }])
    );
    let orders = table(&timeline, "public.orders");
    assert_eq!(spans(&orders["states"]), [(1, Some(3))]);
    assert_eq!(
        spans(&part(orders, "columns", "id")["states"]),
        [(1, Some(3))]
    );
    // A head is the entity without its parts, its columns in order.
    assert_eq!(orders["states"][0]["value"]["column_order"], json!(["id"]));
    assert_eq!(orders["states"][0]["value"].get("columns"), None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_member_reads_it_and_nobody_else(db: PgPool) {
    let shop = Shop::new(db.clone()).await;
    shop.push(model_json(&["users"])).await;
    let other = Team::new(&db, "globex").await;
    let path = shop.team.path("/databases/shop/timeline");
    for session in [&shop.team.admin, &shop.team.developer, &shop.team.reviewer] {
        assert_eq!(
            shop.app.as_user(session).get(&path).await.status,
            StatusCode::OK
        );
    }
    // Not a member: no such database, whatever it holds.
    let outsider = shop.app.as_user(&shop.team.outsider).get(&path).await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    let elsewhere = shop.app.as_user(&other.admin).get(&path).await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);
    // Another organization's database of the same slug is theirs.
    let theirs = shop
        .app
        .as_user(&other.admin)
        .get(&other.path("/databases/shop/timeline"))
        .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    let anonymous = shop.app.get(&path).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let unknown = shop
        .app
        .as_user(&shop.team.reviewer)
        .get(&shop.team.path("/databases/nope/timeline"))
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let bad = shop
        .app
        .as_user(&shop.team.reviewer)
        .get(&format!("{path}?removed=maybe"))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_database_without_versions_has_an_empty_timeline(db: PgPool) {
    let shop = Shop::new(db).await;
    let timeline = shop.timeline("").await;
    assert_eq!(numbers(&timeline), Vec::<i64>::new());
    assert_eq!(timeline["history"]["tables"], json!([]));
    assert_eq!(timeline["layout"], json!({ "layout": {}, "groups": {} }));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn removed_versions_are_left_out_unless_an_admin_asks_for_them(db: PgPool) {
    let shop = Shop::new(db).await;
    shop.push(model_json(&["users"])).await;
    shop.push(model_json(&["users", "mistake"])).await;
    shop.push(model_json(&["users", "orders"])).await;
    let removed = shop
        .app
        .as_user(&shop.team.admin)
        .post(
            &shop.team.path("/databases/shop/versions/2/remove"),
            &json!({ "reason": "Pushed from staging" }),
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.json());

    // The history goes from v1 straight to v3: `mistake` never was.
    let timeline = shop.timeline("").await;
    assert_eq!(numbers(&timeline), [1, 3]);
    assert!(
        timeline["history"]["tables"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != "public.mistake")
    );
    assert_eq!(
        spans(&table(&timeline, "public.orders")["states"]),
        [(1, None)]
    );

    // Developers and reviewers do not see removed versions.
    let path = shop.team.path("/databases/shop/timeline?removed=true");
    for session in [&shop.team.developer, &shop.team.reviewer] {
        let refused = shop.app.as_user(session).get(&path).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.code(), "forbidden");
    }
    let admin = shop.app.as_user(&shop.team.admin).get(&path).await;
    assert_eq!(admin.status, StatusCode::OK);
    let all = admin.json();
    assert_eq!(numbers(&all), [1, 2, 3]);
    assert_eq!(all["versions"][1]["removal_reason"], "Pushed from staging");
    assert_ne!(all["versions"][1]["removed_at"], Value::Null);
    assert_eq!(all["versions"][0]["removed_at"], Value::Null);
    assert_eq!(
        spans(&table(&all, "public.mistake")["states"]),
        [(1, Some(2))]
    );

    // Restored, it is back for everyone.
    let restored = shop
        .app
        .as_user(&shop.team.admin)
        .post_empty(&shop.team.path("/databases/shop/versions/2/restore"))
        .await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.json());
    assert_eq!(numbers(&shop.timeline("").await), [1, 2, 3]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn steps_are_computed_once_and_a_new_version_adds_one(db: PgPool) {
    let shop = Shop::new(db).await;
    shop.push(model_json(&["users"])).await;
    shop.push(model_json(&["users", "orders"])).await;
    let first = shop.timeline("").await;
    assert_eq!(shop.app.state.timelines.len(), 2);
    assert_eq!(shop.app.state.timelines.histories_len(), 1);
    // Read again: the same, the built history from the cache.
    assert_eq!(shop.timeline("").await, first);
    assert_eq!(shop.app.state.timelines.len(), 2);
    assert_eq!(shop.app.state.timelines.histories_len(), 1);

    shop.push(model_json(&["orders"])).await;
    let after = shop.timeline("").await;
    assert_eq!(numbers(&after), [1, 2, 3]);
    assert_eq!(shop.app.state.timelines.len(), 3);
    assert_eq!(
        spans(&table(&after, "public.users")["states"]),
        [(0, Some(2))]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_layout_places_every_table_the_history_had(db: PgPool) {
    let shop = Shop::new(db.clone()).await;
    let version = |tables: &[&str], layout: Value| {
        let model = crate::harness::model(tables);
        NewVersion {
            database_id: shop.id,
            source: serde_json::to_string(&model).unwrap(),
            model,
            source_kind: SourceKind::RemusJson,
            origin: VersionOrigin::Push,
            message: None,
            created_by: None,
            created_by_token: None,
            layout: Some(VersionLayout::from_value(&layout)),
            expected_base: None,
            publishes_draft: None,
            approvals: None,
            test_override: None,
            migration_sql: None,
            compiled_on: None,
            discussion: Vec::new(),
            renames: None,
        }
    };
    store(
        &db,
        version(
            &["users", "legacy"],
            json!({ "layout": { "public.users": { "x": 0, "y": 0 },
                                "public.legacy": { "x": 400, "y": 0 } },
                    "groups": {} }),
        ),
        false,
    )
    .await
    .unwrap();
    store(
        &db,
        version(
            &["users"],
            json!({ "layout": { "public.users": { "x": 50, "y": 60 } }, "groups": {} }),
        ),
        false,
    )
    .await
    .unwrap();
    let timeline = shop.timeline("").await;
    assert_eq!(
        timeline["layout"]["layout"],
        json!({ "public.users": { "x": 50.0, "y": 60.0 },
                "public.legacy": { "x": 400.0, "y": 0.0 } })
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admins_view_with_removed_versions_never_reaches_a_member(db: PgPool) {
    let shop = Shop::new(db).await;
    shop.push(model_json(&["users"])).await;
    shop.push(model_json(&["users", "mistake"])).await;
    shop.push(model_json(&["users", "orders"])).await;
    let removed = shop
        .app
        .as_user(&shop.team.admin)
        .post(
            &shop.team.path("/databases/shop/versions/2/remove"),
            &json!({ "reason": "Pushed from staging" }),
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK);

    // The admin asks first: the history through the removed version is built
    // and cached.
    let admin = shop
        .app
        .as_user(&shop.team.admin)
        .get(&shop.team.path("/databases/shop/timeline?removed=true"))
        .await;
    assert_eq!(numbers(&admin.json()), [1, 2, 3]);
    // Then a member, and the admin without removed versions: neither gets it.
    for session in [&shop.team.reviewer, &shop.team.admin] {
        let timeline = shop
            .app
            .as_user(session)
            .get(&shop.team.path("/databases/shop/timeline"))
            .await
            .json();
        assert_eq!(numbers(&timeline), [1, 3]);
        assert!(
            timeline["history"]["tables"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["id"] != "public.mistake"),
            "{timeline}"
        );
        assert!(
            timeline["versions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["removed_at"].is_null())
        );
    }
    assert_eq!(shop.app.state.timelines.histories_len(), 2);
}
