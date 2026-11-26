//! Turns Docker containers (from every Docker endpoint) into apps.
//!
//! An app is a compose project (grouped by the `com.docker.compose.project` label) or a lone
//! container. CasaOS apps get their title, icon and web port from the `x-casaos` block of
//! their compose file.

use super::State;
use super::db::ImageCheck;
use super::stats;
use hearth_casaos::ComposeFile;
use hearth_core::model::{
    AppDetail, AppState, AppSummary, ContainerSummary, DockerEndpoint, ImageVersion, LatestInfo, ManagedBy,
};
use hearth_registry::ImageRef;
use serde_json::Value;
use std::collections::BTreeMap;

const LABEL_PROJECT: &str = "com.docker.compose.project";
const LABEL_SERVICE: &str = "com.docker.compose.service";
const LABEL_WORKDIR: &str = "com.docker.compose.project.working_dir";
const LABEL_CONFIG_FILES: &str = "com.docker.compose.project.config_files";

pub const CASAOS_APPS_DIR: &str = "/var/lib/casaos/apps";
pub const HEARTH_APPS_DIR: &str = "/var/lib/hearth/apps";

/// One container as listed by Docker, plus what we learn from inspecting it.
#[derive(Debug, Clone, Default)]
pub struct RawContainer {
    pub endpoint: Option<DockerEndpoint>,
    pub id: String,
    pub name: String,
    pub image_ref: String,
    pub image_id: String,
    pub state: String,
    pub status: String,
    pub labels: BTreeMap<String, String>,
}

impl RawContainer {
    pub fn from_list_json(endpoint: &DockerEndpoint, v: &Value) -> Option<RawContainer> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let name = v
            .get("Names")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        let labels = v
            .get("Labels")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some(RawContainer {
            endpoint: Some(endpoint.clone()),
            id: Some(s("Id")).filter(|i| !i.is_empty())?,
            name,
            image_ref: s("Image"),
            image_id: s("ImageID"),
            state: s("State"),
            status: s("Status"),
            labels,
        })
    }

    fn project(&self) -> Option<&str> {
        self.labels
            .get(LABEL_PROJECT)
            .map(String::as_str)
            .filter(|p| !p.is_empty())
    }
}

/// Containers grouped into one app, before enrichment.
#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub endpoint: DockerEndpoint,
    pub managed_by: ManagedBy,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
    pub containers: Vec<RawContainer>,
}

pub fn group(containers: Vec<RawContainer>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for c in containers {
        let endpoint = c.endpoint.clone().unwrap_or(DockerEndpoint::Root);
        let key = c.project().unwrap_or(&c.name).to_string();
        if let Some(g) = groups.iter_mut().find(|g| g.endpoint == endpoint && g.id == key) {
            g.containers.push(c);
            continue;
        }
        let working_dir = c.labels.get(LABEL_WORKDIR).cloned().filter(|w| !w.is_empty());
        let managed_by = match (c.project(), working_dir.as_deref()) {
            (None, _) => ManagedBy::Container,
            (Some(_), Some(w)) if w.starts_with(CASAOS_APPS_DIR) => ManagedBy::Casaos,
            (Some(_), Some(w)) if w.starts_with(HEARTH_APPS_DIR) => ManagedBy::Hearth,
            (Some(_), _) => ManagedBy::Compose,
        };
        let config_files = c
            .labels
            .get(LABEL_CONFIG_FILES)
            .map(|f| {
                f.split(',')
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        groups.push(Group {
            id: key,
            endpoint,
            managed_by,
            working_dir,
            config_files,
            containers: vec![c],
        });
    }
    // Same name in two Docker daemons: make ids unique by suffixing the non-root one.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for g in &groups {
        *counts.entry(g.id.clone()).or_default() += 1;
    }
    for g in &mut groups {
        if counts.get(&g.id).copied().unwrap_or(0) > 1 && g.endpoint != DockerEndpoint::Root {
            g.id = format!("{}@{}", g.id, g.endpoint.label().replace(':', ""));
        }
    }
    groups.sort_by(|a, b| a.id.cmp(&b.id));
    groups
}

pub fn app_state(states: &[&str]) -> AppState {
    let running = states.iter().filter(|s| **s == "running").count();
    if states.iter().any(|s| matches!(*s, "restarting" | "dead")) {
        AppState::Unhealthy
    } else if running == states.len() && running > 0 {
        AppState::Running
    } else if running == 0 {
        AppState::Stopped
    } else {
        AppState::Partial
    }
}

pub fn app_health(healths: &[Option<String>]) -> Option<String> {
    let has = |h: &str| healths.iter().any(|x| x.as_deref() == Some(h));
    if has("unhealthy") {
        Some("unhealthy".into())
    } else if has("starting") {
        Some("starting".into())
    } else if has("healthy") {
        Some("healthy".into())
    } else {
        None
    }
}

/// Finds the RepoDigest that belongs to `reference` (same registry/repository).
pub fn matching_repo_digest(reference: &str, repo_digests: &[String]) -> Option<String> {
    let want = ImageRef::parse(reference).ok()?.name();
    repo_digests.iter().find_map(|rd| {
        let (name, digest) = rd.split_once('@')?;
        (ImageRef::parse(name).ok()?.name() == want).then(|| digest.to_string())
    })
}

/// The digest the running image was published as: its RepoDigest for this reference, else
/// (containerd image store, e.g. images pushed or tagged locally) the image's own content
/// descriptor, which is the same index/manifest digest a registry reports.
pub fn running_digest(reference: &str, repo_digests: &[String], image: &Value) -> Option<String> {
    matching_repo_digest(reference, repo_digests).or_else(|| {
        image
            .pointer("/Descriptor/digest")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

/// The cache key for a reference: normalised, tag kept, pinned digest dropped.
pub fn check_key(reference: &str) -> Option<String> {
    let r = ImageRef::parse(reference).ok()?;
    Some(r.with_tag(&r.tag).to_string())
}

pub fn latest_info(check: &ImageCheck, running_digest: Option<&str>, pinned: bool) -> LatestInfo {
    LatestInfo {
        pinned,
        checked_at: check.checked_at,
        digest: check.digest.clone(),
        version: check.version.clone(),
        update_available: match (check.digest.as_deref(), running_digest) {
            (Some(remote), Some(running)) => !pinned && remote != running,
            _ => false,
        },
        newer_in_track: check.newer_in_track.clone(),
        newer_major: check.newer_major.clone(),
        error: check.error.clone(),
    }
}

struct Enriched {
    summary: AppSummary,
    detail_extra: (Vec<String>, Vec<String>, Option<String>),
}

/// Lists every app on every Docker endpoint. Returns the apps and any warnings.
pub async fn list(state: &State) -> (Vec<AppDetail>, Vec<String>) {
    let mut warnings = Vec::new();
    let endpoints = match state.helper.endpoints().await {
        Ok(e) => e,
        Err(e) => return (Vec::new(), vec![format!("cannot list Docker endpoints: {e}")]),
    };
    let mut raw = Vec::new();
    for ep in &endpoints {
        match state.helper.containers(ep).await {
            Ok(list) => raw.extend(list.iter().filter_map(|v| RawContainer::from_list_json(ep, v))),
            Err(e) => warnings.push(format!("Docker {}: {e}", ep.label())),
        }
    }
    let hearth_apps: Vec<String> = state
        .helper
        .call_as(&hearth_core::proto::Request::ListHearthApps)
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    for mut g in group(raw) {
        // An adopted app keeps its old containers (and their labels) until its first change.
        if g.endpoint == DockerEndpoint::Root && hearth_apps.contains(&g.id) {
            g.managed_by = ManagedBy::Hearth;
        }
        let e = enrich(state, &g).await;
        let (external_variables, missing_devices, store_app_id) = e.detail_extra;
        out.push(AppDetail {
            summary: e.summary,
            working_dir: g.working_dir.clone(),
            config_files: g.config_files.clone(),
            external_variables,
            missing_devices,
            store_app_id,
        });
    }
    out.extend(super::kinds::list(state).await);
    let https = state.with_db(|db| db.list_app_https()).unwrap_or_default();
    for a in &mut out {
        let h = https.iter().find(|h| h.app == a.summary.id);
        a.summary.https_port = h.map(|h| h.port);
        a.summary.https_subdomain = h.and_then(|h| h.subdomain.clone());
    }
    (out, warnings)
}

async fn enrich(state: &State, g: &Group) -> Enriched {
    let mut containers = Vec::new();
    let mut images: Vec<ImageVersion> = Vec::new();
    let mut total_mem: Option<u64> = None;
    let mut healths = Vec::new();

    for c in &g.containers {
        let inspect = state
            .helper
            .inspect_container(&g.endpoint, &c.id)
            .await
            .unwrap_or(Value::Null);
        let pid = inspect.pointer("/State/Pid").and_then(Value::as_u64).unwrap_or(0);
        let health = inspect
            .pointer("/State/Health/Status")
            .and_then(Value::as_str)
            .map(str::to_string);
        let memory = if c.state == "running" {
            stats::container_memory(pid)
        } else {
            None
        };
        if let Some(m) = memory {
            *total_mem.get_or_insert(0) += m;
        }
        healths.push(health.clone());
        let service = c.labels.get(LABEL_SERVICE).cloned();
        containers.push(ContainerSummary {
            id: c.id.chars().take(12).collect(),
            name: c.name.clone(),
            service: service.clone(),
            state: c.state.clone(),
            status: c.status.clone(),
            health,
            image: c.image_ref.clone(),
            memory_bytes: memory,
            restart_count: inspect.get("RestartCount").and_then(Value::as_u64),
            started_at: inspect
                .pointer("/State/StartedAt")
                .and_then(Value::as_str)
                .filter(|s| !s.starts_with("0001"))
                .map(str::to_string),
        });

        // Version info, once per service.
        let service_name = service.unwrap_or_else(|| c.name.clone());
        if images.iter().any(|i| i.service == service_name) {
            continue;
        }
        // The reference the container was created from (Config.Image), else what `ps` shows.
        let reference = inspect
            .pointer("/Config/Image")
            .and_then(Value::as_str)
            .unwrap_or(&c.image_ref)
            .to_string();
        let image = if c.image_id.is_empty() {
            Value::Null
        } else {
            state
                .helper
                .inspect_image(&g.endpoint, &c.image_id)
                .await
                .unwrap_or(Value::Null)
        };
        let repo_digests: Vec<String> = image
            .get("RepoDigests")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let running_digest = running_digest(&reference, &repo_digests, &image);
        let labels: BTreeMap<String, String> = image
            .pointer("/Config/Labels")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let running_version = hearth_registry::client::version_from_labels(&labels);
        let latest = check_key(&reference)
            .and_then(|k| state.with_db(|db| db.image_check(&k).ok().flatten()))
            .map(|chk| latest_info(&chk, running_digest.as_deref(), reference.contains('@')));
        images.push(ImageVersion {
            service: service_name,
            reference,
            running_digest,
            running_version,
            latest,
        });
    }

    // CasaOS metadata (title, icon, web port) from the app's compose file.
    let mut title = g.id.clone();
    let mut icon = None;
    let mut web_url = None;
    let mut extra = (Vec::new(), Vec::new(), None);
    let mut hide_from_dashboard = false;
    if g.managed_by == ManagedBy::Casaos && g.endpoint == DockerEndpoint::Root {
        match state.helper.casaos_app(&g.id).await {
            Ok(text) => match ComposeFile::parse(&text, &g.id) {
                Ok(compose) => {
                    if let Some(t) = compose.title() {
                        title = t;
                    }
                    if let Some(x) = compose.x_casaos() {
                        icon = x.icon.clone();
                        if let Some(port) = x.port_map.as_deref().filter(|p| !p.is_empty()) {
                            let scheme = x.scheme.as_deref().unwrap_or("http");
                            let index = x.index.as_deref().unwrap_or("/");
                            let host = x.hostname.clone().unwrap_or_else(|| state.hostname.clone());
                            web_url = Some(format!("{scheme}://{host}:{port}{index}"));
                        }
                        extra.2 = x.store_app_id.or(x.id);
                    }
                    extra.0 = compose.external_variables().into_iter().collect();
                    extra.1 = compose.missing_devices(|p| std::path::Path::new(p).exists());
                }
                Err(e) => tracing::warn!(app = %g.id, "unreadable CasaOS compose file: {e}"),
            },
            Err(e) => tracing::warn!(app = %g.id, "cannot read CasaOS compose file: {e}"),
        }
    }
    // Hearth apps: title, icon and web link from x-hearth.
    if g.managed_by == ManagedBy::Hearth && g.endpoint == DockerEndpoint::Root {
        let req = hearth_core::proto::Request::ReadAppFiles {
            app: g.id.clone(),
            include_env: false,
        };
        if let Ok(v) = state.helper.call(&req).await
            && let Some(text) = v.get("compose").and_then(Value::as_str)
            && let Ok(doc) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
            && let Some(xh) = doc.get("x-hearth")
        {
            if let Some(t) = xh.get("title").and_then(|t| t.as_str()) {
                title = t.to_string();
            }
            icon = xh.get("icon").and_then(|t| t.as_str()).map(str::to_string);
            extra.2 = xh
                .get("store")
                .and_then(|s| s.get("id"))
                .and_then(|t| t.as_str())
                .map(str::to_string);
            hide_from_dashboard = xh.get("dashboard").and_then(|d| d.as_bool()) == Some(false);
            if let Some(w) = super::engine::web_of(text) {
                let scheme = xh
                    .get("web")
                    .and_then(|w| w.get("scheme"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("http");
                web_url = Some(format!("{scheme}://{}:{}{}", state.hostname, w.port, w.path));
            }
        }
    }

    let states: Vec<&str> = g.containers.iter().map(|c| c.state.as_str()).collect();
    Enriched {
        summary: AppSummary {
            id: g.id.clone(),
            title,
            kind: hearth_core::model::AppKind::Compose,
            dashboard: web_url.is_some() && !hide_from_dashboard,
            managed_by: g.managed_by,
            endpoint: Some(g.endpoint.clone()),
            unit: None,
            // Only apps whose files Hearth manages can be changed.
            read_only: !(g.managed_by == ManagedBy::Hearth && g.endpoint == DockerEndpoint::Root),
            state: app_state(&states),
            health: app_health(&healths),
            memory_bytes: total_mem,
            web_url,
            https_port: None,
            https_subdomain: None,
            icon,
            containers,
            images,
        },
        detail_extra: extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn c(
        endpoint: DockerEndpoint,
        name: &str,
        project: Option<&str>,
        workdir: Option<&str>,
        state: &str,
    ) -> RawContainer {
        let mut labels = BTreeMap::new();
        if let Some(p) = project {
            labels.insert(LABEL_PROJECT.to_string(), p.to_string());
            labels.insert(LABEL_SERVICE.to_string(), name.to_string());
        }
        if let Some(w) = workdir {
            labels.insert(LABEL_WORKDIR.to_string(), w.to_string());
        }
        RawContainer {
            endpoint: Some(endpoint),
            id: format!("{name}-id"),
            name: name.to_string(),
            state: state.to_string(),
            labels,
            ..Default::default()
        }
    }

    #[test]
    fn groups_by_project_and_detects_casaos() {
        let groups = group(vec![
            c(
                DockerEndpoint::Root,
                "immich-server",
                Some("immich"),
                Some("/var/lib/casaos/apps/immich"),
                "running",
            ),
            c(
                DockerEndpoint::Root,
                "immich-redis",
                Some("immich"),
                Some("/var/lib/casaos/apps/immich"),
                "running",
            ),
            c(
                DockerEndpoint::Root,
                "jellyfin",
                Some("jellyfin"),
                Some("/var/lib/casaos/apps/jellyfin"),
                "exited",
            ),
            c(
                DockerEndpoint::Rootless { uid: 1000 },
                "matter-server",
                None,
                None,
                "running",
            ),
            c(
                DockerEndpoint::Root,
                "web",
                Some("site"),
                Some("/home/me/site"),
                "running",
            ),
        ]);
        let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["immich", "jellyfin", "matter-server", "site"]);
        assert_eq!(groups[0].containers.len(), 2);
        assert_eq!(groups[0].managed_by, ManagedBy::Casaos);
        assert_eq!(groups[2].managed_by, ManagedBy::Container);
        assert_eq!(groups[2].endpoint, DockerEndpoint::Rootless { uid: 1000 });
        assert_eq!(groups[3].managed_by, ManagedBy::Compose);
    }

    #[test]
    fn same_name_in_two_daemons_gets_unique_ids() {
        let groups = group(vec![
            c(DockerEndpoint::Root, "piper", None, None, "running"),
            c(
                DockerEndpoint::Rootless { uid: 1000 },
                "piper",
                None,
                None,
                "running",
            ),
        ]);
        let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["piper", "piper@rootless1000"]);
    }

    #[test]
    fn states_and_health() {
        assert_eq!(app_state(&["running", "running"]), AppState::Running);
        assert_eq!(app_state(&["running", "exited"]), AppState::Partial);
        assert_eq!(app_state(&["exited", "created"]), AppState::Stopped);
        assert_eq!(app_state(&["running", "restarting"]), AppState::Unhealthy);
        assert_eq!(
            app_health(&[Some("healthy".into()), None]),
            Some("healthy".into())
        );
        assert_eq!(
            app_health(&[Some("healthy".into()), Some("starting".into())]),
            Some("starting".into())
        );
        assert_eq!(app_health(&[None, None]), None);
    }

    #[test]
    fn repo_digest_matching_normalises_names() {
        let rds = vec![
            "ghcr.io/other/thing@sha256:111".to_string(),
            "linuxserver/jellyfin@sha256:222".to_string(),
        ];
        assert_eq!(
            matching_repo_digest("linuxserver/jellyfin:latest", &rds).as_deref(),
            Some("sha256:222")
        );
        assert_eq!(
            matching_repo_digest("docker.io/linuxserver/jellyfin:10.11.2", &rds).as_deref(),
            Some("sha256:222")
        );
        assert_eq!(matching_repo_digest("nginx", &rds), None);
    }

    #[test]
    fn update_available_only_when_digests_differ() {
        let chk = ImageCheck {
            digest: Some("sha256:new".into()),
            ..Default::default()
        };
        assert!(latest_info(&chk, Some("sha256:old"), false).update_available);
        assert!(!latest_info(&chk, Some("sha256:new"), false).update_available);
        // A digest-pinned reference never shows an update: pulling it gives the same build.
        assert!(!latest_info(&chk, Some("sha256:old"), true).update_available);
        assert!(
            !latest_info(&chk, None, false).update_available,
            "unknown running digest is not an update"
        );
    }

    #[test]
    fn list_json_parsing() {
        let v = json!({"Id":"abc","Names":["/jellyfin"],"Image":"linuxserver/jellyfin:latest","ImageID":"sha256:x","State":"running","Status":"Up 2 hours","Labels":{"com.docker.compose.project":"jellyfin"}});
        let r = RawContainer::from_list_json(&DockerEndpoint::Root, &v).unwrap_or_default();
        assert_eq!(r.name, "jellyfin");
        assert_eq!(r.project(), Some("jellyfin"));
        assert!(RawContainer::from_list_json(&DockerEndpoint::Root, &json!({})).is_none());
    }

    #[test]
    fn check_key_drops_pinned_digest() {
        assert_eq!(
            check_key("valkey/valkey:9@sha256:3eeb").as_deref(),
            Some("docker.io/valkey/valkey:9")
        );
    }
}
