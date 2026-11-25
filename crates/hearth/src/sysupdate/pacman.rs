//! Arch Linux and derivatives (CachyOS, EndeavourOS, Manjaro): the pacman tools.
//!
//! Updates are listed with `checkupdates` (pacman-contrib), which syncs a temporary copy of the
//! package databases, so listing never leaves the system half-synced (no partial upgrades).
//! Installing is always a full `pacman -Syu`: Arch doesn't support upgrading only some packages.

use super::{Tx, run};
use hearth_core::model::{BackendInfo, SystemUpdate};
use std::path::Path;
use std::time::Duration;

const PACMAN: &str = "/usr/bin/pacman";
const CHECKUPDATES: &str = "/usr/bin/checkupdates";

pub async fn probe() -> Option<BackendInfo> {
    if !Path::new(PACMAN).exists() {
        return None;
    }
    let version = run::capture(PACMAN, &["--version"], Duration::from_secs(10))
        .await
        .ok()
        .and_then(|c| {
            c.stdout.lines().find_map(|l| {
                l.split_once("Pacman v")
                    .map(|(_, v)| v.split_whitespace().next().unwrap_or("").to_string())
            })
        })
        .unwrap_or_default();
    let has_checkupdates = Path::new(CHECKUPDATES).exists();
    Some(BackendInfo {
        kind: "pacman".into(),
        available: has_checkupdates,
        detail: if version.is_empty() {
            "pacman".into()
        } else {
            format!("pacman {version}")
        },
        note: (!has_checkupdates).then(|| "install pacman-contrib for update checks (checkupdates)".into()),
    })
}

/// Where checkupdates keeps its copy of the sync databases. As root (the helper) it's Hearth's
/// own folder, so it survives restarts; otherwise checkupdates' default under /tmp.
fn checkupdates_db() -> String {
    if is_root() {
        "/var/lib/hearth/checkupdates-db/".into()
    } else {
        let uid = proc_uid().unwrap_or(0);
        format!(
            "{}/checkup-db-{uid}/",
            std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())
        )
    }
}

fn proc_uid() -> Option<u32> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn is_root() -> bool {
    proc_uid() == Some(0)
}

/// `--nosync` on a database that was never synced compares against nothing and reports "no
/// updates", so it's only used once a sync has happened.
fn db_was_synced(db: &str) -> bool {
    std::fs::read_dir(Path::new(db).join("sync"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".db"))
}

pub async fn updates(refresh: bool, tx: &Tx) -> Result<Vec<SystemUpdate>, String> {
    if !Path::new(CHECKUPDATES).exists() {
        return Err("checkupdates is missing: install pacman-contrib".into());
    }
    let db = checkupdates_db();
    let env = [("CHECKUPDATES_DB", db.as_str())];
    let sync = refresh || !db_was_synced(&db);
    let c = if sync {
        let _ = tx.send("refreshing package lists (checkupdates)".into());
        run::capture_with(CHECKUPDATES, &["--nocolor"], &env, Duration::from_secs(600)).await?
    } else {
        run::capture_with(
            CHECKUPDATES,
            &["--nocolor", "--nosync"],
            &env,
            Duration::from_secs(120),
        )
        .await?
    };
    match c.code {
        0 | 2 => Ok(parse_checkupdates(&c.stdout)),
        _ => Err(format!("checkupdates failed: {}", c.stderr.trim())),
    }
}

/// `name old -> new` lines.
pub fn parse_checkupdates(out: &str) -> Vec<SystemUpdate> {
    out.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, old, arrow, new) = (w.next()?, w.next()?, w.next()?, w.next()?);
            (arrow == "->").then(|| SystemUpdate {
                id: format!("pacman:{name}"),
                backend: "pacman".into(),
                name: name.to_string(),
                arch: None,
                current_version: Some(old.to_string()),
                new_version: new.to_string(),
                repo: None,
                security: false,
                severity: "unknown".into(),
                summary: String::new(),
            })
        })
        .collect()
}

/// Full system upgrade.
pub async fn upgrade(tx: &Tx) -> Result<(), String> {
    let code = run::unconfined(
        "pacman",
        PACMAN,
        &["-Syu", "--noconfirm", "--noprogressbar"],
        tx,
        Duration::from_secs(3 * 3600),
    )
    .await?;
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "pacman -Syu failed (exit code {code}); see the log above"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkupdates_output_parses() {
        let out = "linux-cachyos 6.12.5-2 -> 6.12.6-1\nmesa 1:24.3.1-1 -> 1:24.3.2-1\n\nnot a line\n";
        let u = parse_checkupdates(out);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].id, "pacman:linux-cachyos");
        assert_eq!(u[0].current_version.as_deref(), Some("6.12.5-2"));
        assert_eq!(u[1].new_version, "1:24.3.2-1");
        assert!(!u[1].security);
    }
}
