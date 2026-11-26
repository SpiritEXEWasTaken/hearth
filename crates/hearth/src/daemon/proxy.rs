//! HTTPS for apps: Hearth listens on a port per app, with the same certificates as the web UI,
//! and forwards to the app's own plain-HTTP port on this machine (websockets included).
//!
//! Tailscale certificates only cover the machine's own name, so apps get a port each
//! (`https://jarvis.<tailnet>.ts.net:8443`) rather than a subdomain.

use super::State;
use super::auth::Peer;
use super::db::AppHttps;
use super::web::{Handler, accept_loop, bind, tls_acceptor};
use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Ports Hearth suggests for app HTTPS, first free one wins.
pub const SUGGEST_FROM: u16 = 8443;
pub const SUGGEST_TO: u16 = 8999;
const MAX_CONNECTIONS_PER_APP: usize = 256;

#[derive(Default)]
pub struct Proxies {
    running: Mutex<HashMap<u16, (AppHttps, tokio::task::AbortHandle)>>,
}

impl Proxies {
    /// Starts and stops listeners so they match the database. Returns problems per app.
    pub fn sync(&self, state: &State) -> HashMap<String, String> {
        let wanted = state.with_db(|db| db.list_app_https()).unwrap_or_default();
        let mut problems = HashMap::new();
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        running.retain(|port, (h, handle)| {
            let keep = wanted.iter().any(|w| w == h && w.port == *port);
            if !keep {
                handle.abort();
                tracing::info!(app = %h.app, port, "app HTTPS stopped");
            }
            keep
        });
        let Some(acceptor) = tls_acceptor(state) else {
            return problems;
        };
        for w in wanted {
            if running.contains_key(&w.port) {
                continue;
            }
            let addr: SocketAddr = format!("[::]:{}", w.port).parse().expect("address");
            let listener =
                match bind(addr).or_else(|_| bind(format!("0.0.0.0:{}", w.port).parse().expect("address"))) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(app = %w.app, port = w.port, "app HTTPS not listening: {e}");
                        problems.insert(w.app.clone(), format!("port {} can't be used: {e}", w.port));
                        continue;
                    }
                };
            let target = w.target_port;
            let handler: Handler = Arc::new(move |peer, req| Box::pin(forward(target, peer, req)));
            let task = tokio::spawn(accept_loop(
                listener,
                acceptor.clone(),
                Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS_PER_APP)),
                handler,
            ));
            tracing::info!(app = %w.app, port = w.port, target, "app HTTPS listening");
            running.insert(w.port, (w, task.abort_handle()));
        }
        problems
    }

    pub fn is_running(&self, port: u16) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&port)
    }
}

/// Headers that belong to one connection and must not be passed on.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn bad_gateway(target: u16, why: impl std::fmt::Display) -> Response<Body> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>App not answering</title>\
         <body style=\"font-family:system-ui;max-width:36rem;margin:4rem auto;padding:0 1rem\">\
         <h1>The app isn't answering</h1><p>Hearth forwards this address to port {target} on the server, \
         but nothing answered there ({why}). The app may be stopped or still starting.</p></body>"
    );
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = StatusCode::BAD_GATEWAY;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r
}

/// Forwards one request to `127.0.0.1:<target>` and streams the answer back.
pub async fn forward(target: u16, peer: Peer, mut req: Request<hyper::body::Incoming>) -> Response<Body> {
    let wants_upgrade = req.headers().contains_key(header::UPGRADE);
    let client_upgrade = wants_upgrade.then(|| hyper::upgrade::on(&mut req));

    let stream = match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", target)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return bad_gateway(target, e),
        Err(_) => return bad_gateway(target, "timed out"),
    };
    let _ = stream.set_nodelay(true);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(x) => x,
        Err(e) => return bad_gateway(target, e),
    };
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });

    let (mut parts, body) = req.into_parts();
    let host = parts.headers.get(header::HOST).cloned();
    let upgrade_value = parts.headers.get(header::UPGRADE).cloned();
    for h in HOP_BY_HOP {
        parts.headers.remove(*h);
    }
    if let Some(v) = upgrade_value {
        parts
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        parts.headers.insert(header::UPGRADE, v);
    }
    // Only the address Hearth saw counts: whatever the client claims in these headers is replaced.
    let client_ip = peer.ip.map(|i| i.to_string()).unwrap_or_default();
    if let Ok(v) = HeaderValue::from_str(&client_ip) {
        parts.headers.insert("x-forwarded-for", v.clone());
        parts.headers.insert("x-real-ip", v);
    }
    parts
        .headers
        .insert("x-forwarded-proto", HeaderValue::from_static("https"));
    if let Some(h) = host {
        parts.headers.insert("x-forwarded-host", h);
    }
    // Only the path goes upstream; the Host header stays what the browser sent.
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    parts.uri = match path.parse() {
        Ok(u) => u,
        Err(e) => return bad_gateway(target, e),
    };
    let upstream_req = Request::from_parts(parts, body);

    let mut resp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => return bad_gateway(target, e),
    };

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(c), Ok(u)) = (client_upgrade.await, upstream_upgrade.await) {
                let mut c = TokioIo::new(c);
                let mut u = TokioIo::new(u);
                let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
            }
        });
        let (parts, _) = resp.into_parts();
        return Response::from_parts(parts, Body::empty());
    }
    let (mut parts, body) = resp.into_parts();
    for h in HOP_BY_HOP {
        parts.headers.remove(*h);
    }
    Response::from_parts(parts, Body::new(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_gateway_explains() {
        let r = bad_gateway(8123, "connection refused");
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    }
}
