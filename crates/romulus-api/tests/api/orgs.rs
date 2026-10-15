//! Organizations: create, list mine, get, rename; and the role matrix of
//! every organization endpoint.

use axum::http::{Method, StatusCode};
use romulus_api::domain::Role;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::harness::{Team, TestApp, member, org, signed_in, user};

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_creator_becomes_admin(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let app = TestApp::new(db);
    let response = app
        .as_user(&alice)
        .post(
            "/api/orgs",
            &json!({ "name": "  Acme Inc. ", "slug": "acme" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED);
    let created = response.json();
    assert_eq!(created["name"], "Acme Inc.");
    assert_eq!(created["slug"], "acme");
    assert_eq!(created["role"], "admin");

    let mine = app.as_user(&alice).get("/api/orgs").await.json();
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(mine[0]["slug"], "acme");
    let members = app
        .as_user(&alice)
        .get("/api/orgs/acme/members")
        .await
        .json();
    assert_eq!(members[0]["role"], "admin");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn slugs_are_unique(db: PgPool) {
    org(&db, "acme").await;
    let alice = signed_in(&db, "Alice").await;
    let response = TestApp::new(db)
        .as_user(&alice)
        .post("/api/orgs", &json!({ "name": "Acme", "slug": "acme" }))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "slug_taken");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn invalid_names_and_slugs_name_their_field(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let app = TestApp::new(db.clone());
    let cases = [
        (json!({ "name": " ", "slug": "acme" }), "name"),
        (json!({ "name": "x".repeat(101), "slug": "acme" }), "name"),
        (json!({ "name": "Acme", "slug": "Acme" }), "slug"),
        (json!({ "name": "Acme", "slug": "acme-" }), "slug"),
        (json!({ "name": "Acme", "slug": "" }), "slug"),
        (json!({ "name": "Settings", "slug": "settings" }), "slug"),
        (json!({ "name": "Invite", "slug": "invite" }), "slug"),
    ];
    for (body, field) in cases {
        let response = app.as_user(&alice).post("/api/orgs", &body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(response.code(), "validation");
        assert!(response.json()["details"][field].is_string(), "{body}");
    }
    let orgs = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM organizations"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(orgs, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_database_refuses_reserved_slugs_too(db: PgPool) {
    let err = sqlx::query!("INSERT INTO organizations (name, slug) VALUES ('App', 'app')")
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(
        romulus_api::db::constraint(&err),
        Some("organizations_slug_not_reserved")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn i_list_only_my_organizations_with_my_role(db: PgPool) {
    let alice = signed_in(&db, "Alice").await;
    let (zeta, beta, other) = (
        org(&db, "zeta").await,
        org(&db, "beta").await,
        org(&db, "other").await,
    );
    member(&db, zeta, alice.user, Role::Reviewer).await;
    member(&db, beta, alice.user, Role::Admin).await;
    let bob = user(&db, "Bob").await;
    member(&db, other, bob, Role::Admin).await;

    let mine = TestApp::new(db)
        .as_user(&alice)
        .get("/api/orgs")
        .await
        .json();
    let mine: Vec<(&str, &str)> = mine
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["slug"].as_str().unwrap(), o["role"].as_str().unwrap()))
        .collect();
    assert_eq!(mine, [("beta", "admin"), ("zeta", "reviewer")]);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_renames_and_moves_the_organization(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    org(&db, "taken").await;
    let app = TestApp::new(db);
    let admin = app.as_user(&team.admin);

    let renamed = admin
        .patch(&team.path(""), &json!({ "name": "Acme Labs" }))
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json()["name"], "Acme Labs");
    assert_eq!(renamed.json()["slug"], "acme");

    let taken = admin
        .patch(&team.path(""), &json!({ "slug": "taken" }))
        .await;
    assert_eq!(taken.code(), "slug_taken");

    let moved = admin
        .patch(&team.path(""), &json!({ "slug": "acme-labs" }))
        .await;
    assert_eq!(moved.json()["slug"], "acme-labs");
    assert_eq!(
        admin.get("/api/orgs/acme-labs").await.status,
        StatusCode::OK
    );
    assert_eq!(
        admin.get("/api/orgs/acme").await.status,
        StatusCode::NOT_FOUND
    );
}

/// Who may call what (docs/MVP.md "Roles"). Each case runs against a fresh
/// team, so a successful removal cannot hide the next case's answer.
#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_organization_endpoint_enforces_the_role_matrix(db: PgPool) {
    struct Case {
        method: Method,
        path: &'static str,
        body: Option<Value>,
        min: Role,
        ok: StatusCode,
    }
    let case = |method, path, body, min, ok| Case {
        method,
        path,
        body,
        min,
        ok,
    };
    let cases = [
        case(Method::GET, "", None, Role::Reviewer, StatusCode::OK),
        case(
            Method::PATCH,
            "",
            Some(json!({ "name": "Renamed" })),
            Role::Admin,
            StatusCode::OK,
        ),
        case(
            Method::GET,
            "/members",
            None,
            Role::Reviewer,
            StatusCode::OK,
        ),
        case(
            Method::PATCH,
            "/members/{target}",
            Some(json!({ "role": "reviewer" })),
            Role::Admin,
            StatusCode::OK,
        ),
        case(
            Method::DELETE,
            "/members/{target}",
            None,
            Role::Admin,
            StatusCode::NO_CONTENT,
        ),
        case(
            Method::GET,
            "/invitations",
            None,
            Role::Admin,
            StatusCode::OK,
        ),
        case(
            Method::POST,
            "/invitations",
            Some(json!({ "email": "new@romulus.test", "role": "developer" })),
            Role::Admin,
            StatusCode::CREATED,
        ),
        case(
            Method::DELETE,
            "/invitations/{invitation}",
            None,
            Role::Admin,
            StatusCode::NO_CONTENT,
        ),
    ];

    let app = TestApp::new(db.clone());
    let mut n = 0;
    for case in &cases {
        for actor in ["admin", "developer", "reviewer", "outsider", "anonymous"] {
            n += 1;
            let team = Team::new(&db, &format!("org-{n}")).await;
            let target = user(&db, &format!("Target-{n}")).await;
            member(&db, team.org, target, Role::Developer).await;
            let invitation = sqlx::query_scalar!(
                "INSERT INTO invitations (org_id, email, role, token_hash, expires_at)
                 VALUES ($1, 'pending@romulus.test', 'reviewer', $2, now() + interval '7 days') RETURNING id",
                team.org,
                romulus_api::token::hash(&romulus_api::token::generate()),
            )
            .fetch_one(&db)
            .await
            .unwrap();
            let path = team.path(
                &case
                    .path
                    .replace("{target}", &target.to_string())
                    .replace("{invitation}", &invitation.to_string()),
            );
            let session = match actor {
                "admin" => Some(&team.admin),
                "developer" => Some(&team.developer),
                "reviewer" => Some(&team.reviewer),
                "outsider" => Some(&team.outsider),
                _ => None,
            };
            let expected = match actor {
                "anonymous" => StatusCode::UNAUTHORIZED,
                "outsider" => StatusCode::NOT_FOUND,
                role => {
                    let role: Role = serde_json::from_value(json!(role)).unwrap();
                    if role.at_least(case.min) {
                        case.ok
                    } else {
                        StatusCode::FORBIDDEN
                    }
                }
            };
            let response = app
                .call(case.method.clone(), &path, session, case.body.as_ref())
                .await;
            assert_eq!(
                response.status,
                expected,
                "{} {} as {actor}: {}",
                case.method,
                case.path,
                String::from_utf8_lossy(&response.body)
            );
        }
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn signed_out_visitors_cannot_list_or_create(db: PgPool) {
    let app = TestApp::new(db);
    assert_eq!(app.get("/api/orgs").await.status, StatusCode::UNAUTHORIZED);
    let response = app
        .post_json("/api/orgs", &json!({ "name": "Acme", "slug": "acme" }))
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reviewer_is_told_who_can(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let response = TestApp::new(db)
        .as_user(&team.reviewer)
        .patch(&team.path(""), &json!({ "name": "Mine now" }))
        .await;
    assert_eq!(response.code(), "forbidden");
    assert!(
        response.json()["message"]
            .as_str()
            .unwrap()
            .contains("admins")
    );
}
