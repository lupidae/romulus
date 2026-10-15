//! Drafts live: the API on a real socket, a y-websocket client made of yrs,
//! a compiler that needs no sandbox, and versions to start drafts from.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use futures_util::{SinkExt, StreamExt};
use romulus_api::{
    AppState, Config, app,
    collab::{
        Hub,
        compile::{Compiled, SqlCompiler, created_at},
        doc::{Roots, new_doc},
        protocol::MESSAGE_EVENT,
        room::Settings,
    },
    domain::Schema,
};
use romulus_sandbox::{Code, Diagnostic, LineIndex};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message as Frame, client::IntoClientRequest, http::HeaderValue},
};
use uuid::Uuid;
use yrs::{
    Doc, ReadTxn, Transact, TransactionMut, Update,
    sync::{
        Message, SyncMessage,
        awareness::{AwarenessUpdate, AwarenessUpdateEntry},
    },
    updates::{decoder::Decode, encoder::Encode},
};

use super::harness::{ORIGIN, Session, TestApp};

/// Short timings, so tests wait milliseconds, not seconds.
pub fn quick() -> Settings {
    Settings {
        debounce: Duration::from_millis(20),
        max_wait: Duration::from_millis(100),
        flush_every: Duration::from_millis(10),
        compact_after: 200,
        idle_unload: Duration::from_millis(50),
        retry_unavailable: Duration::from_millis(200),
        ..Settings::default()
    }
}

/// An app whose drafts compile with [`draft_compiler`].
pub fn live_app(db: PgPool, settings: Settings) -> TestApp {
    let hub = Hub::with_settings(db.clone(), draft_compiler(), settings);
    TestApp::with_state(AppState::new(db, Config::for_tests()).with_collab(hub))
}

/// Serves the app on a local port, for websockets.
pub async fn serve(test: &TestApp) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    let router = app(test.state.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    addr
}

/// A compiler without a sandbox: a table per `CREATE TABLE`, and an error
/// wherever the text says `ERROR`.
pub fn draft_compiler() -> SqlCompiler {
    SqlCompiler::from_fn(|sql| match sql.find("ERROR") {
        Some(at) => Compiled {
            model: None,
            diagnostics: vec![
                Diagnostic::error(Code::Sql, "syntax error at or near \"ERROR\"")
                    .with_span(LineIndex::new(sql).span(sql, at..at + 5)),
            ],
        },
        None => Compiled {
            model: Some(model_of(sql)),
            diagnostics: Vec::new(),
        },
    })
}

/// What [`draft_compiler`] makes of `sql`.
pub fn model_of(sql: &str) -> Schema {
    let mut tables: Vec<String> = created_at(sql).into_keys().collect();
    tables.sort();
    let entities: Vec<Value> = tables
        .iter()
        .map(|name| {
            let (schema, name) = name.split_once('.').expect("qualified");
            json!({
                "schema": schema, "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null,
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

/// Version `number` of the database: its SQL, the model that SQL compiles to,
/// and a layout (`versions.layout`).
pub async fn version(db: &PgPool, database: Uuid, number: i32, sql: &str, layout: Value) -> Uuid {
    let model = serde_json::to_value(model_of(sql)).expect("json");
    sqlx::query_scalar!(
        "INSERT INTO versions (database_id, number, model, source, source_kind, origin, layout)
         VALUES ($1, $2, $3, $4, 'sql', 'upload', $5) RETURNING id",
        database,
        number,
        model,
        sql,
        layout,
    )
    .fetch_one(db)
    .await
    .expect("insert version")
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A y-websocket client: a doc of its own, kept in sync like the browser's.
pub struct Client {
    socket: Socket,
    pub doc: Doc,
    pub roots: Roots,
    /// Server events (tag 100), in order.
    pub events: Vec<Value>,
    /// Awareness states by client id (`null` ones removed).
    pub awareness: HashMap<u64, Value>,
    /// Sync step 2 received.
    pub synced: bool,
    /// How the server closed the socket.
    pub closed: Option<u16>,
}

/// Why connecting failed: the HTTP status of the refused upgrade.
#[derive(Debug)]
pub struct Refused(pub u16);

impl Client {
    pub async fn connect(
        addr: SocketAddr,
        path: &str,
        session: Option<&Session>,
        origin: &str,
    ) -> Result<Self, Refused> {
        let mut request = format!("ws://{addr}{path}")
            .into_client_request()
            .expect("request");
        let headers = request.headers_mut();
        headers.insert("origin", HeaderValue::from_str(origin).expect("origin"));
        if let Some(session) = session {
            headers.insert(
                "cookie",
                HeaderValue::from_str(&session.cookie()).expect("cookie"),
            );
        }
        let socket = match connect_async(request).await {
            Ok((socket, _)) => socket,
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                return Err(Refused(response.status().as_u16()));
            }
            Err(err) => panic!("connect: {err}"),
        };
        let doc = new_doc();
        Ok(Self {
            roots: Roots::of(&doc),
            doc,
            socket,
            events: Vec::new(),
            awareness: HashMap::new(),
            synced: false,
            closed: None,
        })
    }

    /// Connects with the web app's `Origin`, and syncs.
    pub async fn join(addr: SocketAddr, path: &str, session: &Session) -> Self {
        let mut client = Self::connect(addr, path, Some(session), ORIGIN)
            .await
            .expect("upgrade");
        client.sync().await;
        client
    }

    pub fn sql(&self) -> String {
        self.roots.sql(&self.doc.transact())
    }

    pub fn last_event(&self, kind: &str) -> Option<&Value> {
        self.events.iter().rev().find(|e| e["type"] == kind)
    }

    pub async fn send(&mut self, message: &Message) {
        self.send_raw(message.encode_v1()).await;
    }

    pub async fn send_raw(&mut self, bytes: Vec<u8>) {
        self.socket
            .send(Frame::Binary(bytes.into()))
            .await
            .expect("send");
    }

    /// Sends step 1 and waits for step 2.
    pub async fn sync(&mut self) {
        let sv = self.doc.transact().state_vector();
        self.send(&Message::Sync(SyncMessage::SyncStep1(sv))).await;
        self.until(|c| c.synced).await;
    }

    /// Edits the local doc and sends the update, as the browser does.
    pub async fn edit(&mut self, edit: impl FnOnce(&Roots, &mut TransactionMut)) {
        let before = self.doc.transact().state_vector();
        {
            let mut txn = self.doc.transact_mut();
            edit(&self.roots, &mut txn);
        }
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        self.send(&Message::Sync(SyncMessage::Update(update))).await;
    }

    /// Announces an awareness state for `client`.
    pub async fn announce(&mut self, client: u64, clock: u32, state: &Value) {
        let update = AwarenessUpdate {
            clients: HashMap::from([(
                yrs::block::ClientID::new(client),
                AwarenessUpdateEntry {
                    clock,
                    json: state.to_string().into(),
                },
            )]),
        };
        self.send(&Message::Awareness(update)).await;
    }

    pub fn client_id(&self) -> u64 {
        self.doc.client_id().get()
    }

    /// Handles messages until `done` holds (5 s at most).
    pub async fn until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(self) {
            let next = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .unwrap_or_else(|_| panic!("timed out; events so far: {:?}", self.events));
            if !self.handle(next).await {
                assert!(done(self), "the socket closed ({:?}) first", self.closed);
                return;
            }
        }
    }

    /// Handles whatever arrives within `quiet`.
    pub async fn settle(&mut self, quiet: Duration) {
        while let Ok(next) = tokio::time::timeout(quiet, self.socket.next()).await {
            if !self.handle(next).await {
                return;
            }
        }
    }

    /// One frame; false once the socket is closed.
    async fn handle(
        &mut self,
        next: Option<Result<Frame, tokio_tungstenite::tungstenite::Error>>,
    ) -> bool {
        match next {
            Some(Ok(Frame::Binary(bytes))) => {
                for message in romulus_api::collab::protocol::decode(&bytes).expect("y-sync") {
                    self.receive(message).await;
                }
                true
            }
            Some(Ok(Frame::Close(frame))) => {
                self.closed = Some(frame.map_or(1005, |f| f.code.into()));
                false
            }
            Some(Ok(_)) => true,
            Some(Err(_)) | None => {
                self.closed.get_or_insert(1006);
                false
            }
        }
    }

    async fn receive(&mut self, message: Message) {
        match message {
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                // y-websocket answers with what the server lacks.
                let update = self.doc.transact().encode_state_as_update_v1(&sv);
                self.send(&Message::Sync(SyncMessage::SyncStep2(update)))
                    .await;
            }
            Message::Sync(SyncMessage::SyncStep2(update) | SyncMessage::Update(update)) => {
                self.doc
                    .transact_mut()
                    .apply_update(Update::decode_v1(&update).expect("update"))
                    .expect("apply");
                self.synced = true;
            }
            Message::Awareness(update) => {
                for (client, entry) in update.clients {
                    let state: Value = serde_json::from_str(&entry.json).expect("json");
                    if state.is_null() {
                        self.awareness.remove(&client.get());
                    } else {
                        self.awareness.insert(client.get(), state);
                    }
                }
            }
            Message::Custom(MESSAGE_EVENT, body) => {
                self.events
                    .push(serde_json::from_slice(&body).expect("event json"));
            }
            _ => {}
        }
    }
}

/// The content of a draft as stored: snapshot and updates.
pub async fn stored_sql(db: &PgPool, draft: Uuid) -> String {
    let mut updates: Vec<Vec<u8>> = sqlx::query_scalar!(
        "SELECT state FROM draft_snapshots WHERE draft_id = $1",
        draft
    )
    .fetch_all(db)
    .await
    .expect("snapshot");
    let upto = sqlx::query_scalar!(
        "SELECT upto_update_id FROM draft_snapshots WHERE draft_id = $1",
        draft
    )
    .fetch_optional(db)
    .await
    .expect("upto")
    .unwrap_or(0);
    updates.extend(
        sqlx::query_scalar!(
            r#"SELECT update AS "update!" FROM draft_updates WHERE draft_id = $1 AND id > $2 ORDER BY id"#,
            draft,
            upto
        )
        .fetch_all(db)
        .await
        .expect("updates"),
    );
    romulus_api::collab::doc::read(&updates).expect("decodes").0
}
