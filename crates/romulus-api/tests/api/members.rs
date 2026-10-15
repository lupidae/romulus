//! Members: list, change role, remove, leave; the last-admin rule through
//! the API; and the events that close websockets.

use axum::http::StatusCode;
use romulus_api::{domain::Role, orgs::events::MembershipChange};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::broadcast::error::TryRecvError;

use crate::harness::{Team, TestApp, member, session, user};

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn members_see_everyone_with_their_role(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let members = TestApp::new(db)
        .as_user(&team.reviewer)
        .get(&team.path("/members"))
        .await
        .json();
    let members: Vec<(&str, &str)> = members
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["name"].as_str().unwrap(), m["role"].as_str().unwrap()))
        .collect();
    assert_eq!(
        members,
        [
            ("Admin-acme", "admin"),
            ("Developer-acme", "developer"),
            ("Reviewer-acme", "reviewer")
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_changes_a_role_and_their_sockets_hear_it(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let mut events = app.state.membership.subscribe();

    let response = app
        .as_user(&team.admin)
        .patch(
            &team.path(&format!("/members/{}", team.developer.user)),
            &json!({ "role": "reviewer" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["role"], "reviewer");
    assert_eq!(
        events.try_recv().unwrap(),
        MembershipChange::RoleChanged {
            org_id: team.org,
            user_id: team.developer.user,
            role: Role::Reviewer,
        }
    );

    // The same role again changes nothing, so nothing is closed.
    app.as_user(&team.admin)
        .patch(
            &team.path(&format!("/members/{}", team.developer.user)),
            &json!({ "role": "reviewer" }),
        )
        .await;
    assert_eq!(events.try_recv(), Err(TryRecvError::Empty));

    // The demoted developer is a reviewer from their very next request.
    let response = app
        .as_user(&team.developer)
        .get(&team.path("/invitations"))
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_last_admin_cannot_be_demoted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let mut events = app.state.membership.subscribe();
    let response = app
        .as_user(&team.admin)
        .patch(
            &team.path(&format!("/members/{}", team.admin.user)),
            &json!({ "role": "developer" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "last_admin");
    assert_eq!(
        events.try_recv(),
        Err(TryRecvError::Empty),
        "nothing committed, nothing published"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_steps_down_once_there_is_another(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let admin = app.as_user(&team.admin);
    admin
        .patch(
            &team.path(&format!("/members/{}", team.developer.user)),
            &json!({ "role": "admin" }),
        )
        .await;
    let response = admin
        .patch(
            &team.path(&format!("/members/{}", team.admin.user)),
            &json!({ "role": "reviewer" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    // And is now a reviewer.
    assert_eq!(
        admin.get(&team.path("/invitations")).await.status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn concurrent_demotions_leave_one_admin(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    member(&db, team.org, user(&db, "Second").await, Role::Admin).await;
    let second = sqlx::query_scalar!("SELECT id FROM users WHERE name = 'Second'")
        .fetch_one(&db)
        .await
        .unwrap();
    let second = session(&db, second).await;
    let app = TestApp::new(db.clone());

    // Each admin demotes themselves at the same time.
    let (first, other) = (app.as_user(&team.admin), app.as_user(&second));
    let (first_path, other_path) = (
        team.path(&format!("/members/{}", team.admin.user)),
        team.path(&format!("/members/{}", second.user)),
    );
    let body = json!({ "role": "developer" });
    let (a, b) = tokio::join!(
        first.patch(&first_path, &body),
        other.patch(&other_path, &body)
    );
    let mut statuses = [a.status, b.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let admins = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM memberships WHERE org_id = $1 AND role = 'admin'"#,
        team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(admins, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_removes_a_member(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let mut events = app.state.membership.subscribe();
    let response = app
        .as_user(&team.admin)
        .delete(&team.path(&format!("/members/{}", team.developer.user)))
        .await;
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert_eq!(
        events.try_recv().unwrap(),
        MembershipChange::Removed {
            org_id: team.org,
            user_id: team.developer.user
        }
    );
    // Gone at once: the organization no longer exists for them.
    let response = app.as_user(&team.developer).get(&team.path("")).await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    // And removing them again is a 404.
    let again = app
        .as_user(&team.admin)
        .delete(&team.path(&format!("/members/{}", team.developer.user)))
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn any_member_can_leave(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let response = app
        .as_user(&team.reviewer)
        .delete(&team.path(&format!("/members/{}", team.reviewer.user)))
        .await;
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    // But not remove someone else.
    let response = app
        .as_user(&team.developer)
        .delete(&team.path(&format!("/members/{}", team.admin.user)))
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_last_admin_cannot_leave(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let response = TestApp::new(db)
        .as_user(&team.admin)
        .delete(&team.path(&format!("/members/{}", team.admin.user)))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "last_admin");
    assert!(
        response.json()["message"]
            .as_str()
            .unwrap()
            .contains("before you leave")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn changing_someone_outside_the_organization_is_a_404(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let other = Team::new(&db, "other").await;
    let app = TestApp::new(db);
    let response = app
        .as_user(&team.admin)
        .patch(
            &team.path(&format!("/members/{}", other.developer.user)),
            &json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    let response = app
        .as_user(&team.admin)
        .patch(
            &team.path("/members/not-a-uuid"),
            &json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unknown_role_is_a_validation_error(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let response = TestApp::new(db)
        .as_user(&team.admin)
        .patch(
            &team.path(&format!("/members/{}", team.developer.user)),
            &json!({ "role": "owner" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
}
