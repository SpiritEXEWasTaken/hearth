//! The app store (M6a): the signed catalog, host checks, and installing apps from it.
//!
//! - The catalog comes from GitHub Pages (`HEARTH_STORE_URL` overrides it for tests). Its
//!   `index.json` must carry a minisign signature from one of the trusted keys; bundles, icons
//!   and screenshots are checked against the hashes in it. An older catalog than the one already
//!   here is refused, so an old (signed) catalog can't be replayed.
//! - Everything is cached in `<state>/store/`, so the store page works offline and the UI never
//!   loads anything from other sites.
//! - Installing resolves the install options into variants and `.env` values, runs the host
//!   checks, creates the data folders, and hands the result to the normal install engine (health
//!   gate, automatic undo). Generated passwords stay in the app's `.env`; admins can see the
//!   ones the entry marks with `show_as` on the app's page.

use super::State;
use super::engine::{self, InstallRequest};
use super::jobs::{JobError, JobHandle};
use hearth_core::proto::{self, DataDirRequest, Request};
use hearth_store::catalog::{self, Bundle, ImageInfo, Index, IndexApp};
use hearth_store::lint::{find_vars, images_of};
use hearth_store::model::{Entry, HttpsNeed, OptionKind};
use hearth_store::resolve::{self, Hardware};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const MAX_DOWNLOAD: usize = 8 * 1024 * 1024;
/// Refresh the catalog this often in the background.
pub const REFRESH_EVERY: Duration = Duration::from_secs(12 * 3600);

pub struct Store {
    url: String,
    keys: Vec<String>,
    dir: PathBuf,
    http: reqwest::Client,
    inner: Mutex<Inner>,
    refresh_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
    index: Option<Index>,
    bundles: HashMap<String, Bundle>,
    refreshed_at: Option<i64>,
    error: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Store {
    pub fn new(state_dir: &Path) -> Store {
        let mut url =
            std::env::var("HEARTH_STORE_URL").unwrap_or_else(|_| hearth_store::DEFAULT_URL.to_string());
        if !url.ends_with('/') {
            url.push('/');
        }
        let mut keys: Vec<String> = hearth_store::TRUSTED_KEYS
            .iter()
            .filter(|k| !k.contains("PLACEHOLDER"))
            .map(|k| k.to_string())
            .collect();
        if let Ok(k) = std::env::var("HEARTH_STORE_PUBKEY") {
            keys.push(k);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let s = Store {
            url,
            keys,
            dir: state_dir.join("store"),
            http,
            inner: Mutex::new(Inner::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
        };
        s.load_cached();
        s
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn key_refs(&self) -> Vec<&str> {
        self.keys.iter().map(String::as_str).collect()
    }

    /// Loads the catalog saved by the last refresh (checked again: the files could have been
    /// changed on disk).
    fn load_cached(&self) {
        let (Ok(idx), Ok(sig)) = (
            std::fs::read(self.dir.join("index.json")),
            std::fs::read_to_string(self.dir.join("index.json.minisig")),
        ) else {
            return;
        };
        let index = match catalog::verify_index(&idx, &sig, &self.key_refs()) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("saved store catalog ignored: {e}");
                return;
            }
        };
        let mut bundles = HashMap::new();
        for a in &index.apps {
            if let Ok(bytes) = std::fs::read(self.dir.join(&a.bundle))
                && let Ok(b) = catalog::verify_bundle(&bytes, a)
            {
                bundles.insert(a.id.clone(), b);
            }
        }
        let mut inner = self.lock();
        inner.index = Some(index);
        inner.bundles = bundles;
    }

    async fn get(&self, rel: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}{rel}", self.url);
        let mut resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("can't reach the store ({url}): {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("the store answered HTTP {} for {rel}", resp.status()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{rel}: {e}"))? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_DOWNLOAD {
                return Err(format!("{rel} is too big"));
            }
        }
        Ok(bytes)
    }

    /// Downloads the catalog if it changed. Returns (apps, bundles downloaded).
    pub async fn refresh(&self) -> Result<(usize, usize), String> {
        let _guard = self.refresh_lock.lock().await;
        let result = self.refresh_inner().await;
        let mut inner = self.lock();
        inner.refreshed_at = Some(now());
        inner.error = result.as_ref().err().cloned();
        result
    }

    async fn refresh_inner(&self) -> Result<(usize, usize), String> {
        if self.keys.is_empty() {
            return Err("this Hearth build has no store key, so it can't check the catalog".into());
        }
        let idx_bytes = self.get("index.json").await?;
        let sig = String::from_utf8(self.get("index.json.minisig").await?)
            .map_err(|_| "the catalog signature isn't text".to_string())?;
        let index = catalog::verify_index(&idx_bytes, &sig, &self.key_refs())?;
        let (old_generated, old_bundles) = {
            let inner = self.lock();
            (inner.index.as_ref().map(|i| i.generated), inner.bundles.clone())
        };
        if let Some(old) = old_generated
            && index.generated < old
        {
            return Err(
                "the store sent an older catalog than the one Hearth already has; keeping the newer one"
                    .into(),
            );
        }
        let mut bundles = HashMap::new();
        let mut fetched = 0;
        for a in &index.apps {
            let cached_ok = old_bundles.contains_key(&a.id)
                && std::fs::read(self.dir.join(&a.bundle))
                    .map(|b| catalog::sha256_hex(&b) == a.sha256)
                    .unwrap_or(false);
            if cached_ok {
                if let Some(b) = old_bundles.get(&a.id) {
                    bundles.insert(a.id.clone(), b.clone());
                }
            } else {
                let bytes = self.get(&a.bundle).await?;
                let b = catalog::verify_bundle(&bytes, a)?;
                write_atomic(&self.dir.join(&a.bundle), &bytes).map_err(|e| e.to_string())?;
                bundles.insert(a.id.clone(), b);
                fetched += 1;
            }
            let icon_ok = std::fs::read(self.dir.join(&a.icon))
                .map(|b| catalog::sha256_hex(&b) == a.icon_sha256)
                .unwrap_or(false);
            if !icon_ok {
                let bytes = self.get(&a.icon).await?;
                if catalog::sha256_hex(&bytes) != a.icon_sha256 {
                    return Err(format!("{}: the icon doesn't match the signed catalog", a.id));
                }
                write_atomic(&self.dir.join(&a.icon), &bytes).map_err(|e| e.to_string())?;
            }
        }
        write_atomic(&self.dir.join("index.json"), &idx_bytes).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join("index.json.minisig"), sig.as_bytes()).map_err(|e| e.to_string())?;
        let n = index.apps.len();
        let mut inner = self.lock();
        inner.index = Some(index);
        inner.bundles = bundles;
        Ok((n, fetched))
    }

    pub fn index(&self) -> Option<Index> {
        self.lock().index.clone()
    }

    pub fn bundle(&self, id: &str) -> Option<(IndexApp, Bundle)> {
        let inner = self.lock();
        let app = inner.index.as_ref()?.apps.iter().find(|a| a.id == id)?.clone();
        let b = inner.bundles.get(id)?.clone();
        Some((app, b))
    }

    pub fn bundles(&self) -> Vec<Bundle> {
        self.lock().bundles.values().cloned().collect()
    }

    pub fn status(&self) -> (Option<i64>, Option<String>) {
        let inner = self.lock();
        (inner.refreshed_at, inner.error.clone())
    }

    pub fn icon_url(&self, app: &IndexApp) -> String {
        format!("{}{}", self.url, app.icon)
    }

    /// The icon from the cache (already checked).
    pub fn icon(&self, id: &str) -> Option<(String, Vec<u8>)> {
        let (a, _) = self.bundle(id)?;
        let bytes = std::fs::read(self.dir.join(&a.icon)).ok()?;
        (catalog::sha256_hex(&bytes) == a.icon_sha256).then(|| (content_type(&a.icon), bytes))
    }

    /// A screenshot, fetched on first use and checked against the bundle's hash.
    pub async fn file(&self, id: &str, path: &str) -> Result<(String, Vec<u8>), String> {
        let (_, b) = self.bundle(id).ok_or_else(|| format!("no store app {id}"))?;
        let hash = b
            .files
            .get(path)
            .ok_or_else(|| format!("{id} has no file {path}"))?;
        let rel = format!("files/{id}/{path}");
        let local = self.dir.join(&rel);
        if let Ok(bytes) = std::fs::read(&local)
            && &catalog::sha256_hex(&bytes) == hash
        {
            return Ok((content_type(path), bytes));
        }
        let bytes = self.get(&rel).await?;
        if &catalog::sha256_hex(&bytes) != hash {
            return Err(format!("{rel} doesn't match the signed catalog"));
        }
        let _ = write_atomic(&local, &bytes);
        Ok((content_type(path), bytes))
    }
}

fn content_type(path: &str) -> String {
    match path.rsplit('.').next().unwrap_or("") {
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Refreshes the catalog shortly after start and then twice a day.
pub fn spawn_refresher(st: State) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            match st.store.refresh().await {
                Ok((n, f)) => tracing::info!(apps = n, downloaded = f, "store catalog refreshed"),
                Err(e) => tracing::warn!("store catalog refresh failed: {e}"),
            }
            tokio::time::sleep(REFRESH_EVERY).await;
        }
    });
}

// ----- host facts -----

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostFacts {
    pub arch: String,
    pub mem_total_mb: u64,
    pub mem_available_mb: u64,
    pub disk_free_mb: Option<u64>,
    pub hardware: Hardware,
    /// Graphics found, in words ("Intel graphics (/dev/dri)").
    pub gpus: Vec<String>,
    #[serde(skip)]
    pub paths: HashMap<String, String>,
}

fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// What this server has. `paths` are folders to check (the daemon can't see /home itself).
pub async fn host_facts(st: &State, paths: Vec<String>) -> HostFacts {
    let mem = super::stats::parse_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default());
    let mut hw = Hardware::default();
    let render = std::fs::read_dir("/dev/dri")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("renderD"))
        .min();
    let mut gpus = Vec::new();
    if let Some(node) = render {
        hw.dri = true;
        let vendor = read_trim(&format!("/sys/class/drm/{node}/device/vendor"));
        let v = match vendor.as_deref() {
            Some("0x8086") => "intel",
            Some("0x1002") => "amd",
            Some("0x10de") => "nvidia",
            _ => "other",
        };
        hw.dri_vendor = Some(v.to_string());
        gpus.push(match v {
            "intel" => "Intel graphics (/dev/dri)".to_string(),
            "amd" => "AMD graphics (/dev/dri)".to_string(),
            "nvidia" => "Nvidia graphics (/dev/dri)".to_string(),
            _ => "A graphics chip (/dev/dri)".to_string(),
        });
    }
    hw.kfd = Path::new("/dev/kfd").exists();
    hw.nvidia = Path::new("/dev/nvidiactl").exists();
    let facts = st
        .helper
        .call(&Request::HostFacts { paths })
        .await
        .unwrap_or(Value::Null);
    hw.nvidia_runtime = facts
        .get("nvidia_runtime")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if hw.nvidia {
        gpus.push(if hw.nvidia_runtime {
            "Nvidia card, ready for containers".to_string()
        } else {
            "Nvidia card, but Docker's Nvidia runtime (nvidia-container-toolkit) isn't installed".to_string()
        });
    }
    let path_kinds = facts
        .get("paths")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("missing").to_string()))
                .collect()
        })
        .unwrap_or_default();
    HostFacts {
        arch: hearth_registry::Platform::host().architecture,
        mem_total_mb: mem.total_bytes / (1024 * 1024),
        mem_available_mb: mem.available_bytes / (1024 * 1024),
        disk_free_mb: facts.get("docker_free_mb").and_then(Value::as_u64),
        hardware: hw,
        gpus,
        paths: path_kinds,
    }
}

// ----- planning -----

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Answers {
    /// App name on this server (default: the entry's `app_name`).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    /// Host ports by their variable (`WEB_PORT`).
    #[serde(default)]
    pub ports: BTreeMap<String, u16>,
    /// Turn on HTTPS for the app after installing (default: when the app needs it).
    #[serde(default)]
    pub https: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// `ok`, `info`, `warn` or `error` (errors block the install).
    pub level: &'static str,
    /// `cpu`, `ram`, `disk`, `ports`, `gpu`, `https`, `folders`, `name`, `options`.
    pub topic: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedPort {
    pub env: Option<String>,
    pub label: String,
    pub port: u16,
    pub protocol: String,
    pub default: u16,
    pub movable: bool,
}

/// How each install option looks on this server (for the install form).
#[derive(Debug, Clone, Serialize)]
pub struct OptionView {
    pub id: String,
    pub value: String,
    /// Choice ids that can't be picked here, with the reason.
    pub unavailable: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallPlan {
    pub entry: String,
    pub name: String,
    pub answers: BTreeMap<String, String>,
    pub options: Vec<OptionView>,
    pub variants: Vec<String>,
    pub ports: Vec<PlannedPort>,
    pub checks: Vec<Check>,
    pub ok: bool,
    pub download_mb: u64,
    pub disk_needed_mb: u64,
    pub https: bool,
    pub data_dir: String,
    /// The compose file that will be written (secrets stay in `.env`).
    pub compose: String,
    #[serde(skip)]
    pub env: BTreeMap<String, String>,
    #[serde(skip)]
    pub generate: BTreeMap<String, String>,
}

fn check(level: &'static str, topic: &'static str, message: impl Into<String>) -> Check {
    Check {
        level,
        topic,
        message: message.into(),
    }
}

pub fn size_text(mb: u64) -> String {
    hearth_store::site::size_text(mb)
}

/// Group IDs of `render` and `video` from /etc/group (for GPU access in containers).
fn group_gid(name: &str) -> Option<u32> {
    let text = std::fs::read_to_string("/etc/group").ok()?;
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

/// Who already uses a port: an app's name if Hearth knows it.
async fn port_owners(st: &State) -> HashMap<(String, u16), String> {
    let mut owners = HashMap::new();
    let (apps, _) = super::apps::list(st).await;
    for a in apps {
        if let Some(url) = &a.summary.web_url
            && let Ok(p) = super::app_https::http_port(url)
        {
            owners.insert(("tcp".to_string(), p), a.summary.title.clone());
        }
    }
    for h in st.with_db(|db| db.list_app_https()).unwrap_or_default() {
        owners.insert(("tcp".to_string(), h.port), format!("HTTPS for {}", h.app));
    }
    owners
}

/// Plans an install: answers → options, variants, ports, checks, final compose.
pub async fn plan(st: &State, id: &str, a: &Answers) -> Result<InstallPlan, String> {
    let (app, bundle) = st
        .store
        .bundle(id)
        .ok_or_else(|| format!("the store has no app '{id}' (refresh the store?)"))?;
    let e = &bundle.entry;
    let mut checks = Vec::new();

    // Name.
    let name = a.name.clone().unwrap_or_else(|| e.app_name.clone());
    let existing = engine::hearth_apps(st).await.unwrap_or_default();
    let (all, _) = super::apps::list(st).await;
    let taken = |n: &str| existing.iter().any(|x| x == n) || all.iter().any(|x| x.summary.id == n);
    let name = if a.name.is_none() && taken(&name) {
        (2..100)
            .map(|i| format!("{name}-{i}"))
            .find(|n| !taken(n))
            .unwrap_or(name)
    } else {
        name
    };
    if !proto::is_valid_hearth_app_name(&name) {
        checks.push(check(
            "error",
            "name",
            format!("'{name}' can't be an app name: use lowercase letters, digits and '-'"),
        ));
    } else if taken(&name) {
        checks.push(check(
            "error",
            "name",
            format!("an app named '{name}' already exists; pick another name"),
        ));
    }
    let same_entry: Vec<String> = all
        .iter()
        .filter(|x| linked_entry(st, &x.summary.id, x.store_app_id.as_deref()).as_deref() == Some(id))
        .map(|x| x.summary.title.clone())
        .collect();
    if !same_entry.is_empty() {
        checks.push(check(
            "info",
            "name",
            format!(
                "{} is already installed ({}); this adds another copy",
                e.name,
                same_entry.join(", ")
            ),
        ));
    }

    // Host facts (folders from folder options outside the app's own data).
    let data_dir = format!("/var/lib/hearth/data/{name}");
    let mut folder_paths = Vec::new();
    for o in &e.options {
        if o.kind == OptionKind::Folder {
            let v = a.options.get(&o.id).cloned().unwrap_or_else(|| o.default.clone());
            if !v.starts_with("${APP_DATA}") {
                folder_paths.push(v);
            }
        }
    }
    let facts = host_facts(st, folder_paths).await;
    let hw = &facts.hardware;

    // Options.
    let r = resolve::resolve(e, &a.options, hw);
    for err in &r.errors {
        checks.push(check("error", "options", err.clone()));
    }
    let mut options = Vec::new();
    for o in &e.options {
        let mut unavailable = BTreeMap::new();
        for c in &o.choices {
            if let Some(need) = &c.needs
                && !hw.has(need)
            {
                unavailable.insert(c.id.clone(), format!("needs {}", resolve::need_words(need)));
            }
        }
        options.push(OptionView {
            id: o.id.clone(),
            value: r.answers.get(&o.id).cloned().unwrap_or_default(),
            unavailable,
        });
    }
    for o in &e.options {
        if o.kind == OptionKind::Folder {
            let v = r.answers.get(&o.id).cloned().unwrap_or_default();
            if !v.starts_with("${APP_DATA}") {
                match facts.paths.get(&v).map(String::as_str) {
                    Some("dir") => checks.push(check("ok", "folders", format!("{}: {v}", o.label))),
                    Some("file") => {
                        checks.push(check("error", "folders", format!("{v} is a file, not a folder")))
                    }
                    _ => checks.push(check(
                        "error",
                        "folders",
                        format!("{v} doesn't exist on this server; create it first or pick another folder"),
                    )),
                }
            }
        }
    }

    // Compose for these options.
    let compose_text = match resolve::render_compose(&bundle.compose, &bundle.variants, &r.variants) {
        Ok(t) => t,
        Err(err) => return Err(format!("the store entry is broken: {err}")),
    };
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&compose_text).map_err(|e| format!("the store entry is broken: {e}"))?;

    // CPU type, per image.
    let arch = facts.arch.clone();
    if !e.requirements.architectures.contains(&arch) {
        checks.push(check(
            "error",
            "cpu",
            format!(
                "{} doesn't run on this server's CPU ({arch}); it supports {}",
                e.name,
                e.requirements.architectures.join(", ")
            ),
        ));
    }
    let mut download_mb = 0;
    let infos: HashMap<&str, &ImageInfo> = bundle.images.iter().map(|i| (i.image.as_str(), i)).collect();
    for (svc, img) in images_of(&doc) {
        if let Some(info) = infos.get(img.as_str()) {
            if !info.platforms.is_empty() && !info.platforms.iter().any(|p| p == &format!("linux/{arch}")) {
                checks.push(check(
                    "error",
                    "cpu",
                    format!("{svc}: {img} has no version for this CPU ({arch}); pick another option"),
                ));
            }
            download_mb += info.size_mb.get(&arch).copied().unwrap_or(0);
        }
    }
    if checks.iter().all(|c| c.topic != "cpu") {
        checks.push(check("ok", "cpu", format!("Runs on this CPU ({arch})")));
    }

    // RAM.
    let need = e.requirements.ram_mb;
    if facts.mem_total_mb < need {
        checks.push(check(
            "warn",
            "ram",
            format!(
                "Needs at least {} of RAM; this server has {} in total",
                size_text(need),
                size_text(facts.mem_total_mb)
            ),
        ));
    } else if facts.mem_available_mb < need {
        checks.push(check(
            "warn",
            "ram",
            format!(
                "Needs about {}; only {} is free right now (other apps use the rest)",
                size_text(need),
                size_text(facts.mem_available_mb)
            ),
        ));
    } else {
        checks.push(check(
            "ok",
            "ram",
            format!(
                "Needs about {}; {} is free",
                size_text(need),
                size_text(facts.mem_available_mb)
            ),
        ));
    }

    // Disk.
    let disk_needed = download_mb + e.requirements.data_mb;
    match facts.disk_free_mb {
        Some(free) if free < disk_needed + 512 => checks.push(check(
            "error",
            "disk",
            format!(
                "Needs about {} of disk space ({} download + {} data); only {} is free",
                size_text(disk_needed),
                size_text(download_mb),
                size_text(e.requirements.data_mb),
                size_text(free)
            ),
        )),
        Some(free) if free < disk_needed * 2 + 2048 => checks.push(check(
            "warn",
            "disk",
            format!(
                "Needs about {}; {} is free, which is tight",
                size_text(disk_needed),
                size_text(free)
            ),
        )),
        Some(free) => checks.push(check(
            "ok",
            "disk",
            format!(
                "Needs about {}; {} is free",
                size_text(disk_needed),
                size_text(free)
            ),
        )),
        None => checks.push(check(
            "info",
            "disk",
            format!("Needs about {} (free space unknown)", size_text(disk_needed)),
        )),
    }

    // Ports.
    let in_use = engine::ports_in_use();
    let owners = port_owners(st).await;
    let https_ports: HashSet<u16> = st
        .with_db(|db| db.list_app_https())
        .unwrap_or_default()
        .iter()
        .map(|h| h.port)
        .collect();
    let busy = |proto: &str, p: u16| {
        in_use.contains(&(proto.to_string(), p)) || (proto == "tcp" && https_ports.contains(&p))
    };
    let mut planned: Vec<PlannedPort> = Vec::new();
    let mut chosen: BTreeMap<String, u16> = BTreeMap::new();
    let mut taken_now: HashSet<(String, u16)> = HashSet::new();
    // Ports sharing a variable (DNS on tcp + udp) are chosen together.
    let mut groups: Vec<(Option<String>, Vec<&hearth_store::model::PortSpec>)> = Vec::new();
    for p in &e.ports {
        match groups.iter_mut().find(|(k, _)| p.env.is_some() && k == &p.env) {
            Some((_, v)) => v.push(p),
            None => groups.push((p.env.clone(), vec![p])),
        }
    }
    for (env, specs) in groups {
        let first = specs[0];
        let protos: Vec<&str> = specs.iter().map(|s| s.protocol.as_str()).collect();
        let requested = env.as_ref().and_then(|k| a.ports.get(k)).copied();
        let start = requested.unwrap_or(first.port);
        let free = |p: u16| {
            protos
                .iter()
                .all(|pr| !busy(pr, p) && !taken_now.contains(&(pr.to_string(), p)))
        };
        let port = if free(start) {
            Some(start)
        } else if requested.is_none() && first.movable && env.is_some() {
            (start.saturating_add(1)..start.saturating_add(200)).find(|p| free(*p))
        } else {
            None
        };
        let label = first.label.clone();
        let proto_text = protos.join("+");
        match port {
            Some(p) => {
                if p != start {
                    let who = owners
                        .get(&("tcp".to_string(), start))
                        .map(|o| format!(" (by {o})"))
                        .unwrap_or_default();
                    checks.push(check(
                        "info",
                        "ports",
                        format!("{label}: port {start} is in use{who}, so {p} is used instead"),
                    ));
                } else {
                    checks.push(check(
                        "ok",
                        "ports",
                        format!("{label}: port {p}/{proto_text} is free"),
                    ));
                }
                for pr in &protos {
                    taken_now.insert((pr.to_string(), p));
                }
                if let Some(k) = &env {
                    chosen.insert(k.clone(), p);
                }
                for s in &specs {
                    planned.push(PlannedPort {
                        env: env.clone(),
                        label: s.label.clone(),
                        port: p,
                        protocol: s.protocol.as_str().to_string(),
                        default: s.port,
                        movable: s.movable,
                    });
                }
            }
            None => {
                let who = protos
                    .iter()
                    .find_map(|pr| owners.get(&(pr.to_string(), start)))
                    .map(|o| format!(" by {o}"))
                    .unwrap_or_default();
                let resolved = start == 53 && Path::new("/run/systemd/resolve/stub-resolv.conf").exists();
                let fix = if resolved && who.is_empty() {
                    "it's most likely systemd-resolved's DNS stub: set DNSStubListener=no in /etc/systemd/resolved.conf and restart systemd-resolved, then try again"
                } else if first.movable && env.is_some() {
                    "pick another port"
                } else {
                    "devices expect this exact port, so stop or remove the app that uses it first"
                };
                checks.push(check(
                    "error",
                    "ports",
                    format!(
                        "{label}: port {start}/{proto_text} is already in use on this server{who}; {fix}"
                    ),
                ));
                for s in &specs {
                    planned.push(PlannedPort {
                        env: env.clone(),
                        label: s.label.clone(),
                        port: start,
                        protocol: s.protocol.as_str().to_string(),
                        default: s.port,
                        movable: s.movable,
                    });
                }
            }
        }
    }

    // GPU.
    if e.requirements.gpu != hearth_store::model::GpuUse::None {
        if facts.gpus.is_empty() {
            checks.push(check(
                "info",
                "gpu",
                "No graphics chip found; the app uses the CPU",
            ));
        } else {
            checks.push(check("ok", "gpu", format!("Found: {}", facts.gpus.join("; "))));
        }
    }

    // HTTPS.
    let need_https = e.web.as_ref().map(|w| w.https).unwrap_or_default();
    let https = a.https.unwrap_or(need_https == HttpsNeed::Required);
    match need_https {
        HttpsNeed::Required if https => checks.push(check(
            "info",
            "https",
            "Needs HTTPS: Hearth turns it on for this app and gives it an https:// address",
        )),
        HttpsNeed::Required => checks.push(check(
            "warn",
            "https",
            "This app needs HTTPS to work properly; turn it on later in the app's HTTPS card",
        )),
        HttpsNeed::Recommended if !https => checks.push(check(
            "info",
            "https",
            "Some features (phone apps, camera) work best over HTTPS; you can turn it on now or later",
        )),
        _ => {}
    }

    // Env: option values, ports, host values. ${APP_DATA} is filled in at install.
    let mut env = r.env.clone();
    for (k, v) in &chosen {
        env.insert(k.clone(), v.to_string());
    }
    let render = group_gid("render").or_else(|| group_gid("video")).unwrap_or(0);
    let video = group_gid("video").unwrap_or(0);
    env.insert("PUID".into(), st.host.puid.to_string());
    env.insert("PGID".into(), st.host.pgid.to_string());
    env.insert("TZ".into(), st.host.tz.clone());
    env.insert("RENDER_GID".into(), render.to_string());
    env.insert("VIDEO_GID".into(), video.to_string());
    let mut generate = BTreeMap::new();
    for (k, v) in &e.env {
        if let Some(g) = &v.generate {
            generate.insert(k.clone(), g.clone());
        }
    }
    let used = find_vars(&compose_text);
    env.retain(|k, _| used.contains(k));

    // x-hearth: title, icon, web link, store details.
    let web_port = e.web.as_ref().and_then(|w| {
        w.port_env
            .as_ref()
            .and_then(|k| chosen.get(k).copied())
            .or(w.port)
    });
    let final_compose = add_x_hearth(
        &compose_text,
        e,
        &app,
        &st.store.icon_url(&app),
        web_port,
        &r.answers,
    )?;
    let ok = checks.iter().all(|c| c.level != "error");
    Ok(InstallPlan {
        entry: id.to_string(),
        name,
        answers: r.answers,
        options,
        variants: r.variants,
        ports: planned,
        checks,
        ok,
        download_mb,
        disk_needed_mb: disk_needed,
        https,
        data_dir,
        compose: final_compose,
        env,
        generate,
    })
}

fn add_x_hearth(
    compose: &str,
    e: &Entry,
    app: &IndexApp,
    icon_url: &str,
    web_port: Option<u16>,
    answers: &BTreeMap<String, String>,
) -> Result<String, String> {
    use serde_yaml_ng::{Mapping, Value as Y};
    let mut doc: Y = serde_yaml_ng::from_str(compose).map_err(|e| e.to_string())?;
    let mut xh = Mapping::new();
    xh.insert("title".into(), e.name.clone().into());
    xh.insert("icon".into(), icon_url.into());
    if let (Some(w), Some(port)) = (&e.web, web_port) {
        let mut web = Mapping::new();
        web.insert("service".into(), w.service.clone().into());
        web.insert("port".into(), Y::Number(port.into()));
        web.insert("path".into(), w.path.clone().into());
        if let Some(h) = &w.health {
            web.insert("health".into(), h.clone().into());
        }
        xh.insert("web".into(), Y::Mapping(web));
        if !w.dashboard {
            xh.insert("dashboard".into(), Y::Bool(false));
        }
    }
    let mut store = Mapping::new();
    store.insert("id".into(), app.id.clone().into());
    let mut opts = Mapping::new();
    for (k, v) in answers {
        opts.insert(k.clone().into(), v.clone().into());
    }
    store.insert("options".into(), Y::Mapping(opts));
    let shown: Vec<Y> = e
        .env
        .iter()
        .filter_map(|(k, v)| {
            let label = v.show_as.as_ref()?;
            let mut m = Mapping::new();
            m.insert("label".into(), label.clone().into());
            m.insert("env".into(), k.clone().into());
            Some(Y::Mapping(m))
        })
        .collect();
    if !shown.is_empty() {
        store.insert("show".into(), Y::Sequence(shown));
    }
    xh.insert("store".into(), Y::Mapping(store));
    if !e.backup_exclude.is_empty() {
        let mut b = Mapping::new();
        b.insert(
            "exclude".into(),
            Y::Sequence(e.backup_exclude.iter().map(|x| x.clone().into()).collect()),
        );
        xh.insert("backup".into(), Y::Mapping(b));
    }
    if let Some(m) = doc.as_mapping_mut() {
        m.insert("x-hearth".into(), Y::Mapping(xh));
    }
    serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())
}

fn random_secret(kind: &str) -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
    Ok(match kind {
        "hex32" => hex::encode(buf),
        _ => {
            const ALPHA: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
            buf.iter()
                .take(24)
                .map(|b| ALPHA[*b as usize % ALPHA.len()] as char)
                .collect()
        }
    })
}

/// Installs a store app (runs as a job).
pub async fn install(
    st: &State,
    job: &JobHandle,
    id: &str,
    a: Answers,
    actor: &str,
) -> Result<Value, JobError> {
    job.log(format!("planning the install of {id}"));
    let p = plan(st, id, &a).await.map_err(JobError::failed)?;
    for c in &p.checks {
        if c.level != "ok" {
            job.log(format!("{}: {}", c.level, c.message));
        }
    }
    if !p.ok {
        let errs: Vec<String> = p
            .checks
            .iter()
            .filter(|c| c.level == "error")
            .map(|c| c.message.clone())
            .collect();
        return Err(JobError::failed(errs.join("; ")));
    }
    let (_, bundle) = st
        .store
        .bundle(id)
        .ok_or_else(|| JobError::failed("the store entry vanished"))?;
    let e = &bundle.entry;

    job.log("creating the app's data folders");
    let dirs: Vec<DataDirRequest> = e
        .data_dirs
        .iter()
        .map(|d| DataDirRequest {
            path: d.path.clone(),
            user: d.owner == "user",
        })
        .collect();
    let prepared = st
        .helper
        .call(&Request::PrepareAppData {
            app: p.name.clone(),
            dirs,
            uid: st.host.puid,
            gid: st.host.pgid,
        })
        .await
        .map_err(|e| JobError::failed(format!("can't create the data folders: {e}")))?;
    let root = prepared
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or(&p.data_dir)
        .to_string();
    let created_root = prepared
        .get("created")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().any(|x| x.as_str() == Some(root.as_str())));

    let mut sets: BTreeMap<String, String> = p
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.replace("${APP_DATA}", &root)))
        .collect();
    sets.insert("APP_DATA".into(), root.clone());
    for (k, kind) in &p.generate {
        sets.insert(k.clone(), random_secret(kind).map_err(JobError::failed)?);
    }

    let req = InstallRequest {
        app: p.name.clone(),
        compose: p.compose.clone(),
        sets,
        start: true,
        actor: actor.to_string(),
    };
    let result = engine::install(st, job, req).await;
    if let Err(err) = result {
        if created_root {
            let _ = st
                .helper
                .call(&Request::DeleteAppData {
                    app: p.name.clone(),
                    volumes: Vec::new(),
                    paths: vec![root.clone()],
                })
                .await;
        }
        return Err(err);
    }
    let mut out = json!({ "app": p.name, "entry": id });
    if p.https {
        job.log("turning on HTTPS for the app");
        match super::app_https::turn_on(st, &p.name, actor).await {
            Ok(urls) => {
                if let Some(u) = urls.first() {
                    job.log(format!("HTTPS address: {u}"));
                }
                out["https_urls"] = json!(urls);
            }
            Err(e) => job.log(format!(
                "warning: HTTPS couldn't be turned on: {e} (use the app's HTTPS card)"
            )),
        }
    }
    let shown: Vec<&String> = e.env.values().filter_map(|v| v.show_as.as_ref()).collect();
    if !shown.is_empty() {
        job.log("sign-in details are on the app's page (admins only)");
    }
    job.log(format!("{} is installed as '{}'", e.name, p.name));
    Ok(out)
}

// ----- links between installed apps and store entries -----

/// The store entry an installed app belongs to: from its compose file (store installs) or a link
/// made by the user.
pub fn linked_entry(st: &State, app: &str, from_compose: Option<&str>) -> Option<String> {
    from_compose
        .filter(|s| s.contains('.'))
        .map(str::to_string)
        .or_else(|| st.with_db(|db| db.store_link(app).ok().flatten()))
}

/// A store entry whose main image the app runs, for "link it?".
pub fn suggest_entry(st: &State, images: &[String]) -> Option<String> {
    let names: HashSet<String> = images
        .iter()
        .map(|i| hearth_store::lint::parse_image(i).name)
        .collect();
    let mut found: Vec<(String, String)> = Vec::new();
    for b in st.store.bundles() {
        let src = hearth_store::catalog::Source {
            dir: PathBuf::new(),
            entry: b.entry.clone(),
            compose: b.compose.clone(),
            variants: b.variants.clone(),
        };
        if let Some(main) = hearth_store::lint::main_image(&src)
            && names.contains(&main)
        {
            found.push((b.entry.name.clone(), b.entry.id.clone()));
        }
    }
    found.sort();
    found.into_iter().next().map(|(_, id)| id)
}
