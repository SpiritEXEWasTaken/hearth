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

use hearth_core::model::{BackendInfo, SystemInfo, SystemUpdate, SystemUpdates, UpgradeResult};
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

#[cfg(test)]
mod tests {
    use super::*;

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
