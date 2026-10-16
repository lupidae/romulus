//! Approvals before publishing: the organization's setting, who may approve
//! (not the draft's editors), approvals tied to the text (any edit resets
//! them, live), admins publishing without them (logged), review requests,
//! and pushes never gated.

use axum::http::StatusCode;
use romulus_api::{
    collab::compile::sha256_hex,
    domain::{Role, TokenScope},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    harness::{Session, Team, TestApp, api_token, database, member, model_json, session, user},
    live::{Client, live_app, quick, serve, version},
};

const V1: &str = "CREATE TABLE users (id bigint);\n";

struct Setup {
    app: TestApp,
    team: Team,
    /// Another developer, who does not edit.
    carol: Session,
    draft: Uuid,
    /// The developer, editing.
    alice: Client,
}

/// `shop` at v1, approvals required = `required`, a draft the developer
/// opened and edited (an editor now).
async fn setup(db: &PgPool, required: i32) -> Setup {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let carol = user(db, "Carol").await;
    member(db, team.org, carol, Role::Developer).await;
    let carol = session(db, carol).await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({})).await;
    let set = app
        .as_user(&team.admin)
        .patch(&team.path(""), &json!({ "approvals_required": required }))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    let draft = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let draft: Uuid = draft["id"].as_str().unwrap().parse().unwrap();
    let addr = serve(&app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{draft}/ws"),
        &team.developer,
    )
    .await;
    append(&mut alice, "CREATE TABLE orders (id bigint);\n").await;
    Setup {
        app,
        team,
        carol,
        draft,
        alice,
    }
}

/// Types `sql` at the end, and waits until the room compiled it.
async fn append(client: &mut Client, sql: &'static str) {
    client
        .edit(|roots, txn| {
            use yrs::Text;
            roots.sql.push(txn, sql);
        })
        .await;
    let expected = sha256_hex(&client.sql());
    client
        .until(|c| {
            c.events
                .iter()
                .any(|e| e["type"] == "compile" && e["sha256"] == expected)
        })
        .await;
}

impl Setup {
    fn path(&self, rest: &str) -> String {
        self.team
            .path(&format!("/databases/shop/drafts/{}{rest}", self.draft))
    }

    fn sha256(&self) -> String {
        sha256_hex(&self.alice.sql())
    }

    async fn approve(&self, who: &Session) -> super::harness::TestResponse {
        self.app
            .as_user(who)
            .post(
                &self.path("/approvals"),
                &json!({ "sha256": self.sha256() }),
            )
            .await
    }

    async fn publish(
        &self,
        who: &Session,
        override_reason: Option<&str>,
    ) -> super::harness::TestResponse {
        let preview = self
            .app
            .as_user(who)
            .get(&self.path("/publish"))
            .await
            .json();
        self.app
            .as_user(who)
            .post(
                &self.path("/publish"),
                &json!({ "sha256": preview["sha256"], "override_reason": override_reason }),
            )
            .await
    }

    async fn review(&self) -> Value {
        self.app
            .as_user(&self.team.reviewer)
            .get(&self.path(""))
            .await
            .json()["review"]
            .clone()
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_waits_for_approvals_from_members_who_did_not_edit(db: PgPool) {
    let mut s = setup(&db, 2).await;

    // The editor cannot approve their own work.
    let own = s.approve(&s.team.developer).await;
    assert_eq!(own.status, StatusCode::FORBIDDEN);
    assert_eq!(own.code(), "edited_draft");
    // An approval is of the text the person saw.
    let stale = s
        .app
        .as_user(&s.team.reviewer)
        .post(&s.path("/approvals"), &json!({ "sha256": sha256_hex(V1) }))
        .await;
    assert_eq!(stale.code(), "draft_changed");
    let malformed = s
        .app
        .as_user(&s.team.reviewer)
        .post(&s.path("/approvals"), &json!({ "sha256": "nope" }))
        .await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    let outsider = s.approve(&s.team.outsider).await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);

    // A reviewer approves: one of two.
    let reviewer = s.approve(&s.team.reviewer).await;
    assert_eq!(reviewer.status, StatusCode::OK, "{}", reviewer.json());
    assert_eq!(reviewer.json()["approvals"], 1);
    assert_eq!(reviewer.json()["required"], 2);
    let refused = s.publish(&s.team.developer, None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "approvals_required");
    // Refused before anything was stored, and the draft is editable again.
    let preview = s
        .app
        .as_user(&s.team.developer)
        .get(&s.path("/publish"))
        .await
        .json();
    assert_eq!(preview["approvals"]["required"], 2);
    assert_eq!(
        preview["approvals"]["approved_by"][0]["name"],
        "Reviewer-acme"
    );
    assert_eq!(preview["approvals"]["can_override"], false);

    // Carol, a developer who did not edit, approves: enough.
    assert_eq!(s.approve(&s.carol).await.json()["approvals"], 2);
    s.alice
        .until(|c| {
            c.last_event("review")
                .is_some_and(|e| e["approvals"].as_array().is_some_and(|a| a.len() == 2))
        })
        .await;
    let review = s.alice.last_event("review").unwrap().clone();
    assert_eq!(review["required"], 2);
    assert_eq!(
        review["editors"],
        json!([s.team.developer.user]),
        "{review}"
    );
    let published = s.publish(&s.team.developer, None).await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
    assert_eq!(published.json()["number"], 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn any_edit_resets_the_approvals_for_everyone(db: PgPool) {
    let mut s = setup(&db, 1).await;
    assert_eq!(s.approve(&s.team.reviewer).await.status, StatusCode::OK);
    s.alice
        .until(|c| {
            c.last_event("review")
                .is_some_and(|e| e["approvals"].as_array().is_some_and(|a| a.len() == 1))
        })
        .await;
    assert_eq!(s.review().await["approvals"], 1);

    append(&mut s.alice, "CREATE TABLE coupons (code text);\n").await;
    s.alice
        .until(|c| {
            c.last_event("review").is_some_and(|e| {
                e["approvals"] == json!([]) && e["outdated"][0]["name"] == "Reviewer-acme"
            })
        })
        .await;
    assert_eq!(s.review().await["approvals"], 0);
    assert_eq!(
        s.publish(&s.team.developer, None).await.code(),
        "approvals_required"
    );

    // Approving again, of the text now, counts.
    assert_eq!(s.approve(&s.team.reviewer).await.json()["approvals"], 1);
    assert_eq!(
        s.publish(&s.team.developer, None).await.status,
        StatusCode::CREATED
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_editor_stays_one_once_the_draft_is_reloaded(db: PgPool) {
    let s = setup(&db, 1).await;
    let Setup {
        app,
        team,
        draft,
        alice,
        ..
    } = s;
    drop(alice);
    // The room writes everything and unloads once nobody is in it.
    for _ in 0..100 {
        if app.state.collab.present(&[draft]).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let editors: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT user_id FROM draft_editors WHERE draft_id = $1",
        draft
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(editors, [team.developer.user]);
    let sql = app.state.collab.content(draft).await.unwrap().sql;
    let own = app
        .as_user(&team.developer)
        .post(
            &team.path(&format!("/databases/shop/drafts/{draft}/approvals")),
            &json!({ "sha256": sha256_hex(&sql) }),
        )
        .await;
    assert_eq!(own.code(), "edited_draft");
}

/// The database's audit events of `action`, newest first.
async fn events(s: &Setup, action: &str) -> Vec<Value> {
    s.app
        .as_user(&s.team.developer)
        .get(&s.team.path("/databases/shop/activity"))
        .await
        .json()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == action)
        .cloned()
        .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn admins_may_publish_without_the_approvals_saying_why(db: PgPool) {
    let s = setup(&db, 2).await;
    let developer = s.publish(&s.team.developer, Some("urgent")).await;
    assert_eq!(developer.status, StatusCode::FORBIDDEN);
    assert_eq!(developer.code(), "override_admins_only");
    let no_reason = s.publish(&s.team.admin, Some("   ")).await;
    assert_eq!(no_reason.code(), "approvals_required");
    let preview = s
        .app
        .as_user(&s.team.admin)
        .get(&s.path("/publish"))
        .await
        .json();
    assert_eq!(preview["approvals"]["can_override"], true);

    assert_eq!(s.approve(&s.team.reviewer).await.status, StatusCode::OK);
    let published = s
        .publish(
            &s.team.admin,
            Some("Hotfix for the outage, reviewed on a call"),
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );

    let overridden = events(&s, "approvals_overridden").await;
    assert_eq!(overridden.len(), 1);
    let event = &overridden[0];
    assert_eq!(event["actor"]["name"], "Admin-acme");
    assert_eq!(event["version_number"], 2);
    assert_eq!(event["reason"], "Hotfix for the outage, reviewed on a call");
    assert_eq!(event["details"]["approvals"], 1);
    assert_eq!(event["details"]["required"], 2);
    assert_eq!(event["details"]["draft_name"], "Draft from v1");
    assert_eq!(event["details"]["approved_by"][0]["name"], "Reviewer-acme");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_keeps_who_approved_with_the_version(db: PgPool) {
    let s = setup(&db, 1).await;
    s.approve(&s.carol).await;
    // A reason given anyway is not an override.
    let published = s.publish(&s.team.admin, Some("just in case")).await;
    assert_eq!(published.status, StatusCode::CREATED);
    assert_eq!(events(&s, "approvals_overridden").await, Vec::<Value>::new());
    let kept = events(&s, "version_published").await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["version_number"], 2);
    assert_eq!(kept[0]["reason"], Value::Null);
    assert_eq!(kept[0]["details"]["approved_by"][0]["name"], "Carol");
    let version = s
        .app
        .as_user(&s.team.reviewer)
        .get(&s.team.path("/databases/shop/versions/2"))
        .await
        .json();
    assert_eq!(version["approvals"][0]["name"], "Carol");

    // The published draft's approvals are the version's now: not withdrawn.
    let withdraw = s.app.as_user(&s.carol).delete(&s.path("/approvals")).await;
    assert_eq!(withdraw.code(), "draft_published");
    // Its row says what it was published with.
    let draft = s
        .app
        .as_user(&s.team.reviewer)
        .get(&s.path(""))
        .await
        .json();
    assert_eq!(draft["review"]["approvals"], 1);
    // And a stored approval cannot be deleted.
    assert!(
        sqlx::query!("DELETE FROM version_approvals")
            .execute(&db)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn approvals_are_checked_again_as_the_version_is_stored(db: PgPool) {
    let s = setup(&db, 1).await;
    assert_eq!(s.approve(&s.carol).await.status, StatusCode::OK);
    // Withdrawn behind the room's back (as a withdrawal landing while the
    // draft is being published): the store sees it and refuses.
    sqlx::query!(
        "DELETE FROM draft_approvals WHERE user_id = $1",
        s.carol.user
    )
    .execute(&db)
    .await
    .unwrap();
    let refused = s.publish(&s.team.developer, None).await;
    assert_eq!(refused.code(), "approvals_required", "{}", refused.json());
    let versions = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM versions"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(versions, 1);

    // Someone who left the organization no longer counts, live or published.
    assert_eq!(s.approve(&s.carol).await.status, StatusCode::OK);
    sqlx::query!("DELETE FROM memberships WHERE user_id = $1", s.carol.user)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(s.review().await["approvals"], 0);
    assert_eq!(
        s.publish(&s.team.developer, None).await.code(),
        "approvals_required"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_draft_whose_editors_are_unknown_is_approved_by_reviewers(db: PgPool) {
    let s = setup(&db, 1).await;
    sqlx::query!(
        "UPDATE drafts SET editors_unknown = true WHERE id = $1",
        s.draft
    )
    .execute(&db)
    .await
    .unwrap();
    let developer = s.approve(&s.carol).await;
    assert_eq!(developer.status, StatusCode::FORBIDDEN);
    assert_eq!(developer.code(), "editors_unknown");
    let reviewer = s.approve(&s.team.reviewer).await;
    assert_eq!(reviewer.status, StatusCode::OK, "{}", reviewer.json());
    assert_eq!(reviewer.json()["approvals"], 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn changing_the_approvals_required_is_in_the_activity(db: PgPool) {
    let s = setup(&db, 1).await;
    s.app
        .as_user(&s.team.admin)
        .patch(&s.team.path(""), &json!({"approvals_required": 3}))
        .await;
    // Unchanged: nothing logged.
    s.app
        .as_user(&s.team.admin)
        .patch(
            &s.team.path(""),
            &json!({"approvals_required": 3, "name": "Acme"}),
        )
        .await;
    let changes = events(&s, "approvals_required_changed").await;
    let steps: Vec<_> = changes
        .iter()
        .map(|e| {
            (
                e["details"]["from"].clone(),
                e["details"]["to"].clone(),
                e["actor"]["name"].clone(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            (json!(1), json!(3), json!("Admin-acme")),
            (json!(0), json!(1), json!("Admin-acme")),
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn withdrawing_takes_the_approval_back(db: PgPool) {
    let mut s = setup(&db, 1).await;
    s.approve(&s.team.reviewer).await;
    let withdrawn = s
        .app
        .as_user(&s.team.reviewer)
        .delete(&s.path("/approvals"))
        .await;
    assert_eq!(withdrawn.status, StatusCode::OK);
    assert_eq!(withdrawn.json()["approvals"], 0);
    s.alice
        .until(|c| {
            c.last_event("review")
                .is_some_and(|e| e["approvals"] == json!([]) && e["outdated"] == json!([]))
        })
        .await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_organization_decides_how_many_approvals(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let org = app.as_user(&team.reviewer).get(&team.path("")).await.json();
    assert_eq!(org["approvals_required"], 0);
    let developer = app
        .as_user(&team.developer)
        .patch(&team.path(""), &json!({"approvals_required": 1}))
        .await;
    assert_eq!(developer.status, StatusCode::FORBIDDEN);
    for wrong in [-1, 11] {
        let response = app
            .as_user(&team.admin)
            .patch(&team.path(""), &json!({"approvals_required": wrong}))
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{wrong}");
    }
    let set = app
        .as_user(&team.admin)
        .patch(&team.path(""), &json!({"approvals_required": 3}))
        .await;
    assert_eq!(set.json()["approvals_required"], 3);
    // Renaming keeps it.
    let renamed = app
        .as_user(&team.admin)
        .patch(&team.path(""), &json!({"name": "Acme"}))
        .await;
    assert_eq!(renamed.json()["approvals_required"], 3);
    let listed = app.as_user(&team.reviewer).get("/api/orgs").await.json();
    assert_eq!(listed[0]["approvals_required"], 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn without_the_setting_there_is_nothing_to_approve(db: PgPool) {
    let s = setup(&db, 0).await;
    let approve = s.approve(&s.team.reviewer).await;
    assert_eq!(approve.code(), "approvals_off");
    assert_eq!(
        s.publish(&s.team.developer, None).await.status,
        StatusCode::CREATED
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_editors_draft_shows_the_new_setting_at_once(db: PgPool) {
    let mut s = setup(&db, 1).await;
    s.app
        .as_user(&s.team.admin)
        .patch(&s.team.path(""), &json!({"approvals_required": 2}))
        .await;
    s.alice
        .until(|c| c.last_event("review").is_some_and(|e| e["required"] == 2))
        .await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn pushes_and_imports_are_never_gated(db: PgPool) {
    let s = setup(&db, 3).await;
    let shop: Uuid = sqlx::query_scalar!("SELECT id FROM databases WHERE slug = 'shop'")
        .fetch_one(&db)
        .await
        .unwrap();
    let token = api_token(&db, shop, TokenScope::Push).await;
    let pushed = s
        .app
        .push(
            shop,
            &token,
            Some("application/json"),
            model_json(&["users", "x"]),
        )
        .await;
    assert_eq!(pushed.status, StatusCode::CREATED, "{}", pushed.json());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_review_request_shows_in_the_drafts_list(db: PgPool) {
    let mut s = setup(&db, 1).await;
    let reviewer = s
        .app
        .as_user(&s.team.reviewer)
        .post_empty(&s.path("/review-request"))
        .await;
    assert_eq!(reviewer.status, StatusCode::FORBIDDEN);
    let requested = s
        .app
        .as_user(&s.team.developer)
        .post_empty(&s.path("/review-request"))
        .await;
    assert_eq!(requested.status, StatusCode::OK, "{}", requested.json());
    assert_eq!(requested.json()["requested"]["name"], "Developer-acme");

    let list = s
        .app
        .as_user(&s.team.reviewer)
        .get(&s.team.path("/databases/shop/drafts"))
        .await
        .json();
    assert_eq!(list[0]["review"]["requested"]["name"], "Developer-acme");
    assert_eq!(list[0]["review"]["required"], 1);
    assert_eq!(list[0]["review"]["approvals"], 0);
    s.alice
        .until(|c| {
            c.last_event("review")
                .is_some_and(|e| e["requested"]["name"] == "Developer-acme")
        })
        .await;

    let cancelled = s
        .app
        .as_user(&s.team.developer)
        .delete(&s.path("/review-request"))
        .await;
    assert_eq!(cancelled.json()["requested"], Value::Null);
}
