//! PackageKit over D-Bus: Debian/Ubuntu (apt), Fedora (dnf5), openSUSE (zypp) and others.
//!
//! PackageKit does the installing in its own daemon, so the helper's sandbox doesn't get in the
//! way. Root is allowed every action by polkit.

use super::{Tx, reboot};
use futures_util::StreamExt;
use hearth_core::model::{BackendInfo, SystemUpdate};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const DEST: &str = "org.freedesktop.PackageKit";
const PATH: &str = "/org/freedesktop/PackageKit";
const IFACE: &str = "org.freedesktop.PackageKit";
const TX_IFACE: &str = "org.freedesktop.PackageKit.Transaction";

/// `PK_FILTER_ENUM_INSTALLED` as a bitfield.
const FILTER_INSTALLED: u64 = 1 << 2;
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
            SystemUpdate {
                id: format!("packagekit:{}", p.id),
                backend: "packagekit".into(),
                name: name.to_string(),
                arch: Some(arch.to_string()).filter(|a| !a.is_empty()),
                current_version: current,
                new_version: version.to_string(),
                repo: Some(data.to_string()).filter(|d| !d.is_empty()),
                security: p.info & 0xffff == INFO_SECURITY,
                severity: severity(p.info).to_string(),
                summary: p.summary,
            }
        })
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

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
