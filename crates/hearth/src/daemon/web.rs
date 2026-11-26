//! The web UI over HTTPS: TLS listener, the embedded UI files, app icons and the CA download.
//!
//! The same API router as the Unix socket is served here, but network clients never get the
//! socket's uid-based rights: they need a browser session (cookie + CSRF header) or a token.

use super::State;
use super::api::ApiError;
use super::auth::{Peer, Principal};
use axum::extract::{ConnectInfo, Path, Request, State as AxState};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hearth_core::Error;
use hearth_core::model::scopes;
use hearth_core::output::Envelope;
use hyper_util::rt::{TokioIo, TokioTimer};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[derive(RustEmbed)]
#[folder = "../../ui/build"]
#[allow_missing = true]
struct Ui;

const MAX_CONNECTIONS: usize = 256;
const ICON_MAX_BYTES: usize = 512 * 1024;

/// The TLS setup shared by the web UI and the app HTTPS ports: certificates picked by SNI.
pub fn tls_acceptor(state: &State) -> Option<tokio_rustls::TlsAcceptor> {
    let config = match rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    {
        Ok(b) => b.with_no_client_auth().with_cert_resolver(state.certs.clone()),
        Err(e) => {
            tracing::error!("TLS setup failed: {e}");
            return None;
        }
    };
    let mut config = config;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Some(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// What a TLS listener does with each request.
pub type Handler = Arc<
    dyn Fn(Peer, hyper::Request<hyper::body::Incoming>) -> futures_util::future::BoxFuture<'static, Response>
        + Send
        + Sync,
>;

/// Accepts TLS connections on `listener` and serves them with `handler` (HTTP/1.1, upgrades
/// allowed), at most `limit` connections at once. Runs until the task is aborted.
pub async fn accept_loop(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    limit: Arc<tokio::sync::Semaphore>,
    handler: Handler,
) {
    loop {
        let (tcp, remote) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue; // too many connections: drop this one
        };
        let (acceptor, handler) = (acceptor.clone(), handler.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tcp.set_nodelay(true);
            let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                Ok(Ok(t)) => t,
                _ => return,
            };
            let peer = Peer {
                uid: None,
                ip: Some(remote.ip().to_canonical()),
            };
            let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let fut = handler(peer, req);
                async move { Ok::<_, std::convert::Infallible>(fut.await) }
            });
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(30))
                .serve_connection(TokioIo::new(tls), svc)
                .with_upgrades();
            if let Err(e) = conn.await {
                tracing::debug!("connection: {e}");
            }
        });
    }
}

/// Starts the HTTPS listeners. Failing to bind (port taken, no permission) is logged, and the
/// daemon keeps serving the CLI on its socket.
pub async fn serve(state: State, listen: Vec<SocketAddr>) {
    let Some(acceptor) = tls_acceptor(&state) else {
        return;
    };
    let app = router(state.clone());
    let limit = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let handler: Handler = Arc::new(move |peer, mut req| {
        let app = app.clone();
        let state = state.clone();
        Box::pin(async move {
            // `<app>.<your domain>`: straight to that app.
            if let Some(base) = state.domain_base()
                && let Some(host) = req
                    .headers()
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| {
                        h.rsplit_once(':')
                            .map_or(h, |(n, p)| {
                                if p.chars().all(|c| c.is_ascii_digit()) {
                                    n
                                } else {
                                    h
                                }
                            })
                            .to_string()
                    })
                && host != base
                && host.ends_with(&format!(".{base}"))
            {
                return match state.proxies.target_for_host(&host, &base) {
                    Some(target) => super::proxy::forward(target, peer, req).await,
                    None => no_such_app(&host),
                };
            }
            req.extensions_mut().insert(ConnectInfo(peer));
            match app.oneshot(req.map(axum::body::Body::new)).await {
                Ok(r) => r,
                Err(e) => match e {},
            }
        })
    });

    let mut bound = 0;
    for addr in listen {
        let listener = match bind(addr) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(%addr, "web UI not listening: {e}");
                continue;
            }
        };
        bound += 1;
        tracing::info!(%addr, "web UI listening (HTTPS)");
        tokio::spawn(accept_loop(
            listener,
            acceptor.clone(),
            limit.clone(),
            handler.clone(),
        ));
    }
    if bound == 0 {
        tracing::warn!("the web UI isn't reachable; the CLI still works");
    }
}

/// Binds with SO_REUSEADDR (quick restarts) and, for `[::]`, dual-stack where the system allows.
pub fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = if addr.is_ipv6() {
        tokio::net::TcpSocket::new_v6()?
    } else {
        tokio::net::TcpSocket::new_v4()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

fn router(state: State) -> Router {
    let api = super::api::router(state.clone());
    let extra = Router::new()
        .route("/ca.crt", get(ca_cert))
        .fallback(static_file)
        .with_state(state);
    api.merge(extra).layer(middleware::from_fn(security_headers))
}

/// Headers for every response. Script rules come from the UI's own CSP (hashes of its inline
/// bootstrap script, in a meta tag); this adds what a meta tag can't express.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'",
            ),
        );
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    resp
}

async fn ca_cert(AxState(state): AxState<State>) -> Response {
    match state.certs.ca_pem() {
        Some(pem) => (
            [
                (header::CONTENT_TYPE, "application/x-x509-ca-cert"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"hearth-ca.crt\"",
                ),
            ],
            pem,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no CA yet").into_response(),
    }
}

async fn static_file(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("v1/") {
        return (
            StatusCode::NOT_FOUND,
            Json(Envelope::<()>::failure(&Error::NotFound {
                what: "API endpoint",
                name: uri.path().to_string(),
            })),
        )
            .into_response();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    let (file, name) = match Ui::get(path) {
        Some(f) => (f, path),
        // Pages of the single-page app (e.g. /apps/jellyfin, /store/org.jellyfin.jellyfin) all
        // load index.html; missing assets (scripts, styles, images) are a real 404.
        None if !looks_like_asset(path) => match Ui::get("index.html") {
            Some(f) => (f, "index.html"),
            None => return no_ui(),
        },
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let cache = if name.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

fn no_such_app(host: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        format!(
            "<!doctype html><meta charset=utf-8><title>No app here</title>\
             <body style=\"font-family:system-ui;max-width:36rem;margin:4rem auto;padding:0 1rem\">\
             <h1>No app uses this address</h1><p>Nothing in Hearth is set up for <b>{}</b>. \
             Give an app this subdomain in its HTTPS card.</p></body>",
            host.replace(['<', '>', '&', '"'], "")
        ),
    )
        .into_response()
}

fn no_ui() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Hearth is running, but this build has no web UI. The CLI and API work as usual.\n",
    )
        .into_response()
}

// ----- API additions served on both the socket and HTTPS -----

pub fn api_routes() -> Router<State> {
    Router::new()
        .route("/v1/tls", get(tls_info))
        .route("/v1/apps/{id}/icon", get(app_icon))
}

async fn tls_info(
    AxState(state): AxState<State>,
    p: Principal,
) -> Result<Json<Envelope<super::tls::TlsInfo>>, ApiError> {
    p.require(scopes::READ)?;
    Ok(Json(Envelope::success(state.certs.info())))
}

/// The app's icon, fetched once from its URL and cached, so the UI never loads anything from
/// other sites.
async fn app_icon(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    p.require(scopes::READ)?;
    let (apps, _) = super::apps::list(&state).await;
    let url = apps
        .into_iter()
        .find(|a| a.summary.id == id)
        .and_then(|a| a.summary.icon)
        .ok_or_else(|| Error::NotFound {
            what: "icon",
            name: id.clone(),
        })?;
    let (ctype, bytes) = icon_bytes(&state, &url).await.map_err(Error::Other)?;
    Ok((
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
            // An SVG opened on its own must not run scripts in Hearth's origin.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn icon_bytes(state: &State, url: &str) -> Result<(String, Vec<u8>), String> {
    use sha2::{Digest, Sha256};
    let dir = state.state_dir.join("icons");
    let key = hex::encode(Sha256::digest(url.as_bytes()));
    let data_path = dir.join(&key);
    let type_path = dir.join(format!("{key}.type"));
    if let (Ok(b), Ok(t)) = (std::fs::read(&data_path), std::fs::read_to_string(&type_path)) {
        return Ok((t, b));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("icon isn't a web address".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| format!("icon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("icon: HTTP {}", resp.status()));
    }
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    // SVG can carry scripts; it's only served with a CSP that forbids them (see app_icon), and only
    // these image types are accepted.
    if !matches!(
        ctype.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
            | "image/svg+xml"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
    ) {
        return Err(format!("icon: not an image ({ctype})"));
    }
    let mut bytes = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("icon: {e}"))? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > ICON_MAX_BYTES {
            return Err("icon too big".into());
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&data_path, &bytes);
    let _ = std::fs::write(&type_path, &ctype);
    Ok((ctype, bytes))
}

/// A path that names a static file (a missing one is a 404, not a page of the app).
fn looks_like_asset(path: &str) -> bool {
    const EXT: &[&str] = &[
        "js",
        "mjs",
        "css",
        "map",
        "png",
        "svg",
        "ico",
        "jpg",
        "jpeg",
        "webp",
        "gif",
        "woff",
        "woff2",
        "ttf",
        "json",
        "txt",
        "webmanifest",
        "html",
        "xml",
        "wasm",
    ];
    path.starts_with("_app/")
        || path
            .rsplit('/')
            .next()
            .and_then(|last| last.rsplit_once('.'))
            .is_some_and(|(_, ext)| EXT.contains(&ext.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod asset_tests {
    use super::looks_like_asset;

    #[test]
    fn store_ids_are_pages() {
        assert!(!looks_like_asset("store/org.jellyfin.jellyfin"));
        assert!(!looks_like_asset("apps/jellyfin"));
        assert!(looks_like_asset("_app/immutable/x"));
        assert!(looks_like_asset("favicon.svg"));
        assert!(looks_like_asset("robots.txt"));
    }
}
