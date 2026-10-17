//! Comment threads (docs/MVP.md "Review round 1", item 6): in a draft's doc,
//! checked and stamped by its room (reviewers comment, never edit), frozen
//! into the version at publish, and on a version's page over REST.

use axum::http::StatusCode;
use romulus_api::{collab::doc::Roots, domain::Role};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{Any, Map, ReadTxn, Text, Transact, TransactionMut};

use super::{
    harness::{Team, TestApp, database, member, session, user},
    live::{Client, live_app, quick, serve, stored_sql, version},
};

const V1: &str = "CREATE TABLE users (id bigint, email text);\n";

struct Draft {
    app: TestApp,
    team: Team,
    id: Uuid,
    ws: String,
}

impl Draft {
    fn path(&self, rest: &str) -> String {
        self.team
            .path(&format!("/databases/shop/drafts/{}{rest}", self.id))
    }
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

fn thread(by: Uuid, anchor: &Value) -> Value {
    json!({"anchor": anchor, "by": by, "at": now(), "resolved": null})
}

fn comment(thread: Uuid, author: Uuid, name: &str, body: &str) -> Value {
    json!({
        "thread": thread, "author": {"id": author, "name": name}, "body": body,
        "mentions": [], "at": now(), "edited": null,
    })
}

fn column_anchor() -> Value {
    json!({"kind": "column", "table": "public.users", "column": "email"})
}

/// Starts a thread with its first comment, in one transaction.
fn start(
    t: Uuid,
    c: Uuid,
    thread: Value,
    comment: Value,
) -> impl FnOnce(&Roots, &mut TransactionMut) {
    move |roots, txn| {
        roots
            .threads
            .insert(txn, t.to_string().as_str(), any(&thread));
        roots
            .comments
            .insert(txn, c.to_string().as_str(), any(&comment));
    }
}

fn read<T: ReadTxn>(map: &yrs::MapRef, txn: &T, key: Uuid) -> Option<Value> {
    use yrs::types::ToJson;
    let value = map.get(txn, &key.to_string())?;
    let mut json = String::new();
    value.to_json(txn).to_json(&mut json);
    serde_json::from_str(&json).ok()
}

fn comment_of(client: &Client, key: Uuid) -> Option<Value> {
    read(&client.roots.comments, &client.doc.transact(), key)
}

fn thread_of(client: &Client, key: Uuid) -> Option<Value> {
    read(&client.roots.threads, &client.doc.transact(), key)
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn reviewers_comment_through_operations_and_never_write_the_doc(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let (t, c) = (Uuid::new_v4(), Uuid::new_v4());

    // Bob, a reviewer, starts a thread: the room writes it, as his.
    let answer = bob
        .op(
            json!({"type": "start", "thread": t, "comment": c, "anchor": column_anchor(),
                   "body": "Why text and not citext?"}),
        )
        .await;
    assert_eq!(answer["ok"], true, "{answer}");
    alice.until(|a| comment_of(a, c).is_some()).await;
    let seen = comment_of(&alice, c).unwrap();
    assert_eq!(seen["body"], "Why text and not citext?");
    assert_eq!(
        seen["author"],
        json!({"id": d.team.reviewer.user, "name": "Reviewer-acme"})
    );
    assert_eq!(thread_of(&alice, t).unwrap()["anchor"], column_anchor());
    // His own doc has it from the server (he wrote nothing himself).
    assert!(comment_of(&bob, c).is_some());

    // His document updates go nowhere, comments included.
    let (t2, c2) = (Uuid::new_v4(), Uuid::new_v4());
    bob.edit(start(
        t2,
        c2,
        thread(d.team.reviewer.user, &column_anchor()),
        comment(
            t2,
            d.team.reviewer.user,
            "Reviewer-acme",
            "written directly",
        ),
    ))
    .await;
    let mut bob_again = Client::join(addr, &d.ws, &d.team.reviewer).await;
    bob_again
        .edit(|roots, txn| roots.sql.push(txn, "DROP TABLE users;\n"))
        .await;
    // Alice's own edit after theirs goes through: everything before it was seen.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "-- ok\n"))
        .await;
    alice.settle(std::time::Duration::from_millis(200)).await;
    assert_eq!(alice.sql(), format!("{V1}-- ok\n"));
    assert!(comment_of(&alice, c2).is_none());
    d.app.state.collab.shutdown().await;
    assert_eq!(stored_sql(&db, d.id).await, format!("{V1}-- ok\n"));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn authors_are_stamped_by_the_server(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let (t, c) = (Uuid::new_v4(), Uuid::new_v4());

    // Alice writes a comment as if Bob had.
    let reviewer = d.team.reviewer.user;
    alice
        .edit(start(
            t,
            c,
            thread(reviewer, &json!({"kind": "point", "x": 10.4, "y": 20})),
            comment(t, reviewer, "Reviewer-acme", "Bob agrees, trust me"),
        ))
        .await;
    bob.until(|b| comment_of(b, c).is_some()).await;
    let seen = comment_of(&bob, c).unwrap();
    assert_eq!(
        seen["author"],
        json!({"id": d.team.developer.user, "name": "Developer-acme"})
    );
    let seen_thread = thread_of(&bob, t).unwrap();
    assert_eq!(seen_thread["by"], json!(d.team.developer.user));
    // Points are whole.
    assert_eq!(seen_thread["anchor"]["x"].as_f64(), Some(10.0));
    assert_eq!(seen_thread["anchor"]["y"].as_f64(), Some(20.0));
    // Alice's own doc is corrected too.
    alice
        .until(|a| {
            comment_of(a, c).is_some_and(|v| v["author"]["id"] == json!(d.team.developer.user))
        })
        .await;
    let content = d.app.state.collab.content(d.id).await.unwrap();
    assert_eq!(content.discussion.len(), 1);
    assert_eq!(
        content.discussion[0].comments[0].1.author.id,
        d.team.developer.user
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn operations_are_their_authors_and_bounded(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let (developer, reviewer) = (d.team.developer.user, d.team.reviewer.user);
    let (t, c, r) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let ok = alice
        .op(
            json!({"type": "start", "thread": t, "comment": c, "anchor": column_anchor(),
                   "body": "Rename this?", "mentions": [reviewer, d.team.outsider.user, reviewer]}),
        )
        .await;
    assert_eq!(ok["ok"], true);
    // Mentions are members, once each: the outsider is dropped.
    assert_eq!(
        comment_of(&alice, c).unwrap()["mentions"],
        json!([reviewer])
    );
    bob.until(|b| comment_of(b, c).is_some()).await;

    // Bob may not edit or delete Alice's comment; he may resolve, stamped.
    let refused = bob
        .op(json!({"type": "edit", "comment": c, "body": "Bob was here"}))
        .await;
    assert_eq!(refused["code"], "not_yours");
    let refused = bob.op(json!({"type": "delete", "comment": c})).await;
    assert_eq!(refused["code"], "not_yours");
    let resolved = bob
        .op(json!({"type": "resolve", "thread": t, "resolved": true}))
        .await;
    assert_eq!(resolved["ok"], true);
    alice
        .until(|a| thread_of(a, t).is_some_and(|v| v["resolved"]["by"] == json!(reviewer)))
        .await;
    assert_eq!(comment_of(&alice, c).unwrap()["body"], "Rename this?");

    // Too long, empty, or a thread that is gone: refused.
    let long = bob
        .op(json!({"type": "reply", "thread": t, "comment": r, "body": "x".repeat(4001)}))
        .await;
    assert_eq!(long["code"], "invalid");
    let blank = bob
        .op(json!({"type": "reply", "thread": t, "comment": r, "body": "   "}))
        .await;
    assert_eq!(blank["code"], "invalid");
    let nowhere = bob
        .op(json!({"type": "reply", "thread": Uuid::new_v4(), "comment": r, "body": "hi"}))
        .await;
    assert_eq!(nowhere["code"], "not_found");

    // Bob replies; Alice deletes her comment: the thread stays with his reply.
    let replied = bob
        .op(json!({"type": "reply", "thread": t, "comment": r, "body": "Agreed."}))
        .await;
    assert_eq!(replied["ok"], true);
    let gone = alice.op(json!({"type": "delete", "comment": c})).await;
    assert_eq!(gone["ok"], true);
    bob.until(|b| comment_of(b, c).is_none()).await;
    assert!(thread_of(&bob, t).is_some());
    assert!(comment_of(&bob, r).is_some());
    // His last comment takes the thread along.
    let last = bob.op(json!({"type": "delete", "comment": r})).await;
    assert_eq!(last["ok"], true);
    alice.until(|a| thread_of(a, t).is_none()).await;

    // Alice edits hers directly (an editor): marked edited, its mentions kept.
    let (t2, c2) = (Uuid::new_v4(), Uuid::new_v4());
    alice
        .op(
            json!({"type": "start", "thread": t2, "comment": c2, "anchor": column_anchor(),
                   "body": "Unique?", "mentions": [reviewer]}),
        )
        .await;
    let mut edited = comment_of(&alice, c2).unwrap();
    edited["body"] = json!("Unique per org?");
    edited["mentions"] = json!([d.team.outsider.user]);
    alice
        .edit(|roots, txn| {
            roots
                .comments
                .insert(txn, c2.to_string().as_str(), any(&edited));
        })
        .await;
    bob.until(|b| comment_of(b, c2).is_some_and(|v| !v["edited"].is_null()))
        .await;
    assert_eq!(comment_of(&bob, c2).unwrap()["body"], "Unique per org?");
    assert_eq!(comment_of(&bob, c2).unwrap()["mentions"], json!([reviewer]));
    let _ = developer;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rename_typed_in_the_text_carries_its_threads(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let developer = d.team.developer.user;
    let (t, c) = (Uuid::new_v4(), Uuid::new_v4());
    alice
        .edit(start(
            t,
            c,
            thread(developer, &column_anchor()),
            comment(t, developer, "Developer-acme", "Unique?"),
        ))
        .await;
    alice
        .until(|a| a.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;
    // `users` becomes `members` in its own CREATE statement.
    alice
        .edit(|roots, txn| {
            roots.sql.remove_range(txn, 13, 5);
            roots.sql.insert(txn, 13, "members");
        })
        .await;
    alice
        .until(|a| thread_of(a, t).is_some_and(|v| v["anchor"]["table"] == "public.members"))
        .await;
    assert_eq!(thread_of(&alice, t).unwrap()["anchor"]["column"], "email");
}

/// Publishes the draft as v2 (its SQL as it is).
async fn publish(d: &Draft) {
    let preview = d
        .app
        .as_user(&d.team.developer)
        .get(&d.path("/publish"))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json());
    let published = d
        .app
        .as_user(&d.team.developer)
        .post(
            &d.path("/publish"),
            &json!({"sha256": preview.json()["sha256"]}),
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
async fn publishing_freezes_the_threads_into_the_version(db: PgPool) {
    let d = draft(&db).await;
    let addr = serve(&d.app).await;
    let mut alice = Client::join(addr, &d.ws, &d.team.developer).await;
    let mut bob = Client::join(addr, &d.ws, &d.team.reviewer).await;
    let (developer, reviewer) = (d.team.developer.user, d.team.reviewer.user);
    let (t, c, r) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    bob.op(
        json!({"type": "start", "thread": t, "comment": c, "anchor": column_anchor(),
                  "body": "Needs an index @Developer-acme", "mentions": [developer]}),
    )
    .await;
    alice.until(|a| comment_of(a, c).is_some()).await;
    alice
        .op(
            json!({"type": "reply", "thread": t, "comment": r, "body": "Added.",
                   "mentions": [reviewer]}),
        )
        .await;
    bob.until(|b| comment_of(b, r).is_some()).await;
    // Bob read it (unread markers travel with it).
    let marked = d
        .app
        .as_user(&d.team.reviewer)
        .put(&d.path(&format!("/reads/{t}")), &json!({"at": now()}))
        .await;
    assert_eq!(marked.status, StatusCode::NO_CONTENT);
    let reads = d.app.as_user(&d.team.reviewer).get(&d.path("/reads")).await;
    assert_eq!(reads.json()[0]["thread"], json!(t));
    let outsider = d.app.as_user(&d.team.outsider).get(&d.path("/reads")).await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);
    let unknown = d
        .app
        .as_user(&d.team.reviewer)
        .put(
            &d.path(&format!("/reads/{}", Uuid::new_v4())),
            &json!({"at": now()}),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "no such thread");

    publish(&d).await;

    let threads = d
        .app
        .as_user(&d.team.reviewer)
        .get(&d.team.path("/databases/shop/versions/2/threads"))
        .await;
    assert_eq!(threads.status, StatusCode::OK, "{}", threads.json());
    let threads = threads.json();
    let frozen = &threads[0];
    assert_eq!(frozen["frozen"], true);
    assert_eq!(frozen["anchor"], column_anchor());
    assert_eq!(frozen["created_by"], json!(reviewer));
    assert!(!frozen["read_at"].is_null(), "Bob's read mark came along");
    let comments = frozen["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["author_name"], "Reviewer-acme");
    assert_eq!(comments[1]["body"], "Added.");
    assert_eq!(comments[1]["mentions"], json!([reviewer]));
    // Never into the schema's comments.
    let version = sqlx::query_scalar!(
        "SELECT source FROM versions v JOIN databases d ON d.id = v.database_id
         WHERE d.slug = 'shop' AND v.number = 2"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!version.contains("COMMENT ON"));

    // History: read-only, over the API and in the database.
    let thread_path = d.team.path(&format!(
        "/databases/shop/versions/2/threads/{}",
        frozen["id"].as_str().unwrap()
    ));
    let reply = d
        .app
        .as_user(&d.team.developer)
        .post(&format!("{thread_path}/comments"), &json!({"body": "more"}))
        .await;
    assert_eq!(reply.code(), "thread_frozen");
    let resolve = d
        .app
        .as_user(&d.team.developer)
        .post_empty(&format!("{thread_path}/resolve"))
        .await;
    assert_eq!(resolve.code(), "thread_frozen");
    let tampered = sqlx::query!("UPDATE version_comments SET body = 'rewritten'")
        .execute(&db)
        .await;
    assert!(tampered.is_err(), "frozen comments cannot change");
    let deleted = sqlx::query!("DELETE FROM version_threads WHERE frozen")
        .execute(&db)
        .await;
    assert!(deleted.is_err(), "frozen threads cannot go");

    // …but with their version, once it is purged.
    sqlx::query!(
        "UPDATE versions SET removed_at = now() - interval '31 days', removal_reason = 'gone'
         WHERE number = 2"
    )
    .execute(&db)
    .await
    .unwrap();
    d.app.state.collab.shutdown().await;
    assert_eq!(romulus_api::versions::removal::purge(&db).await.unwrap(), 1);
    let left = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM version_threads"#)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_takes_comments_from_everyone(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let threads = team.path("/databases/shop/versions/1/threads");

    // Reviewers comment; outsiders see nothing; an empty comment is refused.
    let empty = app
        .as_user(&team.reviewer)
        .post(
            &threads,
            &json!({"anchor": {"kind": "table", "table": "public.users"}, "body": "  "}),
        )
        .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    let bad_anchor = app
        .as_user(&team.reviewer)
        .post(
            &threads,
            &json!({"anchor": {"kind": "table", "table": ""}, "body": "x"}),
        )
        .await;
    assert_eq!(bad_anchor.status, StatusCode::UNPROCESSABLE_ENTITY);
    let created = app
        .as_user(&team.reviewer)
        .post(
            &threads,
            &json!({
                "anchor": {"kind": "table", "table": "public.users"},
                "body": "Who owns this table?",
                "mentions": [team.developer.user, team.outsider.user, team.developer.user],
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json());
    let thread = created.json();
    assert_eq!(thread["frozen"], false);
    assert_eq!(
        thread["comments"][0]["mentions"],
        json!([team.developer.user])
    );
    assert!(!thread["read_at"].is_null(), "the author has read it");
    let id = thread["id"].as_str().unwrap();
    assert_eq!(
        app.as_user(&team.outsider).get(&threads).await.status,
        StatusCode::NOT_FOUND
    );

    // A reply, unread for the reviewer until they read it.
    let replied = app
        .as_user(&team.developer)
        .post(
            &format!("{threads}/{id}/comments"),
            &json!({"body": "The billing team."}),
        )
        .await;
    assert_eq!(replied.status, StatusCode::CREATED);
    let mine = replied.json()["comments"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listed = app.as_user(&team.reviewer).get(&threads).await.json();
    let read_at = listed[0]["read_at"].as_str().unwrap().to_owned();
    let replied_at = listed[0]["comments"][1]["created_at"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(read_at < replied_at);
    let marked = app
        .as_user(&team.reviewer)
        .put(&format!("{threads}/{id}/read"), &json!({"at": replied_at}))
        .await;
    assert_eq!(marked.status, StatusCode::NO_CONTENT);

    // Only authors edit and delete their comments.
    let first = thread["comments"][0]["id"].as_str().unwrap();
    let not_yours = app
        .as_user(&team.developer)
        .patch(
            &format!("{threads}/{id}/comments/{first}"),
            &json!({"body": "mine now"}),
        )
        .await;
    assert_eq!(not_yours.code(), "not_yours");
    let edited = app
        .as_user(&team.reviewer)
        .patch(
            &format!("{threads}/{id}/comments/{first}"),
            &json!({"body": "Who owns this table now?"}),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK);
    assert!(!edited.json()["comments"][0]["edited_at"].is_null());

    // Anyone resolves and reopens.
    let resolved = app
        .as_user(&team.reviewer)
        .post_empty(&format!("{threads}/{id}/resolve"))
        .await;
    assert_eq!(resolved.json()["resolved"]["name"], "Reviewer-acme");
    let reopened = app
        .as_user(&team.developer)
        .post_empty(&format!("{threads}/{id}/reopen"))
        .await;
    assert!(reopened.json()["resolved"].is_null());

    // The last comment takes the thread along.
    for (who, comment) in [(&team.developer, mine.as_str()), (&team.reviewer, first)] {
        let gone = app
            .as_user(who)
            .delete(&format!("{threads}/{id}/comments/{comment}"))
            .await;
        assert_eq!(gone.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        app.as_user(&team.reviewer).get(&threads).await.json(),
        json!([])
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_s_discussion_has_the_draft_s_limits(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    version(&db, shop, 1, V1, json!({})).await;
    let threads = team.path("/databases/shop/versions/1/threads");
    // A reviewer's part of it is capped, well within the rate limit.
    let longest = "\u{1D538}".repeat(4000);
    let mut kept = Vec::new();
    let mut refused = None;
    for _ in 0..30 {
        let answer = app
            .as_user(&team.reviewer)
            .post(
                &threads,
                &json!({"anchor": {"kind": "table", "table": "public.users"}, "body": longest}),
            )
            .await;
        if answer.status != StatusCode::CREATED {
            refused = Some(answer);
            break;
        }
        kept.push(answer.json());
    }
    let refused = refused.expect("a cap within the rate limit");
    assert_eq!(refused.code(), "too_much");
    assert!(
        kept.len() * longest.len() <= 256 * 1024,
        "{} kept",
        kept.len()
    );

    // Another reviewer may not delete it; a developer may, and it is recorded.
    let (t0, c0) = (
        kept[0]["id"].as_str().unwrap(),
        kept[0]["comments"][0]["id"].as_str().unwrap(),
    );
    let carol = user(&db, "Carol").await;
    member(&db, team.org, carol, Role::Reviewer).await;
    let carol = session(&db, carol).await;
    let comment = format!("{threads}/{t0}/comments/{c0}");
    assert_eq!(
        app.as_user(&carol).delete(&comment).await.code(),
        "not_yours"
    );
    assert_eq!(
        app.as_user(&team.developer).delete(&comment).await.status,
        StatusCode::NO_CONTENT
    );
    let audited = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_events
           WHERE action::text = 'comment_deleted' AND database_id = $1 AND version_number = 1"#,
        shop,
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    // Room again for the reviewer's next comment.
    let t1 = kept[1]["id"].as_str().unwrap();
    let again = app
        .as_user(&team.reviewer)
        .post(&format!("{threads}/{t1}/comments"), &json!({"body": "ok"}))
        .await;
    assert_eq!(again.status, StatusCode::CREATED);

    // Writes are rate limited per person.
    let mut limited = None;
    for i in 0..80 {
        let action = if i % 2 == 0 { "resolve" } else { "reopen" };
        let answer = app
            .as_user(&team.admin)
            .post_empty(&format!("{threads}/{t1}/{action}"))
            .await;
        if answer.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(i);
            break;
        }
    }
    assert!(
        limited.is_some_and(|i| i >= 20),
        "limited after {limited:?}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_typed_dash_line_never_stays_in_the_sql(db: PgPool) {
    let app = live_app(db.clone(), quick());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    // A version's own `---` line (an upload) stays legal in its drafts.
    let base = "--- Users\nCREATE TABLE users (id bigint);\n";
    version(&db, shop, 1, base, json!({})).await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id = created["id"].as_str().unwrap().to_owned();
    let addr = serve(&app).await;
    let mut alice = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{id}/ws"),
        &team.developer,
    )
    .await;
    alice
        .until(|a| a.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;

    // One typed and never posted: the compile refuses it, located.
    alice
        .edit(|roots, txn| roots.sql.push(txn, "--- is id enough?\n"))
        .await;
    alice
        .until(|a| {
            a.last_event("compile")
                .is_some_and(|e| e["state"] == "failed")
        })
        .await;
    let event = alice.last_event("compile").unwrap();
    assert_eq!(event["diagnostics"][0]["code"], "comment_line");
    assert_eq!(event["diagnostics"][0]["span"]["from"]["line"], 3);
    // Nor can it be published.
    let preview = app
        .as_user(&team.developer)
        .get(&team.path(&format!("/databases/shop/drafts/{id}/publish")))
        .await
        .json();
    assert_eq!(preview["diagnostics"][0]["code"], "comment_line");
    assert!(preview["diff"].is_null());

    // `----` and `-- ---` are SQL comments.
    alice
        .edit(|roots, txn| {
            let sql = roots.sql(txn);
            let at = u32::try_from(sql.find("--- is").unwrap()).unwrap();
            roots.sql.insert(txn, at, "-- ");
        })
        .await;
    alice
        .until(|a| a.last_event("compile").is_some_and(|e| e["state"] == "ok"))
        .await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_frozen_thread_takes_no_comment_even_in_its_own_transaction(db: PgPool) {
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    let v1 = version(&db, shop, 1, V1, json!({})).await;
    let mut tx = db.begin().await.unwrap();
    let thread = sqlx::query_scalar!(
        "INSERT INTO version_threads (version_id, draft_thread, frozen, anchor)
         VALUES ($1, $2, true, '{\"kind\": \"point\", \"x\": 0, \"y\": 0}') RETURNING id",
        v1,
        Uuid::new_v4(),
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let added = sqlx::query!(
        "INSERT INTO version_comments (thread_id, author_name, body) VALUES ($1, 'Mallory', 'late')",
        thread,
    )
    .execute(&mut *tx)
    .await;
    assert!(added.is_err(), "frozen: no comment, not even now");
}
