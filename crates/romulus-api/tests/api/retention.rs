//! Retention: accounts and organizations deleted on request (the admin
//! commands, dry runs first), deleted databases and expired invitations
//! purged 30 days on, when an account was last used, inactive accounts
//! (off unless turned on), and the running API told what a deletion ended.

use std::time::Duration;

use romulus_api::{
    AppState, Config,
    admin::{self, AdminError, OrgDeletion, UserDeletion},
    billing,
    domain::{Role, TokenScope},
    mail::{Mailer, Outbox},
    orgs::events::MembershipChange,
    retention::{
        self,
        deletion::{By, Deleter, DeletionError, Fate},
        idp::IdpOutcome,
        inactive, purge,
    },
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    harness::{
        Team, TestApp, api_token, database, member, model_json, org, session, signed_in, user,
    },
    idp::{FakeIdp, Tamper},
    live::version,
};

fn mailer() -> (Mailer, Outbox) {
    Mailer::memory()
}

/// A database with three versions (the last by `author`), a draft, a frozen
/// discussion on v2, an approval, an API token, a removed version's audit
/// event: what deleting must get past (the history triggers).
async fn filled(db: &PgPool, org: Uuid, slug: &str, author: Uuid) -> Uuid {
    let shop = database(db, org, slug).await;
    version(db, shop, 1, "CREATE TABLE a (id bigint);", json!({})).await;
    let v2 = version(
        db,
        shop,
        2,
        "CREATE TABLE a (id bigint);\nCREATE TABLE b (id bigint);",
        json!({}),
    )
    .await;
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout, created_by)
         SELECT database_id, 3, model, source, source_kind, origin, layout, $2 FROM versions WHERE id = $1",
        v2,
        author,
    )
    .execute(db)
    .await
    .unwrap();
    let draft = sqlx::query_scalar!(
        "INSERT INTO drafts (database_id, name, base_version_id, created_by) VALUES ($1, 'Next', $2, $3) RETURNING id",
        shop,
        v2,
        author,
    )
    .fetch_one(db)
    .await
    .unwrap();
    let thread = sqlx::query_scalar!(
        r#"INSERT INTO version_threads (version_id, draft_thread, frozen, anchor, created_by)
           VALUES ($1, gen_random_uuid(), false, '{"kind": "table"}', $2) RETURNING id"#,
        v2,
        author,
    )
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO version_comments (thread_id, author_id, author_name, body) VALUES ($1, $2, 'Author', 'Looks right')",
        thread,
        author,
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE version_threads SET frozen = true WHERE id = $1",
        thread
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO version_approvals (version_id, database_id, user_id, name, approved_at, sql_sha256)
         VALUES ($1, $2, $3, 'Author', now(), sha256('x'))",
        v2,
        shop,
        author,
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO draft_editors (draft_id, user_id) VALUES ($1, $2)",
        draft,
        author
    )
    .execute(db)
    .await
    .unwrap();
    api_token(db, shop, TokenScope::Push).await;
    sqlx::query!(
        "INSERT INTO audit_events (org_id, database_id, database_name, action, actor_id, actor_name, version_number, reason)
         VALUES ($1, $2, (SELECT name FROM databases WHERE id = $2), 'version_removed', $3, 'Author', 1, 'test')",
        org,
        shop,
        author,
    )
    .execute(db)
    .await
    .unwrap();
    shop
}

async fn count(db: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn exists(db: &PgPool, user: Uuid) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE id = $1) AS "exists!""#,
        user
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// `(versions, of which by a deleted account)` of `database`.
async fn versions(db: &PgPool, database: Uuid) -> (i64, i64) {
    let row = sqlx::query!(
        r#"SELECT count(*) AS "all!", count(*) FILTER (WHERE created_by IS NULL) AS "orphaned!"
           FROM versions WHERE database_id = $1"#,
        database
    )
    .fetch_one(db)
    .await
    .unwrap();
    (row.all, row.orphaned)
}

async fn invite(db: &PgPool, org: Uuid, email: &str, expired_days_ago: i32, accepted: bool) {
    sqlx::query!(
        "INSERT INTO invitations (org_id, email, role, token_hash, expires_at, accepted_at)
         VALUES ($1, $2, 'developer', sha256(gen_random_uuid()::text::bytea),
                 now() - make_interval(days => $3), CASE WHEN $4 THEN now() - interval '40 days' END)",
        org,
        email,
        expired_days_ago,
        accepted,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn delete_org_says_first_then_deletes_everything_it_holds(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    filled(&db, team.org, "shop", team.developer.user).await;
    invite(&db, team.org, "new@acme.io", -3, false).await;
    let other = Team::new(&db, "other").await;
    filled(&db, other.org, "shop", other.developer.user).await;
    let config = Config::for_tests();

    let (mail, outbox) = mailer();
    let OrgDeletion::Planned {
        plan,
        refused: None,
    } = admin::delete_org(&db, &config, mail.clone(), "acme", true)
        .await
        .unwrap()
    else {
        panic!("a dry run plans");
    };
    assert_eq!(
        (
            plan.databases,
            plan.versions,
            plan.drafts,
            plan.members,
            plan.invitations
        ),
        (1, 3, 1, 3, 1)
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM organizations").await,
        2,
        "a dry run deletes nothing"
    );
    assert_eq!(outbox.all().len(), 0);

    let done = admin::delete_org(&db, &config, mail, "acme", false)
        .await
        .unwrap();
    assert!(matches!(done, OrgDeletion::Deleted(_)));
    assert!(done.report()[0].starts_with("deleted acme"));
    // Nothing of acme's is left; its members keep their accounts; the other
    // organization is untouched.
    let left = count(
        &db,
        "SELECT (SELECT count(*) FROM organizations WHERE slug = 'acme')
              + (SELECT count(*) FROM databases d JOIN organizations o ON o.id = d.org_id WHERE o.slug = 'acme')",
    )
    .await;
    assert_eq!(left, 0);
    assert_eq!(count(&db, "SELECT count(*) FROM databases").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM versions").await, 3);
    assert_eq!(count(&db, "SELECT count(*) FROM version_threads").await, 1);
    assert_eq!(
        count(&db, "SELECT count(*) FROM version_approvals").await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM audit_events").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM invitations").await, 0);
    assert_eq!(count(&db, "SELECT count(*) FROM api_tokens").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM memberships").await, 3);
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 8);
    // Its admin is told; the sign-in log keeps that it happened.
    let told = outbox.to("admin-acme@romulus.test");
    assert_eq!(told.len(), 1);
    assert!(told[0].subject.contains("acme"), "{}", told[0].subject);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM auth_events WHERE kind = 'organization_deleted' AND details ->> 'slug' = 'acme'"
        )
        .await,
        1
    );

    assert!(matches!(
        admin::delete_org(&db, &config, Mailer::off(), "acme", true).await,
        Err(AdminError::Invalid(_))
    ));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_organization_still_paying_is_not_deleted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    sqlx::query!(
        "INSERT INTO org_billing (org_id, customer_id, subscription_id, plan, status)
         VALUES ($1, 'cus_1', 'sub_1', 'team', 'active')",
        team.org,
    )
    .execute(&db)
    .await
    .unwrap();
    let config = Config::for_tests();
    // Any status but the two that are over for good: it may still charge,
    // or resume. A dry run or not, the plan comes back with the refusal.
    for status in [
        Some("active"),
        Some("trialing"),
        Some("past_due"),
        Some("unpaid"),
        Some("incomplete"),
        Some("paused"),
        None,
    ] {
        sqlx::query!(
            "UPDATE org_billing SET status = $2 WHERE org_id = $1",
            team.org,
            status
        )
        .execute(&db)
        .await
        .unwrap();
        for dry_run in [true, false] {
            let done = admin::delete_org(&db, &config, Mailer::off(), "acme", dry_run)
                .await
                .unwrap();
            let refused = done.refused().unwrap_or_default().to_owned();
            assert!(
                refused.contains("Stripe"),
                "{status:?} {dry_run}: {refused}"
            );
            let report = done.report();
            assert!(
                report[0].starts_with("refused, nothing deleted: would delete acme")
                    && report[0].contains(&format!("subscription {}", status.unwrap_or("unknown"))),
                "{report:?}"
            );
        }
    }
    assert_eq!(count(&db, "SELECT count(*) FROM organizations").await, 1);

    for (slug, status) in [("ended", "canceled"), ("expired", "incomplete_expired")] {
        let org = org(&db, slug).await;
        sqlx::query!(
            "INSERT INTO org_billing (org_id, customer_id, subscription_id, plan, status)
             VALUES ($1, $2, $2, 'team', $3)",
            org,
            format!("sub_{status}"),
            status,
        )
        .execute(&db)
        .await
        .unwrap();
        let done = admin::delete_org(&db, &config, Mailer::off(), slug, false)
            .await
            .unwrap();
        assert!(matches!(done, OrgDeletion::Deleted(_)), "{status}");
    }
    assert_eq!(count(&db, "SELECT count(*) FROM organizations").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn delete_user_takes_what_the_account_alone_holds_and_leaves_the_rest(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let ada = team.developer.user;
    let shop = filled(&db, team.org, "shop", ada).await;
    // Her own organization, nobody else in it.
    let lab = org(&db, "ada-lab").await;
    member(&db, lab, ada, Role::Admin).await;
    filled(&db, lab, "notes", ada).await;
    // An organization she runs with another admin.
    let pair = org(&db, "pair").await;
    member(&db, pair, ada, Role::Admin).await;
    member(&db, pair, team.admin.user, Role::Admin).await;
    let email = "developer-acme@romulus.test";
    let config = Config::for_tests();

    let (mail, outbox) = mailer();
    let UserDeletion::Planned {
        plans,
        zitadel,
        refused: None,
    } = admin::delete_user(
        &db,
        &config,
        mail.clone(),
        "Developer-Acme@romulus.test",
        true,
    )
    .await
    .unwrap()
    else {
        panic!("a dry run plans");
    };
    assert_eq!(plans.len(), 1);
    assert!(!zitadel);
    let fates: Vec<(&str, bool)> = plans[0]
        .memberships
        .iter()
        .map(|m| (m.org.slug.as_str(), matches!(m.fate, Fate::DeletedWith(_))))
        .collect();
    assert_eq!(fates, [("acme", false), ("ada-lab", true), ("pair", false)]);
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 4);
    assert_eq!(outbox.all().len(), 0);

    let deletion = admin::delete_user(&db, &config, mail, email, false)
        .await
        .unwrap();
    // No Zitadel token: the operator is told what is left to do.
    assert!(
        deletion
            .report()
            .iter()
            .any(|line| line.contains("TO DO: delete the user sub-Developer-acme in Zitadel")),
        "{:?}",
        deletion.report()
    );
    let UserDeletion::Deleted(done) = deletion else {
        panic!("deleted");
    };
    assert_eq!(
        done[0].idp,
        IdpOutcome::Manual {
            user_id: "sub-Developer-acme".to_owned()
        }
    );
    // The account, its session and its own organization are gone…
    assert!(!exists(&db, ada).await);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sessions WHERE user_id NOT IN (SELECT id FROM users)"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM organizations WHERE slug = 'ada-lab'"
        )
        .await,
        0
    );
    // …what she wrote in acme stays acme's, by a deleted account.
    assert_eq!(versions(&db, shop).await, (3, 3));
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM version_comments WHERE author_id IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM audit_events WHERE actor_id IS NULL AND actor_name = 'Author'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM organizations WHERE slug IN ('acme', 'pair')"
        )
        .await,
        2
    );
    // Told, and logged.
    let told = outbox.to(email);
    assert_eq!(told.len(), 1);
    assert!(told[0].text.contains("ada-lab"), "{}", told[0].text);
    let logged = sqlx::query_scalar!(
        "SELECT details FROM auth_events WHERE kind = 'account_deleted' AND email = $1",
        email
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(logged["by"], "request");
    assert_eq!(logged["organizations_deleted"], json!(["ada-lab"]));

    assert!(matches!(
        admin::delete_user(&db, &config, Mailer::off(), email, true).await,
        Err(AdminError::Invalid(_))
    ));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_only_admin_of_an_organization_others_share_is_not_deleted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let config = Config::for_tests();
    let email = "admin-acme@romulus.test";
    // The plan first, even refused (the CLI prints it, then the refusal,
    // and exits 4), a dry run or not.
    for dry_run in [true, false] {
        let done = admin::delete_user(&db, &config, Mailer::off(), email, dry_run)
            .await
            .unwrap();
        let refused = done.refused().unwrap_or_default().to_owned();
        assert!(
            refused.contains("only admin of acme") && refused.contains("delete-org acme"),
            "{dry_run}: {refused}"
        );
        let report = done.report();
        assert!(
            report[0].starts_with("refused, nothing deleted: 1 account would go"),
            "{report:?}"
        );
        assert!(
            report
                .iter()
                .any(|line| line == "  leaves acme, whose only admin it is (2 other members)"),
            "{report:?}"
        );
    }
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 4);

    sqlx::query!(
        "UPDATE memberships SET role = 'admin' WHERE org_id = $1 AND user_id = $2",
        team.org,
        team.developer.user,
    )
    .execute(&db)
    .await
    .unwrap();
    admin::delete_user(&db, &config, Mailer::off(), email, false)
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 3);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_single_sign_on_account_is_deleted_at_the_identity_provider_first(db: PgPool) {
    let idp = FakeIdp::start().await;
    let config = idp.config_with_user_api();
    let alice = user(&db, "Alice").await;
    let refused_first = user(&db, "Bob").await;
    // A password account has nothing at the provider.
    let carol = sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name)
         VALUES ('password|' || gen_random_uuid(), 'carol@romulus.test', true, 'Carol') RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let UserDeletion::Deleted(done) =
        admin::delete_user(&db, &config, Mailer::off(), "alice@romulus.test", false)
            .await
            .unwrap()
    else {
        panic!("deleted");
    };
    assert_eq!(done[0].idp, IdpOutcome::Deleted);
    assert_eq!(idp.deleted_users(), ["sub-Alice"]);

    // The provider failing stops it before anything goes in romulus.
    idp.tamper(Tamper {
        refuse_deletions: true,
        ..Tamper::default()
    });
    assert!(matches!(
        admin::delete_user(&db, &config, Mailer::off(), "bob@romulus.test", false).await,
        Err(AdminError::Idp(_))
    ));
    assert!(exists(&db, refused_first).await);
    idp.tamper(Tamper::default());

    let UserDeletion::Deleted(done) =
        admin::delete_user(&db, &config, Mailer::off(), "carol@romulus.test", false)
            .await
            .unwrap()
    else {
        panic!("deleted");
    };
    assert_eq!(done[0].idp, IdpOutcome::NotThere);
    assert_eq!(idp.deleted_users(), ["sub-Alice"]);
    for gone in [alice, carol] {
        assert!(!exists(&db, gone).await);
    }
}

/// `database` deleted `days` ago.
async fn deleted_days_ago(db: &PgPool, database: Uuid, days: i32) {
    sqlx::query!(
        "UPDATE databases SET deleted_at = now() - make_interval(days => $2) WHERE id = $1",
        database,
        days,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleted_databases_are_purged_30_days_on_and_audited(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let old = filled(&db, team.org, "old", team.developer.user).await;
    let recent = filled(&db, team.org, "recent", team.developer.user).await;
    let live = filled(&db, team.org, "live", team.developer.user).await;
    deleted_days_ago(&db, old, 31).await;
    deleted_days_ago(&db, recent, 3).await;

    assert_eq!(purge::databases(&db).await.unwrap(), 1);
    let left: Vec<Uuid> = sqlx::query_scalar!("SELECT id FROM databases ORDER BY slug")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(left, [live, recent]);
    assert_eq!(versions(&db, old).await, (0, 0));
    let purged = sqlx::query!(
        r#"SELECT database_id, database_name, actor_id, details FROM audit_events WHERE action = 'database_purged'"#
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((purged.database_id, purged.actor_id), (None, None));
    assert_eq!(purged.database_name.as_deref(), Some("old"));
    assert_eq!(purged.details["database_slug"], "old");
    assert_eq!(purge::databases(&db).await.unwrap(), 0);
    // Its history stays the organization's: the events it had, by name.
    let history: Vec<(String, Option<Uuid>, Option<String>)> = sqlx::query!(
        r#"SELECT action::text AS "action!", database_id, database_name FROM audit_events
           WHERE database_name = 'old' ORDER BY id"#
    )
    .fetch_all(&db)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.action, r.database_id, r.database_name))
    .collect();
    assert_eq!(
        history,
        [
            ("version_removed".to_owned(), None, Some("old".to_owned())),
            ("database_purged".to_owned(), None, Some("old".to_owned())),
        ]
    );

    // Kept out of the other databases' activity, which keeps its own.
    let app = TestApp::new(db.clone());
    let activity = app
        .as_user(&team.developer)
        .get(&team.path("/databases/live/activity"))
        .await
        .json();
    let actions: Vec<&str> = activity["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["version_removed"], "{activity}");

    // Gone only with the organization.
    sqlx::query!("DELETE FROM organizations WHERE id = $1", team.org)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM audit_events").await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn invitations_nobody_accepted_are_purged_30_days_after_they_expired(db: PgPool) {
    let acme = org(&db, "acme").await;
    invite(&db, acme, "old@acme.io", 31, false).await;
    invite(&db, acme, "recent@acme.io", 3, false).await;
    invite(&db, acme, "pending@acme.io", -5, false).await;
    invite(&db, acme, "joined@acme.io", 60, true).await;

    assert_eq!(purge::invitations(&db).await.unwrap(), 1);
    let left: Vec<String> = sqlx::query_scalar!("SELECT email FROM invitations ORDER BY email")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        left,
        ["joined@acme.io", "pending@acme.io", "recent@acme.io"]
    );
}

/// `user`'s `(last_seen_at older than a minute ago, updated_at)`.
async fn seen(db: &PgPool, user: Uuid) -> (bool, chrono::DateTime<chrono::Utc>) {
    let row = sqlx::query!(
        r#"SELECT last_seen_at < now() - interval '1 minute' AS "old!", updated_at FROM users WHERE id = $1"#,
        user
    )
    .fetch_one(db)
    .await
    .unwrap();
    (row.old, row.updated_at)
}

async fn last_seen_ago(db: &PgPool, user: Uuid, ago: &str, session_too: bool) {
    sqlx::query!(
        "UPDATE users SET last_seen_at = now() - $2::text::interval, inactivity_notice_at = now() - interval '1 day'
         WHERE id = $1",
        user,
        ago,
    )
    .execute(db)
    .await
    .unwrap();
    if session_too {
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = now() - interval '10 minutes' WHERE user_id = $1",
            user
        )
        .execute(db)
        .await
        .unwrap();
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_account_counts_as_used_at_sign_in_and_hourly_while_its_session_is(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let (_, updated_at) = seen(&db, alice.user).await;

    // Used again two hours later: moved, and the notice is void.
    last_seen_ago(&db, alice.user, "2 hours", true).await;
    assert_eq!(app.as_user(&alice).get("/api/me").await.status, 200);
    assert!(!seen(&db, alice.user).await.0);
    let notice = sqlx::query_scalar!(
        "SELECT inactivity_notice_at FROM users WHERE id = $1",
        alice.user
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(notice, None);
    // Not a change of the account.
    assert_eq!(seen(&db, alice.user).await.1, updated_at);

    // Within the hour, a session's use does not write the account.
    sqlx::query!(
        "UPDATE users SET last_seen_at = now() - interval '30 minutes' WHERE id = $1",
        alice.user
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now() - interval '10 minutes' WHERE user_id = $1",
        alice.user
    )
    .execute(&db)
    .await
    .unwrap();
    app.as_user(&alice).get("/api/me").await;
    assert!(seen(&db, alice.user).await.0);

    // A sign-in always does (the dev sign-in, as Playwright's).
    let mut config = Config::for_tests();
    config.dev_signin = true;
    let app = TestApp::with_config(db.clone(), config);
    last_seen_ago(&db, alice.user, "2 years", false).await;
    let response = app
        .get("/api/auth/dev-signin?email=alice@romulus.test&next=/app/")
        .await;
    assert_eq!(response.status, 303, "{}", response.json());
    assert!(!seen(&db, alice.user).await.0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_agent_using_its_persons_token_uses_the_account(db: PgPool) {
    let app = TestApp::new(db.clone());
    let alice = signed_in(&db, "Alice").await;
    let secret = super::mcp::pat(&db, &alice, romulus_api::domain::AccessTokenScope::Read).await;
    last_seen_ago(&db, alice.user, "2 years", false).await;
    let agent = romulus_api::auth::access_tokens::Agent::lookup(&app.state, &secret)
        .await
        .unwrap();
    agent.touch(&db).await.unwrap();
    assert!(!seen(&db, alice.user).await.0);
    let notice = sqlx::query_scalar!(
        "SELECT inactivity_notice_at FROM users WHERE id = $1",
        alice.user
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(notice, None);
}

/// An app whose sweeps can delete inactive accounts (or not), and what it
/// mails.
fn sweeping(db: &PgPool, on: bool) -> (AppState, Outbox) {
    let mut config = Config::for_tests();
    config.delete_inactive_accounts = on;
    let (mail, outbox) = mailer();
    (AppState::new(db.clone(), config).with_mailer(mail), outbox)
}

async fn unused_for(db: &PgPool, user: Uuid, ago: &str) {
    sqlx::query!(
        "UPDATE users SET last_seen_at = now() - $2::text::interval, inactivity_notice_at = NULL WHERE id = $1",
        user,
        ago,
    )
    .execute(db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn inactive_accounts_are_kept_unless_deleting_them_is_on(db: PgPool) {
    let old = user(&db, "Old").await;
    unused_for(&db, old, "5 years").await;
    let (state, outbox) = sweeping(&db, false);
    assert!(!state.config.delete_inactive_accounts);
    let swept = retention::sweep(&state).await;
    assert_eq!((swept.notices, swept.accounts), (0, 0));
    assert_eq!(outbox.all().len(), 0);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM users WHERE inactivity_notice_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 1);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_inactive_account_is_warned_30_days_ahead_then_deleted(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let ada = team.admin.user;
    // Her own organization goes with her; acme gets another admin.
    let lab = org(&db, "ada-lab").await;
    member(&db, lab, ada, Role::Admin).await;
    // Recent enough: no notice yet.
    let soon = user(&db, "Soon").await;
    unused_for(&db, soon, "2 years 6 months").await;
    // Used once warned: kept.
    let back = user(&db, "Back").await;
    unused_for(&db, ada, "3 years").await;
    unused_for(&db, back, "3 years").await;
    let (state, outbox) = sweeping(&db, true);

    let swept = retention::sweep(&state).await;
    assert_eq!((swept.notices, swept.accounts), (2, 0));
    let notice = outbox.to("admin-acme@romulus.test");
    assert_eq!(notice.len(), 1);
    assert!(
        notice[0]
            .subject
            .starts_with("Your romulus account will be deleted on")
    );
    assert!(notice[0].text.contains("ada-lab"), "{}", notice[0].text);
    assert!(
        notice[0].text.contains("only admin of acme"),
        "{}",
        notice[0].text
    );
    assert_eq!(outbox.to("soon@romulus.test").len(), 0);
    // Once only.
    assert_eq!(retention::sweep(&state).await.notices, 0);

    // 30 days later, unused: deleted. `back` signed in meanwhile.
    sqlx::query!("UPDATE users SET inactivity_notice_at = now() - interval '31 days' WHERE inactivity_notice_at IS NOT NULL")
        .execute(&db)
        .await
        .unwrap();
    let back_session = session(&db, back).await;
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now() - interval '10 minutes' WHERE user_id = $1",
        back
    )
    .execute(&db)
    .await
    .unwrap();
    let app = TestApp::with_state(state.clone());
    assert_eq!(app.as_user(&back_session).get("/api/me").await.status, 200);

    let swept = retention::sweep(&state).await;
    assert_eq!(swept.accounts, 1);
    let left: Vec<String> = sqlx::query_scalar!("SELECT name FROM users ORDER BY name")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        left,
        [
            "Back",
            "Developer-acme",
            "Outsider-acme",
            "Reviewer-acme",
            "Soon"
        ]
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM organizations WHERE slug = 'ada-lab'"
        )
        .await,
        0
    );
    // acme kept an admin: its developer, told so, audited.
    let role = sqlx::query_scalar!(
        r#"SELECT role AS "role: Role" FROM memberships WHERE org_id = $1 AND user_id = $2"#,
        team.org,
        team.developer.user,
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(role, Role::Admin);
    let appointed = sqlx::query!(
        "SELECT reason, details FROM audit_events WHERE action = 'admin_appointed' AND org_id = $1",
        team.org
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(appointed.details["member"]["name"], "Developer-acme");
    assert!(appointed.reason.unwrap().contains("3 years"));
    assert_eq!(outbox.to("developer-acme@romulus.test").len(), 1);
    let logged =
        sqlx::query_scalar!("SELECT details FROM auth_events WHERE kind = 'account_deleted'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(logged["by"], "inactivity");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_inactive_account_alone_in_a_paying_organization_is_kept(db: PgPool) {
    let payer = user(&db, "Payer").await;
    let solo = org(&db, "solo").await;
    member(&db, solo, payer, Role::Admin).await;
    sqlx::query!(
        "INSERT INTO org_billing (org_id, customer_id, subscription_id, plan, status)
         VALUES ($1, 'cus_1', 'sub_1', 'starter', 'active')",
        solo,
    )
    .execute(&db)
    .await
    .unwrap();
    unused_for(&db, payer, "4 years").await;
    let (state, outbox) = sweeping(&db, true);
    assert_eq!(retention::sweep(&state).await, retention::Swept::default());
    assert_eq!(outbox.all().len(), 0);

    // Paused (it may resume), or incomplete (it may be paid), and already
    // warned: not deleted either, and not a failure to put off.
    for status in ["paused", "incomplete"] {
        sqlx::query!(
            "UPDATE org_billing SET status = $2 WHERE org_id = $1",
            solo,
            status
        )
        .execute(&db)
        .await
        .unwrap();
        due_for_deletion(&db, payer).await;
        assert_eq!(
            retention::sweep(&state).await,
            retention::Swept::default(),
            "{status}"
        );
        assert!(exists(&db, payer).await, "{status}");
        assert_eq!(put_off(&db, payer).await, (0, None), "{status}");
    }
    // Over for good: the account and its organization go.
    sqlx::query!(
        "UPDATE org_billing SET status = 'canceled' WHERE org_id = $1",
        solo
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(retention::sweep(&state).await.accounts, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM organizations").await, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_running_api_hears_what_a_deletion_ended(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let app = TestApp::new(db.clone());
    let mut changes = app.state.membership.subscribe();
    let listener = retention::live::listen(app.state.clone());
    // The listener is up once a notice sent now arrives.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut tx = db.begin().await.unwrap();
            retention::live::notify(
                &mut tx,
                retention::live::Notice::MemberRemoved {
                    org: Uuid::nil(),
                    user: Uuid::nil(),
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            if let Ok(Ok(_)) =
                tokio::time::timeout(Duration::from_millis(200), changes.recv()).await
            {
                break;
            }
        }
    })
    .await
    .expect("the listener starts");
    while changes.try_recv().is_ok() {}

    // As the CLI would, from its own process: the members' sockets close.
    admin::delete_org(&db, &Config::for_tests(), Mailer::off(), "acme", false)
        .await
        .unwrap();
    let mut removed = Vec::new();
    while removed.len() < 3 {
        let change = tokio::time::timeout(Duration::from_secs(10), changes.recv())
            .await
            .expect("a notice")
            .unwrap();
        if let MembershipChange::Removed { org_id, user_id } = change {
            assert_eq!(org_id, team.org);
            removed.push(user_id);
        }
    }
    removed.sort();
    let mut members = vec![team.admin.user, team.developer.user, team.reviewer.user];
    members.sort();
    assert_eq!(removed, members);
    listener.abort();
}

/// `user` unused for over 3 years, warned 31 days ago: due for deletion.
async fn due_for_deletion(db: &PgPool, user: Uuid) {
    sqlx::query!(
        "UPDATE users SET last_seen_at = now() - interval '3 years 1 day',
                          inactivity_notice_at = now() - interval '31 days',
                          inactivity_failures = 0, inactivity_retry_at = NULL
         WHERE id = $1",
        user
    )
    .execute(db)
    .await
    .unwrap();
}

/// `user`'s `(inactivity_failures, inactivity_retry_at - now() in minutes)`.
async fn put_off(db: &PgPool, user: Uuid) -> (i32, Option<f64>) {
    let row = sqlx::query!(
        r#"SELECT inactivity_failures,
                  extract(epoch FROM inactivity_retry_at - now())::float8 / 60 AS minutes
           FROM users WHERE id = $1"#,
        user
    )
    .fetch_one(db)
    .await
    .unwrap();
    (row.inactivity_failures, row.minutes)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_sign_in_during_an_inactive_deletion_keeps_the_account_and_its_zitadel_user(db: PgPool) {
    let idp = FakeIdp::start().await;
    let config = idp.config_with_user_api();
    let ada = user(&db, "Ada").await;
    due_for_deletion(&db, ada).await;

    // A sign-in under way: it writes the account's row (last_seen_at, the
    // notice cleared) and holds it until it commits.
    let mut signing_in = db.begin().await.unwrap();
    retention::activity::account_used(&mut *signing_in, ada)
        .await
        .unwrap();
    let deleter = Deleter::new(db.clone(), &config, Mailer::off());
    let deletion = tokio::spawn(async move { deleter.delete_account(ada, By::Inactivity).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !deletion.is_finished(),
        "the deletion waits for the sign-in"
    );
    assert_eq!(idp.deleted_users(), Vec::<String>::new());
    signing_in.commit().await.unwrap();

    // Seen used under the lock: nothing deleted, at Zitadel either.
    let outcome = deletion.await.unwrap();
    assert!(
        matches!(outcome, Err(DeletionError::NoLongerDue)),
        "{outcome:?}"
    );
    assert_eq!(idp.deleted_users(), Vec::<String>::new());
    assert!(exists(&db, ada).await);

    // Unused, it goes: its Zitadel user first.
    due_for_deletion(&db, ada).await;
    Deleter::new(db.clone(), &config, Mailer::off())
        .delete_account(ada, By::Inactivity)
        .await
        .unwrap();
    assert_eq!(idp.deleted_users(), ["sub-Ada"]);
    assert!(!exists(&db, ada).await);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn ci_pushing_with_an_api_token_keeps_its_makers_account_in_use(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let secret = api_token(&db, shop, TokenScope::Push).await;
    sqlx::query!("UPDATE api_tokens SET created_by = $1", team.developer.user)
        .execute(&db)
        .await
        .unwrap();
    last_seen_ago(&db, team.developer.user, "2 years", false).await;

    let app = TestApp::new(db.clone());
    let pushed = app.push(shop, &secret, None, model_json(&["users"])).await;
    assert!(pushed.status.is_success(), "{}", pushed.json());
    assert!(!seen(&db, team.developer.user).await.0);
    let notice = sqlx::query_scalar!(
        "SELECT inactivity_notice_at FROM users WHERE id = $1",
        team.developer.user
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(notice, None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn the_migration_counts_what_was_used_before_it(db: PgPool) {
    // Back to before the retention migrations, with an account used only
    // by its tokens lately.
    let migrator = &romulus_api::db::MIGRATOR;
    migrator.undo(&db, 20_261_020_220_606).await.unwrap();
    let agent_user = made_years_ago(&db, "agent", 5).await;
    let ci_user = made_years_ago(&db, "ci", 6).await;
    let idle = made_years_ago(&db, "idle", 4).await;
    let acme = org(&db, "acme").await;
    let shop = database(&db, acme, "shop").await;
    let alice = session(&db, agent_user).await;
    super::mcp::pat(&db, &alice, romulus_api::domain::AccessTokenScope::Read).await;
    sqlx::query!(
        "UPDATE sessions SET last_seen_at = now() - interval '5 years' WHERE user_id = $1",
        agent_user
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!("UPDATE personal_access_tokens SET last_used_at = now() - interval '10 days'")
        .execute(&db)
        .await
        .unwrap();
    api_token(&db, shop, TokenScope::Push).await;
    sqlx::query!(
        "UPDATE api_tokens SET created_by = $1, last_used_at = now() - interval '3 days'",
        ci_user
    )
    .execute(&db)
    .await
    .unwrap();

    migrator.run(&db).await.unwrap();
    assert_eq!(days_unused(&db, agent_user).await, 10);
    assert_eq!(days_unused(&db, ci_user).await, 3);
    assert!(days_unused(&db, idle).await > 1000);
}

/// An account made, and last changed, `years` ago (the users columns every
/// migration has).
async fn made_years_ago(db: &PgPool, name: &str, years: i32) -> Uuid {
    sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name, created_at, updated_at)
         VALUES ('sub-' || $1, $1 || '@romulus.test', true, $1,
                 now() - make_interval(years => $2), now() - make_interval(years => $2))
         RETURNING id",
        name,
        years,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// Whole days since `user`'s account was last used.
async fn days_unused(db: &PgPool, user: Uuid) -> i32 {
    sqlx::query_scalar!(
        r#"SELECT (extract(epoch FROM now() - last_seen_at) / 86400)::int AS "days!" FROM users WHERE id = $1"#,
        user
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_notice_the_mail_server_refuses_is_put_off_not_recorded(db: PgPool) {
    let ada = user(&db, "Ada").await;
    unused_for(&db, ada, "3 years").await;
    let mut config = Config::for_tests();
    config.delete_inactive_accounts = true;
    // Every send fails.
    let state = AppState::new(db.clone(), config).with_mailer(Mailer::off());

    assert_eq!(retention::sweep(&state).await.notices, 0);
    let notice = sqlx::query_scalar!("SELECT inactivity_notice_at FROM users WHERE id = $1", ada)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(notice, None, "no notice recorded unsent");
    let (failures, minutes) = put_off(&db, ada).await;
    assert_eq!(failures, 1);
    assert!((50.0..70.0).contains(&minutes.unwrap()), "{minutes:?}");
    // Put off: not tried again at once.
    retention::sweep(&state).await;
    assert_eq!(put_off(&db, ada).await.0, 1);
    // Tried again when its time comes, and put off twice as long.
    sqlx::query!(
        "UPDATE users SET inactivity_retry_at = now() - interval '1 second' WHERE id = $1",
        ada
    )
    .execute(&db)
    .await
    .unwrap();
    retention::sweep(&state).await;
    let (failures, minutes) = put_off(&db, ada).await;
    assert_eq!(failures, 2);
    assert!((110.0..130.0).contains(&minutes.unwrap()), "{minutes:?}");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn accounts_put_off_never_hold_the_others_up(db: PgPool) {
    // A hundred accounts whose notices failed, put off, unused for longer
    // than the one that works: the sweep's hundred would be theirs.
    sqlx::query!(
        "INSERT INTO users (oidc_subject, email, email_verified, name, last_seen_at,
                            inactivity_failures, inactivity_retry_at)
         SELECT 'stuck-' || i, 'stuck' || i || '@romulus.test', true, 'Stuck',
                now() - interval '3 years 2 months', 1, now() + interval '1 hour'
         FROM generate_series(1, 100) i"
    )
    .execute(&db)
    .await
    .unwrap();
    let fine = user(&db, "Fine").await;
    unused_for(&db, fine, "3 years 1 month").await;
    let (state, outbox) = sweeping(&db, true);
    assert_eq!(retention::sweep(&state).await.notices, 1);
    assert_eq!(outbox.to("fine@romulus.test").len(), 1);
    assert_eq!(outbox.all().len(), 1);

    // A deletion the identity provider refuses is put off the same way,
    // and the others still go.
    let idp = FakeIdp::start().await;
    let mut config = idp.config_with_user_api();
    config.delete_inactive_accounts = true;
    let (mail, _) = mailer();
    let state = AppState::new(db.clone(), config).with_mailer(mail);
    idp.tamper(Tamper {
        refuse_deletions: true,
        ..Tamper::default()
    });
    let sso = user(&db, "Sso").await;
    due_for_deletion(&db, sso).await;
    let password = sqlx::query_scalar!(
        "INSERT INTO users (oidc_subject, email, email_verified, name)
         VALUES ('password|' || gen_random_uuid(), 'pw@romulus.test', true, 'Pw') RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    due_for_deletion(&db, password).await;
    assert_eq!(inactive::delete_due(&state).await.unwrap(), 1);
    assert!(!exists(&db, password).await);
    assert!(exists(&db, sso).await);
    let (failures, minutes) = put_off(&db, sso).await;
    assert_eq!(failures, 1);
    assert!(minutes.unwrap() > 50.0, "{minutes:?}");
    assert_eq!(inactive::delete_due(&state).await.unwrap(), 0);
    assert_eq!(idp.deleted_users(), Vec::<String>::new());
}

/// The advisory locks held on this test's database.
const ADVISORY_LOCKS_HELD: &str = "SELECT count(*) FROM pg_locks
     WHERE locktype = 'advisory'
       AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_sweep_gives_its_lock_back_before_it_returns(db: PgPool) {
    // Closing the lock's connection alone freed it only once the server's
    // backend had exited: a sweep right after could find it held and skip.
    let (state, _) = sweeping(&db, true);
    for _ in 0..20 {
        retention::sweep(&state).await;
        assert_eq!(count(&db, ADVISORY_LOCKS_HELD).await, 0);
        assert_eq!(billing::events::sweep(&db).await, Some(0));
        assert_eq!(count(&db, ADVISORY_LOCKS_HELD).await, 0);
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_notice_goes_once_a_claim_and_a_sweep_at_a_time(db: PgPool) {
    let ada = user(&db, "Ada").await;
    unused_for(&db, ada, "3 years").await;
    let (state, outbox) = sweeping(&db, true);

    // Another process sweeping holds the lock: this one skips.
    let mut other = db.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(hashtext($1))")
        .bind(retention::SWEEP_LOCK)
        .execute(&mut *other)
        .await
        .unwrap();
    assert_eq!(retention::sweep(&state).await, retention::Swept::default());
    assert_eq!(outbox.all().len(), 0);
    sqlx::query("SELECT pg_advisory_unlock(hashtext($1))")
        .bind(retention::SWEEP_LOCK)
        .execute(&mut *other)
        .await
        .unwrap();

    // A notice claimed (being sent elsewhere) is claimed once, and a sweep
    // meanwhile sends nothing.
    let last_seen = sqlx::query_scalar!("SELECT last_seen_at FROM users WHERE id = $1", ada)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(
        inactive::claim(&state, ada, last_seen)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        inactive::claim(&state, ada, last_seen)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(retention::sweep(&state).await.notices, 0);
    assert_eq!(outbox.all().len(), 0);
    // The claim run out (its sender died mid-send): sent, once.
    sqlx::query!(
        "UPDATE users SET inactivity_retry_at = now() - interval '1 second' WHERE id = $1",
        ada
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(retention::sweep(&state).await.notices, 1);
    assert_eq!(retention::sweep(&state).await.notices, 0);
    assert_eq!(outbox.to("ada@romulus.test").len(), 1);
}
