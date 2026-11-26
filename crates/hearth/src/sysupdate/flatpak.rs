//! Flatpak (system installation), through the `flatpak` command.

use super::{Tx, run};
use hearth_core::model::{BackendInfo, PackageDetail, PackageEntry, PackagePlan, SystemUpdate};
use hearth_core::proto::PackageAction;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const FLATPAK: &str = "/usr/bin/flatpak";

pub async fn probe() -> Option<BackendInfo> {
    if !Path::new(FLATPAK).exists() {
        return None;
    }
    let version = run::capture(FLATPAK, &["--version"], Duration::from_secs(10))
        .await
        .map(|c| c.stdout.trim().to_string())
        .unwrap_or_default();
    Some(BackendInfo {
        kind: "flatpak".into(),
        available: true,
        detail: version,
        note: None,
    })
}

pub async fn updates(refresh: bool, tx: &Tx) -> Result<Vec<SystemUpdate>, String> {
    if refresh {
        let _ = tx.send("checking Flatpak remotes".into());
    }
    let mut args = vec![
        "remote-ls",
        "--updates",
        "--system",
        "--columns=ref,application,version,origin,name",
    ];
    if !refresh {
        args.push("--cached");
    }
    let mut c = run::capture(FLATPAK, &args, Duration::from_secs(300)).await?;
    if c.code != 0 && !refresh {
        // No cached summary yet (e.g. root never fetched one): fetch it instead.
        let _ = tx.send("no cached Flatpak summary, fetching it".into());
        args.retain(|a| *a != "--cached");
        c = run::capture(FLATPAK, &args, Duration::from_secs(300)).await?;
    }
    if c.code != 0 {
        return Err(format!("flatpak remote-ls failed: {}", c.stderr.trim()));
    }
    let l = run::capture(
        FLATPAK,
        &["list", "--system", "--columns=ref,version"],
        Duration::from_secs(60),
    )
    .await?;
    Ok(parse(&c.stdout, &l.stdout))
}

/// `remote-ls` lines: ref, application, version, origin, name (tab-separated);
/// `list` lines: ref, version.
pub fn parse(remote: &str, installed: &str) -> Vec<SystemUpdate> {
    let current: HashMap<&str, &str> = installed
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            Some((p.next()?.trim(), p.next().unwrap_or("").trim()))
        })
        .collect();
    remote
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\t').map(str::trim).collect();
            let r#ref = *p.first().filter(|r| r.contains('/'))?;
            let app = p.get(1).copied().unwrap_or(r#ref);
            let arch = r#ref.split('/').nth(2).map(str::to_string);
            Some(SystemUpdate {
                id: format!("flatpak:{ref}"),
                backend: "flatpak".into(),
                name: app.to_string(),
                arch,
                current_version: current
                    .get(r#ref)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string()),
                new_version: p
                    .get(2)
                    .filter(|v| !v.is_empty())
                    .unwrap_or(&"(new build)")
                    .to_string(),
                repo: p.get(3).filter(|v| !v.is_empty()).map(|v| v.to_string()),
                security: false,
                severity: "unknown".into(),
                summary: p.get(4).map(|v| v.to_string()).unwrap_or_default(),
            })
        })
        .collect()
}

/// A Flatpak ref Hearth will pass on the command line: `app/org.Example/x86_64/stable` style.
pub fn is_valid_ref(r: &str) -> bool {
    !r.is_empty() && !r.starts_with('-') && r.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
}

/// Updates the given refs (all when empty).
pub async fn upgrade(refs: &[String], tx: &Tx) -> Result<(), String> {
    if let Some(bad) = refs.iter().find(|r| !is_valid_ref(r)) {
        return Err(format!("'{bad}' isn't a Flatpak ref"));
    }
    let mut args: Vec<&str> = vec!["update", "-y", "--noninteractive", "--system"];
    args.extend(refs.iter().map(String::as_str));
    let code = run::unconfined("flatpak", FLATPAK, &args, tx, Duration::from_secs(3600)).await?;
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "flatpak update failed (exit code {code}); see the log above"
        ))
    }
}

// ----- packages (M4b): Flatpak apps -----

async fn installed_apps() -> Vec<(String, String)> {
    run::capture(
        FLATPAK,
        &["list", "--system", "--app", "--columns=application,version"],
        Duration::from_secs(60),
    )
    .await
    .map(|c| {
        c.stdout
            .lines()
            .filter_map(|l| {
                let mut p = l.split('\t');
                Some((
                    p.next()?.trim().to_string(),
                    p.next().unwrap_or("").trim().to_string(),
                ))
            })
            .collect()
    })
    .unwrap_or_default()
}

pub async fn search(terms: &[String]) -> Result<Vec<PackageEntry>, String> {
    let mut args = vec![
        "search",
        "--columns=application,version,branch,remotes,name,description",
    ];
    args.extend(terms.iter().map(String::as_str));
    let c = run::capture(FLATPAK, &args, Duration::from_secs(120)).await?;
    if c.code != 0 {
        return Err(format!("flatpak search failed: {}", c.stderr.trim()));
    }
    let installed = installed_apps().await;
    Ok(parse_search(&c.stdout, &installed))
}

/// `flatpak search` lines: application, version, branch, remotes, name, description.
pub fn parse_search(out: &str, installed: &[(String, String)]) -> Vec<PackageEntry> {
    out.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('\t').map(str::trim).collect();
            let app = *p.first().filter(|a| a.contains('.'))?;
            let name = p.get(4).copied().unwrap_or(app);
            let desc = p.get(5).copied().unwrap_or("");
            Some(PackageEntry {
                backend: "flatpak".into(),
                name: app.to_string(),
                version: p.get(1).copied().unwrap_or("").to_string(),
                arch: None,
                // The first remote (search lists them comma-separated).
                repo: p
                    .get(3)
                    .and_then(|r| r.split(',').next())
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty()),
                installed: installed.iter().any(|(a, _)| a == app),
                summary: if desc.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}: {desc}")
                },
            })
        })
        .collect()
}

pub async fn info(app: &str) -> Result<PackageDetail, String> {
    let found = search(&[app.to_string()]).await?;
    let e = found
        .into_iter()
        .find(|e| e.name == app)
        .ok_or_else(|| format!("no Flatpak app called '{app}'"))?;
    let installed_version = installed_apps()
        .await
        .into_iter()
        .find(|(a, _)| a == app)
        .map(|(_, v)| v);
    Ok(PackageDetail {
        description: e.summary.clone(),
        installed_version,
        url: (e.repo.as_deref() == Some("flathub")).then(|| format!("https://flathub.org/apps/{app}")),
        license: None,
        size_bytes: None,
        entry: e,
    })
}

/// Flatpak has no dry run: the plan lists the apps and their remote; the runtimes they need are
/// installed with them.
pub async fn change(
    action: PackageAction,
    names: &[String],
    simulate: bool,
    tx: &Tx,
) -> Result<PackagePlan, String> {
    let mut plan = PackagePlan {
        action: action.as_str().into(),
        backend: "flatpak".into(),
        names: names.to_vec(),
        ..Default::default()
    };
    if let Some(bad) = names.iter().find(|n| !is_valid_ref(n)) {
        plan.errors.push(format!("'{bad}' isn't a Flatpak app id"));
        return Ok(plan);
    }
    let installed = installed_apps().await;
    let mut remotes: Vec<(String, String)> = Vec::new();
    for n in names {
        let is_installed = installed.iter().any(|(a, _)| a == n);
        match action {
            PackageAction::Install if is_installed => plan.errors.push(format!("{n} is already installed")),
            PackageAction::Remove if !is_installed => plan.errors.push(format!("{n} isn't installed")),
            PackageAction::Install => {
                let found = search(std::slice::from_ref(n))
                    .await?
                    .into_iter()
                    .find(|e| &e.name == n);
                match found {
                    Some(e) => {
                        remotes.push((e.repo.clone().unwrap_or_else(|| "flathub".into()), n.clone()));
                        plan.install.push(e);
                    }
                    None => plan
                        .errors
                        .push(format!("no Flatpak app called '{n}' in the configured remotes")),
                }
            }
            PackageAction::Remove => plan.remove.push(PackageEntry {
                backend: "flatpak".into(),
                name: n.clone(),
                version: installed
                    .iter()
                    .find(|(a, _)| a == n)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
                arch: None,
                repo: None,
                installed: true,
                summary: String::new(),
            }),
        }
    }
    if action == PackageAction::Install && plan.errors.is_empty() {
        plan.warnings
            .push("the runtimes these apps need are installed with them if missing".into());
    }
    if simulate || !plan.errors.is_empty() {
        return Ok(plan);
    }
    match action {
        PackageAction::Install => {
            for (remote, app) in &remotes {
                let args = [
                    "install",
                    "-y",
                    "--noninteractive",
                    "--system",
                    remote.as_str(),
                    app.as_str(),
                ];
                let code = run::unconfined("flatpak", FLATPAK, &args, tx, Duration::from_secs(3600)).await?;
                if code != 0 {
                    plan.errors
                        .push(format!("flatpak install {app} failed (exit code {code})"));
                }
            }
        }
        PackageAction::Remove => {
            let mut args: Vec<&str> = vec!["uninstall", "-y", "--noninteractive", "--system"];
            args.extend(names.iter().map(String::as_str));
            let code = run::unconfined("flatpak", FLATPAK, &args, tx, Duration::from_secs(1800)).await?;
            if code != 0 {
                plan.errors
                    .push(format!("flatpak uninstall failed (exit code {code})"));
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_ls_output_parses() {
        let remote = "app/org.mozilla.firefox/x86_64/stable\torg.mozilla.firefox\t133.0\tflathub\tFirefox\n\
                      runtime/org.freedesktop.Platform/x86_64/24.08\torg.freedesktop.Platform\t\tflathub\tFreedesktop Platform\n";
        let installed = "app/org.mozilla.firefox/x86_64/stable\t132.0.2\n";
        let u = parse(remote, installed);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].id, "flatpak:app/org.mozilla.firefox/x86_64/stable");
        assert_eq!(u[0].current_version.as_deref(), Some("132.0.2"));
        assert_eq!(u[0].new_version, "133.0");
        assert_eq!(u[0].arch.as_deref(), Some("x86_64"));
        assert_eq!(u[1].new_version, "(new build)");
        assert_eq!(u[1].current_version, None);
    }

    #[test]
    fn search_output_parses() {
        let out = "org.mozilla.firefox\t133.0\tstable\tflathub\tFirefox\tFast, Private & Safe Web Browser\n\
                   org.gimp.GIMP\t2.10.38\tstable\tflathub,fedora\tGNU Image Manipulation Program\t\n";
        let installed = vec![("org.gimp.GIMP".to_string(), "2.10.36".to_string())];
        let r = parse_search(out, &installed);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].summary, "Firefox: Fast, Private & Safe Web Browser");
        assert!(!r[0].installed);
        assert!(r[1].installed);
        assert_eq!(r[1].repo.as_deref(), Some("flathub"));
        assert_eq!(r[1].summary, "GNU Image Manipulation Program");
    }

    #[test]
    fn refs_are_checked() {
        assert!(is_valid_ref("app/org.mozilla.firefox/x86_64/stable"));
        assert!(!is_valid_ref("--system"));
        assert!(!is_valid_ref("app/x; rm -rf /"));
    }
}
