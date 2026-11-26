//! System updates in the daemon (M4): the cached update list, the API, a daily check, and the
//! optional security-update schedule (off by default). Hearth never reboots: it only reports
//! that a reboot is needed.

use super::State;
use super::api::ApiError;
use super::auth::Principal;
use super::db::now;
use super::jobs::{JobError, JobHandle};
use axum::extract::{Query, State as AxState};
use axum::routing::{get, post};
use axum::{Json, Router};
use hearth_core::Error;
use hearth_core::model::{
    JobRef, PackageDetail, PackageEntry, PackagePlan, SystemInfo, SystemUpdate, SystemUpdates, UpgradePlan,
    UpgradeResult, scopes,
};
use hearth_core::output::Envelope;
use hearth_core::proto::{PackageAction, Request};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

type ApiResult<T> = Result<Json<Envelope<T>>, ApiError>;

/// How often the package lists are refreshed by themselves.
const CHECK_INTERVAL: i64 = 24 * 3600;
/// First automatic check after the daemon starts.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(300);
/// The job lock shared by all system jobs (they never overlap).
const LOCK: &str = "system";

/// The last update list, kept in memory and in `<state>/system-updates.json`.
pub struct SystemState {
    updates: Mutex<SystemUpdates>,
    file: PathBuf,
}

impl SystemState {
    pub fn load(state_dir: &std::path::Path) -> Self {
        let file = state_dir.join("system-updates.json");
        let updates = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        SystemState {
            updates: Mutex::new(updates),
            file,
        }
    }

    pub fn get(&self) -> SystemUpdates {
        self.updates.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn store(&self, u: SystemUpdates) {
        if let Ok(b) = serde_json::to_vec_pretty(&u) {
            let tmp = self.file.with_extension("json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, &self.file);
            }
        }
        *self.updates.lock().unwrap_or_else(|p| p.into_inner()) = u;
    }
}

pub fn routes() -> Router<State> {
    Router::new()
        .route("/v1/system", get(get_system))
        .route("/v1/system/check", post(check))
        .route("/v1/system/upgrade", post(upgrade))
        .route("/v1/packages/search", get(package_search))
        .route("/v1/packages/info", get(package_info))
        .route("/v1/packages/install", post(package_install))
        .route("/v1/packages/remove", post(package_remove))
}

fn ok<T: serde::Serialize>(data: T) -> ApiResult<T> {
    Ok(Json(Envelope::success(data)))
}

fn actor(p: &Principal) -> String {
    p.actor()
}

async fn get_system(AxState(st): AxState<State>, p: Principal) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    let info: SystemInfo =
        serde_json::from_value(st.helper.call(&Request::SystemInfo).await.map_err(Error::Other)?)
            .map_err(|e| Error::Other(e.to_string()))?;
    let mut updates = st.system.get();
    // The reboot status is always fresh; the list is from the last check.
    updates.reboot = info.reboot.clone();
    ok(json!({ "info": info, "updates": updates }))
}

/// Refreshes the package lists and the update list (read scope: it changes nothing installed).
async fn check(AxState(st): AxState<State>, p: Principal) -> ApiResult<JobRef> {
    p.require(scopes::READ)?;
    let st2 = st.clone();
    let id = st
        .jobs
        .start(&st, "system-check", LOCK, &actor(&p), move |job| async move {
            run_check(&st2, &job, true).await.map(|u| json!(u))
        });
    ok(JobRef { job: id })
}

pub async fn run_check(st: &State, job: &JobHandle, refresh: bool) -> Result<SystemUpdates, JobError> {
    let j = job.clone();
    let v = st
        .helper
        .call_stream(&Request::SystemUpdates { refresh }, move |l| j.log(l))
        .await?;
    let u: SystemUpdates = serde_json::from_value(v).map_err(|e| e.to_string())?;
    for e in &u.errors {
        job.log(format!("warning: {e}"));
    }
    let security = u.updates.iter().filter(|x| x.security).count();
    job.log(format!(
        "{} update(s) available, {security} of them security update(s)",
        u.updates.len()
    ));
    if u.reboot.needed {
        job.log(format!("a reboot is needed: {}", u.reboot.reasons.join("; ")));
    }
    st.system.store(u.clone());
    Ok(u)
}

#[derive(Debug, Default, Deserialize)]
pub struct UpgradeBody {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub security_only: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// Which updates an upgrade request means, from the cached list.
pub fn plan(cached: &SystemUpdates, b: &UpgradeBody, now: i64) -> Result<UpgradePlan, Error> {
    if b.ids.is_empty() && !b.all && !b.security_only {
        return Err(Error::Usage(
            "say which updates to install: their ids or names, --security, or --all".into(),
        ));
    }
    if cached.checked_at == 0 {
        return Err(Error::Usage(
            "there's no update list yet: run `hearth system updates --check` first".into(),
        ));
    }
    let mut notes = Vec::new();
    let mut selected: Vec<SystemUpdate> = if b.ids.is_empty() {
        cached
            .updates
            .iter()
            .filter(|u| !b.security_only || u.security)
            .cloned()
            .collect()
    } else {
        let mut out: Vec<SystemUpdate> = Vec::new();
        for want in &b.ids {
            let matches: Vec<&SystemUpdate> = cached
                .updates
                .iter()
                .filter(|u| &u.id == want || &u.name == want)
                .filter(|u| !b.security_only || u.security)
                .collect();
            if matches.is_empty() {
                return Err(Error::NotFound {
                    what: "system update",
                    name: want.clone(),
                });
            }
            for m in matches {
                if !out.iter().any(|u| u.id == m.id) {
                    out.push(m.clone());
                }
            }
        }
        out
    };
    if b.security_only
        && cached
            .updates
            .iter()
            .any(|u| u.backend == "pacman" || u.backend == "flatpak")
    {
        notes.push("Arch and Flatpak have no security channel, so their updates aren't included".into());
    }
    let pacman_all: Vec<&SystemUpdate> = cached.updates.iter().filter(|u| u.backend == "pacman").collect();
    let pacman_selected = selected.iter().filter(|u| u.backend == "pacman").count();
    if pacman_selected > 0 && pacman_selected < pacman_all.len() {
        notes.push(format!(
            "Arch can't upgrade only some packages: all {} pacman updates are installed together",
            pacman_all.len()
        ));
        for u in pacman_all {
            if !selected.iter().any(|s| s.id == u.id) {
                selected.push(u.clone());
            }
        }
    }
    let age = now - cached.checked_at;
    if age > CHECK_INTERVAL + 3600 {
        notes.push(format!(
            "the update list is {} hours old: `hearth system updates --check` refreshes it",
            age / 3600
        ));
    }
    if selected.iter().any(|u| u.name.starts_with("linux")) {
        notes.push("a kernel is updated: a reboot will be needed (Hearth never reboots by itself)".into());
    }
    Ok(UpgradePlan {
        updates: selected,
        notes,
        checked_at: cached.checked_at,
    })
}

async fn upgrade(AxState(st): AxState<State>, p: Principal, Json(b): Json<UpgradeBody>) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    let plan = plan(&st.system.get(), &b, now())?;
    if b.dry_run {
        return ok(json!(plan));
    }
    p.require(scopes::SYSTEM_WRITE).map_err(|_| {
        ApiError(Error::Unauthorized(
            "installing system updates needs the 'system:write' scope: create a token with `sudo hearth token create --name NAME --scope read --scope system:write`".into(),
        ))
    })?;
    if plan.updates.is_empty() {
        return Err(Error::Usage("nothing to install: no update matches".into()).into());
    }
    let ids: Vec<String> = plan.updates.iter().map(|u| u.id.clone()).collect();
    let st2 = st.clone();
    let id = st
        .jobs
        .start(&st, "system-upgrade", LOCK, &actor(&p), move |job| async move {
            run_upgrade(&st2, &job, ids).await
        });
    ok(json!(JobRef { job: id }))
}

pub async fn run_upgrade(st: &State, job: &JobHandle, ids: Vec<String>) -> Result<Value, JobError> {
    job.log(format!(
        "installing {} update(s); Hearth never reboots by itself",
        ids.len()
    ));
    let j = job.clone();
    let v = st
        .helper
        .call_stream(
            &Request::SystemUpgrade {
                ids,
                security_only: false,
            },
            move |l| j.log(l),
        )
        .await;
    // Whatever happened, refresh the cached list from what's installed now.
    job.log("reading the update list again");
    let after = run_check(st, job, false).await;
    let r: UpgradeResult = serde_json::from_value(v?).map_err(|e| e.to_string())?;
    if let Ok(u) = &after
        && u.reboot.needed
    {
        job.log("REBOOT NEEDED (Hearth won't reboot by itself)");
    }
    if !r.failed.is_empty() {
        return Err(JobError::failed(format!(
            "{} installed, {} failed: {}",
            r.installed.len(),
            r.failed.len(),
            r.errors.join("; ")
        )));
    }
    Ok(json!(r))
}

// ----- packages (M4b) -----

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    backend: Option<String>,
}

fn backend_of(b: Option<&str>, default: &str) -> Result<String, Error> {
    let b = b.unwrap_or(default);
    if ["native", "flatpak", "all"].contains(&b) {
        Ok(b.to_string())
    } else {
        Err(Error::Usage(format!(
            "unknown backend '{b}' (native, flatpak or all)"
        )))
    }
}

async fn package_search(
    AxState(st): AxState<State>,
    p: Principal,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Vec<PackageEntry>> {
    p.require(scopes::READ)?;
    let terms: Vec<String> = q.q.split_whitespace().map(str::to_string).collect();
    if terms.is_empty() {
        return Err(Error::Usage("give something to search for".into()).into());
    }
    let backend = backend_of(q.backend.as_deref(), "all")?;
    let v = st
        .helper
        .call(&Request::PackageSearch { terms, backend })
        .await
        .map_err(Error::Other)?;
    ok(serde_json::from_value(v).map_err(|e| Error::Other(e.to_string()))?)
}

#[derive(Debug, Deserialize)]
struct InfoQuery {
    name: String,
    #[serde(default)]
    backend: Option<String>,
}

async fn package_info(
    AxState(st): AxState<State>,
    p: Principal,
    Query(q): Query<InfoQuery>,
) -> ApiResult<PackageDetail> {
    p.require(scopes::READ)?;
    let backend = backend_of(q.backend.as_deref(), "native")?;
    let v = st
        .helper
        .call(&Request::PackageInfo {
            name: q.name.clone(),
            backend,
        })
        .await
        .map_err(|e| {
            if e.starts_with("no package") || e.starts_with("no Flatpak app") {
                Error::NotFound {
                    what: "package",
                    name: q.name.clone(),
                }
            } else {
                Error::Other(e)
            }
        })?;
    ok(serde_json::from_value(v).map_err(|e| Error::Other(e.to_string()))?)
}

#[derive(Debug, Default, Deserialize)]
struct ChangeBody {
    names: Vec<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    dry_run: bool,
    /// Removing packages the server needs to stay reachable (SSH, Docker, the kernel…) is
    /// refused unless this is set.
    #[serde(default)]
    allow_critical: bool,
}

/// Packages whose removal can make the server unreachable or unbootable, or break Hearth.
pub fn is_critical(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "openssh-server",
        "openssh",
        "ssh",
        "sudo",
        "systemd",
        "systemd-sysv",
        "dbus",
        "dbus-broker",
        "polkit",
        "packagekit",
        "PackageKit",
        "docker-ce",
        "docker",
        "docker.io",
        "moby-engine",
        "containerd.io",
        "containerd",
        "tailscale",
        "network-manager",
        "NetworkManager",
        "networkmanager",
        "iproute2",
        "ifupdown",
        "netplan.io",
        "systemd-networkd",
        "pacman",
        "apt",
        "dpkg",
        "dnf",
        "rpm",
        "zypper",
        "base",
        "glibc",
        "libc6",
        "bash",
        "coreutils",
        "linux",
    ];
    const PREFIX: &[&str] = &[
        "linux-image",
        "linux-lts",
        "linux-cachyos",
        "linux-zen",
        "kernel",
        "grub",
        "shim",
        "systemd-boot",
    ];
    EXACT.contains(&name) || PREFIX.iter().any(|p| name.starts_with(p))
}

/// Blocks a removal plan that takes critical packages with it, unless allowed.
pub fn guard(plan: &mut PackagePlan, allow_critical: bool) {
    let critical: Vec<String> = plan
        .remove
        .iter()
        .filter(|e| is_critical(&e.name))
        .map(|e| e.name.clone())
        .collect();
    if critical.is_empty() {
        return;
    }
    let msg = format!(
        "this would remove {}, which the server needs (to stay reachable, to boot, or to run Hearth)",
        critical.join(", ")
    );
    if allow_critical {
        plan.warnings.push(msg);
    } else {
        plan.errors
            .push(format!("{msg}; add --allow-critical if you really mean it"));
    }
}

async fn package_install(st: AxState<State>, p: Principal, b: Json<ChangeBody>) -> ApiResult<Value> {
    package_change(st, p, b, PackageAction::Install).await
}

async fn package_remove(st: AxState<State>, p: Principal, b: Json<ChangeBody>) -> ApiResult<Value> {
    package_change(st, p, b, PackageAction::Remove).await
}

async fn package_change(
    AxState(st): AxState<State>,
    p: Principal,
    Json(b): Json<ChangeBody>,
    action: PackageAction,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    let backend = backend_of(b.backend.as_deref(), "native")?;
    if backend == "all" {
        return Err(Error::Usage("choose one backend: native or flatpak".into()).into());
    }
    let v = st
        .helper
        .call(&Request::PackageChange {
            action,
            names: b.names.clone(),
            backend: backend.clone(),
            simulate: true,
        })
        .await
        .map_err(Error::Usage)?;
    let mut plan: PackagePlan = serde_json::from_value(v).map_err(|e| Error::Other(e.to_string()))?;
    guard(&mut plan, b.allow_critical);
    if b.dry_run {
        return ok(json!(plan));
    }
    p.require(scopes::SYSTEM_WRITE).map_err(|_| {
        ApiError(Error::Unauthorized(
            "installing or removing packages needs the 'system:write' scope: create a token with `sudo hearth token create --name NAME --scope read --scope system:write`".into(),
        ))
    })?;
    if !plan.errors.is_empty() {
        return Err(Error::Usage(plan.errors.join("; ")).into());
    }
    let st2 = st.clone();
    let names = b.names.clone();
    let kind = format!("package-{}", action.as_str());
    let id = st
        .jobs
        .start(&st, &kind, LOCK, &actor(&p), move |job| async move {
            job.log(format!("{} {}", action.as_str(), names.join(" ")));
            let j = job.clone();
            let v = st2
                .helper
                .call_stream(
                    &Request::PackageChange {
                        action,
                        names,
                        backend,
                        simulate: false,
                    },
                    move |l| j.log(l),
                )
                .await?;
            // Installed packages change which updates exist.
            let _ = run_check(&st2, &job, false).await;
            Ok(v)
        });
    ok(json!(JobRef { job: id }))
}

/// The `security_updates` setting: `off`, or `HH:MM` local time.
pub fn valid_schedule(v: &str) -> bool {
    if v == "off" {
        return true;
    }
    let Some((h, m)) = v.split_once(':') else {
        return false;
    };
    h.len() == 2
        && m.len() == 2
        && h.parse::<u8>().is_ok_and(|h| h < 24)
        && m.parse::<u8>().is_ok_and(|m| m < 60)
}

/// Daily list refresh, and (only if the person set a time) nightly security updates.
pub async fn scheduler(st: State) {
    tokio::time::sleep(FIRST_CHECK_DELAY).await;
    let mut last_check = 0i64;
    let mut last_security_day = String::new();
    loop {
        let t = now();
        if t - last_check >= CHECK_INTERVAL || st.system.get().checked_at == 0 {
            last_check = t;
            let st2 = st.clone();
            st.jobs
                .start(&st, "system-check", LOCK, "schedule", move |job| async move {
                    run_check(&st2, &job, true).await.map(|u| json!(u))
                });
        }
        let at = st.with_db(|db| db.setting("security_updates"));
        if at != "off"
            && let Ok(z) = jiff::Zoned::now().round(jiff::Unit::Minute)
        {
            let hm = format!("{:02}:{:02}", z.hour(), z.minute());
            let day = z.date().to_string();
            if hm == at && day != last_security_day {
                last_security_day = day;
                let st2 = st.clone();
                st.jobs
                    .start(&st, "system-upgrade", LOCK, "schedule", move |job| async move {
                        job.log("scheduled security updates");
                        let list = run_check(&st2, &job, true).await?;
                        let ids: Vec<String> = list
                            .updates
                            .iter()
                            .filter(|u| u.security)
                            .map(|u| u.id.clone())
                            .collect();
                        if ids.is_empty() {
                            job.log("no security updates");
                            return Ok(json!(UpgradeResult::default()));
                        }
                        run_upgrade(&st2, &job, ids).await
                    });
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(id: &str, backend: &str, security: bool) -> SystemUpdate {
        SystemUpdate {
            id: format!("{backend}:{id}"),
            backend: backend.into(),
            name: id.into(),
            arch: None,
            current_version: Some("1".into()),
            new_version: "2".into(),
            repo: None,
            security,
            severity: if security { "security" } else { "normal" }.into(),
            summary: String::new(),
        }
    }

    fn list(u: Vec<SystemUpdate>) -> SystemUpdates {
        SystemUpdates {
            checked_at: 1000,
            updates: u,
            ..Default::default()
        }
    }

    #[test]
    fn needs_a_selection_and_a_list() {
        let l = list(vec![upd("zstd", "packagekit", true)]);
        assert!(matches!(
            plan(&l, &UpgradeBody::default(), 1000),
            Err(Error::Usage(_))
        ));
        let empty = SystemUpdates::default();
        let all = UpgradeBody {
            all: true,
            ..Default::default()
        };
        assert!(matches!(plan(&empty, &all, 1000), Err(Error::Usage(_))));
    }

    #[test]
    fn security_only_and_names() {
        let l = list(vec![
            upd("zstd", "packagekit", true),
            upd("vim", "packagekit", false),
            upd("app/org.x/x86_64/stable", "flatpak", false),
        ]);
        let sec = UpgradeBody {
            security_only: true,
            ..Default::default()
        };
        let p = plan(&l, &sec, 1000).expect("plan");
        assert_eq!(p.updates.len(), 1);
        assert_eq!(p.updates[0].name, "zstd");
        assert!(p.notes.iter().any(|n| n.contains("no security channel")));
        let by_name = UpgradeBody {
            ids: vec!["vim".into()],
            ..Default::default()
        };
        assert_eq!(
            plan(&l, &by_name, 1000).expect("plan").updates[0].id,
            "packagekit:vim"
        );
        let unknown = UpgradeBody {
            ids: vec!["nope".into()],
            ..Default::default()
        };
        assert!(matches!(plan(&l, &unknown, 1000), Err(Error::NotFound { .. })));
    }

    #[test]
    fn arch_upgrades_are_all_or_nothing() {
        let l = list(vec![
            upd("mesa", "pacman", false),
            upd("linux-cachyos", "pacman", false),
        ]);
        let one = UpgradeBody {
            ids: vec!["mesa".into()],
            ..Default::default()
        };
        let p = plan(&l, &one, 1000).expect("plan");
        assert_eq!(p.updates.len(), 2);
        assert!(p.notes.iter().any(|n| n.contains("can't upgrade only some")));
        assert!(p.notes.iter().any(|n| n.contains("reboot")));
    }

    #[test]
    fn old_lists_get_a_note() {
        let l = list(vec![upd("zstd", "packagekit", true)]);
        let all = UpgradeBody {
            all: true,
            ..Default::default()
        };
        let p = plan(&l, &all, 1000 + 3 * 24 * 3600).expect("plan");
        assert!(p.notes.iter().any(|n| n.contains("hours old")));
    }

    #[test]
    fn critical_packages_are_guarded() {
        let e = |n: &str| PackageEntry {
            backend: "packagekit".into(),
            name: n.into(),
            version: "1".into(),
            arch: None,
            repo: None,
            installed: true,
            summary: String::new(),
        };
        let mut plan = PackagePlan {
            remove: vec![e("htop"), e("openssh-server")],
            ..Default::default()
        };
        guard(&mut plan, false);
        assert_eq!(plan.errors.len(), 1);
        assert!(plan.errors[0].contains("openssh-server"));
        let mut allowed = PackagePlan {
            remove: vec![e("linux-image-amd64")],
            ..Default::default()
        };
        guard(&mut allowed, true);
        assert!(allowed.errors.is_empty());
        assert_eq!(allowed.warnings.len(), 1);
        let mut harmless = PackagePlan {
            remove: vec![e("htop"), e("linuxlogo")],
            ..Default::default()
        };
        guard(&mut harmless, false);
        assert!(harmless.errors.is_empty() && harmless.warnings.is_empty());
        assert!(is_critical("linux-cachyos-lts"));
        assert!(is_critical("linux"));
        assert!(!is_critical("linuxlogo"));
    }

    #[test]
    fn schedules() {
        assert!(valid_schedule("off"));
        assert!(valid_schedule("04:17"));
        assert!(!valid_schedule("4:17"));
        assert!(!valid_schedule("24:00"));
        assert!(!valid_schedule("12:60"));
        assert!(!valid_schedule("on"));
    }
}
