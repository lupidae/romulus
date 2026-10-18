//! A console session (docs/playground.md, "Console API"): one websocket is
//! one Postgres session as `play_read`, so a transaction can span runs and
//! closing the tab rolls it back.
//!
//! Read-only is the role's privileges (deploy/playground/play-init.sql), not
//! a parse of the SQL: the text is split ([`romulus_sandbox::parse`]) only to
//! run and report each statement apart, in the simple query protocol (every
//! value as Postgres's text). `COPY` is refused with an explanation.
//!
//! Limits ([`crate::protocol::limits`]): one run at a time per session, a
//! host-wide queue of statements ([`Host::statements`]), a deadline per
//! statement (30 s read, 2 min develop; `statement_timeout` is the user's to
//! change, so it is only a convenience): past it the statement is cancelled,
//! then its backend terminated. 1 000 rows kept, 64 KiB per value, 16 MiB
//! per run. Each statement is sent in the extended protocol and executed for
//! at most 1 001 rows, then closed ([`Conn::send_limited`]): a huge result
//! stops there, without a cancel request (which could reach the next
//! statement), and the result says it was cut. Results flow through a
//! bounded channel: a slow client stalls its own backend only.
//!
//! A session does not keep its playground running: statements do
//! ([`Entry::statement`]). Stopped (idle, or for room), the session hears
//! why (`stopped`), and its next run starts the playground again; when it
//! cannot start, `unavailable` says why and `open` tries again.

use std::{sync::Arc, time::Duration};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use romulus_play_runner::password::Role;
use romulus_sandbox::pg_query::NodeEnum;
use tokio::{
    sync::{
        broadcast::{self, error::RecvError},
        mpsc,
    },
    task::JoinHandle,
    time::Instant,
};

use super::{
    host::{Entry, Host, HostError, Notice, connect},
    pg::{self, BackendKey, Conn, Event, Field, PgError, Target},
};
use crate::protocol::{
    Access, Column, ConsoleEvent, ConsoleRequest, RunStatus, SqlError, TextSpan, Txn, limits,
};

/// After a cancel request, how long a statement has to stop before its
/// backend is terminated.
const GRACE: Duration = Duration::from_secs(5);
/// Rows are sent in messages of at most this many bytes too.
const BATCH_BYTES: usize = 256 * 1024;

type Out = mpsc::Sender<ConsoleEvent>;

struct Running {
    id: String,
    key: BackendKey,
    target: Target,
    /// Pauses before it started: a pause since means its session is stale.
    epoch: u64,
    task: JoinHandle<Option<Conn>>,
}

/// Where the session's database stands, for the loop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// A session is open (or a run holds it).
    Open,
    /// Someone resets or migrates it: [`Notice::Resume`] brings it back.
    Paused,
    /// Stopped, or it could not start: the next run (or `open`) tries.
    Down,
}

/// Serves one websocket until it closes, the service shuts down, or the
/// playground is deleted.
#[allow(clippy::too_many_lines, reason = "the session's one event loop")]
pub async fn serve(host: Arc<Host>, entry: Arc<Entry>, access: Access, socket: WebSocket) {
    let _counted = entry.session();
    let (mut sink, mut stream) = socket.split();
    let (out, mut outbox) = mpsc::channel::<ConsoleEvent>(16);
    let writer = tokio::spawn(async move {
        while let Some(event) = outbox.recv().await {
            let Ok(text) = serde_json::to_string(&event) else {
                continue;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut notices = entry.notices.subscribe();
    let mut closing = host.closing.subscribe();
    let mut running: Option<Running> = None;
    // Counts pauses and stops: a session from before the last one is to a
    // database that was stopped or replaced.
    let mut epoch = 0u64;
    if *closing.borrow() {
        return;
    }
    // Waits for an operation to end when a session is paused by it: the
    // fallback for a resume it did not hear.
    let mut settle: Option<Settle> = None;
    let (mut conn, mut standing) = match entry.busy() {
        // Being stopped: nothing comes back by itself.
        Some("stopping") => {
            let _ = out
                .send(ConsoleEvent::Stopped {
                    reason: "it was being stopped".to_owned(),
                })
                .await;
            (None, Standing::Down)
        }
        // A reset, a migration or a restart in progress: back once it ends.
        Some(what) => {
            let _ = out
                .send(ConsoleEvent::Paused {
                    reason: format!("the playground is {what}"),
                })
                .await;
            settle = Some(settled(&entry));
            (None, Standing::Paused)
        }
        None => match open(&host, &entry, &out, &mut notices).await {
            Some(conn) => (Some(conn), Standing::Open),
            None => (None, Standing::Down),
        },
    };

    loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let Ok(request) = serde_json::from_str::<ConsoleRequest>(&text) else {
                        break;
                    };
                    match request {
                        ConsoleRequest::Run { id, sql, explain } => {
                            if running.is_some() {
                                let _ = out.send(refused(&id, "A run is in progress in this tab: wait for it, or cancel it.")).await;
                                continue;
                            }
                            if conn.is_none() {
                                if let Some(what) = entry.busy() {
                                    let _ = out.send(refused(&id, &format!("The playground is {what}: the console comes back when it is done."))).await;
                                    continue;
                                }
                                // Stopped, or it could not start: a run starts it.
                                conn = open(&host, &entry, &out, &mut notices).await;
                                standing = if conn.is_some() { Standing::Open } else { Standing::Down };
                            }
                            let Some(session) = conn.take() else {
                                let _ = out.send(refused(&id, "The playground could not start: see why above, then try again.")).await;
                                continue;
                            };
                            running = Some(Running {
                                id: id.clone(),
                                key: session.key(),
                                target: session.target().clone(),
                                epoch,
                                task: tokio::spawn(execute(Arc::clone(&host), Arc::clone(&entry), access, session, id, sql, explain, out.clone())),
                            });
                        }
                        ConsoleRequest::Cancel { id } => {
                            if let Some(run) = running.as_ref().filter(|r| r.id == id) {
                                let (target, key) = (run.target.clone(), run.key);
                                tokio::spawn(async move { let _ = pg::cancel(&target, key).await; });
                            }
                        }
                        ConsoleRequest::Open => {
                            if conn.is_none() && running.is_none() && standing == Standing::Down {
                                if let Some(what) = entry.busy() {
                                    let _ = out.send(ConsoleEvent::Paused { reason: format!("the playground is {what}") }).await;
                                    standing = Standing::Paused;
                                } else {
                                    conn = open(&host, &entry, &out, &mut notices).await;
                                    standing = if conn.is_some() { Standing::Open } else { Standing::Down };
                                }
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            finished = async { (&mut running.as_mut().expect("guarded").task).await }, if running.is_some() => {
                let started = running.take().map_or(epoch, |r| r.epoch);
                match finished {
                    Ok(Some(session)) if standing == Standing::Open && started == epoch => conn = Some(session),
                    Ok(Some(session)) => {
                        session.close().await;
                        // Paused or stopped while it ran: a session to today's
                        // database once it is back.
                    }
                    // The session ended (terminated past its deadline, or
                    // the database went away): a fresh one.
                    Ok(None) | Err(_) => {
                        if standing == Standing::Open {
                            conn = open(&host, &entry, &out, &mut notices).await;
                            if conn.is_none() {
                                standing = Standing::Down;
                            }
                        }
                    }
                }
            },
            () = async { settle.as_mut().expect("guarded").await }, if settle.is_some() => {
                settle = None;
                if standing == Standing::Paused && conn.is_none() && running.is_none() {
                    conn = open(&host, &entry, &out, &mut notices).await;
                    standing = if conn.is_some() { Standing::Open } else { Standing::Down };
                }
            },
            notice = notices.recv() => match notice {
                Ok(Notice::Pause(reason)) => {
                    standing = Standing::Paused;
                    epoch += 1;
                    let_go(&mut conn, running.as_ref()).await;
                    let _ = out.send(ConsoleEvent::Paused { reason }).await;
                    settle = Some(settled(&entry));
                }
                Ok(Notice::Deleting) => {
                    // A failed deletion resumes.
                    standing = Standing::Paused;
                    epoch += 1;
                    let_go(&mut conn, running.as_ref()).await;
                    let _ = out.send(ConsoleEvent::Deleting).await;
                }
                Ok(Notice::Stop(reason)) => {
                    standing = Standing::Down;
                    epoch += 1;
                    let_go(&mut conn, running.as_ref()).await;
                    let _ = out.send(ConsoleEvent::Stopped { reason }).await;
                }
                Ok(Notice::Resume) => {
                    settle = None;
                    if standing == Standing::Paused {
                        if conn.is_none() && running.is_none() {
                            conn = open(&host, &entry, &out, &mut notices).await;
                            standing = if conn.is_some() { Standing::Open } else { Standing::Down };
                        } else {
                            standing = Standing::Open;
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            _ = closing.changed() => break,
        }
    }
    if let Some(run) = running.take() {
        let _ = pg::cancel(&run.target, run.key).await;
        run.task.abort();
    }
    if let Some(session) = conn.take() {
        session.close().await;
    }
    drop(out);
    let _ = writer.await;
}

type Settle = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Ends once no operation runs on the playground (polled: it never holds
/// the operation lock, which a reset or a migration takes with `try_lock`).
fn settled(entry: &Arc<Entry>) -> Settle {
    let entry = Arc::clone(entry);
    Box::pin(async move {
        // The operation that paused us may not have marked itself yet.
        tokio::time::sleep(Duration::from_millis(500)).await;
        while entry.busy().is_some() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}

/// Lets go of the database: the idle session closes, a running statement is
/// cancelled (its session is dropped when it ends).
async fn let_go(conn: &mut Option<Conn>, running: Option<&Running>) {
    if let Some(run) = running {
        let (target, key) = (run.target.clone(), run.key);
        tokio::spawn(async move {
            let _ = pg::cancel(&target, key).await;
        });
    }
    if let Some(session) = conn.take() {
        session.close().await;
    }
}

fn refused(id: &str, message: &str) -> ConsoleEvent {
    ConsoleEvent::Finished {
        id: id.to_owned(),
        status: RunStatus::Error,
        statements: 0,
        rows: None,
        ms: 0,
        error: Some(message.to_owned()),
    }
}

/// What this session did when it found a role changed from inside.
fn restarted() -> ConsoleEvent {
    ConsoleEvent::Restarted {
        reason: "A role was changed from inside the playground (its password or settings): it restarted to undo that.".to_owned(),
    }
}

/// Why a session cannot open, for the person: a stable code and a sentence.
fn unavailable(code: &str, reason: impl Into<String>) -> ConsoleEvent {
    ConsoleEvent::Unavailable {
        code: code.to_owned(),
        reason: reason.into(),
    }
}

/// A session as `play_read`, starting the playground if it is stopped. On
/// failure the person hears why (`unavailable`) and may try again. A role
/// changed from inside (its password refused, its settings not what a start
/// leaves) is undone by a restart first.
async fn open(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    out: &Out,
    notices: &mut broadcast::Receiver<Notice>,
) -> Option<Conn> {
    let mut repaired = false;
    let opened = open_session(host, entry, out, &mut repaired).await;
    if repaired {
        // The restart's own pause and resume, heard while this waited for
        // it: stale for the session it opened.
        *notices = notices.resubscribe();
    }
    opened
}

/// What a role changed from inside the playground.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Changed {
    /// Its settings: a session can still open.
    Settings,
    /// Its password: no session opens until a restart.
    Login,
}

/// Restarts the playground to undo a changed role; `false` when the session
/// cannot open (the person heard why).
async fn repair(host: &Arc<Host>, entry: &Arc<Entry>, out: &Out, changed: Changed) -> bool {
    match host.repair(entry).await {
        Ok(_) => {
            let _ = out.send(restarted()).await;
            true
        }
        // Another session restarts it (or someone resets it): wait for that,
        // then connect.
        Err(HostError::Busy(_)) => {
            host.settled(entry).await;
            true
        }
        // Restarted a moment ago: settings stay until the next start (a
        // restart a minute pauses everyone enough); a login cannot.
        Err(HostError::RepairedRecently) if changed == Changed::Settings => {
            tracing::warn!(playground = %entry.id, "roles changed again soon after a restart: not restarting yet");
            true
        }
        Err(HostError::RepairedRecently) => {
            let _ = out
                .send(unavailable(
                    "not_starting",
                    "Its login was changed from inside the playground again, a moment after a restart to undo that. Try again in a minute.",
                ))
                .await;
            false
        }
        Err(_) => {
            let _ = out
                .send(unavailable(
                    "not_starting",
                    "A role was changed from inside the playground and it could not restart to undo it.",
                ))
                .await;
            false
        }
    }
}

async fn open_session(
    host: &Arc<Host>,
    entry: &Arc<Entry>,
    out: &Out,
    repaired: &mut bool,
) -> Option<Conn> {
    for attempt in 0..4 {
        // Another session's restart, or a reset: connect to what it leaves.
        if entry.busy().is_some() {
            host.settled(entry).await;
        }
        if entry.target().is_none() {
            let _ = out.send(ConsoleEvent::Starting).await;
        }
        let target = match host.ensure_running(entry).await {
            Ok(target) => target,
            Err(err) => {
                // What the person can act on; the host's own errors (paths,
                // Docker) stay in the log.
                let (code, reason) = match &err {
                    HostError::HostFull(_) => {
                        ("host_full", format!("The playground cannot start: {err}."))
                    }
                    HostError::OrgFull(_) => {
                        ("org_full", format!("The playground cannot start: {err}."))
                    }
                    HostError::NotStarting(why) => (
                        "not_starting",
                        format!("The playground's database did not start: {why}."),
                    ),
                    _ => {
                        tracing::warn!(playground = %entry.id, error = %err, "console: cannot start");
                        (
                            "not_starting",
                            "The playground host could not start it. Try again in a moment."
                                .to_owned(),
                        )
                    }
                };
                let _ = out.send(unavailable(code, reason)).await;
                return None;
            }
        };
        if !*repaired && !host.roles_untouched(entry).await {
            *repaired = true;
            if !repair(host, entry, out, Changed::Settings).await {
                return None;
            }
            continue;
        }
        match connect(&target, entry.id, Role::Read, &host.config.secret).await {
            Ok(conn) => {
                let _ = out.send(ConsoleEvent::Ready { txn: Txn::Idle }).await;
                return Some(conn);
            }
            // Its password was changed from inside: a restart puts it back.
            Err(PgError::Db(error)) if error.code == "28P01" && !*repaired => {
                *repaired = true;
                if !repair(host, entry, out, Changed::Login).await {
                    return None;
                }
            }
            // An operation took the database away meanwhile (a restart, a
            // reset): wait for it, then again.
            Err(_) if entry.busy().is_some() && attempt < 3 => {}
            // It stopped behind the service's back (its memory limit):
            // start it again, once.
            Err(PgError::Io(_) | PgError::Timeout) if attempt == 0 => host.forget(entry).await,
            Err(err) => {
                tracing::warn!(playground = %entry.id, error = %err, "console: cannot connect");
                let _ = out
                    .send(unavailable(
                        "unreachable",
                        "The playground cannot be reached. Try again in a moment.",
                    ))
                    .await;
                return None;
            }
        }
    }
    let _ = out
        .send(unavailable(
            "unreachable",
            "The playground cannot be reached.",
        ))
        .await;
    None
}

/// Why a statement was cancelled by the service.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Deadline,
    /// `COPY … TO STDOUT` (from a function): stopped, nothing kept.
    Copy,
}

/// One run: each statement in order, stopping at the first error. Answers
/// the session back, or `None` when it had to be terminated.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one run's inputs, spawned as a task; its statements in order"
)]
async fn execute(
    host: Arc<Host>,
    entry: Arc<Entry>,
    access: Access,
    mut conn: Conn,
    id: String,
    sql: String,
    explain: bool,
    out: Out,
) -> Option<Conn> {
    let started = Instant::now();
    let finish = |status, statements, rows, error: Option<String>| ConsoleEvent::Finished {
        id: id.clone(),
        status,
        statements,
        rows,
        ms: millis(started),
        error,
    };
    if sql.len() > limits::SQL_BYTES {
        let _ = out
            .send(finish(
                RunStatus::Error,
                0,
                None,
                Some(format!(
                    "The SQL is over {} KiB: run less at a time.",
                    limits::SQL_BYTES / 1024
                )),
            ))
            .await;
        return Some(conn);
    }
    let parsed = {
        let text = sql.clone();
        tokio::task::spawn_blocking(move || romulus_sandbox::parse(&text))
            .await
            .ok()?
    };
    if let Some(error) = parsed.errors.first() {
        let error = SqlError {
            sqlstate: error.sqlstate.clone().unwrap_or_else(|| "42601".to_owned()),
            message: error.message.clone(),
            detail: error.detail.clone(),
            hint: error.hint.clone(),
            span: error.span.map(|s| TextSpan::of(&sql, s.range())),
        };
        let message = error.message.clone();
        let _ = out
            .send(ConsoleEvent::Error {
                id: id.clone(),
                index: 0,
                error,
            })
            .await;
        let _ = out
            .send(finish(RunStatus::Error, 0, None, Some(message)))
            .await;
        return Some(conn);
    }
    let statements = &parsed.statements;
    let refusal = if statements.len() > limits::STATEMENTS {
        Some(format!(
            "{} statements: run at most {} at a time.",
            statements.len(),
            limits::STATEMENTS
        ))
    } else if explain && statements.len() != 1 {
        Some("Explain shows one statement's plan: select it, or put the cursor in it.".to_owned())
    } else {
        None
    };
    if let Some(message) = refusal {
        let _ = out
            .send(finish(RunStatus::Error, 0, None, Some(message)))
            .await;
        return Some(conn);
    }

    let deadline = match access {
        Access::Read => host.config.read_deadline,
        Access::Develop => host.config.develop_deadline,
    };
    let mut budget = limits::RUN_BYTES;
    let mut last_rows = None;
    let mut status = RunStatus::Ok;
    let mut first_error = None;
    let mut executed = 0;
    for (index, statement) in statements.iter().enumerate() {
        let span = TextSpan::of(&sql, statement.span.range());
        let text = statement.text(&sql);
        if matches!(statement.node(), NodeEnum::CopyStmt(_)) {
            let error = SqlError {
                sqlstate: "0A000".to_owned(),
                message: "COPY is not available in the console".to_owned(),
                hint: Some("Query the table with SELECT instead.".to_owned()),
                span: Some(span),
                ..SqlError::default()
            };
            first_error = Some(error.message.clone());
            status = RunStatus::Error;
            let _ = out
                .send(ConsoleEvent::Error {
                    id: id.clone(),
                    index,
                    error,
                })
                .await;
            break;
        }
        let (query, offset) = if explain && !matches!(statement.node(), NodeEnum::ExplainStmt(_)) {
            let prefix = "EXPLAIN (ANALYZE, BUFFERS) ";
            (format!("{prefix}{text}"), prefix.len())
        } else {
            (text.to_owned(), 0)
        };
        let permit = if let Ok(permit) = Arc::clone(&host.statements).try_acquire_owned() {
            permit
        } else {
            let _ = out.send(ConsoleEvent::Queued { id: id.clone() }).await;
            Arc::clone(&host.statements).acquire_owned().await.ok()?
        };
        let _ = out
            .send(ConsoleEvent::Statement {
                id: id.clone(),
                index,
                span,
            })
            .await;
        executed += 1;
        let counted = entry.statement();
        let outcome = Statement {
            host: &host,
            entry: &entry,
            conn: &mut conn,
            out: &out,
            id: &id,
            index,
            explain,
        }
        .run(&query, deadline, &mut budget)
        .await;
        drop(counted);
        drop(permit);
        match outcome {
            Outcome::Ok(rows) => last_rows = Some(rows),
            Outcome::Failed {
                mut error,
                cancelled,
            } => {
                // Located in the whole text, past our EXPLAIN prefix.
                error.span = error
                    .span
                    .and_then(|s| s.start.checked_sub(offset))
                    .map(|start| {
                        let at = statement.span.start + start;
                        let end = sql[at..]
                            .find(|c: char| c.is_whitespace() || c == ',' || c == ';')
                            .map_or(statement.span.end, |n| at + n.max(1));
                        TextSpan::of(&sql, at..end.min(statement.span.end).max(at))
                    })
                    .or(Some(span));
                status = if cancelled {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Error
                };
                first_error = Some(error.message.clone());
                let _ = out
                    .send(ConsoleEvent::Error {
                        id: id.clone(),
                        index,
                        error,
                    })
                    .await;
                break;
            }
            Outcome::Gone { terminated } => {
                let message = if terminated {
                    format!(
                        "The statement ran past its {} s limit: it was stopped, and its session ended.",
                        deadline.as_secs()
                    )
                } else {
                    "The session ended while the statement ran (the playground was reset or stopped, or ran out of memory).".to_owned()
                };
                let _ = out
                    .send(finish(RunStatus::Cancelled, executed, None, Some(message)))
                    .await;
                return None;
            }
        }
    }
    let _ = out
        .send(finish(status, executed, last_rows, first_error))
        .await;
    Some(conn)
}

enum Outcome {
    /// Rows the statement returned (or its tag counted).
    Ok(u64),
    Failed {
        error: SqlError,
        /// Cancelled (by the person, or past its deadline).
        cancelled: bool,
    },
    /// The session is gone: terminated past its deadline, or lost.
    Gone { terminated: bool },
}

struct Statement<'a> {
    host: &'a Arc<Host>,
    entry: &'a Arc<Entry>,
    conn: &'a mut Conn,
    out: &'a Out,
    id: &'a str,
    index: usize,
    explain: bool,
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn sql_error(error: &pg::DbError, query: &str) -> SqlError {
    SqlError {
        sqlstate: error.code.clone(),
        message: error.message.clone(),
        detail: error.detail.clone(),
        hint: error.hint.clone(),
        span: error
            .byte_position(query)
            .map(|at| TextSpan::of(query, at..at)),
    }
}

impl Statement<'_> {
    async fn send(&self, event: ConsoleEvent) {
        let _ = self.out.send(event).await;
    }

    async fn columns(&self, fields: &[Field]) -> Vec<Column> {
        let keys: Vec<(u32, i32)> = fields
            .iter()
            .map(|f| (f.type_oid, f.type_modifier))
            .collect();
        let types = match self.entry.cached_types(&keys) {
            Some(types) => types,
            None => self.resolve(&keys).await,
        };
        fields
            .iter()
            .zip(types)
            .map(|(field, (type_name, numeric))| Column {
                name: field.name.clone(),
                type_name,
                numeric,
            })
            .collect()
    }

    /// `format_type` of each (oid, typmod), on the control session.
    async fn resolve(&self, keys: &[(u32, i32)]) -> Vec<(String, bool)> {
        let unknown = || vec![("unknown".to_owned(), false); keys.len()];
        let oids: Vec<String> = keys.iter().map(|(o, _)| o.to_string()).collect();
        let mods: Vec<String> = keys.iter().map(|(_, m)| m.to_string()).collect();
        let query = format!(
            "SELECT pg_catalog.format_type(u.o, u.m), coalesce(t.typcategory = 'N', false) \
             FROM unnest('{{{}}}'::oid[], '{{{}}}'::int4[]) WITH ORDINALITY AS u(o, m, i) \
             LEFT JOIN pg_catalog.pg_type t ON t.oid = u.o ORDER BY u.i",
            oids.join(","),
            mods.join(",")
        );
        let Ok(mut control) = self.entry.control(&self.host.config.secret).await else {
            return unknown();
        };
        let Some(session) = control.as_mut() else {
            return unknown();
        };
        match session.query(&query).await {
            Ok(rows) if rows.len() == keys.len() => {
                let types: Vec<(String, bool)> = rows
                    .into_iter()
                    .map(|r| {
                        let name = r.first().cloned().flatten().unwrap_or_default();
                        let numeric = r.get(1).cloned().flatten().as_deref() == Some("t");
                        (name, numeric)
                    })
                    .collect();
                self.entry
                    .cache_types(keys.iter().copied().zip(types.iter().cloned()));
                types
            }
            Ok(_) => unknown(),
            Err(_) => {
                *control = None;
                unknown()
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one statement's whole protocol exchange"
    )]
    async fn run(self, query: &str, limit: Duration, budget: &mut usize) -> Outcome {
        let started = Instant::now();
        // One row past the kept ones says whether there were more. A plan
        // is read whole.
        let max_rows = if self.explain {
            0
        } else {
            i32::try_from(limits::ROWS + 1).unwrap_or(i32::MAX)
        };
        if let Err(err) = self.conn.send_limited(query, max_rows).await {
            return match err {
                PgError::Io(_) => Outcome::Gone { terminated: false },
                other => Outcome::Failed {
                    error: SqlError {
                        sqlstate: "08000".to_owned(),
                        message: other.to_string(),
                        ..SqlError::default()
                    },
                    cancelled: false,
                },
            };
        }
        let key = self.conn.key();
        let target = self.conn.target().clone();
        let deadline = started + limit;
        let mut stopped: Option<(Stop, Instant)> = None;
        let mut kept = 0usize;
        let mut counted = 0u64;
        // Rows past the limits, read and dropped.
        let mut dropped = false;
        // The statement had more rows than it was executed for.
        let mut suspended = false;
        let mut batch: Vec<Vec<Option<String>>> = Vec::new();
        let mut batch_bytes = 0usize;
        let mut cells_cut = false;
        let mut plan = String::new();
        let mut tag = String::new();
        let mut failure: Option<pg::DbError> = None;
        loop {
            let terminate_at = stopped.map(|(_, at)| at + GRACE);
            let event = tokio::select! {
                biased;
                event = self.conn.next() => event,
                () = tokio::time::sleep_until(deadline), if stopped.is_none() => {
                    stopped = Some((Stop::Deadline, Instant::now()));
                    let _ = pg::cancel(&target, key).await;
                    continue;
                }
                () = async {
                    match terminate_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.host.terminate(self.entry, key.pid).await;
                    return Outcome::Gone { terminated: true };
                }
            };
            let Ok(event) = event else {
                return Outcome::Gone { terminated: false };
            };
            match event {
                Event::Columns(fields) => {
                    if !self.explain {
                        let columns = self.columns(&fields).await;
                        self.send(ConsoleEvent::Columns {
                            id: self.id.to_owned(),
                            index: self.index,
                            columns,
                        })
                        .await;
                    }
                }
                Event::Row(values) => {
                    counted += 1;
                    if self.explain {
                        if let Some(Some(line)) = values.into_iter().next() {
                            plan.push_str(&line);
                            plan.push('\n');
                        }
                        continue;
                    }
                    // Past the limits (the 1 001st row, or the run's bytes):
                    // kept out; the statement stops at that row by itself.
                    if dropped || kept >= limits::ROWS || stopped.is_some() {
                        dropped = true;
                        continue;
                    }
                    let mut row = Vec::with_capacity(values.len());
                    let mut bytes = 0;
                    for value in values {
                        row.push(value.map(|mut v| {
                            if v.len() > limits::CELL_BYTES {
                                let mut end = limits::CELL_BYTES;
                                while !v.is_char_boundary(end) {
                                    end -= 1;
                                }
                                v.truncate(end);
                                cells_cut = true;
                            }
                            bytes += v.len();
                            v
                        }));
                    }
                    if bytes > *budget {
                        dropped = true;
                        continue;
                    }
                    *budget -= bytes;
                    batch_bytes += bytes;
                    batch.push(row);
                    kept += 1;
                    if batch.len() >= limits::ROWS_PER_MESSAGE || batch_bytes >= BATCH_BYTES {
                        batch_bytes = 0;
                        self.send(ConsoleEvent::Rows {
                            id: self.id.to_owned(),
                            index: self.index,
                            rows: std::mem::take(&mut batch),
                        })
                        .await;
                    }
                }
                // The console reads with values (never `next_skipping_rows`).
                Event::OversizedRow(_) | Event::SkippedRow => {
                    counted += 1;
                    cells_cut = true;
                    dropped = true;
                }
                Event::Done(done) => tag = done,
                Event::Suspended => suspended = true,
                Event::Empty => {}
                Event::Error(error) => failure = failure.or(Some(error)),
                Event::Notice(notice) => {
                    self.send(ConsoleEvent::Notice {
                        id: self.id.to_owned(),
                        index: self.index,
                        severity: notice.severity.clone(),
                        message: notice.message.clone(),
                    })
                    .await;
                }
                Event::CopyIn => {
                    let _ = self
                        .conn
                        .copy_fail("COPY is not available in the console")
                        .await;
                }
                Event::CopyOut | Event::CopyData(_) => {
                    if stopped.is_none() {
                        stopped = Some((Stop::Copy, Instant::now()));
                        let _ = pg::cancel(&target, key).await;
                    }
                }
                Event::Ready(txn) => {
                    if !batch.is_empty() {
                        self.send(ConsoleEvent::Rows {
                            id: self.id.to_owned(),
                            index: self.index,
                            rows: std::mem::take(&mut batch),
                        })
                        .await;
                    }
                    self.send(ConsoleEvent::Txn { txn }).await;
                    break;
                }
            }
        }
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stop = stopped.map(|(why, _)| why);
        match failure {
            Some(error) if error.code == "57014" => {
                let mut error = sql_error(&error, query);
                if stop == Some(Stop::Deadline) {
                    error.message = format!(
                        "The statement ran past its {} s limit and was cancelled.",
                        limit.as_secs()
                    );
                    error.hint = Some(
                        "Narrow it (a WHERE, a LIMIT), or look at its plan with Explain."
                            .to_owned(),
                    );
                }
                Outcome::Failed {
                    error,
                    cancelled: true,
                }
            }
            Some(error) => Outcome::Failed {
                error: sql_error(&error, query),
                cancelled: false,
            },
            None => {
                if self.explain {
                    self.send(ConsoleEvent::Plan {
                        id: self.id.to_owned(),
                        index: self.index,
                        text: plan,
                    })
                    .await;
                }
                let rows = tag
                    .rsplit(' ')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(counted);
                if suspended {
                    // Stopped past the kept rows: how many more, nobody
                    // computed.
                    self.done(format!("SELECT {kept}"), kept as u64, true, cells_cut, ms)
                        .await;
                    return Outcome::Ok(kept as u64);
                }
                // Cut only when rows were left out.
                self.done(tag, rows, dropped, cells_cut, ms).await;
                Outcome::Ok(rows)
            }
        }
    }

    async fn done(&self, tag: String, rows: u64, truncated: bool, cells_cut: bool, ms: u64) {
        self.send(ConsoleEvent::Done {
            id: self.id.to_owned(),
            index: self.index,
            tag,
            rows,
            truncated,
            cells_cut,
            ms,
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_a_finished_run_with_its_reason() {
        let ConsoleEvent::Finished { status, error, .. } = refused("r1", "busy") else {
            panic!("finished");
        };
        assert_eq!(status, RunStatus::Error);
        assert_eq!(error.as_deref(), Some("busy"));
    }
}
