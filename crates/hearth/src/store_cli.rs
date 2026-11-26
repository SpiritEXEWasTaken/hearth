//! `hearth store …`: browse the store, see host checks, install with options.

use crate::changes::{confirm, follow};
use crate::cli::{GlobalArgs, StoreCmd};
use crate::client::Client;
use crate::render;
use hearth_core::model::JobRef;
use hearth_core::{Error, ExitCode};
use serde_json::{Map, Value, json};

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn kv(list: &[String], what: &str) -> Result<Map<String, Value>, Error> {
    let mut m = Map::new();
    for item in list {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("{what} '{item}': use KEY=VALUE")))?;
        m.insert(k.to_string(), json!(v));
    }
    Ok(m)
}

/// A store id from an id, an app name (`jellyfin`) or a display name (`Home Assistant`).
async fn resolve_id(c: &Client, wanted: &str) -> Result<String, Error> {
    let list: Value = c.get("/v1/store").await?;
    let apps = list
        .get("apps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let w = wanted.to_lowercase();
    apps.iter()
        .find(|a| s(a, "id") == wanted)
        .or_else(|| apps.iter().find(|a| s(a, "app_name") == w))
        .or_else(|| apps.iter().find(|a| s(a, "name").to_lowercase() == w))
        .map(|a| s(a, "id").to_string())
        .ok_or_else(|| Error::NotFound {
            what: "store app",
            name: wanted.to_string(),
        })
}

fn mb(v: u64) -> String {
    if v >= 1024 {
        format!("{:.1} GB", v as f64 / 1024.0)
    } else {
        format!("{v} MB")
    }
}

fn print_checks(plan: &Value) {
    for c in plan.get("checks").and_then(Value::as_array).into_iter().flatten() {
        let mark = match s(c, "level") {
            "ok" => "  ok   ",
            "info" => "  info ",
            "warn" => "  WARN ",
            _ => "  ERROR",
        };
        println!("{mark} {}", s(c, "message"));
    }
}

fn print_plan(plan: &Value) {
    println!("Install as: {}", s(plan, "name"));
    if let Some(a) = plan.get("answers").and_then(Value::as_object)
        && !a.is_empty()
    {
        let opts: Vec<String> = a
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
            .collect();
        println!("Options:    {}", opts.join("  "));
    }
    let ports: Vec<String> = plan
        .get("ports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| {
            format!(
                "{} {}/{}",
                s(p, "label"),
                p.get("port").and_then(Value::as_u64).unwrap_or(0),
                s(p, "protocol")
            )
        })
        .collect();
    if !ports.is_empty() {
        println!("Ports:      {}", ports.join(", "));
    }
    if plan.get("https").and_then(Value::as_bool) == Some(true) {
        println!("HTTPS:      turned on after installing");
    }
    println!("Checks:");
    print_checks(plan);
}

pub async fn run(c: &Client, g: &GlobalArgs, cmd: &StoreCmd) -> Result<ExitCode, Error> {
    match cmd {
        StoreCmd::List { query, category } => {
            let list: Value = c.get("/v1/store").await?;
            let words: Vec<String> = query.iter().map(|q| q.to_lowercase()).collect();
            let apps: Vec<Value> = list
                .get("apps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|a| category.as_deref().is_none_or(|cat| s(a, "category") == cat))
                .filter(|a| {
                    let text = format!(
                        "{} {} {} {}",
                        s(a, "name"),
                        s(a, "summary"),
                        s(a, "id"),
                        a.get("tags").map(|t| t.to_string()).unwrap_or_default()
                    )
                    .to_lowercase();
                    words.iter().all(|w| text.contains(w))
                })
                .collect();
            if g.json {
                print_json(&json!(apps));
                return Ok(ExitCode::Ok);
            }
            if let Some(e) = list.get("error").and_then(Value::as_str) {
                eprintln!("note: {e}");
            }
            if apps.is_empty() {
                println!("Nothing found.");
                return Ok(ExitCode::Ok);
            }
            let mut rows = vec![vec![
                "NAME".to_string(),
                "ID".into(),
                "CATEGORY".into(),
                "RAM".into(),
                "INSTALLED".into(),
                "SUMMARY".into(),
            ]];
            for a in &apps {
                let installed: Vec<&str> = a
                    .get("installed")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                rows.push(vec![
                    s(a, "name").to_string(),
                    s(a, "id").to_string(),
                    s(a, "category").to_string(),
                    mb(a.pointer("/badges/ram_mb").and_then(Value::as_u64).unwrap_or(0)),
                    installed.join(","),
                    s(a, "summary").chars().take(60).collect(),
                ]);
            }
            render::table(&rows);
            Ok(ExitCode::Ok)
        }
        StoreCmd::Show { app } => {
            let id = resolve_id(c, app).await?;
            let v: Value = c.get(&format!("/v1/store/apps/{}", enc(&id))).await?;
            if g.json {
                print_json(&v);
                return Ok(ExitCode::Ok);
            }
            let e = &v["entry"];
            println!("{} ({id})", s(e, "name"));
            println!("{}", s(e, "summary"));
            println!();
            println!("{}", s(e, "description").trim());
            if let Some(n) = e.get("notice").and_then(Value::as_str) {
                println!("\nNote: {n}");
            }
            println!(
                "\nWebsite: {}",
                e.pointer("/upstream/website")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
            let installed = v["installed"].as_array().cloned().unwrap_or_default();
            if !installed.is_empty() {
                let names: Vec<&str> = installed.iter().filter_map(Value::as_str).collect();
                println!("Installed here as: {}", names.join(", "));
            }
            let opts = e
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !opts.is_empty() {
                println!("\nInstall options (--option ID=VALUE):");
                for o in &opts {
                    let id = s(o, "id");
                    let current = v["plan"]["answers"]
                        .get(id)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    println!("  {id}: {} [{}]", s(o, "label"), current);
                    let unavailable = v["plan"]["options"]
                        .as_array()
                        .and_then(|l| l.iter().find(|x| s(x, "id") == id))
                        .and_then(|x| x.get("unavailable"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    for ch in o.get("choices").and_then(Value::as_array).into_iter().flatten() {
                        let why = unavailable
                            .get(s(ch, "id"))
                            .and_then(Value::as_str)
                            .map(|w| format!("  (not here: {w})"))
                            .unwrap_or_default();
                        println!("      {:<10} {}{why}", s(ch, "id"), s(ch, "label"));
                    }
                    if s(o, "kind") == "switch" {
                        println!("      on | off");
                    }
                }
            }
            println!("\nWith the default options on this server:");
            print_plan(&v["plan"]);
            Ok(ExitCode::Ok)
        }
        StoreCmd::Refresh => {
            let v: Value = c.post("/v1/store/refresh", None).await?;
            if g.json {
                print_json(&v);
            } else {
                println!(
                    "Store catalog: {} apps ({} downloaded).",
                    v["apps"].as_u64().unwrap_or(0),
                    v["downloaded"].as_u64().unwrap_or(0)
                );
            }
            Ok(ExitCode::Ok)
        }
        StoreCmd::Install {
            app,
            name,
            options,
            ports,
            https,
            no_https,
        } => {
            let id = resolve_id(c, app).await?;
            let port_map: Map<String, Value> = kv(ports, "--port")?
                .into_iter()
                .map(|(k, v)| {
                    let n: u16 = v
                        .as_str()
                        .unwrap_or_default()
                        .parse()
                        .map_err(|_| Error::Usage(format!("--port {k}: give a port number")))?;
                    Ok((k, json!(n)))
                })
                .collect::<Result<_, Error>>()?;
            let mut body = json!({
                "options": kv(options, "--option")?,
                "ports": port_map,
            });
            if let Some(n) = name {
                body["name"] = json!(n);
            }
            if *https {
                body["https"] = json!(true);
            } else if *no_https {
                body["https"] = json!(false);
            }
            let path = format!("/v1/store/apps/{}/install", enc(&id));
            let mut dry = body.clone();
            dry["dry_run"] = json!(true);
            let plan: Value = c.post(&path, Some(dry)).await?;
            if g.json && g.dry_run {
                print_json(&plan);
            } else if !g.json {
                print_plan(&plan);
            }
            if g.dry_run {
                return Ok(ExitCode::Ok);
            }
            if plan.get("ok").and_then(Value::as_bool) != Some(true) {
                return Err(Error::Usage("the host checks found problems (see above)".into()));
            }
            // Use the planned name and ports, so what's confirmed is what gets installed.
            body["name"] = plan["name"].clone();
            for p in plan.get("ports").and_then(Value::as_array).into_iter().flatten() {
                if let (Some(var), Some(n)) = (p.get("env").and_then(Value::as_str), p.get("port")) {
                    body["ports"][var] = n.clone();
                }
            }
            confirm(g, &format!("Install {} as '{}'", id, s(&plan, "name")))?;
            let r: JobRef = c.post(&path, Some(body)).await?;
            let code = follow(c, g, r).await?;
            if !g.json && code == ExitCode::Ok {
                let app_name = s(&plan, "name");
                let st: Value = c
                    .get(&format!("/v1/apps/{}/store", enc(app_name)))
                    .await
                    .unwrap_or(Value::Null);
                for line in st
                    .get("after_install")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    println!("next: {}", line.as_str().unwrap_or_default());
                }
                if st
                    .get("credentials")
                    .and_then(Value::as_array)
                    .is_some_and(|c| !c.is_empty())
                {
                    println!("Sign-in details: hearth store credentials {app_name}");
                }
            }
            Ok(code)
        }
        StoreCmd::Link { app, entry } => {
            let id = resolve_id(c, entry).await?;
            let v: Value = c
                .post(
                    &format!("/v1/apps/{}/store", enc(app)),
                    Some(json!({ "entry": id })),
                )
                .await?;
            if g.json {
                print_json(&v);
            } else {
                println!("{app} is linked to {id}.");
            }
            Ok(ExitCode::Ok)
        }
        StoreCmd::Unlink { app } => {
            let v: Value = c
                .post(
                    &format!("/v1/apps/{}/store", enc(app)),
                    Some(json!({ "entry": null })),
                )
                .await?;
            if g.json {
                print_json(&v);
            } else {
                println!("{app} is no longer linked to a store entry.");
            }
            Ok(ExitCode::Ok)
        }
        StoreCmd::Credentials { app } => {
            let v: Value = c.get(&format!("/v1/apps/{}/credentials", enc(app))).await?;
            if g.json {
                print_json(&v);
            } else if v.as_array().is_none_or(|a| a.is_empty()) {
                println!("Hearth didn't generate sign-in details for {app}.");
            } else {
                for cr in v.as_array().into_iter().flatten() {
                    println!("{}: {}", s(cr, "label"), s(cr, "value"));
                }
            }
            Ok(ExitCode::Ok)
        }
    }
}
