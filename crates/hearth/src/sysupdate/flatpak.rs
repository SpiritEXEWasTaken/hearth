//! Flatpak (system installation), through the `flatpak` command.

use super::{Tx, run};
use hearth_core::model::{BackendInfo, SystemUpdate};
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
    let c = run::capture(FLATPAK, &args, Duration::from_secs(300)).await?;
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
    fn refs_are_checked() {
        assert!(is_valid_ref("app/org.mozilla.firefox/x86_64/stable"));
        assert!(!is_valid_ref("--system"));
        assert!(!is_valid_ref("app/x; rm -rf /"));
    }
}
