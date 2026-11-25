//! `hearth system …`: system updates from the CLI.

use crate::changes::{confirm, follow};
use crate::cli::{GlobalArgs, SystemCmd};
use crate::client::Client;
use crate::render;
use hearth_core::model::{JobRef, SystemInfo, SystemUpdate, SystemUpdates, UpgradePlan};
use hearth_core::output::Envelope;
use hearth_core::{Error, ExitCode};
use serde_json::{Value, json};

fn print_json<T: serde::Serialize>(data: &T) {
    println!(
        "{}",
        serde_json::to_string(&Envelope::success(data)).unwrap_or_default()
    );
}

pub async fn run(c: &Client, g: &GlobalArgs, cmd: &SystemCmd) -> Result<ExitCode, Error> {
    match cmd {
        SystemCmd::Info => {
            let v: Value = c.get("/v1/system").await?;
            let info: SystemInfo =
                serde_json::from_value(v["info"].clone()).map_err(|e| Error::Other(e.to_string()))?;
            if g.json {
                print_json(&info);
            } else {
                print_info(&info);
            }
            Ok(ExitCode::Ok)
        }
        SystemCmd::Updates { check, security } => {
            if *check {
                if !g.json {
                    eprintln!("Refreshing the package lists…");
                }
                let r: JobRef = c.post("/v1/system/check", None).await?;
                // Progress goes to stderr; the list itself is printed below (as JSON with --json).
                let code = follow(c, &without_json(g), r).await?;
                if code != ExitCode::Ok {
                    return Ok(code);
                }
            }
            let v: Value = c.get("/v1/system").await?;
            let mut u: SystemUpdates =
                serde_json::from_value(v["updates"].clone()).map_err(|e| Error::Other(e.to_string()))?;
            if *security {
                u.updates.retain(|x| x.security);
            }
            if g.json {
                print_json(&u);
            } else {
                print_updates(&u);
            }
            Ok(if u.updates.is_empty() {
                ExitCode::Ok
            } else {
                ExitCode::UpdatesAvailable
            })
        }
        SystemCmd::Upgrade { ids, all, security } => {
            let body = json!({ "ids": ids, "all": all, "security_only": security, "dry_run": true });
            let plan: UpgradePlan = c.post("/v1/system/upgrade", Some(body)).await?;
            if g.dry_run {
                if g.json {
                    print_json(&plan);
                } else {
                    print_plan(&plan);
                }
                return Ok(ExitCode::Ok);
            }
            if !g.json {
                print_plan(&plan);
            }
            if plan.updates.is_empty() {
                if !g.json {
                    println!("Nothing to install.");
                } else {
                    print_json(&plan);
                }
                return Ok(ExitCode::Ok);
            }
            confirm(g, &format!("Install {} update(s)", plan.updates.len()))?;
            let r: JobRef = c
                .post(
                    "/v1/system/upgrade",
                    Some(json!({ "ids": ids, "all": all, "security_only": security })),
                )
                .await?;
            follow(c, g, r).await
        }
    }
}

fn without_json(g: &GlobalArgs) -> GlobalArgs {
    GlobalArgs {
        json: false,
        ..g.clone()
    }
}

fn print_info(i: &SystemInfo) {
    println!("{} (kernel {})", i.distro_name, i.kernel);
    for b in &i.backends {
        let state = if b.available { "available" } else { "NOT available" };
        let detail = if b.detail.is_empty() {
            String::new()
        } else {
            format!(" ({})", b.detail)
        };
        let note = b.note.as_ref().map(|n| format!(": {n}")).unwrap_or_default();
        println!("  {}{detail}: {state}{note}", b.kind);
    }
    print_reboot(&i.reboot);
}

fn print_reboot(r: &hearth_core::model::RebootStatus) {
    if r.needed {
        println!("REBOOT NEEDED (Hearth never reboots by itself):");
        for reason in &r.reasons {
            println!("  - {reason}");
        }
    } else {
        println!("No reboot needed.");
    }
}

fn rows(updates: &[SystemUpdate]) -> Vec<Vec<String>> {
    let mut rows = vec![vec![
        "NAME".to_string(),
        "CURRENT".into(),
        "NEW".into(),
        "TYPE".into(),
        "FROM".into(),
    ]];
    for u in updates {
        rows.push(vec![
            u.name.clone(),
            u.current_version.clone().unwrap_or_else(|| "-".into()),
            u.new_version.clone(),
            if u.security {
                "SECURITY".into()
            } else {
                u.severity.clone()
            },
            match &u.repo {
                Some(r) => format!("{} ({r})", u.backend),
                None => u.backend.clone(),
            },
        ]);
    }
    rows
}

fn print_updates(u: &SystemUpdates) {
    if u.checked_at == 0 {
        println!("Never checked: run `hearth system updates --check`.");
    } else {
        println!("Checked {}.", render::ago(u.checked_at));
    }
    for e in &u.errors {
        println!("warning: {e}");
    }
    if u.updates.is_empty() {
        println!("No updates.");
    } else {
        let sec = u.updates.iter().filter(|x| x.security).count();
        println!("{} update(s), {sec} security:", u.updates.len());
        render::table(&rows(&u.updates));
    }
    print_reboot(&u.reboot);
}

fn print_plan(p: &UpgradePlan) {
    if !p.updates.is_empty() {
        println!("Would install {} update(s):", p.updates.len());
        render::table(&rows(&p.updates));
    }
    for n in &p.notes {
        println!("note: {n}");
    }
}
