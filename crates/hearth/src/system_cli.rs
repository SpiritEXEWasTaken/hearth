//! `hearth system …`: system updates from the CLI.

use crate::changes::{confirm, follow};
use crate::cli::{AddCmd, AddCommon, GlobalArgs, PackageCmd, SystemCmd};
use crate::client::Client;
use crate::render;
use hearth_core::model::{
    JobRef, PackageDetail, PackageEntry, PackagePlan, SystemInfo, SystemUpdate, SystemUpdates, UpgradePlan,
};
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

// ----- packages (M4b) -----

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub async fn package(c: &Client, g: &GlobalArgs, cmd: &PackageCmd) -> Result<ExitCode, Error> {
    match cmd {
        PackageCmd::Search {
            terms,
            flatpak,
            native,
        } => {
            if terms.is_empty() {
                return Err(Error::Usage("give something to search for".into()));
            }
            let backend = if *flatpak {
                "flatpak"
            } else if *native {
                "native"
            } else {
                "all"
            };
            let list: Vec<PackageEntry> = c
                .get(&format!(
                    "/v1/packages/search?q={}&backend={backend}",
                    enc(&terms.join(" "))
                ))
                .await?;
            if g.json {
                print_json(&list);
            } else if list.is_empty() {
                println!("Nothing found.");
            } else {
                let mut rows = vec![vec![
                    "NAME".to_string(),
                    "VERSION".into(),
                    "INSTALLED".into(),
                    "FROM".into(),
                    "SUMMARY".into(),
                ]];
                for e in &list {
                    rows.push(vec![
                        e.name.clone(),
                        e.version.clone(),
                        if e.installed { "yes".into() } else { String::new() },
                        from(e),
                        e.summary.chars().take(60).collect(),
                    ]);
                }
                render::table(&rows);
            }
            Ok(ExitCode::Ok)
        }
        PackageCmd::Info { name, flatpak } => {
            let backend = if *flatpak { "flatpak" } else { "native" };
            let d: PackageDetail = c
                .get(&format!("/v1/packages/info?name={}&backend={backend}", enc(name)))
                .await?;
            if g.json {
                print_json(&d);
            } else {
                println!("{} {} ({})", d.entry.name, d.entry.version, from(&d.entry));
                match &d.installed_version {
                    Some(v) if *v == d.entry.version => println!("installed: yes"),
                    Some(v) => println!("installed: {v} (newer available)"),
                    None => println!("installed: no"),
                }
                if !d.description.is_empty() {
                    println!("{}", d.description);
                }
                if let Some(u) = &d.url {
                    println!("url: {u}");
                }
                if let Some(l) = &d.license {
                    println!("license: {l}");
                }
                if let Some(s) = d.size_bytes {
                    println!("size: {}", render::bytes(s));
                }
            }
            Ok(ExitCode::Ok)
        }
        PackageCmd::Install { names, flatpak } => change(c, g, "install", names, *flatpak, false).await,
        PackageCmd::Remove {
            names,
            flatpak,
            allow_critical,
        } => change(c, g, "remove", names, *flatpak, *allow_critical).await,
    }
}

fn from(e: &PackageEntry) -> String {
    match &e.repo {
        Some(r) => format!("{} ({r})", e.backend),
        None => e.backend.clone(),
    }
}

async fn change(
    c: &Client,
    g: &GlobalArgs,
    action: &str,
    names: &[String],
    flatpak: bool,
    allow_critical: bool,
) -> Result<ExitCode, Error> {
    if names.is_empty() {
        return Err(Error::Usage(format!("say which packages to {action}")));
    }
    let backend = if flatpak { "flatpak" } else { "native" };
    let path = format!("/v1/packages/{action}");
    let body = |dry_run: bool| json!({ "names": names, "backend": backend, "dry_run": dry_run, "allow_critical": allow_critical });
    let plan: PackagePlan = c.post(&path, Some(body(true))).await?;
    if !g.json || g.dry_run {
        if g.json {
            print_json(&plan);
        } else {
            print_package_plan(&plan);
        }
    }
    if g.dry_run {
        return Ok(ExitCode::Ok);
    }
    if !plan.errors.is_empty() {
        return Err(Error::Usage(plan.errors.join("; ")));
    }
    let count = plan.install.len() + plan.remove.len() + plan.update.len();
    if count == 0 && action == "install" {
        if !g.json {
            println!("Nothing to do.");
        }
        return Ok(ExitCode::Ok);
    }
    confirm(
        g,
        &format!(
            "{} {}",
            if action == "install" { "Install" } else { "Remove" },
            names.join(" ")
        ),
    )?;
    let r: JobRef = c.post(&path, Some(body(false))).await?;
    follow(c, g, r).await
}

fn print_package_plan(p: &PackagePlan) {
    let list = |title: &str, l: &[PackageEntry]| {
        if !l.is_empty() {
            println!("{title} ({}):", l.len());
            for e in l {
                println!("  {} {}", e.name, e.version);
            }
        }
    };
    list("Would install", &p.install);
    list("Would upgrade", &p.update);
    list("Would REMOVE", &p.remove);
    for w in &p.warnings {
        println!("warning: {w}");
    }
    for e in &p.errors {
        println!("error: {e}");
    }
}

// ----- service, native, link and package apps (M4c) -----

fn common_json(c: &AddCommon) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    };
    put("title", c.title.clone().map(Value::from));
    put("icon", c.icon.clone().map(Value::from));
    put("port", c.port.map(Value::from));
    put("path", c.path.clone().map(Value::from));
    put("url", c.url.clone().map(Value::from));
    put(
        "dashboard",
        if c.dashboard {
            Some(Value::Bool(true))
        } else if c.no_dashboard {
            Some(Value::Bool(false))
        } else {
            None
        },
    );
    m
}

pub async fn add(c: &Client, g: &GlobalArgs, cmd: &AddCmd) -> Result<ExitCode, Error> {
    let (kind, mut body, common) = match cmd {
        AddCmd::Link { name, common } => ("link", json!({ "id": name }), common),
        AddCmd::Service {
            unit,
            user,
            id,
            common,
        } => ("service", json!({ "unit": unit, "user": user, "id": id }), common),
        AddCmd::Native {
            name,
            exec,
            user,
            workdir,
            env_file,
            memory_max,
            restart,
            no_start,
            common,
        } => (
            "native",
            json!({
                "id": name, "exec": exec, "user": user, "workdir": workdir, "env_file": env_file,
                "memory_max": memory_max, "restart": restart, "start": !no_start,
            }),
            common,
        ),
        AddCmd::Package {
            package,
            flatpak,
            unit,
            id,
            common,
        } => (
            "package",
            json!({
                "package": package, "package_backend": if *flatpak { "flatpak" } else { "native" },
                "unit": unit, "id": id,
            }),
            common,
        ),
    };
    if let Some(m) = body.as_object_mut() {
        m.insert("kind".into(), Value::from(kind));
        m.extend(common_json(common));
        m.retain(|_, v| !v.is_null());
    }
    let mut preview = body.clone();
    preview["dry_run"] = json!(true);
    let plan: Value = c.post("/v1/apps/add", Some(preview)).await?;
    if g.dry_run || !g.json {
        if g.json {
            print_json(&plan);
        } else {
            println!("Would add {} app '{}':", kind, plan["id"].as_str().unwrap_or(""));
            print!("{}", plan["app_yaml"].as_str().unwrap_or(""));
            if let Some(u) = plan["unit_file"].as_str() {
                println!(
                    "\nwith this systemd unit (hearth-native-{}.service):",
                    plan["id"].as_str().unwrap_or("")
                );
                print!("{u}");
            }
        }
    }
    if g.dry_run {
        return Ok(ExitCode::Ok);
    }
    confirm(
        g,
        &format!("Add {kind} app '{}'", plan["id"].as_str().unwrap_or("")),
    )?;
    let r: JobRef = c.post("/v1/apps/add", Some(body)).await?;
    follow(c, g, r).await
}

pub async fn set(c: &Client, g: &GlobalArgs, app: &str, common: &AddCommon) -> Result<ExitCode, Error> {
    let body = Value::Object(common_json(common));
    if body.as_object().is_some_and(|m| m.is_empty()) {
        return Err(Error::Usage(
            "nothing to change: give --title, --icon, --port, --path, --url, --dashboard or --no-dashboard"
                .into(),
        ));
    }
    if g.dry_run {
        if g.json {
            print_json(&json!({ "would_set": body, "app": app }));
        } else {
            println!("Would change {app}: {body}");
        }
        return Ok(ExitCode::Ok);
    }
    let r: JobRef = c.post(&format!("/v1/apps/{}/set", enc(app)), Some(body)).await?;
    follow(c, g, r).await
}
