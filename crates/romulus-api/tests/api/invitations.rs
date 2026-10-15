//! Copy-link invitations: create, list, revoke, preview, accept, and their
//! edge cases.

use axum::http::StatusCode;
use romulus_api::token;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::harness::{Session, Team, TestApp, session, signed_in, user};

/// Invites `email` as `role` and returns the link's token.
async fn invite(app: &TestApp, team: &Team, email: &str, role: &str) -> String {
    let response = app
        .as_user(&team.admin)
        .post(
            &team.path("/invitations"),
            &json!({ "email": email, "role": role }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{:?}", response.body);
    let url = response.json()["url"].as_str().unwrap().to_owned();
    url.rsplit('/').next().unwrap().to_owned()
}

async fn preview(app: &TestApp, link: &str, viewer: Option<&Session>) -> Value {
    let response = match viewer {
        Some(viewer) => {
            app.as_user(viewer)
                .get(&format!("/api/invitations/{link}"))
                .await
        }
        None => app.get(&format!("/api/invitations/{link}")).await,
    };
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    response.json()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admin_gets_a_one_time_link(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let response = app
        .as_user(&team.admin)
        .post(
            &team.path("/invitations"),
            &json!({ "email": " Bob@Romulus.test ", "role": "developer" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED);
    let created = response.json();
    let url = created["url"].as_str().unwrap();
    assert!(
        url.starts_with("http://localhost:41000/app/invite/"),
        "{url}"
    );
    let link = url.rsplit('/').next().unwrap();
    assert!(token::is_well_formed(link));
    assert_eq!(created["invitation"]["email"], "Bob@Romulus.test");
    assert_eq!(created["invitation"]["role"], "developer");
    assert_eq!(created["invitation"]["invited_by"]["name"], "Admin-acme");

    // Only the hash is stored.
    let stored = sqlx::query_scalar!("SELECT token_hash FROM invitations")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, token::hash(link));

    let expires =
        chrono::DateTime::parse_from_rfc3339(created["invitation"]["expires_at"].as_str().unwrap())
            .unwrap();
    let days = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_hours();
    assert!(
        (167..=168).contains(&days),
        "expires in 7 days, got {days}h"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn inviting_again_replaces_the_link(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let first = invite(&app, &team, "bob@romulus.test", "developer").await;
    let second = invite(&app, &team, "BOB@romulus.test", "reviewer").await;

    let pending = app
        .as_user(&team.admin)
        .get(&team.path("/invitations"))
        .await
        .json();
    assert_eq!(
        pending.as_array().unwrap().len(),
        1,
        "one pending invitation per address"
    );
    assert_eq!(pending[0]["role"], "reviewer");
    assert_eq!(
        app.get(&format!("/api/invitations/{first}")).await.status,
        StatusCode::NOT_FOUND,
        "the old link stops working"
    );
    assert_eq!(preview(&app, &second, None).await["role"], "reviewer");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn members_cannot_be_invited(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let response = TestApp::new(db)
        .as_user(&team.admin)
        .post(
            &team.path("/invitations"),
            &json!({ "email": "DEVELOPER-acme@romulus.test", "role": "admin" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "already_member");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_address_must_look_like_one(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    for email in [
        "",
        "bob",
        "@romulus.test",
        "bob@",
        "bob smith@x.io",
        "a@b@c",
    ] {
        let response = app
            .as_user(&team.admin)
            .post(
                &team.path("/invitations"),
                &json!({ "email": email, "role": "developer" }),
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{email:?}"
        );
        assert!(response.json()["details"]["email"].is_string());
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn pending_invitations_are_listed_newest_first_with_expiry(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    invite(&app, &team, "old@romulus.test", "reviewer").await;
    invite(&app, &team, "new@romulus.test", "developer").await;
    sqlx::query!("UPDATE invitations SET expires_at = now() - interval '1 hour', created_at = now() - interval '8 days' WHERE email = 'old@romulus.test'")
        .execute(&db)
        .await
        .unwrap();

    let pending = app
        .as_user(&team.admin)
        .get(&team.path("/invitations"))
        .await
        .json();
    let pending: Vec<(&str, bool)> = pending
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["email"].as_str().unwrap(),
                i["expired"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pending,
        [("new@romulus.test", false), ("old@romulus.test", true)]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_revoked_link_stops_working(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db);
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let id = app
        .as_user(&team.admin)
        .get(&team.path("/invitations"))
        .await
        .json()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = app
        .as_user(&team.admin)
        .delete(&team.path(&format!("/invitations/{id}")))
        .await;
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/api/invitations/{link}")).await.status,
        StatusCode::NOT_FOUND
    );
    let again = app
        .as_user(&team.admin)
        .delete(&team.path(&format!("/invitations/{id}")))
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_invitation_of_another_organization_cannot_be_revoked(db: PgPool) {
    let acme = Team::new(&db, "acme").await;
    let other = Team::new(&db, "other").await;
    let app = TestApp::new(db);
    invite(&app, &other, "bob@romulus.test", "developer").await;
    let id = app
        .as_user(&other.admin)
        .get(&other.path("/invitations"))
        .await
        .json()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = app
        .as_user(&acme.admin)
        .delete(&acme.path(&format!("/invitations/{id}")))
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_preview_tells_each_viewer_where_they_stand(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;

    let anonymous = preview(&app, &link, None).await;
    assert_eq!(anonymous["status"], "sign_in_required");
    assert_eq!(anonymous["org"], json!({ "name": "acme", "slug": "acme" }));
    assert_eq!(anonymous["email"], "bob@romulus.test");
    assert_eq!(anonymous["role"], "developer");
    assert_eq!(anonymous["invited_by"], "Admin-acme");

    let bob = signed_in(&db, "Bob").await;
    assert_eq!(preview(&app, &link, Some(&bob)).await["status"], "ready");
    assert_eq!(
        preview(&app, &link, Some(&team.outsider)).await["status"],
        "wrong_account"
    );
    let member = preview(&app, &link, Some(&team.reviewer)).await;
    assert_eq!(
        (member["status"].as_str(), member["member_role"].as_str()),
        (Some("already_member"), Some("reviewer"))
    );

    sqlx::query!(
        "UPDATE users SET email_verified = false WHERE id = $1",
        bob.user
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(
        preview(&app, &link, Some(&bob)).await["status"],
        "email_unverified"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn unknown_and_malformed_links_are_404(db: PgPool) {
    let app = TestApp::new(db);
    for link in [token::generate(), "nope".to_owned()] {
        let response = app.get(&format!("/api/invitations/{link}")).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{link}");
        let response = app
            .post_json(&format!("/api/invitations/{link}/accept"), &json!({}))
            .await;
        // Signed out: 401 before anything is looked up.
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_invitee_accepts_and_joins_with_the_role(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "Bob@Romulus.test", "developer").await;
    let bob = signed_in(&db, "Bob").await;

    let response = app
        .as_user(&bob)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    let accepted = response.json();
    assert_eq!(accepted["already_member"], false);
    assert_eq!(accepted["org"]["slug"], "acme");
    assert_eq!(accepted["org"]["role"], "developer");

    let org = app.as_user(&bob).get(&team.path("")).await.json();
    assert_eq!(org["role"], "developer");
    let pending = app
        .as_user(&team.admin)
        .get(&team.path("/invitations"))
        .await
        .json();
    assert_eq!(
        pending,
        json!([]),
        "accepted invitations are no longer pending"
    );

    // The link has served: another account cannot reuse it, even with the right email.
    let bob_again = session(&db, user(&db, "bob2").await).await;
    sqlx::query!(
        "UPDATE users SET email = 'bob@romulus.test' WHERE id = $1",
        bob_again.user
    )
    .execute(&db)
    .await
    .unwrap();
    let reuse = app
        .as_user(&bob_again)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(reuse.status, StatusCode::CONFLICT);
    assert_eq!(reuse.code(), "invitation_used");
    assert_eq!(
        preview(&app, &link, Some(&bob_again)).await["status"],
        "used"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn accepting_twice_is_harmless(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "reviewer").await;
    let bob = signed_in(&db, "Bob").await;
    let path = format!("/api/invitations/{link}/accept");
    let as_bob = app.as_user(&bob);
    assert_eq!(as_bob.post_empty(&path).await.status, StatusCode::OK);
    let again = as_bob.post_empty(&path).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["already_member"], true);
    assert_eq!(again.json()["org"]["role"], "reviewer");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn another_account_is_told_to_switch(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let response = app
        .as_user(&team.outsider)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.code(), "wrong_account");
    let message = response.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("bob@romulus.test") && message.contains("outsider-acme@romulus.test"),
        "{message}"
    );
    // Nothing was consumed: Bob can still accept.
    let bob = signed_in(&db, "Bob").await;
    assert_eq!(preview(&app, &link, Some(&bob)).await["status"], "ready");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_unverified_email_cannot_accept(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let bob = signed_in(&db, "Bob").await;
    sqlx::query!(
        "UPDATE users SET email_verified = false WHERE id = $1",
        bob.user
    )
    .execute(&db)
    .await
    .unwrap();
    let response = app
        .as_user(&bob)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.code(), "email_unverified");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_expired_link_cannot_be_accepted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    sqlx::query!("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let bob = signed_in(&db, "Bob").await;
    assert_eq!(preview(&app, &link, Some(&bob)).await["status"], "expired");
    let response = app
        .as_user(&bob)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.code(), "invitation_expired");
    assert_eq!(
        app.as_user(&bob).get(&team.path("")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_member_opening_someone_elses_link_does_not_use_it(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let response = app
        .as_user(&team.developer)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.json()["already_member"], true);
    let bob = signed_in(&db, "Bob").await;
    assert_eq!(preview(&app, &link, Some(&bob)).await["status"], "ready");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn concurrent_accepts_join_once(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let bob = signed_in(&db, "Bob").await;
    let path = format!("/api/invitations/{link}/accept");
    let as_bob = app.as_user(&bob);
    let (a, b) = tokio::join!(as_bob.post_empty(&path), as_bob.post_empty(&path));
    for response in [&a, &b] {
        assert!(
            response.status == StatusCode::OK || response.code() == "invitation_used",
            "{:?}",
            response.body
        );
    }
    let memberships = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM memberships WHERE user_id = $1"#,
        bob.user
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(memberships, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn accepting_needs_the_csrf_header(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    let bob = signed_in(&db, "Bob").await;
    let request = axum::http::Request::post(format!("/api/invitations/{link}/accept"))
        .header(axum::http::header::COOKIE, bob.cookie())
        .header(axum::http::header::ORIGIN, "http://evil.test")
        .header(romulus_api::csrf::HEADER, "1")
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(app.send(request).await.code(), "csrf");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_admins_invitations_die_with_their_admin_rights(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    // A second admin invites someone, then loses the role (or is removed).
    let second = signed_in(&db, "Second").await;
    crate::harness::member(&db, team.org, second.user, romulus_api::domain::Role::Admin).await;
    // The address the departing admin controls, signed in and verified.
    let accomplice = session(&db, user(&db, "Accomplice").await).await;
    sqlx::query!(
        "UPDATE users SET email = 'm2@personal.test' WHERE id = $1",
        accomplice.user
    )
    .execute(&db)
    .await
    .unwrap();
    for remove in [false, true] {
        let response = app
            .as_user(&second)
            .post(
                &team.path("/invitations"),
                &json!({ "email": "m2@personal.test", "role": "admin" }),
            )
            .await;
        assert_eq!(response.status, StatusCode::CREATED);
        let link = response.json()["url"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();

        let path = team.path(&format!("/members/{}", second.user));
        let change = if remove {
            app.as_user(&team.admin).delete(&path).await
        } else {
            app.as_user(&team.admin)
                .patch(&path, &json!({ "role": "developer" }))
                .await
        };
        assert!(change.status.is_success(), "{:?}", change.body);

        assert_eq!(
            app.get(&format!("/api/invitations/{link}")).await.status,
            StatusCode::NOT_FOUND,
            "remove: {remove}"
        );
        let accept = app
            .as_user(&accomplice)
            .post_empty(&format!("/api/invitations/{link}/accept"))
            .await;
        assert_eq!(accept.status, StatusCode::NOT_FOUND, "remove: {remove}");
        assert_eq!(
            app.as_user(&accomplice).get(&team.path("")).await.status,
            StatusCode::NOT_FOUND,
            "no way back in (remove: {remove})"
        );
        let pending = app
            .as_user(&team.admin)
            .get(&team.path("/invitations"))
            .await
            .json();
        assert_eq!(pending, json!([]), "remove: {remove}");
        // Back to admin for the second round.
        if !remove {
            app.as_user(&team.admin)
                .patch(&path, &json!({ "role": "admin" }))
                .await;
        }
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_invitation_whose_author_is_no_longer_admin_cannot_be_accepted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "admin").await;
    let bob = signed_in(&db, "Bob").await;
    // Whatever path took the role away (here, straight in the database).
    sqlx::query!(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, 'admin')",
        team.org,
        team.outsider.user
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE memberships SET role = 'reviewer' WHERE user_id = $1",
        team.admin.user
    )
    .execute(&db)
    .await
    .unwrap();
    let response = app
        .as_user(&bob)
        .post_empty(&format!("/api/invitations/{link}/accept"))
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn addresses_match_whatever_their_case_beyond_ascii(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    // Typed with a combining accent ("E" + U+0301), upper case: still Élodie.
    let link = invite(&app, &team, "E\u{301}LODIE@romulus.test", "developer").await;
    let stored = sqlx::query_scalar!("SELECT email FROM invitations")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, "ÉLODIE@romulus.test", "stored composed (NFC)");
    let elodie = signed_in(&db, "Elodie").await;
    sqlx::query!(
        "UPDATE users SET email = 'élodie@romulus.test' WHERE id = $1",
        elodie.user
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(preview(&app, &link, Some(&elodie)).await["status"], "ready");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_dead_link_no_longer_tells_whom_it_invited(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let link = invite(&app, &team, "bob@romulus.test", "developer").await;
    assert_eq!(
        preview(&app, &link, None).await["email"],
        "bob@romulus.test"
    );
    sqlx::query!("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let expired = preview(&app, &link, None).await;
    assert_eq!(expired["status"], "expired");
    assert_eq!(expired["email"], Value::Null);
}
