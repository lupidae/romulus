//! A small Postgres client of the frontend/backend protocol: what the
//! service needs and no more. The simple query protocol only (every value
//! arrives as Postgres's own text, so geometry, vectors, ranges and enums
//! read as in psql), SCRAM-SHA-256 only (the playground's `pg_hba` allows
//! nothing else), `COPY … FROM STDIN` for generated rows, notices in line,
//! the transaction state of every `ReadyForQuery`, and cancel requests.
//!
//! Messages are read one at a time and nothing is read ahead: a caller that
//! stops reading stops the backend (Postgres blocks on its socket), which is
//! the console's backpressure. A message past [`MAX_MESSAGE`] (one huge row)
//! is skipped as it arrives, never held in memory.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use bytes::{Buf, BytesMut};
use fallible_iterator::FallibleIterator;
use postgres_protocol::{
    IsNull,
    authentication::sasl::{ChannelBinding, SCRAM_SHA_256, ScramSha256},
    message::{
        backend::{self, Message},
        frontend,
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UnixStream},
};

use crate::protocol::Txn;

/// Larger backend messages are skipped (a row this size cannot be shown).
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Where a playground's Postgres listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A socket directory (`<dir>/.s.PGSQL.5432`).
    Socket(PathBuf),
    /// Local development only.
    Tcp(String, u16),
}

/// What a cancel request needs: the backend's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendKey {
    pub pid: i32,
    secret: i32,
}

/// A Postgres error or notice, its fields as Postgres sent them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbError {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// 1-based, in characters of the query string.
    pub position: Option<usize>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub constraint: Option<String>,
}

impl DbError {
    fn parse(fields: backend::ErrorFields<'_>) -> io::Result<Self> {
        let mut error = Self::default();
        let mut fields = fields;
        while let Some(field) = fields.next()? {
            let value = String::from_utf8_lossy(field.value_bytes()).into_owned();
            match field.type_() {
                b'V' => error.severity = value,
                b'S' if error.severity.is_empty() => error.severity = value,
                b'C' => error.code = value,
                b'M' => error.message = value,
                b'D' => error.detail = Some(value),
                b'H' => error.hint = Some(value),
                b'P' => error.position = value.parse().ok(),
                b's' => error.schema = Some(value),
                b't' => error.table = Some(value),
                b'c' => error.column = Some(value),
                b'n' => error.constraint = Some(value),
                _ => {}
            }
        }
        Ok(error)
    }

    /// The byte offset of `position` in `query`.
    pub fn byte_position(&self, query: &str) -> Option<usize> {
        let chars = self.position?.checked_sub(1)?;
        Some(
            query
                .char_indices()
                .nth(chars)
                .map_or(query.len(), |(i, _)| i),
        )
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PgError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Db(Box<DbError>),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out")]
    Timeout,
}

impl PgError {
    pub fn db(&self) -> Option<&DbError> {
        match self {
            Self::Db(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// A result column as the backend describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
}

/// What the backend says while a query runs.
#[derive(Debug)]
pub enum Event {
    Columns(Vec<Field>),
    /// Values as text; NULL as `None`.
    Row(Vec<Option<String>>),
    /// A row past [`MAX_MESSAGE`] bytes, skipped.
    OversizedRow(usize),
    /// A row read past without decoding its values ([`Conn::next_skipping_rows`]).
    SkippedRow,
    /// A statement finished: its command tag.
    Done(String),
    /// The statement had more rows than the `Execute` asked for
    /// ([`Conn::send_limited`]): it stopped there, nothing more runs.
    Suspended,
    Empty,
    Error(DbError),
    Notice(DbError),
    /// `COPY … FROM STDIN` waits for rows (see [`Conn::copy_in`]).
    CopyIn,
    /// `COPY … TO STDOUT`: its rows follow as [`Event::CopyData`].
    CopyOut,
    CopyData(usize),
    /// The query is over.
    Ready(Txn),
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send + Sync {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync> Io for T {}

/// One session.
pub struct Conn {
    io: Box<dyn Io>,
    target: Target,
    buf: BytesMut,
    key: BackendKey,
    txn: Txn,
}

async fn stream(target: &Target) -> io::Result<Box<dyn Io>> {
    Ok(match target {
        Target::Socket(dir) => Box::new(UnixStream::connect(dir.join(".s.PGSQL.5432")).await?),
        Target::Tcp(host, port) => {
            let stream = TcpStream::connect((host.as_str(), *port)).await?;
            stream.set_nodelay(true)?;
            Box::new(stream)
        }
    })
}

fn protocol(what: impl Into<String>) -> PgError {
    PgError::Protocol(what.into())
}

impl Conn {
    /// Connects to `database` as `user` and authenticates with SCRAM.
    pub async fn connect(
        target: &Target,
        user: &str,
        password: &str,
        database: &str,
        timeout: Duration,
    ) -> Result<Self, PgError> {
        tokio::time::timeout(timeout, Self::handshake(target, user, password, database))
            .await
            .map_err(|_| PgError::Timeout)?
    }

    async fn handshake(
        target: &Target,
        user: &str,
        password: &str,
        database: &str,
    ) -> Result<Self, PgError> {
        let mut conn = Self {
            io: stream(target).await?,
            target: target.clone(),
            buf: BytesMut::with_capacity(8 * 1024),
            key: BackendKey { pid: 0, secret: 0 },
            txn: Txn::Idle,
        };
        let mut out = BytesMut::new();
        frontend::startup_message(
            [
                ("user", user),
                ("database", database),
                ("application_name", "romulus-playground"),
                ("client_encoding", "UTF8"),
                ("DateStyle", "ISO, MDY"),
            ],
            &mut out,
        )?;
        conn.write(&out).await?;
        let mut scram: Option<ScramSha256> = None;
        loop {
            match conn.message().await? {
                Some(Message::AuthenticationSasl(body)) => {
                    let mut mechanisms = body.mechanisms();
                    let mut offered = false;
                    while let Some(mechanism) = mechanisms.next()? {
                        offered |= mechanism == SCRAM_SHA_256;
                    }
                    if !offered {
                        return Err(protocol("the server offers no SCRAM-SHA-256"));
                    }
                    let state =
                        ScramSha256::new(password.as_bytes(), ChannelBinding::unsupported());
                    let mut out = BytesMut::new();
                    frontend::sasl_initial_response(SCRAM_SHA_256, state.message(), &mut out)?;
                    conn.write(&out).await?;
                    scram = Some(state);
                }
                Some(Message::AuthenticationSaslContinue(body)) => {
                    let state = scram
                        .as_mut()
                        .ok_or_else(|| protocol("SASL out of order"))?;
                    state.update(body.data())?;
                    let mut out = BytesMut::new();
                    frontend::sasl_response(state.message(), &mut out)?;
                    conn.write(&out).await?;
                }
                Some(Message::AuthenticationSaslFinal(body)) => {
                    scram
                        .as_mut()
                        .ok_or_else(|| protocol("SASL out of order"))?
                        .finish(body.data())?;
                }
                Some(
                    Message::AuthenticationCleartextPassword
                    | Message::AuthenticationMd5Password(_)
                    | Message::AuthenticationGss
                    | Message::AuthenticationKerberosV5
                    | Message::AuthenticationScmCredential
                    | Message::AuthenticationSspi,
                ) => return Err(protocol("the server asks for something other than SCRAM")),
                Some(Message::BackendKeyData(body)) => {
                    conn.key = BackendKey {
                        pid: body.process_id(),
                        secret: body.secret_key(),
                    };
                }
                Some(
                    Message::AuthenticationOk
                    | Message::ParameterStatus(_)
                    | Message::NoticeResponse(_),
                ) => {}
                Some(Message::ErrorResponse(body)) => {
                    return Err(PgError::Db(Box::new(DbError::parse(body.fields())?)));
                }
                Some(Message::ReadyForQuery(_)) => return Ok(conn),
                Some(_) => return Err(protocol("unexpected message while connecting")),
                None => return Err(protocol("the server closed the connection")),
            }
        }
    }

    pub fn key(&self) -> BackendKey {
        self.key
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The transaction state after the last query.
    pub fn txn(&self) -> Txn {
        self.txn
    }

    async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.io.write_all(bytes).await?;
        self.io.flush().await
    }

    /// Reads until `n` bytes are buffered; false on a clean end of stream.
    async fn fill(&mut self, n: usize) -> io::Result<bool> {
        while self.buf.len() < n {
            self.buf.reserve(n - self.buf.len());
            if self.io.read_buf(&mut self.buf).await? == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The next backend message, or `None` at the end of the stream. A
    /// message over [`MAX_MESSAGE`] is consumed without being kept, and
    /// answered as an [`Oversized`].
    async fn raw(&mut self) -> Result<Option<Raw>, PgError> {
        if !self.fill(5).await? {
            return Ok(None);
        }
        let header = backend::Header::parse(&self.buf)?.ok_or_else(|| protocol("no header"))?;
        let total = usize::try_from(header.len()).map_err(|_| protocol("negative length"))? + 1;
        if total > MAX_MESSAGE {
            let tag = header.tag();
            let mut left = total;
            while left > 0 {
                if self.buf.is_empty() && !self.fill(1).await? {
                    return Ok(None);
                }
                let take = left.min(self.buf.len());
                self.buf.advance(take);
                left -= take;
            }
            return Ok(Some(Raw::Oversized(tag, total)));
        }
        if !self.fill(total).await? {
            return Ok(None);
        }
        let message = Message::parse(&mut self.buf)?.ok_or_else(|| protocol("short message"))?;
        Ok(Some(Raw::Message(message)))
    }

    async fn message(&mut self) -> Result<Option<Message>, PgError> {
        match self.raw().await? {
            Some(Raw::Message(message)) => Ok(Some(message)),
            Some(Raw::Oversized(..)) => Err(protocol("oversized message")),
            None => Ok(None),
        }
    }

    /// Sends a simple query; read its [`Event`]s with [`Conn::next`] until
    /// [`Event::Ready`].
    pub async fn send_query(&mut self, sql: &str) -> Result<(), PgError> {
        if sql.as_bytes().contains(&0) {
            return Err(protocol("the query contains a NUL byte"));
        }
        let mut out = BytesMut::new();
        frontend::query(sql, &mut out)?;
        self.write(&out).await?;
        Ok(())
    }

    /// Sends one statement in the extended protocol, executed for at most
    /// `max_rows` rows (0: all), then closed: a statement past the limit
    /// stops there ([`Event::Suspended`]) without a cancel request, which
    /// could reach whatever runs next. Values come as text, as from a
    /// simple query; read the [`Event`]s with [`Conn::next`] until
    /// [`Event::Ready`].
    pub async fn send_limited(&mut self, sql: &str, max_rows: i32) -> Result<(), PgError> {
        if sql.as_bytes().contains(&0) {
            return Err(protocol("the query contains a NUL byte"));
        }
        let mut out = BytesMut::new();
        frontend::parse("", sql, std::iter::empty(), &mut out)?;
        frontend::bind(
            "",
            "",
            std::iter::empty(),
            std::iter::empty::<()>(),
            |(), _| Ok(IsNull::No),
            std::iter::empty(),
            &mut out,
        )
        .map_err(|_| protocol("bind"))?;
        frontend::describe(b'P', "", &mut out)?;
        frontend::execute("", max_rows, &mut out)?;
        frontend::close(b'P', "", &mut out)?;
        frontend::sync(&mut out);
        self.write(&out).await?;
        Ok(())
    }

    /// The next event of the running query.
    pub async fn next(&mut self) -> Result<Event, PgError> {
        self.event(true).await
    }

    /// [`Conn::next`], rows answered as [`Event::SkippedRow`] without
    /// their values being decoded or kept: a statement whose rows nobody
    /// reads (a migration's `SELECT`) streams through in constant memory.
    pub async fn next_skipping_rows(&mut self) -> Result<Event, PgError> {
        self.event(false).await
    }

    async fn event(&mut self, rows: bool) -> Result<Event, PgError> {
        let raw = self
            .raw()
            .await?
            .ok_or_else(|| protocol("the server closed the connection"))?;
        let message = match raw {
            Raw::Oversized(b'D', len) => return Ok(Event::OversizedRow(len)),
            Raw::Oversized(b'd', len) => return Ok(Event::CopyData(len)),
            Raw::Oversized(..) => return Err(protocol("oversized message")),
            Raw::Message(message) => message,
        };
        Ok(match message {
            Message::RowDescription(body) => {
                let mut fields = body.fields();
                let mut columns = Vec::new();
                while let Some(field) = fields.next()? {
                    columns.push(Field {
                        name: field.name().to_owned(),
                        type_oid: field.type_oid(),
                        type_modifier: field.type_modifier(),
                    });
                }
                Event::Columns(columns)
            }
            Message::DataRow(_) if !rows => Event::SkippedRow,
            Message::DataRow(body) => {
                let buffer = body.buffer();
                let mut ranges = body.ranges();
                let mut row = Vec::new();
                while let Some(range) = ranges.next()? {
                    row.push(range.map(|r| String::from_utf8_lossy(&buffer[r]).into_owned()));
                }
                Event::Row(row)
            }
            Message::CommandComplete(body) => Event::Done(body.tag()?.to_owned()),
            Message::PortalSuspended => Event::Suspended,
            Message::EmptyQueryResponse => Event::Empty,
            Message::ErrorResponse(body) => Event::Error(DbError::parse(body.fields())?),
            Message::NoticeResponse(body) => Event::Notice(DbError::parse(body.fields())?),
            Message::CopyInResponse(_) => Event::CopyIn,
            Message::CopyOutResponse(_) => Event::CopyOut,
            Message::CopyData(body) => Event::CopyData(body.data().len()),
            Message::CopyDone => return Box::pin(self.event(rows)).await,
            Message::ReadyForQuery(body) => {
                self.txn = match body.status() {
                    b'T' => Txn::InTransaction,
                    b'E' => Txn::Failed,
                    _ => Txn::Idle,
                };
                Event::Ready(self.txn)
            }
            Message::ParameterStatus(_)
            | Message::NotificationResponse(_)
            | Message::ParseComplete
            | Message::BindComplete
            | Message::CloseComplete
            | Message::NoData
            | Message::ParameterDescription(_) => {
                return Box::pin(self.event(rows)).await;
            }
            _ => return Err(protocol("unexpected message")),
        })
    }

    /// Refuses a `COPY … FROM STDIN` the server is waiting on.
    pub async fn copy_fail(&mut self, message: &str) -> Result<(), PgError> {
        let mut out = BytesMut::new();
        frontend::copy_fail(message, &mut out)?;
        self.write(&out).await?;
        Ok(())
    }

    /// One chunk of `COPY … FROM STDIN` data (text format, whole lines).
    pub async fn copy_data(&mut self, data: &[u8]) -> Result<(), PgError> {
        let mut out = BytesMut::with_capacity(data.len() + 5);
        frontend::CopyData::new(data)?.write(&mut out);
        self.write(&out).await?;
        Ok(())
    }

    pub async fn copy_done(&mut self) -> Result<(), PgError> {
        let mut out = BytesMut::new();
        frontend::copy_done(&mut out);
        self.write(&out).await?;
        Ok(())
    }

    /// Runs `sql` to the end: the rows of its last result, or its first
    /// error. For the service's own queries, never the console's.
    pub async fn query(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, PgError> {
        self.send_query(sql).await?;
        let mut rows = Vec::new();
        let mut error = None;
        loop {
            match self.next().await? {
                Event::Columns(_) => rows.clear(),
                Event::Row(row) => rows.push(row),
                Event::Error(e) => error = error.or(Some(e)),
                Event::CopyIn => self.copy_fail("not expected").await?,
                Event::Ready(_) => break,
                _ => {}
            }
        }
        match error {
            Some(error) => Err(PgError::Db(Box::new(error))),
            None => Ok(rows),
        }
    }

    /// Ends the session politely.
    pub async fn close(mut self) {
        let mut out = BytesMut::new();
        frontend::terminate(&mut out);
        let _ = self.write(&out).await;
    }
}

enum Raw {
    Message(Message),
    Oversized(u8, usize),
}

/// Asks the backend `key` (at `target`) to cancel what it runs: Postgres's
/// cancel request, on a connection of its own.
pub async fn cancel(target: &Target, key: BackendKey) -> Result<(), PgError> {
    let mut io = tokio::time::timeout(Duration::from_secs(5), stream(target))
        .await
        .map_err(|_| PgError::Timeout)??;
    let mut out = BytesMut::new();
    frontend::cancel_request(key.pid, key.secret, &mut out);
    io.write_all(&out).await?;
    io.flush().await?;
    // The server closes it once it has read the request.
    let mut sink = [0u8; 16];
    let _ = tokio::time::timeout(Duration::from_secs(2), io.read(&mut sink)).await;
    Ok(())
}

/// `<sockets>/<id>`: a playground's socket directory, as the service mounts it.
pub fn socket_dir(sockets: &Path, id: uuid::Uuid) -> PathBuf {
    sockets.join(id.to_string())
}

/// Quotes an identifier for SQL, always.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quotes a string literal for SQL as an escape string (`E'…'`): right
/// whatever `standard_conforming_strings` a session set. A NUL, which no
/// Postgres text holds, is left out.
pub fn quote_literal(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 3);
    quoted.push_str("E'");
    for c in text.chars() {
        match c {
            '\'' => quoted.push_str("''"),
            '\\' => quoted.push_str("\\\\"),
            '\0' => {}
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_characters_turned_into_bytes() {
        let error = DbError {
            position: Some(10),
            ..DbError::default()
        };
        assert_eq!(error.byte_position("SELECT 'é' + x"), Some(10));
        assert_eq!(error.byte_position("SELECT x"), Some(8));
    }

    #[test]
    fn identifiers_and_literals_are_quoted() {
        assert_eq!(quote_ident(r#"we"ird"#), r#""we""ird""#);
        assert_eq!(quote_literal("it's"), "E'it''s'");
        assert_eq!(quote_literal(r"a\'; x"), r"E'a\\''; x'");
        assert_eq!(quote_literal("nul\0"), "E'nul'");
    }
}
