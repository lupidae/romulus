//! Databases: create, list, read, update, soft delete, and who may.

use axum::http::StatusCode;
use romulus_api::domain::TokenScope;
use serde_json::json;
use sqlx::PgPool;

use crate::harness::{Team, TestApp, api_token, database, model_json, org};

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_developer_creates_a_database_and_every_member_sees_it(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let response = app
        .as_user(&team.developer)
        .post(
            &team.path("/databases"),
            &json!({ "name": "  Billing  ", "slug": "billing", "description": "Invoices and plans" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    let created = response.json();
    assert_eq!(created["name"], "Billing");
    assert_eq!(created["slug"], "billing");
    assert_eq!(created["description"], "Invoices and plans");
    assert!(created["latest"].is_null());

    let list = app.as_user(&team.reviewer).get(&team.path("/databases")).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    let one = app
        .as_user(&team.reviewer)
        .get(&team.path("/databases/billing"))
        .await;
    assert_eq!(one.json()["id"], created["id"]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_role_matrix_holds(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "billing").await;
    let app = TestApp::new(db);
    let body = json!({ "name": "Other", "slug": "other" });

    // Reviewers read, but neither create, edit nor delete.
    let reviewer = app.as_user(&team.reviewer);
    assert_eq!(reviewer.get(&team.path("/databases")).await.status, StatusCode::OK);
    assert_eq!(
        reviewer.get(&team.path("/databases/billing")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        reviewer.post(&team.path("/databases"), &body).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reviewer
            .patch(&team.path("/databases/billing"), &json!({ "name": "x" }))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reviewer.delete(&team.path("/databases/billing")).await.status,
        StatusCode::FORBIDDEN
    );

    // Outsiders do not learn the organization exists.
    let outsider = app.as_user(&team.outsider);
    for response in [
        outsider.get(&team.path("/databases")).await,
        outsider.get(&team.path("/databases/billing")).await,
        outsider.post(&team.path("/databases"), &body).await,
        outsider.delete(&team.path("/databases/billing")).await,
    ] {
        assert_eq!(response.status, StatusCode::NOT_FOUND);
    }

    // Signed out: 401.
    assert_eq!(
        app.get(&team.path("/databases")).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Admins can do everything developers can.
    let admin = app.as_user(&team.admin);
    assert_eq!(
        admin.post(&team.path("/databases"), &body).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        admin
            .patch(&team.path("/databases/other"), &json!({ "name": "Renamed" }))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        admin.delete(&team.path("/databases/other")).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn bad_input_names_the_field(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let dev = app.as_user(&team.developer);
    for (body, field) in [
        (json!({ "name": " ", "slug": "a" }), "name"),
        (json!({ "name": "x".repeat(101), "slug": "a" }), "name"),
        (json!({ "name": "A", "slug": "Not A Slug" }), "slug"),
        (json!({ "name": "A", "slug": "settings" }), "slug"),
        (json!({ "name": "A", "slug": "a", "description": "x".repeat(1001) }), "description"),
    ] {
        let response = dev.post(&team.path("/databases"), &body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            response.json()["details"][field].is_string(),
            "{field}: {}",
            response.json()
        );
    }
    let missing = dev.post(&team.path("/databases"), &json!({ "name": "A" })).await;
    assert_eq!(missing.code(), "validation");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn slugs_are_unique_among_live_databases_of_one_org(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let other = org(&db, "globex").await;
    database(&db, other, "billing").await;
    let app = TestApp::new(db);
    let dev = app.as_user(&team.developer);
    let create = json!({ "name": "Billing", "slug": "billing" });

    // Another organization's slug is no obstacle.
    assert_eq!(
        dev.post(&team.path("/databases"), &create).await.status,
        StatusCode::CREATED
    );
    let taken = dev.post(&team.path("/databases"), &create).await;
    assert_eq!(
        (taken.status, taken.code().as_str()),
        (StatusCode::CONFLICT, "slug_taken")
    );

    // Renaming onto a taken slug is refused the same way.
    dev.post(&team.path("/databases"), &json!({ "name": "B", "slug": "b" }))
        .await;
    let moved = dev
        .patch(&team.path("/databases/b"), &json!({ "slug": "billing" }))
        .await;
    assert_eq!(moved.code(), "slug_taken");

    // Deleting frees the slug.
    assert_eq!(
        dev.delete(&team.path("/databases/billing")).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        dev.post(&team.path("/databases"), &create).await.status,
        StatusCode::CREATED
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn updates_change_only_what_is_given(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let dev = app.as_user(&team.developer);
    dev.post(
        &team.path("/databases"),
        &json!({ "name": "Billing", "slug": "billing", "description": "Plans" }),
    )
    .await;

    let response = dev
        .patch(&team.path("/databases/billing"), &json!({ "slug": "payments" }))
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let updated = response.json();
    assert_eq!(
        (&updated["name"], &updated["slug"], &updated["description"]),
        (&json!("Billing"), &json!("payments"), &json!("Plans"))
    );
    // The old address is gone.
    assert_eq!(
        dev.get(&team.path("/databases/billing")).await.status,
        StatusCode::NOT_FOUND
    );

    let cleared = dev
        .patch(&team.path("/databases/payments"), &json!({ "description": "" }))
        .await
        .json();
    assert_eq!(cleared["description"], "");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_deleted_database_disappears_and_its_tokens_stop_working(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let id = database(&db, team.org, "billing").await;
    let token = api_token(&db, id, TokenScope::Push).await;
    let app = TestApp::new(db.clone());
    let dev = app.as_user(&team.developer);

    assert_eq!(
        dev.delete(&team.path("/databases/billing")).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        dev.get(&team.path("/databases/billing")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        dev.delete(&team.path("/databases/billing")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(dev.get(&team.path("/databases")).await.json(), json!([]));

    let push = app
        .push(id, &token, Some("application/json"), model_json(&["users"]))
        .await;
    assert_eq!(
        (push.status, push.code().as_str()),
        (StatusCode::UNAUTHORIZED, "invalid_token")
    );
    let revoked: bool = sqlx::query_scalar!(
        r#"SELECT revoked_at IS NOT NULL AS "revoked!" FROM api_tokens WHERE database_id = $1"#,
        id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(revoked);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_list_shows_each_latest_version_most_recent_first(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    database(&db, team.org, "quiet").await;
    let busy = database(&db, team.org, "busy").await;
    let token = api_token(&db, busy, TokenScope::Push).await;
    let app = TestApp::new(db);
    for tables in [&["users"][..], &["users", "orders"]] {
        let response = app
            .push(busy, &token, Some("application/json"), model_json(tables))
            .await;
        assert_eq!(response.status, StatusCode::CREATED);
    }

    let list = app.as_user(&team.reviewer).get(&team.path("/databases")).await.json();
    let list = list.as_array().unwrap();
    assert_eq!(list[0]["slug"], "busy");
    assert_eq!(list[0]["latest"]["number"], 2);
    assert_eq!(list[0]["latest"]["origin"], "push");
    assert_eq!(list[0]["latest"]["author"]["kind"], "token");
    assert_eq!(list[0]["latest"]["author"]["name"], "Push token");
    assert_eq!(list[1]["slug"], "quiet");
    assert!(list[1]["latest"].is_null());
}
