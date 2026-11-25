//! The helper's operations on Hearth-managed apps: files and history, `docker compose`,
//! images, data deletion and backups.
//!
//! Safety rules here:
//! - App names are validated before they become paths or compose project names.
//! - `docker compose` runs with a fixed command line per action, a clean environment and the
//!   app's own folder; nothing from the request is passed to a shell.
//! - Deleting data only touches the project's own named volumes and folders under
//!   `/DATA/AppData/<app>` or `/var/lib/hearth/data/<app>`.
//! - Backups are built from the app's resolved compose config, read by the helper itself.
//! - Archives are extracted with path checks, and existing data is moved aside first.

use crate::helper::{HelperConfig, Out};
use crate::unixhttp;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use hearth_core::proto::{self, BackupInfo, ComposeAction, Revision};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Paths never backed up, restored or deleted.
const SYSTEM_PREFIXES: &[&str] = &[
    "/etc", "/run", "/var/run", "/dev", "/proc", "/sys", "/usr", "/lib", "/lib64", "/bin", "/sbin", "/boot",
    "/tmp",
];

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn check_app(app: &str) -> Result<(), String> {
    if proto::is_valid_hearth_app_name(app) {
        Ok(())
    } else {
        Err(format!("invalid app name '{app}'"))
    }
}

fn app_dir(cfg: &HelperConfig, app: &str) -> PathBuf {
    cfg.apps_dir.join(app)
}

fn history_dir(cfg: &HelperConfig, app: &str) -> PathBuf {
    app_dir(cfg, app).join(".history")
}

/// Writes `data` to `path` atomically with `mode`, owned by root and `gid`.
fn write_atomic(path: &Path, data: &[u8], mode: u32, gid: Option<u32>) -> Result<(), String> {
    let tmp = path.with_extension("hearth-tmp");
    let mut f = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    if let Some(g) = gid {
        rustix::fs::chown(&tmp, None, Some(rustix::fs::Gid::from_raw(g))).map_err(|e| e.to_string())?;
    }
    f.write_all(data).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn ensure_dir(path: &Path, mode: u32, gid: Option<u32>) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    if let Some(g) = gid {
        rustix::fs::chown(path, None, Some(rustix::fs::Gid::from_raw(g))).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ----- files and history -----

pub fn list_apps(cfg: &HelperConfig) -> Result<Value, String> {
    let mut names: Vec<String> = match fs::read_dir(&cfg.apps_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("compose.yaml").is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| proto::is_valid_hearth_app_name(n))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    names.sort();
    Ok(json!(names))
}

pub fn read_files(cfg: &HelperConfig, app: &str, include_env: bool) -> Result<Value, String> {
    check_app(app)?;
    let dir = app_dir(cfg, app);
    let compose = fs::read_to_string(dir.join("compose.yaml")).map_err(|e| format!("{app}: {e}"))?;
    let env = if include_env {
        Some(fs::read_to_string(dir.join(".env")).unwrap_or_default())
    } else {
        None
    };
    Ok(json!({ "compose": compose, "env": env }))
}

fn revisions(cfg: &HelperConfig, app: &str) -> Vec<(u32, PathBuf)> {
    let mut out: Vec<(u32, PathBuf)> = fs::read_dir(history_dir(cfg, app))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    let n: u32 = name.strip_suffix(".json")?.parse().ok()?;
                    Some((n, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(n, _)| *n);
    out
}

pub fn write_files(
    cfg: &HelperConfig,
    app: &str,
    compose: &str,
    env: &str,
    message: &str,
    actor: &str,
) -> Result<Value, String> {
    check_app(app)?;
    if compose.len() > 2 * 1024 * 1024 || env.len() > 1024 * 1024 {
        return Err("file too large".into());
    }
    let dir = app_dir(cfg, app);
    ensure_dir(&cfg.apps_dir, 0o750, cfg.daemon_gid)?;
    ensure_dir(&dir, 0o750, cfg.daemon_gid)?;
    ensure_dir(&history_dir(cfg, app), 0o700, None)?;
    write_atomic(
        &dir.join("compose.yaml"),
        compose.as_bytes(),
        0o640,
        cfg.daemon_gid,
    )?;
    write_atomic(&dir.join(".env"), env.as_bytes(), 0o600, None)?;
    let next = revisions(cfg, app).last().map(|(n, _)| n + 1).unwrap_or(1);
    let rev = Revision {
        revision: next,
        time: now(),
        actor: actor.chars().take(100).collect(),
        message: message.chars().take(500).collect(),
        compose: Some(compose.to_string()),
        env: Some(env.to_string()),
    };
    let data = serde_json::to_vec_pretty(&rev).map_err(|e| e.to_string())?;
    write_atomic(
        &history_dir(cfg, app).join(format!("{next:06}.json")),
        &data,
        0o600,
        None,
    )?;
    Ok(json!({ "revision": next }))
}

pub fn list_history(cfg: &HelperConfig, app: &str) -> Result<Value, String> {
    check_app(app)?;
    let mut list = Vec::new();
    for (_, path) in revisions(cfg, app) {
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(mut r) = serde_json::from_str::<Revision>(&text)
        {
            r.compose = None;
            r.env = None;
            list.push(r);
        }
    }
    Ok(json!(list))
}

pub fn read_revision(cfg: &HelperConfig, app: &str, revision: u32) -> Result<Value, String> {
    check_app(app)?;
    let path = history_dir(cfg, app).join(format!("{revision:06}.json"));
    let text = fs::read_to_string(&path).map_err(|_| format!("{app} has no revision {revision}"))?;
    serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())
}

pub fn remove_app_dir(cfg: &HelperConfig, app: &str) -> Result<Value, String> {
    check_app(app)?;
    let dir = app_dir(cfg, app);
    if !dir.exists() {
        return Err(format!("{app} has no app folder"));
    }
    ensure_dir(&cfg.removed_dir, 0o700, None)?;
    let dest = cfg.removed_dir.join(format!("{app}-{}", now()));
    fs::rename(&dir, &dest).map_err(|e| e.to_string())?;
    Ok(json!({ "moved_to": dest.display().to_string() }))
}

// ----- docker compose -----

fn compose_base(cfg: &HelperConfig, dir: &Path, app: &str, compose_file: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&cfg.docker_bin);
    cmd.arg("compose")
        .arg("--project-name")
        .arg(app)
        .arg("--project-directory")
        .arg(dir)
        .arg("--file")
        .arg(compose_file);
    let env_file = dir.join(".env");
    if env_file.exists() {
        cmd.arg("--env-file").arg(env_file);
    }
    cmd.env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/root")
        .env("DOCKER_HOST", format!("unix://{}", cfg.docker_socket.display()))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// Runs `docker compose` for an app, streaming its output as progress.
pub async fn compose(
    cfg: &HelperConfig,
    app: &str,
    action: &ComposeAction,
    services: &[String],
    out: &mut Out<'_>,
) -> Result<Value, String> {
    check_app(app)?;
    if let Some(bad) = services.iter().find(|s| !proto::is_valid_service_name(s)) {
        return Err(format!("invalid service name '{bad}'"));
    }
    let dir = app_dir(cfg, app);
    let file = dir.join("compose.yaml");
    if !file.is_file() && !matches!(action, ComposeAction::Down { .. }) {
        return Err(format!("{app} has no compose.yaml"));
    }
    let mut cmd = compose_base(cfg, &dir, app, &file);
    cmd.arg("--progress").arg("plain");
    match action {
        ComposeAction::Pull => {
            cmd.arg("pull");
        }
        ComposeAction::Up {
            no_start,
            no_deps,
            force_recreate,
        } => {
            cmd.arg("up");
            if *no_start {
                cmd.arg("--no-start");
            } else {
                cmd.arg("--detach");
            }
            if *no_deps {
                cmd.arg("--no-deps");
            }
            if *force_recreate {
                cmd.arg("--force-recreate");
            }
            cmd.arg("--pull").arg("never").arg("--remove-orphans");
        }
        ComposeAction::Start => {
            cmd.arg("start");
        }
        ComposeAction::Stop => {
            cmd.arg("stop");
        }
        ComposeAction::Restart => {
            cmd.arg("restart");
        }
        ComposeAction::Down { volumes } => {
            cmd.arg("down");
            if *volumes {
                cmd.arg("--volumes");
            }
        }
    }
    if !matches!(action, ComposeAction::Down { .. }) {
        cmd.arg("--");
        cmd.args(services);
    }
    run_streaming(cmd, out).await
}

async fn run_streaming(mut cmd: tokio::process::Command, out: &mut Out<'_>) -> Result<Value, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run docker compose: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
    for pipe in [
        stdout.map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        stderr.map(|s| Box::new(s) as _),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(l).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut tail: Vec<String> = Vec::new();
    while let Some(line) = rx.recv().await {
        let line = line.trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        out.progress(&line).await;
        tail.push(line);
        if tail.len() > 20 {
            tail.remove(0);
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(json!({ "ok": true }))
    } else {
        Err(format!("the command failed ({status}): {}", tail.join(" | ")))
    }
}

/// `docker compose config --format json` for an installed app.
pub async fn compose_config(cfg: &HelperConfig, app: &str) -> Result<Value, String> {
    check_app(app)?;
    let dir = app_dir(cfg, app);
    let mut cmd = compose_base(cfg, &dir, app, &dir.join("compose.yaml"));
    cmd.arg("config").arg("--format").arg("json");
    let output = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| "docker compose config timed out".to_string())?
        .map_err(|e| format!("cannot run docker compose: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("compose config: {e}"))
}

/// Validates compose text + env in a scratch folder: `docker compose config -q`.
pub async fn validate(cfg: &HelperConfig, app: &str, compose: &str, env: &str) -> Result<Value, String> {
    check_app(app)?;
    let tmp = std::env::temp_dir().join(format!("hearth-validate-{app}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    ensure_dir(&tmp, 0o700, None)?;
    let result = async {
        fs::write(tmp.join("compose.yaml"), compose).map_err(|e| e.to_string())?;
        fs::write(tmp.join(".env"), env).map_err(|e| e.to_string())?;
        let mut cmd = compose_base(cfg, &tmp, app, &tmp.join("compose.yaml"));
        cmd.arg("config").arg("--quiet");
        let output = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .map_err(|_| "docker compose config timed out".to_string())?
            .map_err(|e| format!("cannot run docker compose: {e}"))?;
        if output.status.success() {
            Ok(json!({ "valid": true }))
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
    .await;
    let _ = fs::remove_dir_all(&tmp);
    result
}

// ----- images -----

fn is_valid_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-/:".contains(c))
}

fn is_valid_tag(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn docker(cfg: &HelperConfig, method: http::Method, path: &str) -> Result<unixhttp::Response, String> {
    let resp = unixhttp::request(
        &cfg.docker_socket,
        method,
        path,
        &[],
        None,
        32 * 1024 * 1024,
        Duration::from_secs(60),
    )
    .await
    .map_err(|e| format!("docker unreachable: {e}"))?;
    if resp.status >= 300 {
        let msg = serde_json::from_slice::<Value>(&resp.body)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&resp.body).into_owned());
        return Err(format!("docker {}: {msg}", resp.status));
    }
    Ok(resp)
}

pub async fn tag_image(cfg: &HelperConfig, source: &str, repo: &str, tag: &str) -> Result<Value, String> {
    if !proto::is_valid_docker_ref(source) || !is_valid_repo(repo) || !is_valid_tag(tag) {
        return Err("invalid image reference".into());
    }
    let path = format!(
        "/images/{source}/tag?repo={}&tag={}",
        urlencode(repo),
        urlencode(tag)
    );
    docker(cfg, http::Method::POST, &path).await?;
    Ok(json!({ "tagged": format!("{repo}:{tag}") }))
}

pub async fn remove_image(cfg: &HelperConfig, name: &str) -> Result<Value, String> {
    if !proto::is_valid_docker_ref(name) {
        return Err("invalid image reference".into());
    }
    // Never forced: Docker refuses to remove an image a container still uses.
    let resp = docker(cfg, http::Method::DELETE, &format!("/images/{name}")).await?;
    Ok(serde_json::from_slice(&resp.body).unwrap_or(Value::Null))
}

pub async fn list_images(cfg: &HelperConfig) -> Result<Value, String> {
    let resp = docker(cfg, http::Method::GET, "/images/json").await?;
    serde_json::from_slice(&resp.body).map_err(|e| e.to_string())
}

// ----- data deletion -----

/// Data folders Hearth may delete for `app`.
fn deletable_roots(cfg: &HelperConfig, app: &str) -> Vec<PathBuf> {
    vec![Path::new("/DATA/AppData").join(app), cfg.data_dir.join(app)]
}

fn is_under(path: &Path, root: &Path) -> bool {
    let clean = path
        .components()
        .all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    clean && path.starts_with(root)
}

pub async fn delete_data(
    cfg: &HelperConfig,
    app: &str,
    volumes: &[String],
    paths: &[String],
    out: &mut Out<'_>,
) -> Result<Value, String> {
    check_app(app)?;
    let mut deleted = Vec::new();
    let mut kept = Vec::new();
    for v in volumes {
        if !proto::is_valid_docker_ref(v) {
            kept.push(format!("volume {v}: invalid name"));
            continue;
        }
        let info = match docker(cfg, http::Method::GET, &format!("/volumes/{v}")).await {
            Ok(r) => serde_json::from_slice::<Value>(&r.body).unwrap_or(Value::Null),
            Err(e) => {
                kept.push(format!("volume {v}: {e}"));
                continue;
            }
        };
        let owner = info
            .pointer("/Labels/com.docker.compose.project")
            .and_then(Value::as_str);
        if owner != Some(app) {
            kept.push(format!("volume {v}: not created by this app"));
            continue;
        }
        match docker(cfg, http::Method::DELETE, &format!("/volumes/{v}")).await {
            Ok(_) => {
                out.progress(&format!("deleted volume {v}")).await;
                deleted.push(format!("volume {v}"));
            }
            Err(e) => kept.push(format!("volume {v}: {e}")),
        }
    }
    let roots = deletable_roots(cfg, app);
    for p in paths {
        let path = PathBuf::from(p);
        if !roots.iter().any(|r| is_under(&path, r)) {
            kept.push(format!("{p}: outside this app's data folders, not deleted"));
            continue;
        }
        match fs::remove_dir_all(&path).or_else(|_| fs::remove_file(&path)) {
            Ok(()) => {
                out.progress(&format!("deleted {p}")).await;
                deleted.push(p.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => kept.push(format!("{p}: {e}")),
        }
    }
    Ok(json!({ "deleted": deleted, "kept": kept }))
}

// ----- backups -----

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    app: String,
    created_at: i64,
    hearth_version: String,
    binds: Vec<BindEntry>,
    volumes: Vec<VolumeEntry>,
    excluded: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BindEntry {
    index: usize,
    service: String,
    source: String,
    target: String,
    is_dir: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct VolumeEntry {
    name: String,
    mountpoint: String,
}

fn is_system_path(p: &str) -> bool {
    p == "/"
        || SYSTEM_PREFIXES
            .iter()
            .any(|s| p == *s || p.starts_with(&format!("{s}/")))
}

fn backups_dir(cfg: &HelperConfig, app: &str) -> PathBuf {
    cfg.backups_dir.join(app)
}

pub async fn create_backup(
    cfg: &HelperConfig,
    app: &str,
    exclude_targets: &[String],
    out: &mut Out<'_>,
) -> Result<Value, String> {
    check_app(app)?;
    let config = compose_config(cfg, app).await?;
    let mut binds = Vec::new();
    let mut volumes = Vec::new();
    let mut excluded = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(services) = config.get("services").and_then(Value::as_object) {
        for (svc, s) in services {
            for v in s.get("volumes").and_then(Value::as_array).into_iter().flatten() {
                let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
                let source = v
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let target = v
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if exclude_targets.contains(&target) {
                    excluded.push(format!("{svc}:{target}"));
                    continue;
                }
                if kind == "bind" {
                    if source.is_empty()
                        || is_system_path(&source)
                        || !Path::new(&source).exists()
                        || !seen.insert(source.clone())
                    {
                        continue;
                    }
                    let is_dir = Path::new(&source).is_dir();
                    binds.push(BindEntry {
                        index: binds.len(),
                        service: svc.clone(),
                        source,
                        target,
                        is_dir,
                    });
                } else if kind == "volume" && !source.is_empty() {
                    // Named volumes in the resolved config carry the project prefix.
                    let full = config
                        .pointer(&format!("/volumes/{}/name", source))
                        .and_then(Value::as_str)
                        .unwrap_or(&source)
                        .to_string();
                    if !seen.insert(format!("vol:{full}")) {
                        continue;
                    }
                    let info = docker(cfg, http::Method::GET, &format!("/volumes/{full}")).await?;
                    let info: Value = serde_json::from_slice(&info.body).unwrap_or(Value::Null);
                    let mountpoint = info
                        .get("Mountpoint")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if !mountpoint.is_empty() {
                        volumes.push(VolumeEntry {
                            name: full,
                            mountpoint,
                        });
                    }
                }
            }
        }
    }

    let dir = backups_dir(cfg, app);
    ensure_dir(&cfg.backups_dir, 0o700, None)?;
    ensure_dir(&dir, 0o700, None)?;
    let stamp = timestamp();
    let name = format!("{app}-{stamp}.tar.gz");
    let path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.partial"));
    let manifest = Manifest {
        app: app.to_string(),
        created_at: now(),
        hearth_version: env!("CARGO_PKG_VERSION").to_string(),
        binds,
        volumes,
        excluded,
    };
    for b in &manifest.binds {
        out.progress(&format!("backing up {} ({}:{})", b.source, b.service, b.target))
            .await;
    }
    for v in &manifest.volumes {
        out.progress(&format!("backing up volume {}", v.name)).await;
    }
    let app_files = app_dir(cfg, app);
    let tmp2 = tmp.clone();
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    // Writing a large archive is blocking work.
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = fs::File::create(&tmp2).map_err(|e| e.to_string())?;
        fs::set_permissions(&tmp2, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::fast()));
        tar.follow_symlinks(false);
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_json.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", &manifest_json[..])
            .map_err(|e| e.to_string())?;
        for f in ["compose.yaml", ".env"] {
            let p = app_files.join(f);
            if p.exists() {
                tar.append_path_with_name(&p, format!("files/{f}"))
                    .map_err(|e| e.to_string())?;
            }
        }
        for b in &manifest.binds {
            let name = format!("binds/{}", b.index);
            if b.is_dir {
                tar.append_dir_all(&name, &b.source)
                    .map_err(|e| format!("{}: {e}", b.source))?;
            } else {
                tar.append_path_with_name(&b.source, &name)
                    .map_err(|e| format!("{}: {e}", b.source))?;
            }
        }
        for v in &manifest.volumes {
            tar.append_dir_all(format!("volumes/{}", v.name), &v.mountpoint)
                .map_err(|e| format!("volume {}: {e}", v.name))?;
        }
        let gz = tar.into_inner().map_err(|e| e.to_string())?;
        let file = gz.finish().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("backup failed: {e}"));
    }
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    out.progress(&format!(
        "wrote {} ({} MiB)",
        path.display(),
        size / (1024 * 1024)
    ))
    .await;
    Ok(json!(BackupInfo {
        file: name,
        size_bytes: size,
        created_at: now(),
    }))
}

fn timestamp() -> String {
    // YYYYMMDD-HHMMSS in UTC, without a date library.
    let secs = now();
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days → civil date algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn list_backups(cfg: &HelperConfig, app: &str) -> Result<Value, String> {
    check_app(app)?;
    let mut list: Vec<BackupInfo> = fs::read_dir(backups_dir(cfg, app))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    if !proto::is_valid_backup_name(&name) {
                        return None;
                    }
                    let meta = e.metadata().ok()?;
                    let created = meta
                        .modified()
                        .ok()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs() as i64;
                    Some(BackupInfo {
                        file: name,
                        size_bytes: meta.len(),
                        created_at: created,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(json!(list))
}

pub fn prune_backups(cfg: &HelperConfig, app: &str, keep: u32) -> Result<Value, String> {
    let list: Vec<BackupInfo> = serde_json::from_value(list_backups(cfg, app)?).unwrap_or_default();
    let excess = list.len().saturating_sub(keep.max(1) as usize);
    let mut removed = Vec::new();
    for b in list.into_iter().take(excess) {
        if fs::remove_file(backups_dir(cfg, app).join(&b.file)).is_ok() {
            removed.push(b.file);
        }
    }
    Ok(json!({ "removed": removed }))
}

/// Restores an archive. Existing data is moved aside to `<path>.hearth-pre-restore-<time>` first
/// and moved back if extraction fails. The app should be stopped by the caller.
pub async fn restore_backup(
    cfg: &HelperConfig,
    app: &str,
    file: &str,
    out: &mut Out<'_>,
) -> Result<Value, String> {
    check_app(app)?;
    if !proto::is_valid_backup_name(file) {
        return Err("invalid backup name".into());
    }
    let path = backups_dir(cfg, app).join(file);
    if !path.is_file() {
        return Err(format!("no backup {file} for {app}"));
    }
    let manifest: Manifest = {
        let f = fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut ar = tar::Archive::new(GzDecoder::new(f));
        let mut found = None;
        for entry in ar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if entry
                .path()
                .map(|p| p == Path::new("manifest.json"))
                .unwrap_or(false)
            {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut entry, &mut s).map_err(|e| e.to_string())?;
                found = Some(serde_json::from_str::<Manifest>(&s).map_err(|e| format!("bad manifest: {e}"))?);
                break;
            }
        }
        found.ok_or("backup has no manifest")?
    };
    if manifest.app != app {
        return Err(format!("this backup belongs to '{}', not '{app}'", manifest.app));
    }
    // Every destination must be a real data path; never system paths.
    for b in &manifest.binds {
        if !b.source.starts_with('/') || is_system_path(&b.source) || b.source.contains("..") {
            return Err(format!("refusing to restore into {}", b.source));
        }
    }
    let stamp = now();
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut targets: Vec<(String, PathBuf, bool)> = Vec::new(); // (archive prefix, destination, is_dir)
    for b in &manifest.binds {
        targets.push((format!("binds/{}", b.index), PathBuf::from(&b.source), b.is_dir));
    }
    for v in &manifest.volumes {
        let info = docker(cfg, http::Method::GET, &format!("/volumes/{}", v.name)).await;
        let mountpoint = match info {
            Ok(r) => serde_json::from_slice::<Value>(&r.body)
                .ok()
                .and_then(|i| i.get("Mountpoint").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| v.mountpoint.clone()),
            Err(_) => v.mountpoint.clone(),
        };
        if !mountpoint.starts_with("/var/lib/docker/volumes/") {
            return Err(format!(
                "unexpected mountpoint for volume {}: {mountpoint}",
                v.name
            ));
        }
        targets.push((format!("volumes/{}", v.name), PathBuf::from(mountpoint), true));
    }
    for (_, dest, _) in &targets {
        if dest.exists() {
            let aside = PathBuf::from(format!("{}.hearth-pre-restore-{stamp}", dest.display()));
            fs::rename(dest, &aside).map_err(|e| format!("cannot move {} aside: {e}", dest.display()))?;
            out.progress(&format!("moved current {} aside", dest.display()))
                .await;
            moved.push((dest.clone(), aside));
        }
    }
    let path2 = path.clone();
    let targets2: Vec<(String, PathBuf, bool)> = targets.clone();
    let extracted = tokio::task::spawn_blocking(move || extract(&path2, &targets2))
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = extracted {
        // Put everything back.
        for (_, dest, _) in &targets {
            let _ = fs::remove_dir_all(dest).or_else(|_| fs::remove_file(dest));
        }
        for (dest, aside) in &moved {
            let _ = fs::rename(aside, dest);
        }
        return Err(format!("restore failed, previous data put back: {e}"));
    }
    for (_, dest, _) in &targets {
        out.progress(&format!("restored {}", dest.display())).await;
    }
    // The app's files from the backup become a new revision.
    let mut revision = None;
    if let (Some(compose), env) = backup_files(cfg, app, file)? {
        let r = write_files(
            cfg,
            app,
            &compose,
            &env.unwrap_or_default(),
            &format!("restored from backup {file}"),
            "hearth",
        )?;
        revision = r.get("revision").cloned();
        out.progress("restored compose.yaml and .env").await;
    }
    Ok(json!({
        "revision": revision,
        "restored": targets.iter().map(|(_, d, _)| d.display().to_string()).collect::<Vec<_>>(),
        "previous_data": moved.iter().map(|(_, a)| a.display().to_string()).collect::<Vec<_>>(),
    }))
}

fn extract(archive: &Path, targets: &[(String, PathBuf, bool)]) -> Result<(), String> {
    let f = fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(GzDecoder::new(f));
    ar.set_preserve_permissions(true);
    ar.set_preserve_ownerships(true);
    ar.set_unpack_xattrs(false);
    for (_, dest, is_dir) in targets {
        if *is_dir {
            fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        } else if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path().map_err(|e| e.to_string())?.into_owned();
        for (prefix, dest, is_dir) in targets {
            let Ok(rel) = p.strip_prefix(prefix) else { continue };
            if !*is_dir {
                if rel.as_os_str().is_empty() {
                    entry
                        .unpack(dest)
                        .map_err(|e| format!("{}: {e}", dest.display()))?;
                }
            } else if rel.as_os_str().is_empty() {
                // The directory itself: already created.
            } else {
                if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
                    return Err(format!("unsafe path in archive: {}", p.display()));
                }
                // Refuse to write through a symlink created by an earlier entry (it could point
                // outside `dest`).
                let staged = dest.join(rel);
                let mut ancestor = dest.clone();
                for c in rel
                    .parent()
                    .map(|p| p.components().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    ancestor.push(c);
                    if fs::symlink_metadata(&ancestor)
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false)
                    {
                        return Err(format!("unsafe symlink in archive at {}", ancestor.display()));
                    }
                }
                if let Some(parent) = staged.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                entry
                    .unpack(&staged)
                    .map_err(|e| format!("{}: {e}", staged.display()))?;
            }
            break;
        }
    }
    Ok(())
}

/// The `compose.yaml` and `.env` stored in a backup.
fn backup_files(
    cfg: &HelperConfig,
    app: &str,
    file: &str,
) -> Result<(Option<String>, Option<String>), String> {
    check_app(app)?;
    if !proto::is_valid_backup_name(file) {
        return Err("invalid backup name".into());
    }
    let f = fs::File::open(backups_dir(cfg, app).join(file)).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(GzDecoder::new(std::io::BufReader::new(f)));
    let (mut compose, mut env) = (None, None);
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path().map_err(|e| e.to_string())?.into_owned();
        let mut s = String::new();
        if p == Path::new("files/compose.yaml") {
            std::io::Read::read_to_string(&mut entry, &mut s).map_err(|e| e.to_string())?;
            compose = Some(s);
        } else if p == Path::new("files/.env") {
            std::io::Read::read_to_string(&mut entry, &mut s).map_err(|e| e.to_string())?;
            env = Some(s);
        }
    }
    Ok((compose, env))
}

// ----- migration support -----

/// Archives everything CasaOS needs to be restored as it was, plus the settings of every
/// container and image.
pub async fn backup_casaos(cfg: &HelperConfig, out: &mut Out<'_>) -> Result<Value, String> {
    let dir = cfg.backups_dir.join("casaos");
    ensure_dir(&cfg.backups_dir, 0o700, None)?;
    ensure_dir(&dir, 0o700, None)?;
    // Container and image settings as JSON, so a recreate can always be compared or undone.
    let list = docker(cfg, http::Method::GET, "/containers/json?all=true").await?;
    let list: Vec<Value> = serde_json::from_slice(&list.body).unwrap_or_default();
    let mut inspected = Vec::new();
    for c in &list {
        if let Some(id) = c.get("Id").and_then(Value::as_str)
            && let Ok(r) = docker(cfg, http::Method::GET, &format!("/containers/{id}/json")).await
        {
            inspected.push(serde_json::from_slice::<Value>(&r.body).unwrap_or(Value::Null));
        }
    }
    let images = list_images(cfg).await.unwrap_or(Value::Null);
    let containers_json = serde_json::to_vec_pretty(&inspected).map_err(|e| e.to_string())?;
    let images_json = serde_json::to_vec_pretty(&images).map_err(|e| e.to_string())?;
    out.progress(&format!("saved the settings of {} containers", inspected.len()))
        .await;

    let mut paths: Vec<PathBuf> = vec!["/var/lib/casaos".into(), "/etc/casaos".into()];
    for unit_dir in [
        "/etc/systemd/system",
        "/usr/lib/systemd/system",
        "/lib/systemd/system",
    ] {
        if let Ok(rd) = fs::read_dir(unit_dir) {
            for e in rd.filter_map(|e| e.ok()) {
                let name = e.file_name().to_string_lossy().into_owned();
                // /lib is often a link to /usr/lib: archive each real file once.
                let real = fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
                if name.starts_with("casaos")
                    && name.ends_with(".service")
                    && !paths
                        .iter()
                        .any(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == real)
                {
                    paths.push(e.path());
                }
            }
        }
    }
    for p in &paths {
        out.progress(&format!("archiving {}", p.display())).await;
    }
    let name = format!("casaos-{}.tar.gz", timestamp());
    let path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.partial"));
    let tmp2 = tmp.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = fs::File::create(&tmp2).map_err(|e| e.to_string())?;
        fs::set_permissions(&tmp2, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::default()));
        tar.follow_symlinks(false);
        for (name, data) in [
            ("containers.json", &containers_json),
            ("images.json", &images_json),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o600);
            h.set_cksum();
            tar.append_data(&mut h, name, &data[..])
                .map_err(|e| e.to_string())?;
        }
        for p in &paths {
            let rel = format!("files{}", p.display());
            if p.is_dir() {
                tar.append_dir_all(&rel, p)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
            } else if p.exists() {
                tar.append_path_with_name(p, &rel)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        let file = tar
            .into_inner()
            .map_err(|e| e.to_string())?
            .finish()
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("CasaOS backup failed: {e}"));
    }
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    out.progress(&format!("wrote {} ({} KiB)", path.display(), size / 1024))
        .await;
    Ok(json!(BackupInfo {
        file: path.display().to_string(),
        size_bytes: size,
        created_at: now(),
    }))
}

/// `pg_dumpall` inside a Postgres container, streamed (demultiplexed) into a gzip file.
pub async fn dump_postgres(
    cfg: &HelperConfig,
    app: &str,
    service: &str,
    out: &mut Out<'_>,
) -> Result<Value, String> {
    if !proto::is_valid_app_name(app) || !proto::is_valid_service_name(service) {
        return Err("invalid app or service name".into());
    }
    let list = docker(cfg, http::Method::GET, "/containers/json").await?;
    let list: Vec<Value> = serde_json::from_slice(&list.body).unwrap_or_default();
    let container = list
        .iter()
        .find(|c| {
            c.pointer("/Labels/com.docker.compose.project")
                .and_then(Value::as_str)
                == Some(app)
                && c.pointer("/Labels/com.docker.compose.service")
                    .and_then(Value::as_str)
                    == Some(service)
        })
        .ok_or_else(|| format!("{app}/{service} has no running container"))?;
    let id = container
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let image = container.get("Image").and_then(Value::as_str).unwrap_or_default();
    if !(image.contains("postgres") || image.contains("pgvecto") || image.contains("vectorchord")) {
        return Err(format!("{app}/{service} ({image}) doesn't look like Postgres"));
    }
    let info = docker(cfg, http::Method::GET, &format!("/containers/{id}/json")).await?;
    let info: Value = serde_json::from_slice(&info.body).unwrap_or(Value::Null);
    let user = info
        .pointer("/Config/Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|e| e.strip_prefix("POSTGRES_USER="))
        .unwrap_or("postgres")
        .to_string();
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("unexpected POSTGRES_USER".into());
    }
    let exec = json!({
        "AttachStdout": true,
        "AttachStderr": true,
        "Cmd": ["pg_dumpall", "-U", user, "--clean", "--if-exists"],
    });
    let resp = unixhttp::request(
        &cfg.docker_socket,
        http::Method::POST,
        &format!("/containers/{id}/exec"),
        &[],
        Some(serde_json::to_vec(&exec).unwrap_or_default()),
        1024 * 1024,
        Duration::from_secs(30),
    )
    .await
    .map_err(|e| e.to_string())?;
    let exec_id = serde_json::from_slice::<Value>(&resp.body)
        .ok()
        .and_then(|v| v.get("Id").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| format!("docker refused the exec: {}", String::from_utf8_lossy(&resp.body)))?;

    let dir = backups_dir(cfg, app);
    ensure_dir(&cfg.backups_dir, 0o700, None)?;
    ensure_dir(&dir, 0o700, None)?;
    let name = format!("{app}-{service}-db-{}.sql.gz", timestamp());
    let path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.partial"));
    out.progress(&format!(
        "dumping the {app}/{service} database (pg_dumpall as {user})"
    ))
    .await;
    let file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let mut gz = GzEncoder::new(file, Compression::default());
    let mut stderr = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut written: u64 = 0;
    let status = unixhttp::request_streaming(
        &cfg.docker_socket,
        http::Method::POST,
        &format!("/exec/{exec_id}/start"),
        Some(br#"{"Detach":false,"Tty":false}"#.to_vec()),
        |chunk| {
            pending.extend_from_slice(chunk);
            // Demultiplex complete frames: [stream, 0, 0, 0, size(4, BE)] payload.
            loop {
                if pending.len() < 8 {
                    break;
                }
                let size = u32::from_be_bytes([pending[4], pending[5], pending[6], pending[7]]) as usize;
                if pending.len() < 8 + size {
                    break;
                }
                let stream = pending[0];
                let payload: Vec<u8> = pending[8..8 + size].to_vec();
                pending.drain(..8 + size);
                if stream == 2 {
                    if stderr.len() < 64 * 1024 {
                        stderr.extend_from_slice(&payload);
                    }
                } else {
                    gz.write_all(&payload)?;
                    written += payload.len() as u64;
                }
            }
            Ok(())
        },
    )
    .await
    .map_err(|e| format!("dump failed: {e}"))?;
    let file = gz.finish().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let result = docker(cfg, http::Method::GET, &format!("/exec/{exec_id}/json")).await?;
    let code = serde_json::from_slice::<Value>(&result.body)
        .ok()
        .and_then(|v| v.get("ExitCode").and_then(Value::as_i64))
        .unwrap_or(-1);
    if status >= 300 || code != 0 || written == 0 {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "pg_dumpall failed (exit {code}): {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    out.progress(&format!(
        "wrote {} ({} MiB of SQL, {} KiB compressed)",
        path.display(),
        written / (1024 * 1024),
        size / 1024
    ))
    .await;
    Ok(json!(BackupInfo {
        file: path.display().to_string(),
        size_bytes: size,
        created_at: now(),
    }))
}

/// Asks CasaOS to install an app from its own compose file (used to hand an app back; CasaOS
/// sets its own compose labels only when it creates the app itself).
pub async fn casaos_apply(cfg: &HelperConfig, app: &str, out: &mut Out<'_>) -> Result<Value, String> {
    if !proto::is_valid_app_name(app) {
        return Err("invalid app name".into());
    }
    let file = cfg.casaos_apps_dir.join(app).join("docker-compose.yml");
    if !file.is_file() {
        return Err(format!("CasaOS has no file for {app} ({})", file.display()));
    }
    let bin = ["/usr/bin/casaos-cli", "/usr/local/bin/casaos-cli"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .ok_or("casaos-cli isn't installed")?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["app-management", "install", "-f"])
        .arg(&file)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/root")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    out.progress(&format!(
        "casaos-cli app-management install -f {}",
        file.display()
    ))
    .await;
    let fut = run_streaming(cmd, out);
    tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .map_err(|_| "casaos-cli didn't finish within 2 minutes".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_deletion_is_limited_to_app_roots() {
        let root = Path::new("/DATA/AppData/myapp");
        assert!(is_under(Path::new("/DATA/AppData/myapp/config"), root));
        assert!(!is_under(Path::new("/DATA/AppData/myapp/../other"), root));
        assert!(!is_under(Path::new("/DATA/AppData/myapp2"), root));
        assert!(!is_under(Path::new("/DATA/Media"), root));
    }

    #[test]
    fn system_paths() {
        assert!(is_system_path("/etc/localtime"));
        assert!(is_system_path("/run/dbus"));
        assert!(is_system_path("/"));
        assert!(!is_system_path("/DATA/AppData/x"));
        assert!(!is_system_path("/etcetera"));
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_782), (2026, 11, 25));
    }

    #[test]
    fn repo_and_tag_validation() {
        assert!(is_valid_repo("hearth-rollback/jellyfin-jellyfin"));
        assert!(is_valid_repo("linuxserver/jellyfin"));
        assert!(is_valid_repo("ghcr.io/immich-app/immich-server"));
        assert!(!is_valid_repo("Upper/Case"));
        assert!(!is_valid_repo("a/../b"));
        assert!(is_valid_tag("20261125-101500"));
        assert!(!is_valid_tag("a/b"));
    }
}
