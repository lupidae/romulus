//! HTTP/1.1 over a Unix socket, one connection per request: Docker's API
//! (the runner) and the runner's (the service). Nothing here is long-lived
//! or busy enough to want a pool.

use std::{path::Path, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, StatusCode,
    header::{CONTENT_TYPE, HOST},
};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum UnixHttpError {
    #[error("cannot connect to {path}: {source}")]
    Connect {
        path: String,
        source: std::io::Error,
    },
    #[error("HTTP: {0}")]
    Http(#[from] hyper::Error),
    #[error("request: {0}")]
    Request(#[from] hyper::http::Error),
    #[error("no answer within {0:?}")]
    Timeout(Duration),
}

/// One request: its status and whole body.
pub async fn request(
    socket: &Path,
    method: Method,
    path_and_query: &str,
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<(StatusCode, Bytes), UnixHttpError> {
    let exchange = async {
        let stream =
            UnixStream::connect(socket)
                .await
                .map_err(|source| UnixHttpError::Connect {
                    path: socket.display().to_string(),
                    source,
                })?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        let driver = tokio::spawn(connection);
        let request = Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(HOST, "localhost")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))?;
        let response = sender.send_request(request).await?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        driver.abort();
        Ok((status, bytes))
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| UnixHttpError::Timeout(timeout))?
}
