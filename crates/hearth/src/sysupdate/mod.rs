//! System updates (M4): the distro's packages and Flatpak apps.
//!
//! Runs in the root helper. Backends:
//! - PackageKit over D-Bus on Debian/Ubuntu, Fedora, openSUSE and most other distros;
//! - the pacman tools on Arch and its derivatives (PackageKit is ignored there even if present);
//! - Flatpak, when installed.
//!
//! Installs have to write /usr and /etc, which the helper's sandbox forbids: PackageKit works in
//! its own daemon, and pacman/flatpak run as transient systemd units. Hearth never reboots;
//! [`reboot::status`] says whether a reboot is needed and why.

pub mod flatpak;
pub mod packagekit;
pub mod pacman;
pub mod reboot;
mod run;

use hearth_core::model::{
    BackendInfo, PackageDetail, PackageEntry, PackagePlan, PackageResult, SystemInfo, SystemUpdate,
    SystemUpdates, UpgradeResult,
};
use hearth_core::proto::{PackageAction, is_valid_package_name};
use std::collections::BTreeSet;

/// Progress lines go here.
pub type Tx = tokio::sync::mpsc::UnboundedSender<String>;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct OsRelease {
    pub id: String,
    pub id_like: Vec<String>,
    pub pretty_name: String,
}

impl OsRelease {
    pub fn is_arch(&self) -> bool {
        self.id == "arch" || self.id_like.iter().any(|l| l == "arch")
    }
}

pub fn os_release() -> OsRelease {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    parse_os_release(&text)
}

pub fn parse_os_release(text: &str) -> OsRelease {
    let mut o = OsRelease::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "ID" => o.id = v,
            "ID_LIKE" => o.id_like = v.split_whitespace().map(str::to_string).collect(),
            "PRETTY_NAME" => o.pretty_name = v,
            _ => {}
        }
    }
    o
}

/// The backends Hearth uses on this host, in order.
async fn backends(os: &OsRelease) -> Vec<BackendInfo> {
    let mut out = Vec::new();
    let pacman = if os.is_arch() { pacman::probe().await } else { None };
    match pacman {
        Some(p) => out.push(p),
        None => out.push(packagekit::probe().await.unwrap_or_else(|| BackendInfo {
            kind: "packagekit".into(),
            available: false,
            detail: String::new(),
            note: Some(packagekit_hint(os)),
        })),
    }
    if let Some(f) = flatpak::probe().await {
        out.push(f);
    }
    out
}

fn packagekit_hint(os: &OsRelease) -> String {
    let family = |x: &str| os.id == x || os.id_like.iter().any(|l| l == x);
    let cmd = if family("debian") || family("ubuntu") {
        "apt-get -y install packagekit"
    } else if family("fedora") || family("rhel") {
        "dnf -y install PackageKit"
    } else if family("suse") || os.id.starts_with("opensuse") {
        "zypper -n install PackageKit"
    } else {
        "install PackageKit with your package manager"
    };
    format!("PackageKit isn't installed: {cmd}")
}

pub async fn info() -> SystemInfo {
    let os = os_release();
    SystemInfo {
        distro_id: os.id.clone(),
        distro_name: os.pretty_name.clone(),
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        backends: backends(&os).await,
        reboot: reboot::status(),
    }
}

pub async fn updates(refresh: bool, tx: &Tx) -> SystemUpdates {
    let os = os_release();
    let mut out = SystemUpdates {
        checked_at: now(),
        ..Default::default()
    };
    for b in backends(&os).await {
        if !b.available {
            out.errors.push(format!(
                "{}: {}",
                b.kind,
                b.note.unwrap_or_else(|| "not available".into())
            ));
            continue;
        }
        let r = match b.kind.as_str() {
            "packagekit" => packagekit::updates(refresh, tx).await,
            "pacman" => pacman::updates(refresh, tx).await,
            "flatpak" => flatpak::updates(refresh, tx).await,
            _ => continue,
        };
        match r {
            Ok(mut u) => out.updates.append(&mut u),
            Err(e) => out.errors.push(format!("{}: {e}", b.kind)),
        }
    }
    out.reboot = reboot::status();
    out
}

/// Installs updates. `ids` come from the current list (see [`SystemUpdate::id`]); empty means
/// everything (or every security update with `security_only`).
pub async fn upgrade(ids: &[String], security_only: bool, tx: &Tx) -> Result<UpgradeResult, String> {
    let current = updates(false, tx).await;
    let selected: Vec<&SystemUpdate> = if ids.is_empty() {
        current
            .updates
            .iter()
            .filter(|u| !security_only || u.security)
            .collect()
    } else {
        let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        let found: Vec<&SystemUpdate> = current
            .updates
            .iter()
            .filter(|u| wanted.contains(u.id.as_str()))
            .collect();
        let missing: Vec<&str> = wanted
            .iter()
            .filter(|w| !found.iter().any(|u| u.id == **w))
            .copied()
            .collect();
        for m in &missing {
            let _ = tx.send(format!("skipping {m}: it's no longer in the update list"));
        }
        found
    };
    let mut result = UpgradeResult::default();
    if selected.is_empty() {
        let _ = tx.send("nothing to install".into());
        result.reboot = reboot::status();
        return Ok(result);
    }
    let of =
        |b: &str| -> Vec<&SystemUpdate> { selected.iter().copied().filter(|u| u.backend == b).collect() };

    let pk = of("packagekit");
    if !pk.is_empty() {
        let pk_ids: Vec<String> = pk
            .iter()
            .map(|u| u.id.trim_start_matches("packagekit:").to_string())
            .collect();
        match packagekit::upgrade(&pk_ids, tx).await {
            Ok((_, warnings)) => {
                result.installed.extend(pk.iter().map(|u| u.id.clone()));
                result.errors.extend(warnings);
            }
            Err(e) => {
                result.failed.extend(pk.iter().map(|u| u.id.clone()));
                result.errors.push(format!("packagekit: {e}"));
            }
        }
    }
    let pm = of("pacman");
    if !pm.is_empty() {
        let all = current.updates.iter().filter(|u| u.backend == "pacman").count();
        if pm.len() < all {
            let _ = tx.send(format!(
                "Arch can't upgrade only some packages: installing all {all} pacman updates"
            ));
        }
        match pacman::upgrade(tx).await {
            Ok(()) => result.installed.extend(
                current
                    .updates
                    .iter()
                    .filter(|u| u.backend == "pacman")
                    .map(|u| u.id.clone()),
            ),
            Err(e) => {
                result.failed.extend(pm.iter().map(|u| u.id.clone()));
                result.errors.push(format!("pacman: {e}"));
            }
        }
    }
    let fp = of("flatpak");
    if !fp.is_empty() {
        let refs: Vec<String> = fp
            .iter()
            .map(|u| u.id.trim_start_matches("flatpak:").to_string())
            .collect();
        match flatpak::upgrade(&refs, tx).await {
            Ok(()) => result.installed.extend(fp.iter().map(|u| u.id.clone())),
            Err(e) => {
                result.failed.extend(fp.iter().map(|u| u.id.clone()));
                result.errors.push(format!("flatpak: {e}"));
            }
        }
    }
    result.reboot = reboot::status();
    if result.reboot.needed {
        let _ = tx.send(format!(
            "a reboot is needed: {}",
            result.reboot.reasons.join("; ")
        ));
    }
    if result.installed.is_empty() && !result.failed.is_empty() {
        return Err(result.errors.join("; "));
    }
    Ok(result)
}

// ----- packages (M4b) -----

/// The distro's own package backend on this host: `packagekit` or `pacman`.
async fn native_backend() -> Result<String, String> {
    let os = os_release();
    let b = backends(&os)
        .await
        .into_iter()
        .find(|b| b.kind != "flatpak")
        .ok_or("no package backend found")?;
    if b.available {
        Ok(b.kind)
    } else {
        Err(b.note.unwrap_or_else(|| format!("{} isn't available", b.kind)))
    }
}

fn check_names(names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("no package names given".into());
    }
    match names.iter().find(|n| !is_valid_package_name(n)) {
        Some(bad) => Err(format!("'{bad}' isn't a valid package name")),
        None => Ok(()),
    }
}

pub async fn package_search(terms: &[String], backend: &str) -> Result<Vec<PackageEntry>, String> {
    check_names(terms)?;
    let mut out = Vec::new();
    if backend == "native" || backend == "all" {
        match native_backend().await?.as_str() {
            "pacman" => out.append(&mut pacman::search(terms).await?),
            _ => out.append(&mut packagekit::search(terms).await?),
        }
    }
    if (backend == "flatpak" || backend == "all") && flatpak::probe().await.is_some() {
        match flatpak::search(terms).await {
            Ok(mut f) => out.append(&mut f),
            Err(e) if backend == "flatpak" => return Err(e),
            Err(_) => {}
        }
    } else if backend == "flatpak" {
        return Err("Flatpak isn't installed".into());
    }
    rank_search(&mut out, terms);
    out.truncate(500);
    Ok(out)
}

/// Best matches first, so the cut at 500 keeps them: exact name, then names starting with the
/// search, then names containing every word, then the rest (matches in descriptions). Installed
/// packages win ties; otherwise the backend's order is kept.
pub fn rank_search(list: &mut [PackageEntry], terms: &[String]) {
    let query = terms.join(" ").to_lowercase();
    let words: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let score = |p: &PackageEntry| -> i64 {
        let n = p.name.to_lowercase();
        let len = n.len().min(100) as i64;
        let s = if n == query {
            1000
        } else if n.starts_with(&query) {
            500 - len
        } else if words.iter().all(|w| n.contains(w.as_str())) {
            300 - len
        } else if words.iter().any(|w| n.contains(w.as_str())) {
            100
        } else {
            0
        };
        s * 2 + i64::from(p.installed)
    };
    list.sort_by_cached_key(|p| std::cmp::Reverse(score(p)));
}

pub async fn package_info(name: &str, backend: &str) -> Result<PackageDetail, String> {
    check_names(std::slice::from_ref(&name.to_string()))?;
    if backend == "flatpak" {
        return flatpak::info(name).await;
    }
    match native_backend().await?.as_str() {
        "pacman" => pacman::info(name).await,
        _ => packagekit::info(name).await,
    }
}

/// With `simulate`, returns the plan. Otherwise makes the change and returns a
/// [`PackageResult`], or the errors.
pub async fn package_change(
    action: PackageAction,
    names: &[String],
    backend: &str,
    simulate: bool,
    tx: &Tx,
) -> Result<serde_json::Value, String> {
    check_names(names)?;
    let plan: PackagePlan = if backend == "flatpak" {
        if flatpak::probe().await.is_none() {
            return Err("Flatpak isn't installed".into());
        }
        flatpak::change(action, names, simulate, tx).await?
    } else {
        match native_backend().await?.as_str() {
            "pacman" => pacman::change(action, names, simulate, tx).await?,
            _ => packagekit::change(action, names, simulate, tx).await?,
        }
    };
    if simulate {
        return serde_json::to_value(plan).map_err(|e| e.to_string());
    }
    if !plan.errors.is_empty() {
        return Err(plan.errors.join("; "));
    }
    serde_json::to_value(PackageResult {
        action: action.as_str().into(),
        names: names.to_vec(),
        reboot: reboot::status(),
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_ranks_exact_names_first() {
        let e = |n: &str, installed: bool| PackageEntry {
            backend: "packagekit".into(),
            name: n.into(),
            version: "1".into(),
            arch: None,
            repo: None,
            installed,
            summary: String::new(),
        };
        let mut l = vec![
            e("acpica-tools", false),
            e("slurm", false),
            e("sl", false),
            e("busl", false),
            e("slack", true),
        ];
        rank_search(&mut l, &["sl".to_string()]);
        let names: Vec<&str> = l.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["sl", "slack", "slurm", "busl", "acpica-tools"]);
    }

    #[test]
    fn os_release_parses() {
        let o =
            parse_os_release("NAME=\"CachyOS Linux\"\nPRETTY_NAME=\"CachyOS\"\nID=cachyos\nID_LIKE=arch\n");
        assert!(o.is_arch());
        assert_eq!(o.pretty_name, "CachyOS");
        let d = parse_os_release("PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nID=debian\n");
        assert!(!d.is_arch());
        assert_eq!(
            packagekit_hint(&d),
            "PackageKit isn't installed: apt-get -y install packagekit"
        );
        let s = parse_os_release("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n");
        assert_eq!(
            packagekit_hint(&s),
            "PackageKit isn't installed: zypper -n install PackageKit"
        );
    }
}
