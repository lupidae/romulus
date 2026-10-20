//! Renames in migrations: detected ones offered with their state, turned
//! on or off for a request (`renames`) or for everyone in the draft (the
//! doc's `renames` map, editors only), known ones from the draft's own
//! `ALTER … RENAME` statements, and the version keeping what the person saw
//! at publish, so the migration between the two versions is that one.

use axum::http::StatusCode;
use romulus_api::{
    AppState, Config,
    collab::{
        Hub,
        compile::{Compiled, SqlCompiler},
        doc::Roots,
    },
    domain::Schema,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::{Map, Text, TransactionMut};

use crate::{
    harness::{Team, TestApp, database},
    live::{Client, quick, serve},
};

/// A compiler without a sandbox that knows columns: `CREATE TABLE t (a
/// text, b integer)`, then `ALTER TABLE t RENAME [COLUMN a] TO b`.
pub(crate) fn rename_compiler() -> SqlCompiler {
    SqlCompiler::from_fn(|sql| Compiled {
        model: Some(model_of(sql)),
        diagnostics: Vec::new(),
    })
}

fn model_of(sql: &str) -> Schema {
    let text: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut tables: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for statement in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let words: Vec<&str> = statement.split_whitespace().collect();
        if let Some(rest) = statement.strip_prefix("CREATE TABLE ") {
            let (name, columns) = rest.split_once('(').expect("CREATE TABLE t (…)");
            let columns = columns.trim_end().trim_end_matches(')');
            let columns = columns
                .split(',')
                .filter_map(|c| {
                    let mut parts = c.split_whitespace();
                    Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
                })
                .collect();
            tables.push((name.trim().to_owned(), columns));
        } else if let ["ALTER", "TABLE", table, "RENAME", "COLUMN", from, "TO", to] =
            words.as_slice()
        {
            let t = tables.iter_mut().find(|(n, _)| n == table).expect("table");
            let c = t.1.iter_mut().find(|(n, _)| n == from).expect("column");
            (*to).clone_into(&mut c.0);
        } else if let ["ALTER", "TABLE", table, "RENAME", "TO", to] = words.as_slice() {
            let t = tables.iter_mut().find(|(n, _)| n == table).expect("table");
            (*to).clone_into(&mut t.0);
        }
    }
    let entities: Vec<Value> = tables
        .iter()
        .map(|(name, columns)| {
            let columns: Vec<Value> = columns
                .iter()
                .map(|(c, ty)| {
                    json!({
                        "name": c, "type": ty, "type_schema": "pg_catalog", "type_name": ty,
                        "type_kind": "b", "not_null": false, "default": null,
                        "identity": null, "generated": null, "comment": null,
                    })
                })
                .collect();
            json!({
                "schema": "public", "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null, "columns": columns,
            })
        })
        .collect();
    <Schema as serde::Deserialize>::deserialize(json!({
        "remus_version": "0.2", "database": "sandbox", "server_version": "17",
        "entities": entities,
    }))
    .expect("a model")
    .canonical()
}

pub(crate) const V1: &str = "CREATE TABLE users (id integer, email text, note text);\n";
pub(crate) const RENAMED: &str =
    "CREATE TABLE users (id integer, email_address text, note text);\n";
pub(crate) const KEY: &str = "column/public/users/email/email_address";

pub(crate) async fn version(db: &PgPool, database: Uuid, number: i32, sql: &str) {
    let model = serde_json::to_value(model_of(sql)).expect("json");
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, $2, $3, $4, 'sql', 'upload', '{}')",
        database,
        number,
        model,
        sql,
    )
    .execute(db)
    .await
    .expect("insert version");
}

struct Draft {
    app: TestApp,
    team: Team,
    shop: Uuid,
    id: String,
    editor: Client,
}

/// `shop` at v1, a draft from it, and its developer in it.
async fn draft(db: &PgPool) -> Draft {
    let hub = Hub::with_settings(db.clone(), rename_compiler(), quick());
    let app = TestApp::with_state(AppState::new(db.clone(), Config::for_tests()).with_collab(hub));
    let team = Team::new(db, "acme").await;
    let shop = database(db, team.org, "shop").await;
    version(db, shop, 1, V1).await;
    let created = app
        .as_user(&team.developer)
        .post_empty(&team.path("/databases/shop/drafts"))
        .await
        .json();
    let id = created["id"].as_str().expect("id").to_owned();
    let addr = serve(&app).await;
    let editor = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{id}/ws"),
        &team.developer,
    )
    .await;
    Draft {
        app,
        team,
        shop,
        id,
        editor,
    }
}

impl Draft {
    /// Replaces the draft's SQL and waits for the room to compile it: its
    /// `sha256`. The room handles a socket's messages in order, so what the
    /// editor wrote before is in the room too.
    async fn write(&mut self, sql: &str) -> String {
        let before = self.compiles();
        self.editor
            .edit(|roots: &Roots, txn: &mut TransactionMut| {
                let old = roots.sql(txn);
                let len = u32::try_from(old.encode_utf16().count()).expect("short");
                roots.sql.remove_range(txn, 0, len);
                roots.sql.insert(txn, 0, sql);
            })
            .await;
        self.editor
            .until(|c| {
                c.events
                    .iter()
                    .filter(|e| e["type"] == "compile" && e["state"] == "ok")
                    .count()
                    > before
            })
            .await;
        let event = self
            .editor
            .events
            .iter()
            .rev()
            .find(|e| e["type"] == "compile" && e["state"] == "ok")
            .expect("compiled");
        event["sha256"].as_str().expect("sha256").to_owned()
    }

    fn compiles(&self) -> usize {
        self.editor
            .events
            .iter()
            .filter(|e| e["type"] == "compile" && e["state"] == "ok")
            .count()
    }

    /// Turns a rename on or off for everyone in the draft (the editor's
    /// write to the doc), then a new compile so the room has it.
    async fn choose(&mut self, key: &'static str, on: bool, sql: &str) -> String {
        self.editor
            .edit(|roots: &Roots, txn: &mut TransactionMut| {
                roots.renames.insert(txn, key, on);
            })
            .await;
        self.write(sql).await
    }

    /// Switches a rename for everyone (`None`: back to romulus's own
    /// choice) without touching the text, and waits until the room has it.
    async fn toggle(&mut self, key: &str, on: Option<bool>) {
        self.editor
            .edit(|roots: &Roots, txn: &mut TransactionMut| match on {
                Some(on) => {
                    roots.renames.insert(txn, key, on);
                }
                None => {
                    roots.renames.remove(txn, key);
                }
            })
            .await;
        for _ in 0..100 {
            let preview = self
                .app
                .as_user(&self.team.developer)
                .get(&self.path("/publish"))
                .await
                .json();
            if preview["renames"].get(key).and_then(Value::as_bool) == on {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the room never had {key} = {on:?}");
    }

    async fn preview(&self) -> Value {
        self.app
            .as_user(&self.team.developer)
            .get(&self.path("/publish"))
            .await
            .json()
    }

    fn path(&self, rest: &str) -> String {
        self.team
            .path(&format!("/databases/shop/drafts/{}{rest}", self.id))
    }

    async fn migration(&self, sha: &str, renames: Option<Value>) -> Value {
        let mut path = self.path(&format!("/migration?sha256={sha}"));
        if let Some(renames) = renames {
            path.push_str("&renames=");
            path.push_str(&urlencode(&renames.to_string()));
        }
        let response = self.app.as_user(&self.team.reviewer).get(&path).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        response.json()
    }
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn statements(body: &Value) -> Vec<&str> {
    body["statements"]
        .as_array()
        .expect("statements")
        .iter()
        .map(|s| s["sql"].as_str().expect("sql"))
        .collect()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_column_renamed_in_the_text_is_offered_on_and_can_be_turned_off(db: PgPool) {
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    let body = draft.migration(&sha, None).await;
    assert_eq!(
        statements(&body),
        ["ALTER TABLE public.users RENAME COLUMN email TO email_address;"]
    );
    let candidate = &body["renames"][0];
    assert_eq!(candidate["key"], KEY);
    assert_eq!(candidate["confidence"], "high");
    assert_eq!(candidate["applied"], true);
    assert_eq!(candidate["rename"]["kind"], "column");
    assert_eq!(candidate["rename"]["parent"], "users");
    assert_eq!(body["destructive"], 0);

    // Off for this request: dropped and added, and the data goes.
    let off = draft.migration(&sha, Some(json!({KEY: false}))).await;
    assert_eq!(
        statements(&off),
        [
            "ALTER TABLE public.users ADD COLUMN email_address text;",
            "ALTER TABLE public.users DROP COLUMN email;",
        ]
    );
    assert_eq!(off["destructive"], 1);
    assert_eq!(off["renames"][0]["applied"], false);
    assert_eq!(
        off["statements"][0]["alternative"],
        "ALTER TABLE public.users RENAME COLUMN email TO email_address;"
    );
    assert!(
        off["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "possible_rename"),
        "{off}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_choice_in_the_draft_is_everyone_s_and_reviewers_cannot_make_one(db: PgPool) {
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    // A reviewer's write to the choices is dropped, like any of their edits.
    let addr = serve(&draft.app).await;
    let mut reviewer = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{}/ws", draft.id),
        &draft.team.reviewer,
    )
    .await;
    reviewer
        .edit(|roots: &Roots, txn: &mut TransactionMut| {
            roots.renames.insert(txn, KEY, false);
        })
        .await;
    reviewer.settle(std::time::Duration::from_millis(200)).await;
    let body = draft.migration(&sha, None).await;
    assert_eq!(body["renames"][0]["applied"], true, "a reviewer's choice");

    // The developer's is stored with the draft: every request reads it.
    let sha = draft.choose(KEY, false, &format!("{RENAMED}\n")).await;
    let stored = draft.migration(&sha, None).await;
    assert_eq!(stored["renames"][0]["applied"], false);
    assert_eq!(stored["destructive"], 1);
    // A request turns it back on for itself.
    let on = draft.migration(&sha, Some(json!({KEY: true}))).await;
    assert_eq!(on["renames"][0]["applied"], true);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rename_the_draft_runs_is_known_and_cannot_be_turned_off(db: PgPool) {
    let mut draft = draft(&db).await;
    // What a graph edit appends.
    let sha = draft
        .write(
            "CREATE TABLE users (id integer, email text, note text);\n-- edits from the graph\nALTER TABLE users RENAME COLUMN note TO remark;\n",
        )
        .await;
    let body = draft
        .migration(
            &sha,
            Some(json!({"column/public/users/note/remark": false})),
        )
        .await;
    assert_eq!(
        statements(&body),
        ["ALTER TABLE public.users RENAME COLUMN note TO remark;"]
    );
    assert_eq!(body["renames"][0]["confidence"], "certain");
    assert_eq!(body["renames"][0]["applied"], true);
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn renames_must_be_a_json_object_of_booleans(db: PgPool) {
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    for bad in ["[1]", "{\"k\": 1}", "nope"] {
        let response = draft
            .app
            .as_user(&draft.team.reviewer)
            .get(&draft.path(&format!(
                "/migration?sha256={sha}&renames={}",
                urlencode(bad)
            )))
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(response.code(), "bad_request");
    }
    let response = draft
        .app
        .as_user(&draft.team.reviewer)
        .get(&draft.team.path(&format!(
            "/databases/shop/migration?from=1&to=1&renames={}",
            urlencode("[]")
        )))
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}

/// Publishes the draft as the developer, with these renames on screen.
async fn publish(draft: &Draft, renames: Option<Value>) {
    let preview = draft
        .app
        .as_user(&draft.team.developer)
        .get(&draft.path("/publish"))
        .await
        .json();
    let mut body = json!({"sha256": preview["sha256"], "message": "Rename"});
    if let Some(renames) = renames {
        body["renames"] = renames;
    }
    let published = draft
        .app
        .as_user(&draft.team.developer)
        .post(&draft.path("/publish"), &body)
        .await;
    assert_eq!(
        published.status,
        StatusCode::CREATED,
        "{}",
        published.json()
    );
}

async fn version_migration(draft: &Draft, from: i32, to: i32) -> Value {
    let response = draft
        .app
        .as_user(&draft.team.reviewer)
        .get(
            &draft
                .team
                .path(&format!("/databases/shop/migration?from={from}&to={to}")),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
    response.json()
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_version_keeps_the_renames_it_was_published_with(db: PgPool) {
    let mut draft = draft(&db).await;
    draft.write(RENAMED).await;
    publish(&draft, None).await;
    let stored = sqlx::query_scalar!(
        "SELECT renames FROM versions WHERE database_id = $1 AND number = 2",
        draft.shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        stored["known"],
        json!([{"kind": "column", "schema": "public", "parent": "users", "from": "email", "to": "email_address"}])
    );
    // v1 → v2 is the migration published: the rename, known now.
    let forward = version_migration(&draft, 1, 2).await;
    assert_eq!(
        statements(&forward),
        ["ALTER TABLE public.users RENAME COLUMN email TO email_address;"]
    );
    assert_eq!(forward["renames"][0]["confidence"], "certain");
    // And back: the rename undone.
    let back = version_migration(&draft, 2, 1).await;
    assert_eq!(
        statements(&back),
        ["ALTER TABLE public.users RENAME COLUMN email_address TO email;"]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rename_turned_off_at_publish_stays_off_between_the_versions(db: PgPool) {
    let mut draft = draft(&db).await;
    draft.write(RENAMED).await;
    draft.toggle(KEY, Some(false)).await;
    publish(&draft, Some(json!({KEY: false}))).await;
    let forward = version_migration(&draft, 1, 2).await;
    assert_eq!(
        statements(&forward),
        [
            "ALTER TABLE public.users ADD COLUMN email_address text;",
            "ALTER TABLE public.users DROP COLUMN email;",
        ]
    );
    assert_eq!(forward["renames"][0]["applied"], false);
    // Someone looking at it may still try it on.
    let tried = draft
        .app
        .as_user(&draft.team.reviewer)
        .get(&draft.team.path(&format!(
            "/databases/shop/migration?from=1&to=2&renames={}",
            urlencode(&json!({KEY: true}).to_string())
        )))
        .await
        .json();
    assert_eq!(
        statements(&tried),
        ["ALTER TABLE public.users RENAME COLUMN email TO email_address;"]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn renames_follow_through_several_versions(db: PgPool) {
    let mut draft = draft(&db).await;
    draft.write(RENAMED).await;
    publish(&draft, None).await;
    // v3 renames the table with ALTER (known), from a draft of v2.
    let created = draft
        .app
        .as_user(&draft.team.developer)
        .post_empty(&draft.team.path("/databases/shop/drafts"))
        .await
        .json();
    draft.id = created["id"].as_str().unwrap().to_owned();
    let addr = serve(&draft.app).await;
    draft.editor = Client::join(
        addr,
        &format!("/api/orgs/acme/drafts/{}/ws", draft.id),
        &draft.team.developer,
    )
    .await;
    draft
        .write("CREATE TABLE users (id integer, email_address text, note text);\nALTER TABLE users RENAME TO accounts;\n")
        .await;
    publish(&draft, None).await;
    let span = version_migration(&draft, 1, 3).await;
    assert_eq!(
        statements(&span),
        [
            "ALTER TABLE public.users RENAME TO accounts;",
            "ALTER TABLE public.accounts RENAME COLUMN email TO email_address;",
        ]
    );
    let back = version_migration(&draft, 3, 1).await;
    assert_eq!(
        statements(&back),
        [
            "ALTER TABLE public.accounts RENAME TO users;",
            "ALTER TABLE public.users RENAME COLUMN email_address TO email;",
        ]
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn publishing_with_other_renames_than_the_draft_s_is_refused(db: PgPool) {
    let mut draft = draft(&db).await;
    draft.write(RENAMED).await;
    // On screen: off. In the draft: on (nobody switched it there).
    let preview = draft.preview().await;
    let refused = draft
        .app
        .as_user(&draft.team.developer)
        .post(
            &draft.path("/publish"),
            &json!({"sha256": preview["sha256"], "renames": {KEY: false}}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "renames_changed");

    // Switched in the draft: the preview of before is of other renames.
    draft.toggle(KEY, Some(false)).await;
    let stale = draft
        .app
        .as_user(&draft.team.developer)
        .post(
            &draft.path("/publish"),
            &json!({"sha256": preview["sha256"], "renames": {KEY: false}}),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "draft_changed");
    let now = draft.preview().await;
    assert_ne!(
        now["sha256"], preview["sha256"],
        "a switch is a change to review"
    );
    assert_eq!(now["renames"], json!({KEY: false}));
    publish(&draft, Some(json!({KEY: false}))).await;
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_rename_switched_outdates_the_approvals_and_an_approval_is_of_the_renames_seen(
    db: PgPool,
) {
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    let set = draft
        .app
        .as_user(&draft.team.admin)
        .patch(&draft.team.path(""), &json!({"approvals_required": 1}))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    let approved = draft
        .app
        .as_user(&draft.team.reviewer)
        .post(
            &draft.path("/approvals"),
            &json!({"sha256": sha, "renames": {}}),
        )
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.json());
    let approvers = |preview: &Value| {
        preview["approvals"]["approved_by"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(approvers(&draft.preview().await), 1);

    // Switched: the approval was of the other choice.
    draft.toggle(KEY, Some(false)).await;
    assert_eq!(approvers(&draft.preview().await), 0);
    // An approval of the renames as they were is refused…
    let stale = draft
        .app
        .as_user(&draft.team.reviewer)
        .post(
            &draft.path("/approvals"),
            &json!({"sha256": sha, "renames": {}}),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "draft_changed");
    // An approval that says nothing of them saw none switched: refused too.
    let silent = draft
        .app
        .as_user(&draft.team.reviewer)
        .post(&draft.path("/approvals"), &json!({"sha256": sha}))
        .await;
    assert_eq!(silent.status, StatusCode::CONFLICT);
    assert_eq!(silent.code(), "draft_changed");
    // …of the renames as they are, given.
    let again = draft
        .app
        .as_user(&draft.team.reviewer)
        .post(
            &draft.path("/approvals"),
            &json!({"sha256": sha, "renames": {KEY: false}}),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json());
    assert_eq!(approvers(&draft.preview().await), 1);

    // Back to romulus's own choice: what the approval was of no longer is.
    draft.toggle(KEY, None).await;
    assert_eq!(approvers(&draft.preview().await), 0);
    draft.toggle(KEY, Some(false)).await;
    assert_eq!(approvers(&draft.preview().await), 1, "and back again");
    publish(&draft, Some(json!({KEY: false}))).await;
    let kept = sqlx::query_scalar!(
        "SELECT count(*) FROM version_approvals a JOIN versions v ON v.id = a.version_id
         WHERE v.database_id = $1 AND v.number = 2",
        draft.shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(kept, Some(1));
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_forged_decision_renames_nothing_and_the_version_does_not_keep_it(db: PgPool) {
    const FORGED: &str = "column/public/users/id/admin_id";
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    draft.toggle(FORGED, Some(true)).await;
    let forged = json!({FORGED: true, "table/public//users/admins": true});
    let body = draft.migration(&sha, Some(forged)).await;
    assert_eq!(
        statements(&body),
        ["ALTER TABLE public.users RENAME COLUMN email TO email_address;"]
    );
    assert_eq!(body["renames"].as_array().unwrap().len(), 1);
    publish(&draft, Some(json!({FORGED: true}))).await;
    let stored = sqlx::query_scalar!(
        "SELECT renames FROM versions WHERE database_id = $1 AND number = 2",
        draft.shop
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(stored["known"].as_array().unwrap().len(), 1);
    assert!(
        !stored.to_string().contains("admin"),
        "only candidates are kept: {stored}"
    );
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn every_decision_in_the_draft_counts_and_a_request_has_a_cap(db: PgPool) {
    let mut draft = draft(&db).await;
    let sha = draft.write(RENAMED).await;
    // Thousands of keys sorting before the real one: it is still read.
    draft
        .editor
        .edit(|roots: &Roots, txn: &mut TransactionMut| {
            for n in 0..2100 {
                roots.renames.insert(txn, format!("a/{n:05}"), true);
            }
        })
        .await;
    draft.toggle(KEY, Some(false)).await;
    let body = draft.migration(&sha, None).await;
    assert_eq!(body["renames"][0]["applied"], false);

    // A request carries at most 2000.
    let many: serde_json::Map<String, Value> = (0..2001)
        .map(|n| (format!("a/{n:05}"), Value::Bool(true)))
        .collect();
    let response = draft
        .app
        .as_user(&draft.team.reviewer)
        .get(&draft.path(&format!(
            "/migration?sha256={sha}&renames={}",
            urlencode(&Value::Object(many).to_string())
        )))
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.code(), "bad_request");
}

/// Version `number` with the renames it was published with.
async fn published(db: &PgPool, database: Uuid, number: i32, sql: &str, renames: Value) {
    let model = serde_json::to_value(model_of(sql)).expect("json");
    sqlx::query!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout, renames)
         VALUES ($1, $2, $3, $4, 'sql', 'upload', '{}', $5)",
        database,
        number,
        model,
        sql,
        renames,
    )
    .execute(db)
    .await
    .expect("insert version");
}

fn column(from: &str, to: &str) -> Value {
    json!({"kind": "column", "schema": "public", "parent": "t", "from": from, "to": to})
}

#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]
async fn a_range_follows_each_version_s_renames_in_turn_a_swap_then_a_rename(db: PgPool) {
    let app = TestApp::new(db.clone());
    let team = Team::new(&db, "acme").await;
    let shop = database(&db, team.org, "shop").await;
    // v1 t(a integer, b text); v2 swaps them; v3 renames b (a's integers) to c.
    published(
        &db,
        shop,
        1,
        "CREATE TABLE t (a integer, b text);\n",
        json!({}),
    )
    .await;
    published(
        &db,
        shop,
        2,
        "CREATE TABLE t (b integer, a text);\n",
        json!({"known": [column("a", "b"), column("b", "a")]}),
    )
    .await;
    published(
        &db,
        shop,
        3,
        "CREATE TABLE t (c integer, a text);\n",
        json!({"known": [column("b", "c")]}),
    )
    .await;
    let migration = async |from: i32, to: i32| -> Value {
        let path = team.path(&format!("/databases/shop/migration?from={from}&to={to}"));
        let response = app.as_user(&team.reviewer).get(&path).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        response.json()
    };
    // The integers end in c, the text in a: a's rows become c's, b's a's.
    let forward = migration(1, 3).await;
    assert_eq!(
        statements(&forward),
        [
            "ALTER TABLE public.t RENAME COLUMN a TO c;",
            "ALTER TABLE public.t RENAME COLUMN b TO a;",
        ]
    );
    assert_eq!(forward["destructive"], 0);
    // And back: c's rows to a, a's to b.
    let back = migration(3, 1).await;
    assert_eq!(
        statements(&back),
        [
            "ALTER TABLE public.t RENAME COLUMN a TO b;",
            "ALTER TABLE public.t RENAME COLUMN c TO a;",
        ]
    );
    assert_eq!(back["destructive"], 0);
}
