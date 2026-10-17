//! What a hostile client cannot do to a draft's discussion (and, through it,
//! to its SQL): raw y-sync updates crafted by hand, as an attacker would.

use std::time::Duration;

use romulus_api::{collab::doc::Roots, domain::Role};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{
    Any, Array, ArrayRef, Doc, Map, ReadTxn, Text, Transact, branch::BranchPtr, types::ToJson,
    updates::encoder::Encode,
};

use super::{
    harness::{ORIGIN, Team, TestApp, database, member, session, user},
    live::{Client, MESSAGE_COMMENT, live_app, quick, serve, stored_sql, version},
};

const V1: &str = "CREATE TABLE users (id bigint, email text);\n";

struct Draft {
    app: TestApp,
    team: Team,
    id: Uuid,
    ws: String,
}

async fn draft(db: &PgPool) -> Draft {
    let app = live_app(db.clone(), quick());
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1, json!({"layout": {}})).await;
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
        id,
    }
}

fn any(value: &Value) -> Any {
    Any::from_json(&value.to_string()).expect("json")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A thread and its first comment, as a client would write them.
fn thread_entries(by: Uuid, name: &str, body: &str) -> (Uuid, Value, Uuid, Value) {
    let (t, c) = (Uuid::new_v4(), Uuid::new_v4());
    (
        t,
        json!({"anchor": {"kind": "table", "table": "public.users"}, "by": by, "at": now(), "resolved": null}),
        c,
        json!({"thread": t, "author": {"id": by, "name": name}, "body": body, "mentions": [], "at": now(), "edited": null}),
    )
}

fn has<T: ReadTxn>(map: &yrs::MapRef, txn: &T, key: Uuid) -> bool {
    map.get(txn, &key.to_string())
        .is_some_and(|v| !matches!(v.to_json(txn), Any::Null))
}

/// Alice (developer) announces her cursor: her client id is hers now.
async fn announced(client: &mut Client) {
    let id = client.client_id();
    client.announce(id, 1, &json!({"cursor": null})).await;
    client.read_for(Duration::from_millis(50)).await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reviewer_cannot_write_with_an_editor_s_client_id(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- one\n"))
        .await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    bob.until(|b| b.sql().contains("-- one")).await;

    // Bob writes a harmless-looking thread at Alice's next clock, as Alice.
    let (t, thread, c, comment) = thread_entries(d.team.reviewer.user, "Reviewer-acme", "hi");
    bob.edit_as(alice.client_id(), move |roots, txn| {
        roots
            .threads
            .insert(txn, t.to_string().as_str(), any(&thread));
        roots
            .comments
            .insert(txn, c.to_string().as_str(), any(&comment));
    })
    .await;
    bob.read_for(Duration::from_millis(100)).await;
    // Alice's real edit at that clock must not be taken for a duplicate.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- two\n"))
        .await;
    alice.read_for(Duration::from_millis(200)).await;

    d.app.state.collab.shutdown().await;
    assert_eq!(stored_sql(&db, d.id).await, format!("{V1}-- one\n-- two\n"));
    let threads = threads_stored(&db, d.id).await;
    assert!(!threads.contains(&t), "the forged thread was never applied");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_editor_cannot_write_with_another_person_s_client_id(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- one\n"))
        .await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    admin.until(|a| a.sql().contains("-- one")).await;

    admin
        .edit_as(alice.client_id(), |roots, txn| {
            roots.sql.push(txn, "-- forged\n");
        })
        .await;
    admin.read_for(Duration::from_millis(100)).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- two\n"))
        .await;
    alice.read_for(Duration::from_millis(200)).await;
    d.app.state.collab.shutdown().await;
    assert_eq!(stored_sql(&db, d.id).await, format!("{V1}-- one\n-- two\n"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_client_id_stays_its_person_s_after_they_leave(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- one\n"))
        .await;
    admin.until(|a| a.sql().contains("-- one")).await;
    let hers = alice.client_id();
    drop(alice);
    admin.read_for(Duration::from_millis(100)).await;

    // Gone, her id is free to announce? Not for someone else: never theirs.
    admin.announce(hers, 9, &json!({"cursor": null})).await;
    admin.read_for(Duration::from_millis(50)).await;
    admin
        .edit_as(hers, |roots, txn| roots.sql.push(txn, "-- as alice\n"))
        .await;
    admin.read_for(Duration::from_millis(200)).await;
    d.app.state.collab.shutdown().await;
    assert_eq!(stored_sql(&db, d.id).await, format!("{V1}-- one\n"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn keyless_content_never_stays_in_the_comment_maps(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;

    // List items in the maps' branches: invisible to every map reading.
    let hide = |roots: &romulus_api::collab::doc::Roots, txn: &mut yrs::TransactionMut| {
        for map in [&roots.threads, &roots.comments] {
            let list = ArrayRef::from(BranchPtr::from(map.as_ref()));
            list.insert(txn, 0, "x".repeat(50_000));
        }
    };
    alice.edit(hide).await;
    bob.edit(hide).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- after\n"))
        .await;
    admin.until(|a| a.sql().contains("-- after")).await;
    admin.read_for(Duration::from_millis(100)).await;
    for map in [&admin.roots.threads, &admin.roots.comments] {
        let branch: &yrs::branch::Branch = map.as_ref();
        assert_eq!(
            (branch.block_len, branch.content_len),
            (0, 0),
            "list content was removed for everyone"
        );
    }
    d.app.state.collab.shutdown().await;
    let doc = romulus_api::collab::doc::new_doc();
    let roots = romulus_api::collab::doc::Roots::of(&doc);
    for update in stored_updates(&db, d.id).await {
        doc.transact_mut()
            .apply_update(
                <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&update).unwrap(),
            )
            .unwrap();
    }
    for map in [&roots.threads, &roots.comments] {
        let branch: &yrs::branch::Branch = map.as_ref();
        assert_eq!((branch.block_len, branch.content_len), (0, 0), "nor stored");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn deleting_a_thread_never_takes_others_replies(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let developer = d.team.developer.user;
    let (t, thread, c, comment) = thread_entries(developer, "Developer-acme", "mine");
    alice
        .edit(move |roots, txn| {
            roots
                .threads
                .insert(txn, t.to_string().as_str(), any(&thread));
            roots
                .comments
                .insert(txn, c.to_string().as_str(), any(&comment));
        })
        .await;
    admin
        .until(|a| has(&a.roots.comments, &a.doc.transact(), c))
        .await;
    // The admin replies (directly: an editor).
    let reply = Uuid::new_v4();
    let reply_value = json!({"thread": t, "author": {"id": d.team.admin.user, "name": "Admin-acme"},
                             "body": "theirs", "mentions": [], "at": now(), "edited": null});
    admin
        .edit(move |roots, txn| {
            roots
                .comments
                .insert(txn, reply.to_string().as_str(), any(&reply_value));
        })
        .await;
    alice
        .until(|a| has(&a.roots.comments, &a.doc.transact(), reply))
        .await;

    // Alice deletes her thread, her comment and the admin's reply at once.
    alice
        .edit(move |roots, txn| {
            roots.threads.remove(txn, &t.to_string());
            roots.comments.remove(txn, &c.to_string());
            roots.comments.remove(txn, &reply.to_string());
        })
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    bob.read_for(Duration::from_millis(100)).await;
    let txn = bob.doc.transact();
    assert!(has(&bob.roots.threads, &txn, t), "the thread stays");
    assert!(has(&bob.roots.comments, &txn, reply), "the reply stays");
    assert!(!has(&bob.roots.comments, &txn, c), "her own comment may go");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn comment_operations_are_budgeted_per_person_not_per_connection(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let t = Uuid::new_v4();
    let started = alice
        .op(
            json!({"type": "start", "thread": t, "comment": Uuid::new_v4(),
                   "anchor": {"kind": "point", "x": 0, "y": 0}, "body": "budget"}),
        )
        .await;
    assert_eq!(started["ok"], true);
    let mut sent = 1;
    let mut refused = None;
    let mut first = Client::join(addr, &d.ws, &d.team.reviewer).await;
    for i in 0..20 {
        let answer = first
            .op(json!({"type": "resolve", "thread": t, "resolved": i % 2 == 0}))
            .await;
        sent += 1;
        if answer["ok"] == false {
            refused = Some(answer);
            break;
        }
    }
    // Reconnecting gives no fresh budget.
    drop(first);
    let mut second = Client::join(addr, &d.ws, &d.team.reviewer).await;
    for i in 0..40 {
        if refused.is_some() {
            break;
        }
        let answer = second
            .op(json!({"type": "resolve", "thread": t, "resolved": i % 2 == 0}))
            .await;
        sent += 1;
        if answer["ok"] == false {
            refused = Some(answer);
        }
    }
    let refused = refused.expect("refused at some point");
    assert_eq!(refused["code"], "rate_limited");
    assert!(sent <= 40, "refused after {sent} operations");
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn one_person_opens_a_draft_in_so_many_tabs(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut tabs = Vec::new();
    // room::MAX_TABS
    for _ in 0..8 {
        tabs.push(Client::join(addr, &d.ws, &d.team.reviewer).await);
    }
    let mut extra = Client::connect(addr, &d.ws, Some(&d.team.reviewer), ORIGIN)
        .await
        .expect("upgrade");
    extra.until(|c| c.closed.is_some()).await;
    assert_eq!(extra.closed, Some(4429));
    // Someone else still gets in.
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    alice.read_for(Duration::from_millis(50)).await;
    assert_eq!(alice.closed, None);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reviewer_cannot_fill_a_draft_with_comments(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    // The longest comment there is: 4000 characters of four bytes each.
    let longest = "\u{1D538}".repeat(4000);
    let mut sent = Vec::new();
    let mut refused = None;
    for _ in 0..30 {
        let (thread, comment) = (Uuid::new_v4(), Uuid::new_v4());
        let answer = bob
            .op(
                json!({"type": "start", "thread": thread, "comment": comment,
                       "anchor": {"kind": "table", "table": "public.users"}, "body": longest}),
            )
            .await;
        if answer["ok"] == false {
            refused = Some(answer);
            break;
        }
        sent.push((thread, comment));
    }
    let refused = refused.expect("a cap well within the budget");
    assert_eq!(refused["code"], "too_much", "{refused}");
    assert!(
        sent.len() * longest.len() <= 256 * 1024,
        "{} comments of {} bytes kept",
        sent.len(),
        longest.len()
    );

    // Editors keep editing.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- still editing\n"))
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    assert_eq!(alice.closed, None);

    // An admin and a developer delete anyone's comments; it is in the activity.
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    let (t0, c0) = sent[0];
    let deleted = admin.op(json!({"type": "delete", "comment": c0})).await;
    assert_eq!(deleted["ok"], true, "{deleted}");
    let (t1, _) = sent[1];
    let gone = alice
        .op(json!({"type": "delete_thread", "thread": t1}))
        .await;
    assert_eq!(gone["ok"], true, "{gone}");
    // Another reviewer may not.
    let carol = user(&db, "Carol").await;
    member(&db, d.team.org, carol, Role::Reviewer).await;
    let mut carol = Client::join(addr, &d.ws, &session(&db, carol).await).await;
    let (_, c2) = sent[2];
    let refused = carol.op(json!({"type": "delete", "comment": c2})).await;
    assert_eq!(refused["code"], "not_yours", "{refused}");
    // Room again for Bob.
    let again = bob
        .op(json!({"type": "reply", "thread": sent[3].0, "comment": Uuid::new_v4(), "body": "ok"}))
        .await;
    assert_eq!(again["ok"], true, "{again}");

    d.app.state.collab.shutdown().await;
    assert!(stored_sql(&db, d.id).await.contains("-- still editing"));
    let threads = threads_stored(&db, d.id).await;
    assert!(!threads.contains(&t0) && !threads.contains(&t1));
    let audited = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_events
           WHERE action::text = 'comment_deleted' AND details->>'draft_id' = $1::text"#,
        d.id.to_string(),
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 2);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_frame_of_comment_operations_is_bounded(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let member = d.team.developer.user;
    let mentions: Vec<Uuid> = (0..200)
        .map(|i| if i % 2 == 0 { member } else { Uuid::new_v4() })
        .collect();
    let mut frame = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..20 {
        let id = Uuid::new_v4().to_string();
        let body = serde_json::to_vec(&json!({"id": id, "op": {
            "type": "start", "thread": Uuid::new_v4(), "comment": Uuid::new_v4(),
            "anchor": {"kind": "point", "x": 0, "y": 0}, "body": "hi", "mentions": mentions,
        }}))
        .unwrap();
        frame.extend(yrs::sync::Message::Custom(MESSAGE_COMMENT, body).encode_v1());
        ids.push(id);
    }
    bob.send_raw(frame).await;
    let answered = |c: &Client| {
        c.events
            .iter()
            .filter(|e| e["type"] == "comment_result")
            .count()
            >= 20
    };
    bob.until(answered).await;
    let results: Vec<&Value> = ids
        .iter()
        .map(|id| {
            bob.events
                .iter()
                .find(|e| e["type"] == "comment_result" && e["id"] == id.as_str())
                .expect("answered")
        })
        .collect();
    let ok = results.iter().filter(|r| r["ok"] == true).count();
    assert_eq!(ok, 8, "at most so many operations a frame: {results:?}");
    assert!(results[8..].iter().all(|r| r["code"] == "rate_limited"));
    bob.read_for(Duration::from_millis(100)).await;
    let txn = bob.doc.transact();
    for (_, value) in comments_in(&bob.roots.comments, &txn) {
        assert_eq!(value["mentions"], json!([member]), "members only, once");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_step_2_carrying_another_editor_s_unsent_edits_loses_nothing(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    // The admin's edit reached Alice, not the server (lost with a restart).
    let in_sync = alice.doc.transact().state_vector();
    let mut admin = Client::connect(addr, &d.ws, Some(&d.team.admin), ORIGIN)
        .await
        .expect("upgrade");
    let unsent = {
        let before = admin.doc.transact().state_vector();
        admin
            .roots
            .sql
            .push(&mut admin.doc.transact_mut(), "-- admin, unsaved\n");
        admin.doc.transact().encode_state_as_update_v1(&before)
    };
    alice
        .doc
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&unsent).unwrap())
        .unwrap();
    alice
        .roots
        .sql
        .push(&mut alice.doc.transact_mut(), "-- alice, offline\n");
    // Back after a restart, the admin's tab has announced itself, its step
    // 2 not there yet; Alice's step 2 carries both: she syncs again later.
    let admin_id = admin.client_id();
    admin.announce(admin_id, 1, &json!({"cursor": null})).await;
    // (Not reading: its client would answer the server's step 1 at once.)
    tokio::time::sleep(Duration::from_millis(50)).await;
    let step2 = alice.doc.transact().encode_state_as_update_v1(&in_sync);
    alice
        .send(&yrs::sync::Message::Sync(
            yrs::sync::SyncMessage::SyncStep2(step2),
        ))
        .await;
    alice.until(|a| a.closed.is_some()).await;
    assert_eq!(
        alice.closed,
        Some(4003),
        "told to sync again, not to reload"
    );
    // The admin's own step 2 brings his edit; Alice reconnects.
    admin.sync().await;
    let mut alice = alice.reconnect(addr, &d.ws, &d.team.developer).await;
    alice.read_for(Duration::from_millis(100)).await;
    admin
        .edit(|roots, txn| roots.sql.push(txn, "-- admin again\n"))
        .await;
    admin.read_for(Duration::from_millis(100)).await;
    alice.until(|a| a.sql().contains("-- admin again")).await;
    assert_eq!((alice.closed, admin.closed), (None, None));
    d.app.state.collab.shutdown().await;
    let stored = stored_sql(&db, d.id).await;
    for line in [
        "-- admin, unsaved\n",
        "-- alice, offline\n",
        "-- admin again\n",
    ] {
        assert_eq!(stored.matches(line).count(), 1, "{line} in {stored}");
    }
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_tab_that_only_announced_an_id_yields_it_to_the_one_writing_with_it(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    // Alice's tab is open (its awareness announced), nothing typed yet.
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    admin
        .until(|a| a.awareness.contains_key(&alice.client_id()))
        .await;
    // The admin writes with her id alone: whoever brings structs for an id
    // keeps it; her tab, which never wrote with it, is told to reload.
    let hers = alice.client_id();
    admin
        .edit_as(hers, |roots, txn| {
            roots.sql.push(txn, "-- written with her id\n");
        })
        .await;
    alice.until(|a| a.closed.is_some()).await;
    assert_eq!(alice.closed, Some(4409));
    admin.read_for(Duration::from_millis(100)).await;
    assert_eq!(admin.closed, None);
    // Reloaded (a new id), she loses nothing and edits on.
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- alice\n"))
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    assert_eq!(alice.closed, None);
    d.app.state.collab.shutdown().await;
    assert_eq!(
        stored_sql(&db, d.id).await,
        format!("{V1}-- written with her id\n-- alice\n")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_reviewer_announcing_a_sleeping_tab_s_id_never_keeps_its_edits_out(db: PgPool) {
    let d = draft(&db).await;
    let reviewer = d.team.reviewer.clone();
    // A reviewer's announcement never held it: it stays, cursor gone.
    owner_returns(&db, &d, &reviewer, None).await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_editor_announcing_a_sleeping_tab_s_id_never_keeps_its_edits_out(db: PgPool) {
    let d = draft(&db).await;
    let admin = d.team.admin.clone();
    // An editor's tab that only announced it is told to reload.
    owner_returns(&db, &d, &admin, Some(4409)).await;
}

/// Alice's tab sleeps with edits the server never got; the room unloads;
/// `squatter` announces her client id and stays; she comes back: her edits
/// land, and so do the next ones; the squatter is let go as `told`.
async fn owner_returns(
    db: &PgPool,
    d: &Draft,
    squatter: &super::harness::Session,
    told: Option<u16>,
) {
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- before\n"))
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    // Her laptop sleeps; the room unloads; she keeps typing offline.
    alice.hang_up().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    alice
        .roots
        .sql
        .push(&mut alice.doc.transact_mut(), "-- offline\n");
    // Someone who saw her id announces it (and stays).
    let mut other = Client::join(addr, &d.ws, squatter).await;
    other
        .announce(alice.client_id(), 7, &json!({"cursor": null}))
        .await;
    other.read_for(Duration::from_millis(50)).await;
    // She is back: her step 2 brings her edits.
    let mut alice = alice.reconnect(addr, &d.ws, &d.team.developer).await;
    alice.read_for(Duration::from_millis(100)).await;
    assert_eq!(alice.closed, None, "never told to go away");
    announced(&mut alice).await;
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- after\n"))
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    other.read_for(Duration::from_millis(100)).await;
    assert_eq!((alice.closed, other.closed), (None, told));
    d.app.state.collab.shutdown().await;
    assert_eq!(
        stored_sql(db, d.id).await,
        format!("{V1}-- before\n-- offline\n-- after\n")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_stored_waiting_comment_lands_on_its_own_never_as_someone_s(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let base = {
        let first = Client::join(addr, &d.ws, &d.team.admin).await;
        first
            .doc
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default())
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Client 7777 started a thread; client 8888 then rewrote its comment.
    // Only 8888's change was stored: it waits on 7777's.
    let (t, thread, c, comment) = thread_entries(d.team.developer.user, "Developer-acme", "first");
    let doc_of = |id: u64| {
        Doc::with_options(yrs::Options {
            client_id: yrs::block::ClientID::new(id),
            offset_kind: yrs::OffsetKind::Utf16,
            ..yrs::Options::default()
        })
    };
    let first = doc_of(7_777);
    let roots = Roots::of(&first);
    first
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&base).unwrap())
        .unwrap();
    let before = first.transact().state_vector();
    {
        let mut txn = first.transact_mut();
        roots
            .threads
            .insert(&mut txn, t.to_string().as_str(), any(&thread));
        roots
            .comments
            .insert(&mut txn, c.to_string().as_str(), any(&comment));
    }
    let missing = first.transact().encode_state_as_update_v1(&before);
    let second = doc_of(8_888);
    let second_roots = Roots::of(&second);
    let all = first
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    second
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&all).unwrap())
        .unwrap();
    let known = second.transact().state_vector();
    let mut rewritten = comment.clone();
    rewritten["body"] = json!("waited");
    second_roots.comments.insert(
        &mut second.transact_mut(),
        c.to_string().as_str(),
        any(&rewritten),
    );
    let waiting = second.transact().encode_state_as_update_v1(&known);
    sqlx::query!(
        "INSERT INTO draft_updates (draft_id, update) VALUES ($1, $2)",
        d.id,
        &waiting,
    )
    .execute(&db)
    .await
    .unwrap();
    // The room loads it, waiting; Alice brings what it waits on.
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    alice
        .send(&yrs::sync::Message::Sync(yrs::sync::SyncMessage::Update(
            missing,
        )))
        .await;
    alice.read_for(Duration::from_millis(200)).await;
    admin.read_for(Duration::from_millis(200)).await;
    // It landed for everyone (sent), and its comment change was taken back.
    assert!(
        admin
            .doc
            .transact()
            .state_vector()
            .get(&yrs::block::ClientID::new(8_888))
            > 0,
        "what landed was sent"
    );
    let body = comment_body(&admin, c);
    assert_eq!(body.as_deref(), Some("first"), "never Alice's, never kept");
    d.app.state.collab.shutdown().await;
}

fn comment_body(client: &Client, key: Uuid) -> Option<String> {
    let txn = client.doc.transact();
    let value = client.roots.comments.get(&txn, &key.to_string())?;
    let json = serde_json::to_value(value.to_json(&txn)).ok()?;
    json["body"].as_str().map(str::to_owned)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn offline_edits_and_typing_before_step_2_lose_nothing(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    // Offline: edits the server never got.
    alice
        .roots
        .sql
        .push(&mut alice.doc.transact_mut(), "-- offline\n");
    // Back online: a keystroke goes out before the tab's step 2.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- typed at once\n"))
        .await;
    alice.read_for(Duration::from_millis(200)).await;
    assert_eq!(alice.closed, None, "never told to reload");
    admin.until(|a| a.sql().contains("-- typed at once")).await;
    d.app.state.collab.shutdown().await;
    assert_eq!(
        stored_sql(&db, d.id).await,
        format!("{V1}-- offline\n-- typed at once\n")
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_stored_update_waiting_on_what_was_never_stored_is_kept(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let base = {
        let first = Client::join(addr, &d.ws, &d.team.admin).await;
        first
            .doc
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default())
    };
    // The room unloads; then a row appears whose struct waits on
    // characters of another client, never stored.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (waiting, missing) = waiting_on(7_777, 8_888, &base, "KEPT");
    sqlx::query!(
        "INSERT INTO draft_updates (draft_id, update) VALUES ($1, $2)",
        d.id,
        &waiting,
    )
    .execute(&db)
    .await
    .unwrap();
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    admin
        .edit(|roots, txn| roots.sql.push(txn, "-- edited\n"))
        .await;
    admin.read_for(Duration::from_millis(100)).await;
    d.app.state.collab.shutdown().await;
    // Compacted into the snapshot: still there, still waiting.
    let doc = romulus_api::collab::doc::new_doc();
    let roots = Roots::of(&doc);
    for update in stored_updates(&db, d.id).await {
        doc.transact_mut()
            .apply_update(
                <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&update).unwrap(),
            )
            .unwrap();
    }
    assert!(doc.transact().has_missing_updates(), "kept, waiting");
    doc.transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&missing).unwrap())
        .unwrap();
    assert!(roots.sql(&doc.transact()).contains("KEPT"));
}

/// An update by `writer` inserting `text` inside characters `client` has
/// not written yet, and the update with those characters.
fn waiting_on(client: u64, writer: u64, base: &[u8], text: &str) -> (Vec<u8>, Vec<u8>) {
    let doc = |id: u64| {
        Doc::with_options(yrs::Options {
            client_id: yrs::block::ClientID::new(id),
            offset_kind: yrs::OffsetKind::Utf16,
            ..yrs::Options::default()
        })
    };
    let theirs = doc(client);
    let roots = Roots::of(&theirs);
    theirs
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(base).unwrap())
        .unwrap();
    let before = theirs.transact().state_vector();
    let length = roots.sql(&theirs.transact()).encode_utf16().count();
    roots.sql.push(&mut theirs.transact_mut(), "xxxxxx");
    let missing = theirs.transact().encode_state_as_update_v1(&before);
    let forged = theirs.transact().state_vector();
    let mine = doc(writer);
    let my_roots = Roots::of(&mine);
    let full = theirs
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    mine.transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&full).unwrap())
        .unwrap();
    my_roots.sql.insert(
        &mut mine.transact_mut(),
        u32::try_from(length).unwrap() + 3,
        text,
    );
    (mine.transact().encode_state_as_update_v1(&forged), missing)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn an_update_waiting_on_someone_s_future_edit_never_lands(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    announced(&mut alice).await;
    let mut admin = Client::join(addr, &d.ws, &d.team.admin).await;
    // The admin forges Alice's next characters on a scratch copy, writes
    // inside them, and sends only their own insert: it waits on Alice.
    let length = u32::try_from(V1.encode_utf16().count()).unwrap();
    let state = alice
        .doc
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    let scratch = Doc::with_options(yrs::Options {
        client_id: yrs::block::ClientID::new(alice.client_id()),
        offset_kind: yrs::OffsetKind::Utf16,
        ..yrs::Options::default()
    });
    let roots = Roots::of(&scratch);
    scratch
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&state).unwrap())
        .unwrap();
    roots.sql.push(&mut scratch.transact_mut(), "xxxxxx");
    let forged = scratch.transact().state_vector();
    let theirs = Doc::with_options(yrs::Options {
        client_id: yrs::block::ClientID::new(admin.client_id()),
        offset_kind: yrs::OffsetKind::Utf16,
        ..yrs::Options::default()
    });
    let their_roots = Roots::of(&theirs);
    let full = scratch
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    theirs
        .transact_mut()
        .apply_update(<yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&full).unwrap())
        .unwrap();
    their_roots
        .sql
        .insert(&mut theirs.transact_mut(), length + 3, "EVE");
    let waiting = theirs.transact().encode_state_as_update_v1(&forged);
    admin
        .send(&yrs::sync::Message::Sync(yrs::sync::SyncMessage::Update(
            waiting,
        )))
        .await;
    admin.read_for(Duration::from_millis(200)).await;
    assert_eq!(admin.closed, None, "asked to sync again, not let go");
    // Alice types past the clock it waited on.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- alice types on\n"))
        .await;
    alice.read_for(Duration::from_millis(100)).await;
    d.app.state.collab.shutdown().await;
    let stored = stored_sql(&db, d.id).await;
    assert!(!stored.contains("EVE"), "{stored}");
    assert_eq!(stored, format!("{V1}-- alice types on\n"));
}

/// The comments in a doc, by key.
fn comments_in<T: ReadTxn>(map: &yrs::MapRef, txn: &T) -> Vec<(String, Value)> {
    map.iter(txn)
        .map(|(key, value)| {
            let any = value.to_json(txn);
            (key.to_string(), serde_json::to_value(&any).unwrap())
        })
        .collect()
}

async fn stored_updates(db: &PgPool, draft: Uuid) -> Vec<Vec<u8>> {
    let mut updates: Vec<Vec<u8>> = sqlx::query_scalar!(
        "SELECT state FROM draft_snapshots WHERE draft_id = $1",
        draft
    )
    .fetch_all(db)
    .await
    .unwrap();
    let upto = sqlx::query_scalar!(
        "SELECT upto_update_id FROM draft_snapshots WHERE draft_id = $1",
        draft
    )
    .fetch_optional(db)
    .await
    .unwrap()
    .unwrap_or(0);
    updates.extend(
        sqlx::query_scalar!(
            r#"SELECT update AS "update!" FROM draft_updates WHERE draft_id = $1 AND id > $2 ORDER BY id"#,
            draft,
            upto
        )
        .fetch_all(db)
        .await
        .unwrap(),
    );
    updates
}

async fn threads_stored(db: &PgPool, draft: Uuid) -> Vec<Uuid> {
    let doc = romulus_api::collab::doc::new_doc();
    let roots = romulus_api::collab::doc::Roots::of(&doc);
    for update in stored_updates(db, draft).await {
        doc.transact_mut()
            .apply_update(
                <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&update).unwrap(),
            )
            .unwrap();
    }
    let txn = doc.transact();
    roots
        .threads
        .keys(&txn)
        .filter_map(|k| k.parse().ok())
        .collect()
}
