//! The collab server over a real websocket: who may connect, sync and
//! persistence, read-only reviewers, stamped awareness, compaction, compile
//! on type, renames typed in the text, freezing.

use std::time::Duration;

use romulus_api::{domain::Role, orgs::events::MembershipChange};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{
    Text,
    sync::{Message, SyncMessage},
};

use super::{
    harness::{ORIGIN, Team, TestApp, database, signed_in},
    live::{Client, live_app, quick, serve, stored_sql, version},
};

const V1: &str = "CREATE TABLE users (id bigint);\n";

struct Draft {
    app: TestApp,
    team: Team,
    database: Uuid,
    id: Uuid,
    ws: String,
}

async fn draft_with(db: &PgPool, settings: romulus_api::collab::room::Settings) -> Draft {
    let app = live_app(db.clone(), settings);
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(
        db,
        shop,
        1,
        V1,
        json!({"layout": {"public.users": {"x": 10.0, "y": 20.0}}}),
    )
    .await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    Draft {
        ws: format!("/api/orgs/acme/drafts/{id}/ws"),
        app,
        team,
        database: shop,
        id,
    }
}

async fn draft(db: &PgPool) -> Draft {
    draft_with(db, quick()).await
}

fn append(
    text: &'static str,
) -> impl FnOnce(&romulus_api::collab::doc::Roots, &mut yrs::TransactionMut) {
    move |roots, txn| roots.sql.push(txn, text)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn only_members_from_our_origin_may_connect(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let refused = |client: Result<Client, super::live::Refused>| client.err().map(|r| r.0);

    assert_eq!(
        refused(Client::connect(addr, &d.ws, None, ORIGIN).await),
        Some(401)
    );
    assert_eq!(
        refused(Client::connect(addr, &d.ws, Some(&d.team.developer), "http://evil.test").await),
        Some(403)
    );
    assert_eq!(
        refused(Client::connect(addr, &d.ws, Some(&d.team.outsider), ORIGIN).await),
        Some(404)
    );
    // A member of another organization cannot reach it through theirs.
    let other = Team::new(&db, "other").await;
    let through_other = format!("/api/orgs/other/drafts/{}/ws", d.id);
    assert_eq!(
        refused(Client::connect(addr, &through_other, Some(&other.developer), ORIGIN).await),
        Some(404)
    );
    let unknown = format!("/api/orgs/acme/drafts/{}/ws", Uuid::nil());
    assert_eq!(
        refused(Client::connect(addr, &unknown, Some(&d.team.developer), ORIGIN).await),
        Some(404)
    );
    assert!(
        Client::connect(addr, &d.ws, Some(&d.team.reviewer), ORIGIN)
            .await
            .is_ok()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn joining_gets_the_content_the_rights_and_the_last_compile(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    assert_eq!(alice.sql(), V1);
    alice.until(|c| c.last_event("compile").is_some()).await;
    let welcome = alice.last_event("welcome").unwrap();
    assert_eq!(
        (
            welcome["role"].clone(),
            welcome["read_only"].clone(),
            welcome["status"].clone()
        ),
        (json!("developer"), json!(false), json!("open"))
    );
    let compile = alice.last_event("compile").unwrap();
    assert_eq!(compile["state"], "ok");
    assert_eq!(compile["model"]["entities"][0]["name"], "users");

    let bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let welcome = bob.last_event("welcome").unwrap();
    assert_eq!(
        (welcome["role"].clone(), welcome["read_only"].clone()),
        (json!("reviewer"), json!(true))
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn edits_reach_everyone_and_survive_a_restart(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut carol = Client::join(addr, &d.ws, &d.team.admin).await;

    alice.edit(append("CREATE TABLE a (id int);\n")).await;
    carol.edit(append("CREATE TABLE c (id int);\n")).await;
    for client in [&mut alice, &mut carol] {
        client
            .until(|c| c.sql().contains("TABLE a") && c.sql().contains("TABLE c"))
            .await;
    }
    assert_eq!(alice.sql(), carol.sql());

    // The API stops (sockets close with 1012, rooms are written) and starts again.
    d.app.state.collab.shutdown().await;
    alice.until(|c| c.closed.is_some()).await;
    assert_eq!(alice.closed, Some(1012));
    assert!(stored_sql(&db, d.id).await.contains("TABLE c"));

    let restarted = live_app(db.clone(), quick());
    let addr = serve(&restarted).await;
    let again = Client::join(addr, &d.ws, &d.team.reviewer).await;
    assert_eq!(again.sql(), carol.sql());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reviewers_cannot_change_the_document(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;

    // A forged update, sent raw, as a modified client would.
    bob.edit(append("DROP TABLE users;\n")).await;
    let forged = romulus_api::collab::doc::new_doc();
    let raw = {
        let roots = romulus_api::collab::doc::Roots::of(&forged);
        let mut txn = yrs::Transact::transact_mut(&forged);
        roots.sql.insert(&mut txn, 0, "-- pwned\n");
        drop(txn);
        yrs::ReadTxn::encode_state_as_update_v1(
            &yrs::Transact::transact(&forged),
            &yrs::StateVector::default(),
        )
    };
    bob.send(&Message::Sync(SyncMessage::SyncStep2(raw.clone())))
        .await;
    bob.send(&Message::Sync(SyncMessage::Update(raw))).await;

    // Alice's edit after it arrives; Bob's never did.
    alice.edit(append("-- alice\n")).await;
    bob.until(|c| c.sql().contains("-- alice")).await;
    alice.settle(Duration::from_millis(100)).await;
    assert!(!alice.sql().contains("DROP"));
    assert!(!alice.sql().contains("pwned"));
    d.app.state.collab.shutdown().await;
    let stored = stored_sql(&db, d.id).await;
    assert!(stored.contains("-- alice") && !stored.contains("DROP") && !stored.contains("pwned"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn awareness_carries_the_server_s_identity(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;

    // Bob (a reviewer) claims to be someone else; his cursor still goes through.
    let bob_id = bob.client_id();
    bob.announce(
        bob_id,
        1,
        &json!({
            "user": {"id": d.team.admin.user, "name": "The Admin", "color": "red"},
            "cursor": {"anchor": 1, "head": 2},
            "script": "<img src=x>",
        }),
    )
    .await;
    alice.until(|c| c.awareness.contains_key(&bob_id)).await;
    let seen = &alice.awareness[&bob_id];
    assert_eq!(seen["user"]["name"], "Reviewer-acme");
    assert_eq!(seen["user"]["id"], json!(d.team.reviewer.user));
    assert_eq!(seen["user"]["role"], "reviewer");
    assert!(
        seen["user"]["color"]
            .as_str()
            .unwrap()
            .starts_with("var(--color-presence-")
    );
    assert_eq!(seen["cursor"], json!({"anchor": 1, "head": 2}));
    assert_eq!(seen.get("script"), None);

    // Nobody can speak for someone else's client id.
    let alice_id = alice.client_id();
    alice
        .announce(alice_id, 1, &json!({"pointer": {"x": 1, "y": 2}}))
        .await;
    bob.until(|c| c.awareness.contains_key(&alice_id)).await;
    bob.announce(alice_id, 9, &json!({"pointer": {"x": 666, "y": 666}}))
        .await;
    bob.announce(alice_id, 10, &Value::Null).await;
    alice
        .announce(alice_id, 2, &json!({"pointer": {"x": 3, "y": 4}}))
        .await;
    bob.until(|c| {
        c.awareness
            .get(&alice_id)
            .is_some_and(|s| s["pointer"]["x"] == 3)
    })
    .await;

    // Leaving removes the cursor for everyone.
    drop(alice);
    bob.until(|c| !c.awareness.contains_key(&alice_id)).await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn updates_are_compacted_into_a_snapshot(db: PgPool) {
    let d = draft_with(
        &db,
        romulus_api::collab::room::Settings {
            compact_after: 3,
            ..quick()
        },
    )
    .await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    for i in 0..8 {
        alice
            .edit(move |roots, txn| roots.sql.push(txn, &format!("-- {i}\n")))
            .await;
        // Separate writes: longer than the flush interval.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    alice.settle(Duration::from_millis(100)).await;
    d.app.state.collab.shutdown().await;

    let snapshot = sqlx::query!(
        "SELECT upto_update_id FROM draft_snapshots WHERE draft_id = $1",
        d.id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let left = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM draft_updates WHERE draft_id = $1"#,
        d.id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let folded = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM draft_updates WHERE draft_id = $1 AND id <= $2"#,
        d.id,
        snapshot.upto_update_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(folded, 0, "folded rows are deleted");
    assert_eq!(left, 0, "unloading compacts the rest");
    let stored = stored_sql(&db, d.id).await;
    assert!(
        stored.starts_with(V1) && stored.ends_with("-- 7\n"),
        "{stored}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_role_change_closes_the_member_s_sockets(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut carol = Client::join(addr, &d.ws, &d.team.admin).await;

    // Through the API, as an admin would.
    let changed = d
        .app
        .as_user(&d.team.admin)
        .patch(
            &d.team.path(&format!("/members/{}", d.team.developer.user)),
            &json!({"role": "reviewer"}),
        )
        .await;
    assert_eq!(
        changed.status,
        axum::http::StatusCode::OK,
        "{}",
        changed.json()
    );
    alice.until(|c| c.closed.is_some()).await;
    assert_eq!(alice.closed, Some(4001));

    // Reconnecting gives the new rights.
    let again = Client::join(addr, &d.ws, &d.team.developer).await;
    assert_eq!(again.last_event("welcome").unwrap()["read_only"], true);
    // Others are untouched.
    d.app
        .state
        .membership
        .publish(MembershipChange::RoleChanged {
            org_id: d.team.org,
            user_id: Uuid::nil(),
            role: Role::Reviewer,
        });
    carol.edit(append("-- still here\n")).await;
    carol.settle(Duration::from_millis(50)).await;
    assert_eq!(carol.closed, None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn compile_results_reach_everyone(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;

    alice.edit(append("CREATE TABLE ERROR;\n")).await;
    for client in [&mut alice, &mut bob] {
        client
            .until(|c| {
                c.last_event("compile")
                    .is_some_and(|e| e["state"] == "failed")
            })
            .await;
        let failed = client.last_event("compile").unwrap();
        assert_eq!(failed["diagnostics"][0]["code"], "sql");
        let at = V1.encode_utf16().count() + "CREATE TABLE ".len();
        assert_eq!(failed["diagnostics"][0]["span"]["utf16_start"], at);
        assert_eq!(failed["sha256"].as_str().unwrap().len(), 64);
        assert_eq!(failed["model"], Value::Null);
    }

    // A late joiner gets the errors and the last model that compiled.
    let mut carol = Client::join(addr, &d.ws, &d.team.admin).await;
    carol.until(|c| c.last_event("compile").is_some()).await;
    let joined = carol.last_event("compile").unwrap();
    assert_eq!(joined["state"], "failed");
    assert_eq!(joined["model"]["entities"][0]["name"], "users");

    alice
        .edit(|roots, txn| {
            let sql = roots.sql(txn);
            let at = sql.find("ERROR").unwrap();
            roots.sql.remove_range(txn, u32::try_from(at).unwrap(), 5);
            roots
                .sql
                .insert(txn, u32::try_from(at).unwrap(), "fixed (id int)");
        })
        .await;
    bob.until(|c| c.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;
    let names: Vec<_> = bob.last_event("compile").unwrap()["model"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].clone())
        .collect();
    assert_eq!(names, [json!("fixed"), json!("users")]);
    // Kept for whoever comes next (written once the room is).
    d.app.state.collab.shutdown().await;
    let saved = sqlx::query!(
        "SELECT model, errors FROM draft_compiles WHERE draft_id = $1",
        d.id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(saved.model.is_some() && saved.errors.is_none());
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rename_typed_in_the_text_keeps_the_table_s_place(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    alice.until(|c| c.last_event("compile").is_some()).await;

    // users → members, typed over (through an error on the way).
    alice
        .edit(|roots, txn| {
            roots.sql.remove_range(txn, 13, 5);
            roots.sql.insert(txn, 13, "ERROR");
        })
        .await;
    alice
        .until(|c| {
            c.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;
    alice
        .edit(|roots, txn| {
            roots.sql.remove_range(txn, 13, 5);
            roots.sql.insert(txn, 13, "members");
        })
        .await;
    alice
        .until(|c| {
            let layout = c.roots.layout(&yrs::Transact::transact(&c.doc)).layout;
            layout.contains_key("public.members")
        })
        .await;
    let layout = alice
        .roots
        .layout(&yrs::Transact::transact(&alice.doc))
        .layout;
    assert_eq!(layout["public.members"].x, 10.0);
    assert!(!layout.contains_key("public.users"));
    assert_eq!(alice.sql(), "CREATE TABLE members (id bigint);\n");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_frozen_draft_drops_edits_then_takes_them_back(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;

    let frozen = d
        .app
        .state
        .collab
        .freeze(d.id, "Carol")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frozen.sql, V1);
    alice
        .until(|c| {
            c.last_event("status")
                .is_some_and(|e| e["status"] == "publishing")
        })
        .await;
    alice.edit(append("-- typed while frozen\n")).await;
    alice.settle(Duration::from_millis(50)).await;
    assert_eq!(d.app.state.collab.content(d.id).await.unwrap().sql, V1);
    assert!(!bob.sql().contains("frozen"));

    // Publishing failed: the server asks for what it missed, and gets it.
    d.app.state.collab.unfreeze(d.id);
    alice
        .until(|c| {
            c.last_event("status")
                .is_some_and(|e| e["status"] == "open")
        })
        .await;
    alice.settle(Duration::from_millis(50)).await;
    bob.until(|c| c.sql().contains("typed while frozen")).await;
    assert!(
        d.app
            .state
            .collab
            .content(d.id)
            .await
            .unwrap()
            .sql
            .contains("frozen")
    );
    // Someone publishing already: a second freeze is refused.
    let _ = d
        .app
        .state
        .collab
        .freeze(d.id, "Carol")
        .await
        .unwrap()
        .unwrap();
    assert!(
        d.app
            .state
            .collab
            .freeze(d.id, "Dave")
            .await
            .unwrap()
            .is_err()
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn malformed_frames_cost_only_their_connection(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut mallory = Client::join(addr, &d.ws, &d.team.admin).await;
    // Awareness for a client id past 2^53: yrs asserts on it in debug builds.
    let mut huge = vec![1u8];
    let mut payload = Vec::new();
    payload.push(1);
    payload.extend([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
    payload.extend([1, 2, b'{', b'}']);
    huge.push(u8::try_from(payload.len()).unwrap());
    huge.extend(payload);
    mallory.send_raw(huge).await;
    mallory.send_raw(vec![0, 7, 7, 7]).await;
    mallory.until(|c| c.closed.is_some()).await;

    alice.edit(append("-- fine\n")).await;
    let carol = signed_in(&db, "Carol").await;
    super::harness::member(&db, d.team.org, carol.user, Role::Developer).await;
    let mut late = Client::join(addr, &d.ws, &carol).await;
    late.until(|c| c.sql().contains("-- fine")).await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_the_database_lets_its_drafts_go(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    d.app.state.collab.database_deleted(d.database, "Admin");
    alice.until(|c| c.closed.is_some()).await;
    assert_eq!(alice.closed, Some(4404));
    assert_eq!(alice.last_event("status").unwrap()["status"], "deleted");
}
