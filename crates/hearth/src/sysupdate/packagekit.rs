//! PackageKit over D-Bus: Debian/Ubuntu (apt), Fedora (dnf5), openSUSE (zypp) and others.
//!
//! PackageKit does the installing in its own daemon, so the helper's sandbox doesn't get in the
//! way. Root is allowed every action by polkit.

use super::{Tx, reboot};
use futures_util::StreamExt;
use hearth_core::model::{BackendInfo, PackageDetail, PackageEntry, PackagePlan, SystemUpdate};
use hearth_core::proto::PackageAction;
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const DEST: &str = "org.freedesktop.PackageKit";
const PATH: &str = "/org/freedesktop/PackageKit";
const IFACE: &str = "org.freedesktop.PackageKit";
const TX_IFACE: &str = "org.freedesktop.PackageKit.Transaction";

/// `PK_FILTER_ENUM_*` as bits (checked against pkcon's calls on PackageKit 1.3).
const FILTER_INSTALLED: u64 = 1 << 2;
const FILTER_NOT_INSTALLED: u64 = 1 << 3;
const FILTER_NEWEST: u64 = 1 << 16;
const FILTER_ARCH: u64 = 1 << 18;
const FILTER_NOT_SOURCE: u64 = 1 << 21;
/// `PK_TRANSACTION_FLAG_ENUM_SIMULATE`: report what would happen, change nothing.
const FLAG_SIMULATE: u64 = 1 << 2;
/// `PK_TRANSACTION_FLAG_ENUM_ONLY_TRUSTED`: never install unsigned packages.
const FLAG_ONLY_TRUSTED: u64 = 1 << 1;
const INFO_SECURITY: u32 = 8;
const INFO_BLOCKED: u32 = 9;
const EXIT_SUCCESS: u32 = 1;
/// `PK_RESTART_ENUM_SYSTEM` and `PK_RESTART_ENUM_SECURITY_SYSTEM`.
const RESTART_SYSTEM: [u32; 2] = [4, 6];

#[derive(Debug, Clone)]
pub struct Pkg {
    pub info: u32,
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Default)]
struct Outcome {
    packages: Vec<Pkg>,
    details: Vec<HashMap<String, zbus::zvariant::OwnedValue>>,
    errors: Vec<String>,
    exit: u32,
    restart_system: Vec<String>,
}

fn e(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(name, msg, _) => {
            format!(
                "PackageKit: {}{}",
                name.as_str(),
                msg.map(|m| format!(": {m}")).unwrap_or_default()
            )
        }
        other => format!("PackageKit: {other}"),
    }
}

async fn connect() -> Result<Connection, String> {
    Connection::system()
        .await
        .map_err(|err| format!("system D-Bus: {err}"))
}

pub async fn probe() -> Option<BackendInfo> {
    let conn = match connect().await {
        Ok(c) => c,
        Err(err) => {
            return Some(BackendInfo {
                kind: "packagekit".into(),
                available: false,
                detail: String::new(),
                note: Some(format!("the system D-Bus isn't reachable ({err})")),
            });
        }
    };
    let bus = zbus::fdo::DBusProxy::new(&conn).await.ok()?;
    let activatable = bus.list_activatable_names().await.unwrap_or_default();
    let running = bus.list_names().await.unwrap_or_default();
    let has_owner = activatable
        .iter()
        .chain(running.iter())
        .any(|n| n.as_str() == DEST);
    if !has_owner {
        return None;
    }
    let pk = Proxy::new(&conn, DEST, PATH, IFACE).await.ok()?;
    Some(match pk.get_property::<String>("BackendName").await {
        Ok(name) => BackendInfo {
            kind: "packagekit".into(),
            available: true,
            detail: name,
            note: None,
        },
        Err(err) => BackendInfo {
            kind: "packagekit".into(),
            available: false,
            detail: String::new(),
            note: Some(format!("PackageKit doesn't answer: {err}")),
        },
    })
}

/// Runs one PackageKit transaction to its end.
async fn transaction<B>(
    conn: &Connection,
    method: &str,
    body: &B,
    tx: Option<&Tx>,
    timeout: Duration,
) -> Result<Outcome, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let pk = Proxy::new(conn, DEST, PATH, IFACE).await.map_err(e)?;
    let path: OwnedObjectPath = pk.call("CreateTransaction", &()).await.map_err(e)?;
    let t = Proxy::new(conn, DEST, path, TX_IFACE).await.map_err(e)?;
    let mut signals = t.receive_all_signals().await.map_err(e)?;
    let hints = vec!["interactive=false", "background=false", "locale=C.UTF-8"];
    let _: Result<(), _> = t.call("SetHints", &(hints,)).await;
    let () = t.call(method, body).await.map_err(e)?;
    let mut out = Outcome::default();
    let mut seen = std::collections::HashSet::new();
    let run = async {
        while let Some(msg) = signals.next().await {
            let header = msg.header();
            let Some(member) = header.member() else { continue };
            match member.as_str() {
                "Package" => {
                    if let Ok((info, id, summary)) = msg.body().deserialize::<(u32, String, String)>() {
                        progress(tx, info, &id, &mut seen);
                        out.packages.push(Pkg { info, id, summary });
                    }
                }
                "Packages" => {
                    if let Ok(list) = msg.body().deserialize::<Vec<(u32, String, String)>>() {
                        for (info, id, summary) in list {
                            progress(tx, info, &id, &mut seen);
                            out.packages.push(Pkg { info, id, summary });
                        }
                    }
                }
                "Details" => {
                    if let Ok(d) = msg
                        .body()
                        .deserialize::<HashMap<String, zbus::zvariant::OwnedValue>>()
                    {
                        out.details.push(d);
                    }
                }
                "ErrorCode" => {
                    if let Ok((code, details)) = msg.body().deserialize::<(u32, String)>() {
                        let line = format!("error {code}: {}", details.trim());
                        if let Some(tx) = tx {
                            let _ = tx.send(format!("  {line}"));
                        }
                        out.errors.push(line);
                    }
                }
                "RequireRestart" => {
                    if let Ok((kind, id)) = msg.body().deserialize::<(u32, String)>()
                        && RESTART_SYSTEM.contains(&kind)
                    {
                        out.restart_system.push(id);
                    }
                }
                "Finished" => {
                    if let Ok((exit, _runtime)) = msg.body().deserialize::<(u32, u32)>() {
                        out.exit = exit;
                    }
                    break;
                }
                "Destroy" => break,
                _ => {}
            }
        }
    };
    tokio::time::timeout(timeout, run).await.map_err(|_| {
        format!(
            "PackageKit {method} didn't finish within {} min",
            timeout.as_secs() / 60
        )
    })?;
    Ok(out)
}

/// Prints what PackageKit is doing during an install: each step once per package (PackageKit
/// repeats steps and sends a "finished" after each one).
fn progress(tx: Option<&Tx>, info: u32, id: &str, seen: &mut std::collections::HashSet<String>) {
    let Some(tx) = tx else { return };
    let verb = match info & 0xffff {
        10 => "downloading",
        11 => "updating",
        12 => "installing",
        13 => "removing",
        14 => "cleaning up",
        _ => return,
    };
    let (name, version, ..) = split_id(id);
    let line = format!("  {verb} {name} {version}");
    if seen.insert(line.clone()) {
        let _ = tx.send(line);
    }
}

/// `name;version;arch;data` → (name, version, arch, data).
pub fn split_id(id: &str) -> (&str, &str, &str, &str) {
    let mut p = id.splitn(4, ';');
    (
        p.next().unwrap_or_default(),
        p.next().unwrap_or_default(),
        p.next().unwrap_or_default(),
        p.next().unwrap_or_default(),
    )
}

/// The severity PackageKit gives an update. Since 1.2.4 the upper 16 bits of `info` may carry a
/// separate severity; otherwise the info value itself is the update type.
pub fn severity(info: u32) -> &'static str {
    let (kind, sev) = (info & 0xffff, info >> 16);
    let name = |v: u32| match v {
        // AVAILABLE: an update without a classification.
        2 => Some("normal"),
        3 => Some("low"),
        4 => Some("enhancement"),
        5 => Some("normal"),
        6 => Some("bugfix"),
        7 => Some("important"),
        8 => Some("security"),
        9 => Some("blocked"),
        _ => None,
    };
    if kind == INFO_SECURITY {
        return "security";
    }
    name(sev).or_else(|| name(kind)).unwrap_or("unknown")
}

pub async fn updates(refresh: bool, tx: &Tx) -> Result<Vec<SystemUpdate>, String> {
    let conn = connect().await?;
    if refresh {
        let _ = tx.send("refreshing package lists (PackageKit)".into());
        let r = transaction(&conn, "RefreshCache", &(false,), None, Duration::from_secs(600)).await?;
        if r.exit != EXIT_SUCCESS {
            return Err(format!(
                "refreshing the package lists failed: {}",
                r.errors.join("; ")
            ));
        }
    }
    let r = transaction(&conn, "GetUpdates", &(0u64,), None, Duration::from_secs(300)).await?;
    if r.exit != EXIT_SUCCESS {
        return Err(format!("listing updates failed: {}", r.errors.join("; ")));
    }
    let pkgs: Vec<Pkg> = r
        .packages
        .into_iter()
        .filter(|p| p.info & 0xffff != INFO_BLOCKED)
        .collect();
    // Installed versions, to show "current → new".
    let mut names: Vec<String> = pkgs.iter().map(|p| split_id(&p.id).0.to_string()).collect();
    names.sort();
    names.dedup();
    let mut installed: HashMap<(String, String), String> = HashMap::new();
    if !names.is_empty() {
        let r = transaction(
            &conn,
            "Resolve",
            &(FILTER_INSTALLED, names),
            None,
            Duration::from_secs(120),
        )
        .await?;
        for p in r.packages {
            let (n, v, a, _) = split_id(&p.id);
            installed.insert((n.to_string(), a.to_string()), v.to_string());
        }
    }
    Ok(pkgs
        .into_iter()
        .map(|p| {
            let (name, version, arch, data) = split_id(&p.id);
            let current = installed
                .get(&(name.to_string(), arch.to_string()))
                .or_else(|| installed.iter().find(|((n, _), _)| n == name).map(|(_, v)| v))
                .cloned();
            let (repo, new_package) = update_repo(data);
            let mut security = p.info & 0xffff == INFO_SECURITY;
            let mut sev = severity(p.info).to_string();
            // A package an update pulls in (e.g. a new kernel image): PackageKit gives it no
            // severity. It belongs to its parent update, so it counts as security when it comes
            // from the security channel.
            if new_package && sev == "unknown" {
                sev = "new".into();
                security |= repo.as_deref().is_some_and(|r| r.contains("security"));
            }
            SystemUpdate {
                id: format!("packagekit:{}", p.id),
                backend: "packagekit".into(),
                name: name.to_string(),
                arch: Some(arch.to_string()).filter(|a| !a.is_empty()),
                current_version: current,
                new_version: version.to_string(),
                repo,
                security,
                severity: sev,
                summary: p.summary,
            }
        })
        .collect())
}

/// The repository of an update from its package id's data field, and whether it's a package
/// that isn't installed yet (data like `+auto:debian-security`).
fn update_repo(data: &str) -> (Option<String>, bool) {
    let new_package = data.starts_with('+');
    let repo = data.trim_start_matches('+');
    let repo = repo.rsplit(':').next().unwrap_or(repo);
    (Some(repo.to_string()).filter(|r| !r.is_empty()), new_package)
}

/// Installs the given PackageKit package ids (`name;version;arch;data`).
pub async fn upgrade(ids: &[String], tx: &Tx) -> Result<(Vec<String>, Vec<String>), String> {
    let conn = connect().await?;
    let _ = tx.send(format!("installing {} update(s) with PackageKit", ids.len()));
    let r = transaction(
        &conn,
        "UpdatePackages",
        &(FLAG_ONLY_TRUSTED, ids.to_vec()),
        Some(tx),
        Duration::from_secs(3 * 3600),
    )
    .await?;
    for id in &r.restart_system {
        let (name, version, ..) = split_id(id);
        reboot::mark(&format!("{name} {version} was updated and needs a restart"));
    }
    if r.exit == EXIT_SUCCESS {
        Ok((ids.to_vec(), r.errors))
    } else {
        Err(if r.errors.is_empty() {
            format!("PackageKit ended with exit code {}", r.exit)
        } else {
            r.errors.join("; ")
        })
    }
}

// ----- packages (M4b) -----

fn entry(p: &Pkg) -> PackageEntry {
    let (name, version, arch, data) = split_id(&p.id);
    PackageEntry {
        backend: "packagekit".into(),
        name: name.to_string(),
        version: version.to_string(),
        arch: Some(arch.to_string()).filter(|a| !a.is_empty()),
        // Installed packages have `installed` or `manual:<repo>` / `auto:<repo>` as their data.
        repo: Some(data.rsplit(':').next().unwrap_or(data).to_string())
            .filter(|d| !d.is_empty() && d != "installed"),
        installed: p.info & 0xffff == 1,
        summary: p.summary.clone(),
    }
}

pub async fn search(terms: &[String]) -> Result<Vec<PackageEntry>, String> {
    let conn = connect().await?;
    let r = transaction(
        &conn,
        "SearchNames",
        &(FILTER_NEWEST | FILTER_ARCH | FILTER_NOT_SOURCE, terms.to_vec()),
        None,
        Duration::from_secs(120),
    )
    .await?;
    if r.exit != EXIT_SUCCESS {
        return Err(format!("search failed: {}", r.errors.join("; ")));
    }
    Ok(merge_installed(r.packages.iter().map(entry).collect()))
}

/// One row per name and arch: an installed package and its newer available version become one
/// row, marked installed, with the newest version.
fn merge_installed(list: Vec<PackageEntry>) -> Vec<PackageEntry> {
    let mut out: Vec<PackageEntry> = Vec::new();
    for e in list {
        if let Some(prev) = out.iter_mut().find(|p| p.name == e.name && p.arch == e.arch) {
            prev.installed |= e.installed;
            if super::reboot::version_cmp(&e.version, &prev.version).is_gt() {
                prev.version = e.version;
                if e.repo.is_some() {
                    prev.repo = e.repo;
                }
            }
        } else {
            out.push(e);
        }
    }
    out
}

fn detail_str(d: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> Option<String> {
    d.get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .filter(|s| !s.is_empty() && s != "unknown")
}

pub async fn info(name: &str) -> Result<PackageDetail, String> {
    let conn = connect().await?;
    let names = vec![name.to_string()];
    let r = transaction(
        &conn,
        "Resolve",
        &(FILTER_NEWEST | FILTER_ARCH | FILTER_NOT_SOURCE, names.clone()),
        None,
        Duration::from_secs(120),
    )
    .await?;
    let found = merge_installed(r.packages.iter().map(entry).collect());
    let Some(first) = found.into_iter().next() else {
        return Err(format!("no package called '{name}'"));
    };
    let installed = transaction(
        &conn,
        "Resolve",
        &(FILTER_INSTALLED, names),
        None,
        Duration::from_secs(60),
    )
    .await
    .ok()
    .and_then(|r| r.packages.first().map(|p| split_id(&p.id).1.to_string()));
    // Details of the newest version.
    let id = r
        .packages
        .iter()
        .map(|p| p.id.clone())
        .find(|id| split_id(id).0 == first.name && split_id(id).1 == first.version)
        .unwrap_or_default();
    let d = transaction(&conn, "GetDetails", &(vec![id],), None, Duration::from_secs(60))
        .await
        .ok()
        .and_then(|r| r.details.into_iter().next())
        .unwrap_or_default();
    let size = d
        .get("size")
        .and_then(|v| u64::try_from(v.clone()).ok())
        .filter(|s| *s > 0);
    Ok(PackageDetail {
        installed_version: installed,
        description: detail_str(&d, "description").unwrap_or_else(|| first.summary.clone()),
        url: detail_str(&d, "url"),
        license: detail_str(&d, "license"),
        size_bytes: size,
        entry: PackageEntry {
            installed: first.installed,
            ..first
        },
    })
}

/// Installs or removes packages; with `simulate`, only reports what would happen.
pub async fn change(
    action: PackageAction,
    names: &[String],
    simulate: bool,
    tx: &Tx,
) -> Result<PackagePlan, String> {
    let conn = connect().await?;
    let mut plan = PackagePlan {
        action: action.as_str().into(),
        backend: "packagekit".into(),
        names: names.to_vec(),
        ..Default::default()
    };
    let filter = match action {
        PackageAction::Install => FILTER_NOT_INSTALLED | FILTER_NEWEST | FILTER_ARCH | FILTER_NOT_SOURCE,
        PackageAction::Remove => FILTER_INSTALLED,
    };
    let r = transaction(
        &conn,
        "Resolve",
        &(filter, names.to_vec()),
        None,
        Duration::from_secs(120),
    )
    .await?;
    let mut ids = Vec::new();
    for n in names {
        match r.packages.iter().find(|p| split_id(&p.id).0 == n.as_str()) {
            Some(p) => ids.push(p.id.clone()),
            None => plan.errors.push(match action {
                PackageAction::Install => format!("'{n}' isn't available, or it's already installed"),
                PackageAction::Remove => format!("'{n}' isn't installed"),
            }),
        }
    }
    if !plan.errors.is_empty() || ids.is_empty() {
        return Ok(plan);
    }
    let flags = FLAG_ONLY_TRUSTED | if simulate { FLAG_SIMULATE } else { 0 };
    let progress = if simulate { None } else { Some(tx) };
    let (r, method) = match action {
        PackageAction::Install => (
            transaction(
                &conn,
                "InstallPackages",
                &(flags, ids),
                progress,
                Duration::from_secs(3600),
            )
            .await?,
            "install",
        ),
        // allow_deps: packages that need the removed ones go too; the plan lists them all and
        // the person confirms that list. autoremove: no (leftover dependencies stay).
        PackageAction::Remove => (
            transaction(
                &conn,
                "RemovePackages",
                &(flags, ids, true, false),
                progress,
                Duration::from_secs(3600),
            )
            .await?,
            "remove",
        ),
    };
    if r.exit != EXIT_SUCCESS {
        plan.errors.push(if r.errors.is_empty() {
            format!("PackageKit couldn't {method} (exit code {})", r.exit)
        } else {
            r.errors.join("; ")
        });
        return Ok(plan);
    }
    for p in &r.packages {
        let e = entry(p);
        let list = match p.info & 0xffff {
            12 | 10 => &mut plan.install,
            13 | 15 => &mut plan.remove,
            11 | 19 | 20 => &mut plan.update,
            _ => continue,
        };
        if !list.iter().any(|x| x.name == e.name && x.arch == e.arch) {
            list.push(e);
        }
    }
    for id in &r.restart_system {
        let (name, version, ..) = split_id(id);
        reboot::mark(&format!("{name} {version} was changed and needs a restart"));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_repo_strips_new_package_marker() {
        assert_eq!(
            update_repo("+auto:debian-security"),
            (Some("debian-security".into()), true)
        );
        assert_eq!(
            update_repo("debian-security"),
            (Some("debian-security".into()), false)
        );
        assert_eq!(update_repo(""), (None, false));
    }

    #[test]
    fn package_ids_split() {
        assert_eq!(
            split_id("libzstd1;1.5.7+dfsg-1+deb13u1;amd64;debian-stable-security-main"),
            (
                "libzstd1",
                "1.5.7+dfsg-1+deb13u1",
                "amd64",
                "debian-stable-security-main"
            )
        );
        assert_eq!(split_id("x"), ("x", "", "", ""));
    }

    #[test]
    fn severities() {
        // Captured from PackageKit 1.3.1 on Debian 13: security update, severity in the upper bits.
        assert_eq!(severity(524296), "security");
        assert_eq!(severity(5), "normal");
        assert_eq!(severity((7 << 16) | 6), "important");
        assert_eq!(severity(6), "bugfix");
        assert_eq!(severity(2), "normal");
        assert_eq!(severity(1234), "unknown");
    }
}
