//! Tests the registry client against a local mock registry that behaves like Docker Hub/GHCR:
//! bearer-token challenge, multi-arch index, platform manifest, config blob and paginated tags.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hearth_registry::{ImageRef, Platform, RegistryClient, track};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const TOKEN: &str = "mock-token";

#[derive(Clone)]
struct Mock {
    base: String,
    token_requests: Arc<AtomicUsize>,
}

fn authorized(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn challenge(m: &Mock) -> Response {
    let mut r = StatusCode::UNAUTHORIZED.into_response();
    let v = format!(
        r#"Bearer realm="{}/token",service="mock-registry",scope="repository:acme/app:pull""#,
        m.base
    );
    r.headers_mut().insert(
        "www-authenticate",
        HeaderValue::from_str(&v).unwrap_or(HeaderValue::from_static("")),
    );
    r
}

fn json_with_digest(body: &str, digest: &str, media: &str) -> Response {
    let mut r = body.to_string().into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(media).unwrap_or(HeaderValue::from_static("")),
    );
    r.headers_mut().insert(
        "docker-content-digest",
        HeaderValue::from_str(digest).unwrap_or(HeaderValue::from_static("")),
    );
    r
}

async fn token(State(m): State<Mock>, Query(q): Query<HashMap<String, String>>) -> Response {
    m.token_requests.fetch_add(1, Ordering::SeqCst);
    assert_eq!(q.get("service").map(String::as_str), Some("mock-registry"));
    assert_eq!(
        q.get("scope").map(String::as_str),
        Some("repository:acme/app:pull")
    );
    format!(r#"{{"token":"{TOKEN}"}}"#).into_response()
}

async fn manifest(State(m): State<Mock>, h: HeaderMap, Path(reference): Path<String>) -> Response {
    if !authorized(&h) {
        return challenge(&m);
    }
    match reference.as_str() {
        "1.2.3" => json_with_digest(
            r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[
                {"digest":"sha256:arm","platform":{"os":"linux","architecture":"arm64"}},
                {"digest":"sha256:amd","platform":{"os":"linux","architecture":"amd64"}},
                {"digest":"sha256:att","platform":{"os":"unknown","architecture":"unknown"}}]}"#,
            "sha256:index123",
            "application/vnd.oci.image.index.v1+json",
        ),
        "sha256:amd" => json_with_digest(
            r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"digest":"sha256:cfg"}}"#,
            "sha256:amd",
            "application/vnd.oci.image.manifest.v1+json",
        ),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn blob(State(m): State<Mock>, h: HeaderMap, Path(digest): Path<String>) -> Response {
    if !authorized(&h) {
        return challenge(&m);
    }
    if digest != "sha256:cfg" {
        return StatusCode::NOT_FOUND.into_response();
    }
    r#"{"created":"2026-11-20T00:00:00Z","config":{"Labels":{"org.opencontainers.image.version":"1.2.3"}}}"#
        .into_response()
}

async fn tags(State(m): State<Mock>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authorized(&h) {
        return challenge(&m);
    }
    if !q.contains_key("last") {
        let mut r = r#"{"name":"acme/app","tags":["1.2.3","1.2.4","latest"]}"#.into_response();
        r.headers_mut().insert(
            "link",
            HeaderValue::from_static(r#"</v2/acme/app/tags/list?n=1000&last=latest>; rel="next""#),
        );
        r
    } else {
        r#"{"name":"acme/app","tags":["1.3.0","2.0.0","1.3.1-rc1"]}"#.into_response()
    }
}

async fn start() -> (Mock, RegistryClient) {
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => panic!("bind: {e}"),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => panic!("addr: {e}"),
    };
    let base = format!("http://{addr}");
    let mock = Mock {
        base: base.clone(),
        token_requests: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/token", get(token))
        .route("/v2/acme/app/manifests/{reference}", get(manifest))
        .route("/v2/acme/app/blobs/{digest}", get(blob))
        .route("/v2/acme/app/tags/list", get(tags))
        .with_state(mock.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let client = match RegistryClient::new("hearth-test") {
        Ok(c) => c.with_override("registry.test", &base),
        Err(e) => panic!("client: {e}"),
    };
    (mock, client)
}

fn image() -> ImageRef {
    match ImageRef::parse("registry.test/acme/app:1.2.3") {
        Ok(r) => r,
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn tag_digest_uses_token_challenge_once() {
    let (mock, client) = start().await;
    let d = client.tag_digest(&image()).await;
    assert_eq!(d.ok().as_deref(), Some("sha256:index123"));
    // Second call reuses the cached token.
    let _ = client.tag_digest(&image()).await;
    assert_eq!(mock.token_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn describe_picks_host_platform_and_reads_version_label() {
    let (_mock, client) = start().await;
    let platform = Platform {
        os: "linux".into(),
        architecture: "amd64".into(),
    };
    let remote = match client.describe(&image(), &platform).await {
        Ok(r) => r,
        Err(e) => panic!("describe: {e}"),
    };
    assert_eq!(remote.digest, "sha256:index123");
    assert_eq!(remote.platform_digest.as_deref(), Some("sha256:amd"));
    assert_eq!(remote.version.as_deref(), Some("1.2.3"));
    assert_eq!(
        remote.platforms,
        vec!["linux/arm64".to_string(), "linux/amd64".to_string()]
    );
}

#[tokio::test]
async fn tags_are_paginated_and_tracks_work_on_them() {
    let (_mock, client) = start().await;
    let tags = match client.list_tags(&image()).await {
        Ok(t) => t,
        Err(e) => panic!("tags: {e}"),
    };
    assert_eq!(tags.len(), 6);
    let report = track::analyse("1.2.3", &tags);
    assert_eq!(report.newer_in_track.as_deref(), Some("1.3.0"));
    assert_eq!(report.newer_major.as_deref(), Some("2.0.0"));
}

#[tokio::test]
async fn missing_tag_is_a_status_error() {
    let (_mock, client) = start().await;
    let r = image().with_tag("9.9.9");
    let err = client
        .tag_digest(&r)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("404"), "{err}");
}
