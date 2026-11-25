//! Arch Linux and derivatives (CachyOS, EndeavourOS, Manjaro): the pacman tools.
//!
//! Updates are listed with `checkupdates` (pacman-contrib), which syncs a temporary copy of the
//! package databases, so listing never leaves the system half-synced (no partial upgrades).
//! Installing is always a full `pacman -Syu`: Arch doesn't support upgrading only some packages.

use super::{Tx, run};
use hearth_core::model::{BackendInfo, PackageDetail, PackageEntry, PackagePlan, SystemUpdate};
use hearth_core::proto::PackageAction;
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

// ----- packages (M4b) -----

pub async fn search(terms: &[String]) -> Result<Vec<PackageEntry>, String> {
    let mut args = vec!["-Ss", "--color", "never"];
    args.extend(terms.iter().map(String::as_str));
    let c = run::capture(PACMAN, &args, Duration::from_secs(60)).await?;
    // Exit code 1 with no output means nothing matched.
    if c.code != 0 && !c.stdout.trim().is_empty() {
        return Err(format!("pacman -Ss failed: {}", c.stderr.trim()));
    }
    Ok(parse_search(&c.stdout))
}

/// `repo/name version (groups) [installed]` followed by an indented description line.
pub fn parse_search(out: &str) -> Vec<PackageEntry> {
    let mut list: Vec<PackageEntry> = Vec::new();
    for line in out.lines() {
        if line.starts_with(char::is_whitespace) {
            if let Some(last) = list.last_mut()
                && last.summary.is_empty()
            {
                last.summary = line.trim().to_string();
            }
            continue;
        }
        let mut w = line.split_whitespace();
        let (Some(rn), Some(version)) = (w.next(), w.next()) else {
            continue;
        };
        let Some((repo, name)) = rn.split_once('/') else {
            continue;
        };
        list.push(PackageEntry {
            backend: "pacman".into(),
            name: name.to_string(),
            version: version.to_string(),
            arch: None,
            repo: Some(repo.to_string()),
            installed: line.contains("[installed"),
            summary: String::new(),
        });
    }
    list
}

/// `Key            : value` blocks from `pacman -Si` / `-Qi`.
pub fn parse_info(out: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once(" : ") {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        } else if line.starts_with(char::is_whitespace)
            && let Some(last) = fields.last_mut()
        {
            last.1.push(' ');
            last.1.push_str(line.trim());
        }
    }
    fields
}

/// "12.50 MiB" → bytes.
fn parse_size(s: &str) -> Option<u64> {
    let mut w = s.split_whitespace();
    let n: f64 = w.next()?.parse().ok()?;
    let mult = match w.next()? {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as u64)
}

pub async fn info(name: &str) -> Result<PackageDetail, String> {
    let si = run::capture(
        PACMAN,
        &["-Si", "--color", "never", name],
        Duration::from_secs(60),
    )
    .await?;
    let qi = run::capture(
        PACMAN,
        &["-Qi", "--color", "never", name],
        Duration::from_secs(60),
    )
    .await?;
    let (fields, from_sync) = if si.code == 0 {
        (parse_info(&si.stdout), true)
    } else if qi.code == 0 {
        (parse_info(&qi.stdout), false)
    } else {
        return Err(format!("no package called '{name}'"));
    };
    let get = |k: &str| {
        fields
            .iter()
            .find(|(f, _)| f == k)
            .map(|(_, v)| v.clone())
            .filter(|v| v != "None")
    };
    let installed_version = (qi.code == 0)
        .then(|| {
            parse_info(&qi.stdout)
                .into_iter()
                .find(|(k, _)| k == "Version")
                .map(|(_, v)| v)
        })
        .flatten();
    Ok(PackageDetail {
        entry: PackageEntry {
            backend: "pacman".into(),
            name: get("Name").unwrap_or_else(|| name.to_string()),
            version: get("Version").unwrap_or_default(),
            arch: get("Architecture"),
            repo: if from_sync {
                get("Repository")
            } else {
                Some("local".into())
            },
            installed: installed_version.is_some(),
            summary: get("Description").unwrap_or_default(),
        },
        installed_version,
        description: get("Description").unwrap_or_default(),
        url: get("URL"),
        license: get("Licenses"),
        size_bytes: get("Download Size")
            .or_else(|| get("Installed Size"))
            .and_then(|s| parse_size(&s)),
    })
}

/// `pacman -S/-R --print` lists the targets without changing anything; the real change runs
/// as a transient unit. Installs use the current sync databases (no `-y`, so no partial
/// upgrade); if a package is gone from the mirrors, run a system upgrade first.
pub async fn change(
    action: PackageAction,
    names: &[String],
    simulate: bool,
    tx: &Tx,
) -> Result<PackagePlan, String> {
    let mut plan = PackagePlan {
        action: action.as_str().into(),
        backend: "pacman".into(),
        names: names.to_vec(),
        ..Default::default()
    };
    let op = match action {
        PackageAction::Install => vec!["-S", "--needed"],
        PackageAction::Remove => vec!["-R"],
    };
    let mut args = op.clone();
    args.extend(["--print", "--print-format", "%n %v", "--color", "never"]);
    args.extend(names.iter().map(String::as_str));
    let c = run::capture(PACMAN, &args, Duration::from_secs(120)).await?;
    if c.code != 0 {
        let msg = c
            .stderr
            .lines()
            .chain(c.stdout.lines())
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>();
        plan.errors.push(format!("pacman: {}", msg.join("; ")));
        return Ok(plan);
    }
    let targets: Vec<PackageEntry> = c
        .stdout
        .lines()
        .filter_map(|l| {
            let (n, v) = l.trim().split_once(' ')?;
            Some(PackageEntry {
                backend: "pacman".into(),
                name: n.to_string(),
                version: v.to_string(),
                arch: None,
                repo: None,
                installed: action == PackageAction::Remove,
                summary: String::new(),
            })
        })
        .collect();
    match action {
        PackageAction::Install => plan.install = targets,
        PackageAction::Remove => plan.remove = targets,
    }
    if simulate {
        if action == PackageAction::Install && plan.install.is_empty() {
            plan.warnings.push("already installed: nothing to do".into());
        }
        return Ok(plan);
    }
    let mut args = op;
    args.extend(["--noconfirm", "--noprogressbar"]);
    args.extend(names.iter().map(String::as_str));
    let code = run::unconfined("pacman", PACMAN, &args, tx, Duration::from_secs(3600)).await?;
    if code != 0 {
        plan.errors
            .push(format!("pacman exited with code {code}; see the log above"));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_output_parses() {
        let out = "extra/vim 9.1.0866-1 [installed]\n    Vi Improved, a highly configurable editor\n\
                   core/bash 5.2.037-1 (base) [installed: 5.2.032-1]\n    The GNU Bourne Again shell\n\
                   extra/neovim 0.10.2-1\n    Fork of Vim\n";
        let r = parse_search(out);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].name.as_str(), r[0].installed), ("vim", true));
        assert_eq!(r[0].summary, "Vi Improved, a highly configurable editor");
        assert!(r[1].installed);
        assert_eq!(r[1].repo.as_deref(), Some("core"));
        assert!(!r[2].installed);
    }

    #[test]
    fn info_output_parses() {
        let out = "Repository      : extra\nName            : vim\nVersion         : 9.1.0866-1\n\
                   Description     : Vi Improved\nDownload Size   : 1.50 MiB\nLicenses        : custom:vim\n";
        let f = parse_info(out);
        assert_eq!(
            f.iter().find(|(k, _)| k == "Version").map(|(_, v)| v.as_str()),
            Some("9.1.0866-1")
        );
        assert_eq!(parse_size("1.50 MiB"), Some(1572864));
        assert_eq!(parse_size("12 B"), Some(12));
    }

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
