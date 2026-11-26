//! A small OCI distribution client: digests, manifests, config labels and tag lists.
//!
//! Anonymous pulls only (enough for public images). Authentication follows the standard
//! `WWW-Authenticate: Bearer realm=…,service=…,scope=…` challenge, which covers Docker Hub,
//! GHCR, lscr.io, Quay and most self-hosted registries.
//!
//! Requests are kept to a minimum because of Docker Hub's pull limits: digest checks use `HEAD`
//! (which Docker Hub doesn't count), and full manifests/configs are only fetched when needed.

use crate::{ImageRef, RegistryError};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";

const MAX_TAG_PAGES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
}

impl Platform {
    /// The platform of the machine Hearth runs on.
    pub fn host() -> Platform {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            "arm" => "arm",
            "riscv64" => "riscv64",
            other => other,
        };
        Platform {
            os: "linux".into(),
            architecture: architecture.into(),
        }
    }
}

/// What a tag points to in the registry right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    /// The digest of what the tag points to (an index for multi-arch images).
    pub digest: String,
    /// The digest of the manifest for this host's platform.
    pub platform_digest: Option<String>,
    /// Human-readable version from the image config labels.
    pub version: Option<String>,
    /// Image creation time from the config, if present.
    pub created: Option<String>,
    /// Platforms the image is published for (`linux/amd64`, …).
    pub platforms: Vec<String>,
    /// Compressed download size for this platform (sum of the layers), in bytes.
    pub size: Option<u64>,
}

pub struct RegistryClient {
    http: reqwest::Client,
    /// registry host → base URL (tests point registries at a local mock server).
    overrides: HashMap<String, String>,
    tokens: Mutex<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(rename = "mediaType")]
    media_type: Option<String>,
    manifests: Option<Vec<IndexEntry>>,
    config: Option<Descriptor>,
    layers: Option<Vec<Layer>>,
}

#[derive(Deserialize)]
struct Layer {
    size: Option<u64>,
}

#[derive(Deserialize)]
struct IndexEntry {
    digest: String,
    platform: Option<PlatformJson>,
}

#[derive(Deserialize)]
struct PlatformJson {
    os: String,
    architecture: String,
}

#[derive(Deserialize)]
struct Descriptor {
    digest: String,
}

#[derive(Deserialize)]
struct ImageConfig {
    created: Option<String>,
    architecture: Option<String>,
    os: Option<String>,
    config: Option<ImageConfigInner>,
}

#[derive(Deserialize)]
struct ImageConfigInner {
    #[serde(rename = "Labels")]
    labels: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct TagList {
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
}

impl RegistryClient {
    pub fn new(user_agent: &str) -> Result<Self, RegistryError> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(RegistryClient {
            http,
            overrides: HashMap::new(),
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Sends requests for `registry` to `base_url` instead (e.g. `http://127.0.0.1:5000`). For tests.
    pub fn with_override(mut self, registry: &str, base_url: &str) -> Self {
        self.overrides
            .insert(registry.to_string(), base_url.trim_end_matches('/').to_string());
        self
    }

    fn base(&self, r: &ImageRef) -> String {
        self.overrides.get(&r.registry).cloned().unwrap_or_else(|| {
            // Like Docker, talk plain HTTP to registries on this machine.
            let host = r.registry.split(':').next().unwrap_or_default();
            let scheme = if matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
                "http"
            } else {
                "https"
            };
            format!("{scheme}://{}", r.api_host())
        })
    }

    /// The digest the image's tag points to now. Uses `HEAD` (not counted by Docker Hub's limits).
    pub async fn tag_digest(&self, r: &ImageRef) -> Result<String, RegistryError> {
        let url = format!("{}/v2/{}/manifests/{}", self.base(r), r.repository, r.tag);
        let resp = self.send(r, reqwest::Method::HEAD, &url, MANIFEST_ACCEPT).await?;
        if let Some(d) = header_digest(&resp) {
            return Ok(d);
        }
        // Some registries omit the header on HEAD: fall back to GET.
        let resp = self.send(r, reqwest::Method::GET, &url, MANIFEST_ACCEPT).await?;
        header_digest(&resp)
            .ok_or_else(|| RegistryError::Protocol(format!("no Docker-Content-Digest for {url}")))
    }

    /// Full description of what the tag points to, including the version label for `platform`.
    pub async fn describe(&self, r: &ImageRef, platform: &Platform) -> Result<RemoteImage, RegistryError> {
        let url = format!("{}/v2/{}/manifests/{}", self.base(r), r.repository, r.tag);
        let resp = self.send(r, reqwest::Method::GET, &url, MANIFEST_ACCEPT).await?;
        let digest = header_digest(&resp).unwrap_or_default();
        let top: Manifest = resp.json().await?;

        let mut platforms = Vec::new();
        let (platform_digest, manifest) = if let Some(entries) = &top.manifests {
            for e in entries {
                if let Some(p) = &e.platform
                    && p.os != "unknown"
                {
                    platforms.push(format!("{}/{}", p.os, p.architecture));
                }
            }
            let chosen = entries.iter().find(|e| {
                e.platform
                    .as_ref()
                    .is_some_and(|p| p.os == platform.os && p.architecture == platform.architecture)
            });
            match chosen {
                Some(e) => {
                    let url = format!("{}/v2/{}/manifests/{}", self.base(r), r.repository, e.digest);
                    let m: Manifest = self
                        .send(r, reqwest::Method::GET, &url, MANIFEST_ACCEPT)
                        .await?
                        .json()
                        .await?;
                    (Some(e.digest.clone()), Some(m))
                }
                None => (None, None),
            }
        } else {
            let is_manifest =
                top.config.is_some() || top.media_type.as_deref().is_some_and(|m| m.contains("manifest"));
            if is_manifest {
                (Some(digest.clone()), Some(top))
            } else {
                return Err(RegistryError::Protocol(format!("unrecognised manifest for {r}")));
            }
        };

        let size = manifest
            .as_ref()
            .and_then(|m| m.layers.as_ref())
            .map(|l| l.iter().filter_map(|x| x.size).sum());
        let (version, created) = match manifest.and_then(|m| m.config) {
            Some(cfg) => {
                let url = format!("{}/v2/{}/blobs/{}", self.base(r), r.repository, cfg.digest);
                let c: ImageConfig = self
                    .send(r, reqwest::Method::GET, &url, "*/*")
                    .await?
                    .json()
                    .await?;
                if platforms.is_empty()
                    && let (Some(os), Some(arch)) = (&c.os, &c.architecture)
                {
                    // A single-platform image: its config says which platform it is.
                    platforms.push(format!("{os}/{arch}"));
                }
                let labels = c.config.and_then(|c| c.labels).unwrap_or_default();
                (version_from_labels(&labels), c.created)
            }
            None => (None, None),
        };
        Ok(RemoteImage {
            digest,
            platform_digest,
            version,
            created,
            platforms,
            size,
        })
    }

    /// All tags of the repository (paginated, capped at a sane number of pages).
    pub async fn list_tags(&self, r: &ImageRef) -> Result<Vec<String>, RegistryError> {
        let base = self.base(r);
        let mut url = format!("{base}/v2/{}/tags/list?n=1000", r.repository);
        let mut all = Vec::new();
        for _ in 0..MAX_TAG_PAGES {
            let resp = self
                .send(r, reqwest::Method::GET, &url, "application/json")
                .await?;
            let next = next_link(&resp, &base);
            let page: TagList = resp.json().await?;
            all.extend(page.tags.unwrap_or_default());
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        Ok(all)
    }

    async fn send(
        &self,
        r: &ImageRef,
        method: reqwest::Method,
        url: &str,
        accept: &str,
    ) -> Result<reqwest::Response, RegistryError> {
        let key = format!("{}/{}", r.registry, r.repository);
        for attempt in 0..2 {
            let mut req = self.http.request(method.clone(), url).header("Accept", accept);
            let cached = self.tokens.lock().ok().and_then(|t| t.get(&key).cloned());
            if let Some(tok) = cached {
                req = req.bearer_auth(tok);
            }
            let resp = req.send().await?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                let challenge = resp
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| RegistryError::Auth(format!("401 without challenge from {url}")))?;
                let token = self.fetch_token(&challenge, r).await?;
                if let Ok(mut t) = self.tokens.lock() {
                    t.insert(key.clone(), token);
                }
                continue;
            }
            if !resp.status().is_success() {
                return Err(RegistryError::Status {
                    status: resp.status().as_u16(),
                    url: url.to_string(),
                });
            }
            return Ok(resp);
        }
        Err(RegistryError::Auth(format!(
            "still unauthorized after token for {url}"
        )))
    }

    async fn fetch_token(&self, challenge: &str, r: &ImageRef) -> Result<String, RegistryError> {
        let params = parse_bearer_challenge(challenge)
            .ok_or_else(|| RegistryError::Auth(format!("unsupported challenge: {challenge}")))?;
        let realm = params
            .get("realm")
            .ok_or_else(|| RegistryError::Auth("challenge without realm".into()))?;
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| format!("repository:{}:pull", r.repository));
        let mut query = vec![("scope", scope)];
        if let Some(s) = params.get("service") {
            query.push(("service", s.clone()));
        }
        let resp = self.http.get(realm).query(&query).send().await?;
        if !resp.status().is_success() {
            return Err(RegistryError::Auth(format!(
                "token endpoint returned {}",
                resp.status()
            )));
        }
        let t: TokenResponse = resp.json().await?;
        t.token
            .or(t.access_token)
            .ok_or_else(|| RegistryError::Auth("token response without token".into()))
    }
}

fn header_digest(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// `Link: </v2/x/tags/list?n=1000&last=abc>; rel="next"` → absolute URL.
fn next_link(resp: &reqwest::Response, base: &str) -> Option<String> {
    let link = resp.headers().get("link")?.to_str().ok()?;
    let start = link.find('<')? + 1;
    let end = link[start..].find('>')? + start;
    if !link[end..].contains("rel=\"next\"") {
        return None;
    }
    let target = &link[start..end];
    Some(if target.starts_with("http") {
        target.to_string()
    } else {
        format!("{base}{target}")
    })
}

/// Parses `Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="…"`.
pub fn parse_bearer_challenge(header: &str) -> Option<HashMap<String, String>> {
    let rest = header
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| header.trim().strip_prefix("bearer "))?;
    let mut out = HashMap::new();
    let mut s = rest.trim();
    while !s.is_empty() {
        let eq = s.find('=')?;
        let key = s[..eq].trim().trim_start_matches(',').trim().to_ascii_lowercase();
        s = &s[eq + 1..];
        let value;
        if let Some(stripped) = s.strip_prefix('"') {
            let close = stripped.find('"')?;
            value = stripped[..close].to_string();
            s = &stripped[close + 1..];
        } else {
            let end = s.find(',').unwrap_or(s.len());
            value = s[..end].trim().to_string();
            s = &s[end..];
        }
        out.insert(key, value);
        s = s.trim_start().trim_start_matches(',').trim_start();
    }
    Some(out)
}

/// Picks a human-readable version from image labels.
pub fn version_from_labels(labels: &BTreeMap<String, String>) -> Option<String> {
    // linuxserver.io: "Linuxserver.io version:- 10.11.2-ubu2404-ls45 Build-date:- 2026-11-20T…"
    if let Some(bv) = labels.get("build_version")
        && let Some(rest) = bv.split("version:-").nth(1)
        && let Some(v) = rest.split_whitespace().next()
    {
        return Some(v.to_string());
    }
    for key in [
        "org.opencontainers.image.version",
        "io.hass.version",
        "org.label-schema.version",
        "version",
    ] {
        if let Some(v) = labels.get(key) {
            let v = v.trim();
            if !v.is_empty() && v != "latest" {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_challenge_docker_hub() {
        let p = parse_bearer_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#,
        )
        .unwrap_or_default();
        assert_eq!(
            p.get("realm").map(String::as_str),
            Some("https://auth.docker.io/token")
        );
        assert_eq!(p.get("service").map(String::as_str), Some("registry.docker.io"));
        assert_eq!(
            p.get("scope").map(String::as_str),
            Some("repository:library/nginx:pull")
        );
    }

    #[test]
    fn bearer_challenge_rejects_basic() {
        assert!(parse_bearer_challenge("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn versions_from_labels() {
        let mut l = BTreeMap::new();
        l.insert(
            "build_version".to_string(),
            "Linuxserver.io version:- 10.11.2-ubu2404-ls45 Build-date:- 2026-11-20T03:00:00+00:00"
                .to_string(),
        );
        l.insert(
            "org.opencontainers.image.version".to_string(),
            "10.11.2-ubu2404-ls45".to_string(),
        );
        assert_eq!(version_from_labels(&l).as_deref(), Some("10.11.2-ubu2404-ls45"));

        let mut l = BTreeMap::new();
        l.insert(
            "org.opencontainers.image.version".to_string(),
            "v3.4.2".to_string(),
        );
        assert_eq!(version_from_labels(&l).as_deref(), Some("v3.4.2"));

        let mut l = BTreeMap::new();
        l.insert(
            "org.opencontainers.image.version".to_string(),
            "latest".to_string(),
        );
        assert_eq!(version_from_labels(&l), None);
    }
}
