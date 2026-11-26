//! Minimal HTTP/1.1 over a Unix socket, used to talk to Docker and to the Hearth daemon.

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

pub struct Response {
    pub status: u16,
    pub body: Bytes,
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

pub async fn request(
    socket: &Path,
    method: http::Method,
    path_and_query: &str,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
    max_body: usize,
    timeout: Duration,
) -> io::Result<Response> {
    let fut = async {
        let stream = UnixStream::connect(socket).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(io_err)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut req = http::Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(
                "host",
                headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("host"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("localhost"),
            );
        for (k, v) in headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("host")) {
            req = req.header(*k, v);
        }
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let req = req
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(io_err)?;
        let resp = sender.send_request(req).await.map_err(io_err)?;
        let status = resp.status().as_u16();
        let body = Limited::new(resp.into_body(), max_body)
            .collect()
            .await
            .map_err(|e| io_err(format!("response body: {e}")))?
            .to_bytes();
        Ok(Response { status, body })
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request timed out"))?
}

/// Like [`request`], but hands the response body to `on_chunk` as it arrives (for large streams).
/// Returns the status code.
pub async fn request_streaming(
    socket: &Path,
    method: http::Method,
    path_and_query: &str,
    body: Option<Vec<u8>>,
    mut on_chunk: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<u16> {
    let stream = UnixStream::connect(socket).await?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(io_err)?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut req = http::Request::builder()
        .method(method)
        .uri(path_and_query)
        .header("host", "localhost");
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(io_err)?;
    let resp = sender.send_request(req).await.map_err(io_err)?;
    let status = resp.status().as_u16();
    let mut body = resp.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(io_err)?;
        if let Some(data) = frame.data_ref() {
            on_chunk(data)?;
        }
    }
    Ok(status)
}
