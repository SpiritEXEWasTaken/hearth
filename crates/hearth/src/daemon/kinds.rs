//! Service, native, link and package apps (M4c). Each is an `app.yaml` in the app's folder;
//! the helper does the systemd work.

use super::State;
use super::api::ApiError;
use super::auth::Principal;
use super::jobs::{JobError, JobHandle};
use hearth_core::Error;
use hearth_core::model::{
    AppDefinition, AppDetail, AppKind, AppState, AppSummary, LogLine, ManagedBy, UnitStatus, scopes,
};
use hearth_core::proto::{self, Logs, Request, UnitAction};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a reachability result is reused.
const REACH_CACHE_SECS: i64 = 30;
/// How long "is this package installed?" is cached.
const PACKAGE_CACHE_SECS: i64 = 300;

pub async fn definitions(st: &State) -> Vec<(String, AppDefinition)> {
    let list: Vec<(String, String)> = st
        .helper
        .call_as(&Request::ListAppDefinitions)
        .await
        .unwrap_or_default();
    list.into_iter()
        .filter_map(
            |(id, yaml)| match serde_yaml_ng::from_str::<AppDefinition>(&yaml) {
                Ok(d) => Some((id, d)),
                Err(e) => {
                    tracing::warn!(app = id, "bad app.yaml: {e}");
                    None
                }
            },
        )
        .collect()
}

pub async fn find(st: &State, id: &str) -> Option<AppDefinition> {
    definitions(st)
        .await
        .into_iter()
        .find(|(i, _)| i == id)
        .map(|(_, d)| d)
}

/// An app's unit: for a service, its owner when it's a user unit.
fn unit_of(id: &str, d: &AppDefinition) -> Option<(String, Option<String>)> {
    let unit = d.unit_name(id)?;
    let user = if d.kind == AppKind::Service {
        d.user.clone()
    } else {
        None
    };
    Some((unit, user))
}

async fn unit_status(st: &State, unit: &str, user: Option<&str>) -> Option<UnitStatus> {
    st.helper
        .call_as(&Request::UnitStatus {
            unit: unit.to_string(),
            user: user.map(str::to_string),
        })
        .await
        .ok()
}

/// Can the address be reached? HTTP(S): any answer below 500 counts (a login page is fine).
/// Anything else (`ssh://`, `rdp://`, `host:port`): a TCP connection.
pub async fn reachable(url: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, (i64, bool)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let now = super::db::now();
    if let Some((t, v)) = cache.lock().ok().and_then(|c| c.get(url).copied())
        && now - t < REACH_CACHE_SECS
    {
        return v;
    }
    let ok = check_reachable(url).await;
    if let Ok(mut c) = cache.lock() {
        c.insert(url.to_string(), (now, ok));
    }
    ok
}

async fn check_reachable(url: &str) -> bool {
    if url.starts_with("http://") || url.starts_with("https://") {
        let client = match reqwest::Client::builder()
            .timeout(CHECK_TIMEOUT)
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        return client
            .get(url)
            .send()
            .await
            .is_ok_and(|r| r.status().as_u16() < 500);
    }
    let Some((host, port)) = host_port(url) else {
        return false;
    };
    tokio::time::timeout(
        CHECK_TIMEOUT,
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// `scheme://host:port/…` or `host:port` → (host, port), with default ports for a few schemes.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("", url),
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let default = match scheme {
        "ssh" | "sftp" => Some(22),
        "rdp" => Some(3389),
        "vnc" => Some(5900),
        "smb" => Some(445),
        "ftp" => Some(21),
        _ => None,
    };
    if let Some(stripped) = authority.strip_prefix('[') {
        let (h, after) = stripped.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).or(default)?;
        return Some((h.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((authority.to_string(), default?)),
    }
}

fn package_cache() -> &'static Mutex<HashMap<String, (i64, bool)>> {
    static C: OnceLock<Mutex<HashMap<String, (i64, bool)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn package_installed(st: &State, name: &str, backend: &str) -> bool {
    let key = format!("{backend}:{name}");
    let now = super::db::now();
    if let Some((t, v)) = package_cache().lock().ok().and_then(|c| c.get(&key).copied())
        && now - t < PACKAGE_CACHE_SECS
    {
        return v;
    }
    let installed = st
        .helper
        .call(&Request::PackageInfo {
            name: name.to_string(),
            backend: backend.to_string(),
        })
        .await
        .ok()
        .and_then(|v| v.get("installed").and_then(Value::as_bool))
        .unwrap_or(false);
    if let Ok(mut c) = package_cache().lock() {
        c.insert(key, (now, installed));
    }
    installed
}

fn unit_state(u: &UnitStatus) -> AppState {
    match u.active_state.as_str() {
        "active" | "reloading" => AppState::Running,
        "inactive" => AppState::Stopped,
        _ => AppState::Unhealthy,
    }
}

pub async fn summary(st: &State, id: &str, d: &AppDefinition) -> AppDetail {
    let web_url = d.web_url(&st.hostname);
    let unit = match unit_of(id, d) {
        Some((u, user)) => unit_status(st, &u, user.as_deref()).await,
        None => None,
    };
    let (state, health) = match d.kind {
        AppKind::Link => {
            let ok = match &web_url {
                Some(u) => reachable(u).await,
                None => false,
            };
            (
                if ok {
                    AppState::Running
                } else {
                    AppState::Unhealthy
                },
                Some(if ok { "reachable" } else { "unreachable" }.to_string()),
            )
        }
        _ => {
            let mut package_note = None;
            let state = match (&unit, d.kind) {
                // A stopped native app's unit is unlinked until it starts again.
                (Some(u), AppKind::Native) if u.load_state == "not-found" => AppState::Stopped,
                (Some(u), _) if u.load_state == "not-found" => AppState::Unhealthy,
                (Some(u), _) => unit_state(u),
                (None, AppKind::Package) => {
                    let pkg = d.package.clone().unwrap_or_else(|| id.to_string());
                    let backend = d.package_backend.clone().unwrap_or_else(|| "native".into());
                    if package_installed(st, &pkg, &backend).await {
                        package_note = Some("installed");
                        AppState::Running
                    } else {
                        package_note = Some("not installed");
                        AppState::Stopped
                    }
                }
                (None, _) => AppState::Unhealthy,
            };
            // With a web page, a running app is also checked over HTTP.
            let health = match (&web_url, state) {
                (Some(u), AppState::Running) => Some(
                    if reachable(u).await {
                        "healthy"
                    } else {
                        "unhealthy"
                    }
                    .into(),
                ),
                _ => package_note.map(str::to_string),
            };
            (state, health)
        }
    };
    AppDetail {
        summary: AppSummary {
            id: id.to_string(),
            title: d.title.clone(),
            kind: d.kind,
            dashboard: d.on_dashboard(&st.hostname),
            managed_by: ManagedBy::Hearth,
            endpoint: None,
            memory_bytes: unit.as_ref().and_then(|u| u.memory_bytes),
            unit,
            read_only: false,
            state,
            health,
            web_url,
            https_port: None,
            https_subdomain: None,
            icon: d.icon.clone(),
            containers: Vec::new(),
            images: Vec::new(),
        },
        working_dir: d.workdir.clone(),
        config_files: vec![format!("/var/lib/hearth/apps/{id}/app.yaml")],
        external_variables: Vec::new(),
        missing_devices: Vec::new(),
        store_app_id: None,
    }
}

/// All of them, checked at the same time (an unreachable link takes a few seconds).
pub async fn list(st: &State) -> Vec<AppDetail> {
    let defs = definitions(st).await;
    futures_util::future::join_all(defs.iter().map(|(id, d)| summary(st, id, d))).await
}

// ----- adding and changing -----

/// What `hearth add …` sends.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct AddBody {
    pub kind: AppKind,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub def: AppDefinitionInput,
    /// Native apps start right away unless false.
    #[serde(default = "yes")]
    pub start: bool,
    #[serde(default)]
    pub dry_run: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct AppDefinitionInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub dashboard: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub exec: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env_file: Option<String>,
    #[serde(default)]
    pub memory_max: Option<String>,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub package_backend: Option<String>,
}

/// `smbd.service` → `smbd`; `Devmon@devmon.service` → `devmon-devmon`.
pub fn id_from(name: &str) -> String {
    let base = name
        .strip_suffix(".service")
        .unwrap_or(name)
        .rsplit('/')
        .next()
        .unwrap_or(name);
    let mut id: String = base
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    id = id.trim_matches('-').to_string();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    id.chars().take(63).collect()
}

fn ensure_service_suffix(u: &str) -> String {
    if u.contains('.') {
        u.to_string()
    } else {
        format!("{u}.service")
    }
}

fn abs_path(what: &str, p: &str) -> Result<(), Error> {
    if !p.starts_with('/') || p.contains('\n') || p.contains('\0') {
        return Err(Error::Usage(format!("{what} must be an absolute path")));
    }
    Ok(())
}

/// Checks the request and turns it into an app id and definition.
pub fn build(b: &AddBody, caller_user: Option<&str>) -> Result<(String, AppDefinition), Error> {
    let i = &b.def;
    let mut d = AppDefinition {
        kind: b.kind,
        title: i.title.clone().unwrap_or_default(),
        icon: i.icon.clone(),
        dashboard: i.dashboard,
        url: i.url.clone(),
        port: i.port,
        path: i.path.clone(),
        ..Default::default()
    };
    if let Some(u) = &d.url
        && (u.contains(char::is_whitespace) || u.len() > 2000)
    {
        return Err(Error::Usage("the address can't contain spaces".into()));
    }
    let id = match b.kind {
        AppKind::Compose => {
            return Err(Error::Usage(
                "compose apps are added with `hearth install`".into(),
            ));
        }
        AppKind::Link => {
            let url = d
                .url
                .clone()
                .ok_or_else(|| Error::Usage("a link needs --url".into()))?;
            if host_port(&url).is_none() && !url.starts_with("http") {
                return Err(Error::Usage(format!(
                    "'{url}' isn't an address Hearth can check (use http(s)://, ssh://, rdp:// or host:port)"
                )));
            }
            b.id.clone()
                .ok_or_else(|| Error::Usage("a link needs a name".into()))?
        }
        AppKind::Service => {
            let unit = ensure_service_suffix(
                i.unit
                    .as_deref()
                    .ok_or_else(|| Error::Usage("which unit?".into()))?,
            );
            if !proto::is_valid_unit_name(&unit) {
                return Err(Error::Usage(format!("'{unit}' isn't a valid unit name")));
            }
            if let Some(u) = &i.user
                && !proto::is_valid_user_name(u)
            {
                return Err(Error::Usage(format!("'{u}' isn't a valid user name")));
            }
            d.user = i.user.clone();
            let id = b.id.clone().unwrap_or_else(|| id_from(&unit));
            d.unit = Some(unit);
            id
        }
        AppKind::Native => {
            let exec = i
                .exec
                .clone()
                .ok_or_else(|| Error::Usage("a native app needs --exec".into()))?;
            if exec.contains('\n') || exec.contains('\0') {
                return Err(Error::Usage("--exec must be one line".into()));
            }
            abs_path(
                "the program in --exec",
                exec.split_whitespace().next().unwrap_or(""),
            )?;
            let user = i
                .user
                .clone()
                .or_else(|| caller_user.map(str::to_string))
                .ok_or_else(|| Error::Usage("say which user runs it: --user NAME".into()))?;
            if !proto::is_valid_user_name(&user) {
                return Err(Error::Usage(format!("'{user}' isn't a valid user name")));
            }
            for (what, p) in [("--workdir", &i.workdir), ("--env-file", &i.env_file)] {
                if let Some(p) = p {
                    abs_path(what, p)?;
                }
            }
            if let Some(m) = &i.memory_max
                && !valid_memory(m)
            {
                return Err(Error::Usage(format!(
                    "--memory-max '{m}' isn't a size like 512M or 2G"
                )));
            }
            let restart = i.restart.clone().unwrap_or_else(|| "on-failure".into());
            if !["on-failure", "always", "no"].contains(&restart.as_str()) {
                return Err(Error::Usage("--restart is on-failure, always or no".into()));
            }
            d.exec = Some(exec);
            d.user = Some(user);
            d.workdir = i.workdir.clone();
            d.env_file = i.env_file.clone();
            d.memory_max = i.memory_max.clone();
            d.restart = Some(restart);
            b.id.clone()
                .ok_or_else(|| Error::Usage("a native app needs a name".into()))?
        }
        AppKind::Package => {
            let pkg = i
                .package
                .clone()
                .ok_or_else(|| Error::Usage("which package?".into()))?;
            if !proto::is_valid_package_name(&pkg) {
                return Err(Error::Usage(format!("'{pkg}' isn't a valid package name")));
            }
            let backend = i.package_backend.clone().unwrap_or_else(|| "native".into());
            if !["native", "flatpak"].contains(&backend.as_str()) {
                return Err(Error::Usage("the package backend is native or flatpak".into()));
            }
            if let Some(u) = &i.unit {
                let u = ensure_service_suffix(u);
                if !proto::is_valid_unit_name(&u) {
                    return Err(Error::Usage(format!("'{u}' isn't a valid unit name")));
                }
                d.unit = Some(u);
            }
            let id =
                b.id.clone()
                    .unwrap_or_else(|| id_from(pkg.rsplit('.').next().unwrap_or(&pkg)));
            d.package = Some(pkg);
            d.package_backend = Some(backend);
            id
        }
    };
    if !proto::is_valid_hearth_app_name(&id) {
        return Err(Error::Usage(format!(
            "'{id}' can't be an app name (lowercase letters, digits, '-' and '_'); choose one with --id"
        )));
    }
    if d.title.is_empty() {
        d.title = id.clone();
    }
    Ok((id, d))
}

fn valid_memory(m: &str) -> bool {
    let digits = m.trim_end_matches(['K', 'M', 'G', 'T']);
    !digits.is_empty() && digits.len() + 1 >= m.len() && digits.chars().all(|c| c.is_ascii_digit())
}

/// systemd treats `%` as a specifier and `$` as a variable in ExecStart; Hearth passes the
/// command line through literally.
fn systemd_escape(s: &str) -> String {
    s.replace('%', "%%").replace('$', "$$")
}

/// The hardened unit Hearth writes for a native app.
pub fn native_unit(id: &str, d: &AppDefinition) -> String {
    let mut u = format!(
        "# Written by Hearth for the app '{id}'. Change it with Hearth, not by hand.\n\
         [Unit]\nDescription=Hearth app: {}\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nType=simple\nUser={}\n",
        d.title.replace('\n', " "),
        d.user.as_deref().unwrap_or("nobody")
    );
    if let Some(w) = &d.workdir {
        u.push_str(&format!("WorkingDirectory={w}\n"));
    }
    if let Some(e) = &d.env_file {
        u.push_str(&format!("EnvironmentFile=-{e}\n"));
    }
    u.push_str(&format!(
        "ExecStart={}\n",
        systemd_escape(d.exec.as_deref().unwrap_or("/bin/false"))
    ));
    u.push_str(&format!(
        "Restart={}\nRestartSec=5\n",
        d.restart.as_deref().unwrap_or("on-failure")
    ));
    if let Some(m) = &d.memory_max {
        u.push_str(&format!("MemoryMax={m}\n"));
    }
    u.push_str(
        "NoNewPrivileges=yes\nPrivateTmp=yes\nProtectSystem=full\nProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\nProtectControlGroups=yes\nRestrictSUIDSGID=yes\nLockPersonality=yes\n\n\
         [Install]\nWantedBy=multi-user.target\n",
    );
    u
}

pub fn can_add(p: &Principal, d: &AppDefinition) -> Result<(), ApiError> {
    p.require(scopes::APPS_WRITE).map_err(|_| {
        ApiError(Error::Unauthorized(
            "adding apps needs the 'apps:write' scope".into(),
        ))
    })?;
    if d.kind == AppKind::Native && d.user.as_deref() == Some("root") {
        p.require(scopes::SYSTEM_WRITE).map_err(|_| {
            ApiError(Error::Unauthorized(
                "a native app that runs as root needs the 'system:write' scope as well".into(),
            ))
        })?;
    }
    Ok(())
}

pub async fn add(
    st: &State,
    job: &JobHandle,
    id: &str,
    d: &AppDefinition,
    start: bool,
    actor: &str,
) -> Result<Value, JobError> {
    let yaml = serde_yaml_ng::to_string(d).map_err(|e| e.to_string())?;
    let rev = st
        .helper
        .call(&Request::WriteAppDefinition {
            app: id.to_string(),
            yaml,
            message: format!("added {} app", d.kind.as_str()),
            actor: actor.to_string(),
        })
        .await?;
    job.log(format!(
        "saved /var/lib/hearth/apps/{id}/app.yaml (revision {})",
        rev["revision"]
    ));
    if d.kind == AppKind::Native {
        let unit = native_unit(id, d);
        st.helper
            .call(&Request::InstallNativeUnit {
                app: id.to_string(),
                unit_text: unit,
            })
            .await?;
        job.log(format!("linked hearth-native-{id}.service into systemd"));
        if start {
            let s: UnitStatus = serde_json::from_value(
                st.helper
                    .call(&Request::UnitAction {
                        unit: format!("hearth-native-{id}.service"),
                        user: None,
                        action: UnitAction::Start,
                    })
                    .await?,
            )
            .map_err(|e| e.to_string())?;
            job.log(format!("started: {}/{}", s.active_state, s.sub_state));
            if s.active_state == "failed" {
                return Err(JobError::failed(format!(
                    "{id} failed to start; see `hearth app logs {id}` (it stays added, stopped)"
                )));
            }
        }
    }
    let detail = summary(st, id, d).await;
    Ok(json!(detail))
}

/// Start, stop or restart: for apps with a unit.
pub async fn set_running(
    st: &State,
    job: &JobHandle,
    id: &str,
    d: &AppDefinition,
    action: UnitAction,
) -> Result<Value, JobError> {
    let (unit, user) = unit_of(id, d).ok_or_else(|| {
        JobError::failed(format!(
            "{id} is a {} app without a systemd unit: there's nothing to start or stop",
            d.kind.as_str()
        ))
    })?;
    if d.kind == AppKind::Native && action != UnitAction::Stop {
        // Stopping unlinks the unit (systemctl disable); link it again, from the current
        // definition, before starting.
        st.helper
            .call(&Request::InstallNativeUnit {
                app: id.to_string(),
                unit_text: native_unit(id, d),
            })
            .await?;
    }
    job.log(format!(
        "{} {unit}{}",
        match action {
            UnitAction::Start => "enabling and starting",
            UnitAction::Stop => "stopping and disabling",
            UnitAction::Restart => "restarting",
        },
        user.as_ref().map(|u| format!(" (user {u})")).unwrap_or_default()
    ));
    let v = st
        .helper
        .call(&Request::UnitAction { unit, user, action })
        .await?;
    let s: UnitStatus = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    job.log(format!(
        "now {}/{} ({})",
        s.active_state, s.sub_state, s.unit_file_state
    ));
    if action != UnitAction::Stop && s.active_state == "failed" {
        return Err(JobError::failed(format!(
            "{id} failed; see `hearth app logs {id}`"
        )));
    }
    Ok(v)
}

pub async fn remove(st: &State, job: &JobHandle, id: &str, d: &AppDefinition) -> Result<Value, JobError> {
    if d.kind == AppKind::Native {
        st.helper
            .call(&Request::RemoveNativeUnit { app: id.to_string() })
            .await?;
        job.log(format!("stopped and unlinked hearth-native-{id}.service"));
    } else if d.kind == AppKind::Service {
        job.log("the service itself is left as it is (Hearth only stops showing it)");
    }
    st.helper
        .call(&Request::RemoveAppDir { app: id.to_string() })
        .await?;
    job.log("moved the app folder to /var/lib/hearth/removed/ (nothing deleted)");
    Ok(json!({ "removed": id }))
}

/// Changes title, icon, web address or dashboard placement.
pub async fn set_meta(
    st: &State,
    job: &JobHandle,
    id: &str,
    mut d: AppDefinition,
    i: &AppDefinitionInput,
    actor: &str,
) -> Result<Value, JobError> {
    if let Some(t) = &i.title {
        d.title = t.clone();
    }
    if let Some(v) = &i.icon {
        d.icon = Some(v.clone()).filter(|s| !s.is_empty());
    }
    if let Some(v) = i.dashboard {
        d.dashboard = Some(v);
    }
    if let Some(v) = &i.url {
        d.url = Some(v.clone()).filter(|s| !s.is_empty());
    }
    if let Some(v) = i.port {
        d.port = Some(v).filter(|p| *p != 0);
    }
    if let Some(v) = &i.path {
        d.path = Some(v.clone()).filter(|s| !s.is_empty());
    }
    if d.kind == AppKind::Link && d.url.is_none() {
        return Err(JobError::failed("a link needs an address"));
    }
    let yaml = serde_yaml_ng::to_string(&d).map_err(|e| e.to_string())?;
    let rev = st
        .helper
        .call(&Request::WriteAppDefinition {
            app: id.to_string(),
            yaml,
            message: "changed title/address/dashboard".into(),
            actor: actor.to_string(),
        })
        .await?;
    job.log(format!("saved revision {}", rev["revision"]));
    Ok(json!(summary(st, id, &d).await))
}

pub async fn logs(
    st: &State,
    id: &str,
    d: &AppDefinition,
    tail: u32,
    since: Option<i64>,
) -> Result<Vec<LogLine>, Error> {
    let (unit, user) = unit_of(id, d).ok_or_else(|| {
        Error::Usage(format!(
            "{id} is a {} app without a systemd unit: it has no logs",
            d.kind.as_str()
        ))
    })?;
    let v = st
        .helper
        .call(&Request::UnitLogs {
            unit: unit.clone(),
            user,
            tail,
            since,
        })
        .await
        .map_err(Error::Other)?;
    let logs: Logs = serde_json::from_value(v).map_err(|e| Error::Other(e.to_string()))?;
    Ok(logs
        .lines
        .into_iter()
        .map(|l| LogLine {
            service: unit.clone(),
            stream: l.stream,
            line: l.line,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(kind: AppKind) -> AddBody {
        AddBody {
            kind,
            start: true,
            ..Default::default()
        }
    }

    #[test]
    fn ids_from_units_and_packages() {
        assert_eq!(id_from("smbd.service"), "smbd");
        assert_eq!(id_from("devmon@devmon.service"), "devmon-devmon");
        assert_eq!(id_from("Claude-Remote-Control.service"), "claude-remote-control");
    }

    #[test]
    fn services_need_a_valid_unit() {
        let mut b = body(AppKind::Service);
        b.def.unit = Some("smbd".into());
        let (id, d) = build(&b, None).expect("build");
        assert_eq!(id, "smbd");
        assert_eq!(d.unit.as_deref(), Some("smbd.service"));
        assert!(!d.on_dashboard("jarvis"));
        b.def.unit = Some("bad name".into());
        assert!(build(&b, None).is_err());
        b.def.unit = Some("claude-remote-control".into());
        b.def.user = Some("jarvis".into());
        let (_, d) = build(&b, None).expect("build");
        assert_eq!(d.user.as_deref(), Some("jarvis"));
    }

    #[test]
    fn native_apps_are_checked_and_default_to_the_caller() {
        let mut b = body(AppKind::Native);
        b.id = Some("bot".into());
        b.def.exec = Some("relative/bot --x".into());
        assert!(build(&b, Some("jarvis")).is_err());
        b.def.exec = Some("/opt/bot/bot --port 8080".into());
        assert!(build(&b, None).is_err(), "no user and no caller");
        let (id, d) = build(&b, Some("jarvis")).expect("build");
        assert_eq!((id.as_str(), d.user.as_deref()), ("bot", Some("jarvis")));
        b.def.memory_max = Some("lots".into());
        assert!(build(&b, Some("jarvis")).is_err());
        b.def.memory_max = Some("512M".into());
        b.def.exec = Some("/opt/bot/bot --price 5% $HOME".into());
        let (_, d) = build(&b, Some("jarvis")).expect("build");
        let unit = native_unit("bot", &d);
        assert!(unit.contains("ExecStart=/opt/bot/bot --price 5%% $$HOME\n"));
        assert!(unit.contains("User=jarvis\n"));
        assert!(unit.contains("MemoryMax=512M\n"));
        assert!(unit.contains("NoNewPrivileges=yes"));
    }

    #[test]
    fn links_need_an_address() {
        let mut b = body(AppKind::Link);
        b.id = Some("router".into());
        assert!(build(&b, None).is_err());
        b.def.url = Some("http://192.168.1.1".into());
        let (_, d) = build(&b, None).expect("build");
        assert!(d.on_dashboard("jarvis"));
    }

    #[test]
    fn addresses_to_check() {
        assert_eq!(host_port("rdp://rog.local"), Some(("rog.local".into(), 3389)));
        assert_eq!(host_port("ssh://gobo@dell:2222/"), Some(("dell".into(), 2222)));
        assert_eq!(
            host_port("192.168.1.20:8009"),
            Some(("192.168.1.20".into(), 8009))
        );
        assert_eq!(host_port("[fe80::1]:22"), Some(("fe80::1".into(), 22)));
        assert_eq!(host_port("weird://host"), None);
    }

    #[test]
    fn memory_sizes() {
        assert!(valid_memory("512M"));
        assert!(valid_memory("2G"));
        assert!(valid_memory("1048576"));
        assert!(!valid_memory("M"));
        assert!(!valid_memory("5MB"));
    }
}
