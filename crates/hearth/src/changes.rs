//! CLI commands that change things: they show a plan, ask for confirmation (or require
//! `--yes` when there's no terminal), start a job, and follow it to the end.

use crate::cli::{AppCmd, Command, GlobalArgs, ImagesCmd, JobCmd};
use crate::client::Client;
use crate::render;
use hearth_core::model::{ApplyPlan, HistoryEntry, ImageEntry, Job, JobRef, JobStatus, UpdatePlan};
use hearth_core::output::Envelope;
use hearth_core::proto::BackupInfo;
use hearth_core::{Error, ExitCode};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~@".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn parse_sets(sets: &[String]) -> Result<BTreeMap<String, String>, Error> {
    sets.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.to_string()))
                .filter(|(k, _)| !k.is_empty())
                .ok_or_else(|| Error::Usage(format!("--set needs KEY=VALUE, got '{s}'")))
        })
        .collect()
}

fn read_file(path: &std::path::Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|e| Error::Usage(format!("{}: {e}", path.display())))
}

/// Asks "Continue? [y/N]" on a terminal. Without a terminal, `--yes` is required (exit 4).
fn confirm(g: &GlobalArgs, what: &str) -> Result<(), Error> {
    if g.yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() || g.json {
        return Err(Error::NeedsConfirmation(what.to_string()));
    }
    eprint!("{what}. Continue? [y/N] ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(Error::Io)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err(Error::Other("cancelled".into()))
    }
}

/// Follows a job until it finishes, printing progress to stderr. Returns the matching exit code.
pub async fn follow(c: &Client, g: &GlobalArgs, job_ref: JobRef) -> Result<ExitCode, Error> {
    if g.background {
        if g.json {
            println!(
                "{}",
                serde_json::to_string(&Envelope::success(&job_ref)).unwrap_or_default()
            );
        } else {
            println!(
                "Started job {}. Follow it with: hearth job wait {}",
                job_ref.job, job_ref.job
            );
        }
        return Ok(ExitCode::Ok);
    }
    let mut after = 0usize;
    loop {
        let job: Job = c
            .get(&format!("/v1/jobs/{}?after={after}", enc(&job_ref.job)))
            .await?;
        for line in &job.events {
            eprintln!("{line}");
        }
        after = job.events_from + job.events.len();
        if job.status.is_done() {
            let code = match job.status {
                JobStatus::Succeeded => ExitCode::Ok,
                JobStatus::RolledBack => ExitCode::RolledBack,
                JobStatus::RollbackFailed => ExitCode::RollbackFailed,
                _ => ExitCode::Error,
            };
            if g.json {
                // The full job (all events) as the single JSON document.
                let full: Job = c.get(&format!("/v1/jobs/{}", enc(&job.id))).await?;
                let env = if code == ExitCode::Ok {
                    serde_json::to_string(&Envelope::success(&full))
                } else {
                    serde_json::to_string(&json!({
                        "ok": false,
                        "schema": hearth_core::output::SCHEMA_VERSION,
                        "data": full,
                        "error": {
                            "kind": match job.status {
                                JobStatus::RolledBack => "rolled_back",
                                JobStatus::RollbackFailed => "rollback_failed",
                                _ => "job_failed",
                            },
                            "message": job.error.clone().unwrap_or_default(),
                            "exit_code": code.code(),
                        }
                    }))
                };
                println!("{}", env.unwrap_or_default());
            } else {
                match job.status {
                    JobStatus::Succeeded => eprintln!("Done."),
                    JobStatus::RolledBack => {
                        eprintln!("FAILED, rolled back: {}", job.error.unwrap_or_default())
                    }
                    JobStatus::RollbackFailed => {
                        eprintln!(
                            "FAILED and the rollback failed too: {}",
                            job.error.unwrap_or_default()
                        )
                    }
                    _ => eprintln!("FAILED: {}", job.error.unwrap_or_default()),
                }
            }
            return Ok(code);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

fn print_plan(p: &ApplyPlan) {
    for d in &p.decisions {
        eprintln!("decision: {d}");
    }
    for w in &p.warnings {
        eprintln!("warning: {w}");
    }
    for e in &p.errors {
        eprintln!("error: {e}");
    }
    if !p.compose_diff.is_empty() {
        eprintln!("{}", p.compose_diff.trim_end());
    }
    if !p.env_changes.is_empty() {
        eprintln!(".env: {}", p.env_changes.join(", "));
    }
}

fn print_update_plan(p: &UpdatePlan) {
    if p.services.is_empty() {
        eprintln!("{}: already up to date.", p.app);
        return;
    }
    for s in &p.services {
        let from = s
            .running_version
            .clone()
            .unwrap_or_else(|| short(s.running_digest.as_deref()));
        let to = s
            .new_version
            .clone()
            .unwrap_or_else(|| short(s.new_digest.as_deref()));
        eprintln!(
            "{} {}: {from} → {to} (same tag: {})",
            p.app, s.service, s.reference
        );
        if let Some(t) = &s.newer_in_track {
            eprintln!(
                "  note: a newer tag exists ({t}); change the tag with `hearth app apply` if you want it"
            );
        }
        if let Some(t) = &s.newer_major {
            eprintln!("  note: a new major version exists ({t}); it is never applied automatically");
        }
    }
    if p.backup_first {
        eprintln!("A database image changes: a backup is taken first.");
    }
}

fn short(d: Option<&str>) -> String {
    d.map(|d| {
        let h = d.split_once(':').map(|(_, h)| h).unwrap_or(d);
        format!("@{}", &h[..h.len().min(12)])
    })
    .unwrap_or_else(|| "?".into())
}

fn dry_out<T: serde::Serialize>(g: &GlobalArgs, data: &T) {
    if g.json {
        println!(
            "{}",
            serde_json::to_string(&Envelope::success(data)).unwrap_or_default()
        );
    }
}

/// Handles the change commands; returns None for commands that aren't changes.
pub async fn run(cmd: &Command, g: &GlobalArgs) -> Option<Result<ExitCode, Error>> {
    let c = Client::new(&g.socket);
    Some(match cmd {
        Command::Install {
            name,
            file,
            sets,
            no_start,
        } => install(&c, g, name, file, sets, *no_start).await,
        Command::Uninstall { app, delete_data } => uninstall(&c, g, app, *delete_data).await,
        Command::Backup {
            app,
            exclude,
            no_stop,
        } => {
            let body = json!({ "exclude": exclude, "stop": !no_stop });
            async {
                let r: JobRef = c
                    .post(&format!("/v1/apps/{}/backup", enc(app)), Some(body))
                    .await?;
                follow(&c, g, r).await
            }
            .await
        }
        Command::Backups { app } => {
            async {
                let list: Vec<BackupInfo> = c.get(&format!("/v1/apps/{}/backups", enc(app))).await?;
                if g.json {
                    dry_out(g, &list);
                } else if list.is_empty() {
                    println!("No backups of {app}.");
                } else {
                    let mut rows = vec![vec!["FILE".to_string(), "SIZE".to_string()]];
                    rows.extend(
                        list.iter()
                            .map(|b| vec![b.file.clone(), render::bytes(b.size_bytes)]),
                    );
                    render::table(&rows);
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        Command::Restore { app, file } => {
            async {
                if g.dry_run {
                    let v: Value = c
                        .post(
                            &format!("/v1/apps/{}/restore", enc(app)),
                            Some(json!({ "file": file, "dry_run": true })),
                        )
                        .await?;
                    dry_out(g, &v);
                    if !g.json {
                        println!("Would restore {app} from {file}.");
                    }
                    return Ok(ExitCode::Ok);
                }
                confirm(
                    g,
                    &format!("Restore {app} from {file} (the app stops; its current data is moved aside)"),
                )?;
                let r: JobRef = c
                    .post(
                        &format!("/v1/apps/{}/restore", enc(app)),
                        Some(json!({ "file": file })),
                    )
                    .await?;
                follow(&c, g, r).await
            }
            .await
        }
        Command::Images(ImagesCmd::List) => {
            async {
                let list: Vec<ImageEntry> = c.get("/v1/images").await?;
                if g.json {
                    dry_out(g, &list);
                } else {
                    let mut rows = vec![vec!["IMAGE".into(), "SIZE".into(), "USED".into(), "ID".into()]];
                    for i in &list {
                        rows.push(vec![
                            if i.tags.is_empty() {
                                "<untagged>".into()
                            } else {
                                i.tags.join(", ")
                            },
                            render::bytes(i.size_bytes),
                            if i.in_use { "yes".into() } else { "no".into() },
                            i.id.trim_start_matches("sha256:").chars().take(12).collect(),
                        ]);
                    }
                    render::table(&rows);
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        Command::Images(ImagesCmd::Prune { keep, unused }) => {
            async {
                let preview: Value = c
                    .post(
                        "/v1/images/prune",
                        Some(json!({ "keep": keep, "unused": unused, "dry_run": true })),
                    )
                    .await?;
                let items: Vec<String> = preview
                    .get("would_remove")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if items.is_empty() {
                    if g.json {
                        dry_out(g, &preview);
                    } else {
                        println!("Nothing to remove.");
                    }
                    return Ok(ExitCode::Ok);
                }
                if g.dry_run {
                    dry_out(g, &preview);
                    if !g.json {
                        for i in &items {
                            println!("would remove {i}");
                        }
                    }
                    return Ok(ExitCode::Ok);
                }
                for i in &items {
                    eprintln!("will remove {i}");
                }
                confirm(g, &format!("Remove {} image(s)", items.len()))?;
                let r: JobRef = c
                    .post(
                        "/v1/images/prune",
                        Some(json!({ "keep": keep, "unused": unused })),
                    )
                    .await?;
                follow(&c, g, r).await
            }
            .await
        }
        Command::Job(JobCmd::List) => {
            async {
                let jobs: Vec<Job> = c.get("/v1/jobs").await?;
                if g.json {
                    dry_out(g, &jobs);
                } else if jobs.is_empty() {
                    println!("No jobs since the daemon started.");
                } else {
                    let mut rows = vec![vec![
                        "ID".into(),
                        "KIND".into(),
                        "APP".into(),
                        "STATUS".into(),
                        "ERROR".into(),
                    ]];
                    for j in &jobs {
                        rows.push(vec![
                            j.id.clone(),
                            j.kind.clone(),
                            j.app.clone(),
                            serde_json::to_value(j.status)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default(),
                            j.error
                                .clone()
                                .unwrap_or_default()
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_string(),
                        ]);
                    }
                    render::table(&rows);
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        Command::Job(JobCmd::Wait { id }) => follow(&c, g, JobRef { job: id.clone() }).await,
        Command::Job(JobCmd::Show { id }) => {
            async {
                let j: Job = c.get(&format!("/v1/jobs/{}", enc(id))).await?;
                if g.json {
                    dry_out(g, &j);
                } else {
                    println!("{} {} {} ({:?})", j.id, j.kind, j.app, j.status);
                    for e in &j.events {
                        println!("{e}");
                    }
                    if let Some(e) = &j.error {
                        println!("error: {e}");
                    }
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        Command::Settings { set } => {
            async {
                if set.len() == 2 {
                    let v: Value = c
                        .call_put("/v1/settings", json!({ "key": set[0], "value": set[1] }))
                        .await?;
                    dry_out(g, &v);
                    if !g.json {
                        println!("{} = {}", set[0], set[1]);
                    }
                } else if set.is_empty() {
                    let v: Value = c.get("/v1/settings").await?;
                    if g.json {
                        dry_out(g, &v);
                    } else if let Some(m) = v.as_object() {
                        for (k, s) in m {
                            println!(
                                "{k} = {}    # {}",
                                s.get("value").and_then(Value::as_str).unwrap_or_default(),
                                s.get("description").and_then(Value::as_str).unwrap_or_default()
                            );
                        }
                    }
                } else {
                    return Err(Error::Usage(
                        "use `hearth settings` or `hearth settings KEY VALUE`".into(),
                    ));
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        Command::App(cmd) => return app(&c, g, cmd).await,
        _ => return None,
    })
}

async fn install(
    c: &Client,
    g: &GlobalArgs,
    name: &str,
    file: &std::path::Path,
    sets: &[String],
    no_start: bool,
) -> Result<ExitCode, Error> {
    let compose = read_file(file)?;
    let sets = parse_sets(sets)?;
    let body = json!({ "app": name, "compose": compose, "sets": sets, "start": !no_start });
    let mut preview = body.clone();
    preview["dry_run"] = json!(true);
    let plan: ApplyPlan = c.post("/v1/apps", Some(preview)).await?;
    if g.dry_run || !plan.errors.is_empty() {
        if g.json {
            dry_out(g, &plan);
        } else {
            print_plan(&plan);
        }
        if !plan.errors.is_empty() {
            return Err(Error::Usage(format!(
                "can't install {name}: {}",
                plan.errors.join("; ")
            )));
        }
        return Ok(ExitCode::Ok);
    }
    // The job itself reports the decisions and warnings.
    let r: JobRef = c.post("/v1/apps", Some(body)).await?;
    follow(c, g, r).await
}

async fn uninstall(c: &Client, g: &GlobalArgs, app: &str, delete_data: bool) -> Result<ExitCode, Error> {
    let what = if delete_data {
        format!("Uninstall {app} and DELETE its data (volumes and /DATA/AppData/{app})")
    } else {
        format!("Uninstall {app} (its data is kept)")
    };
    if g.dry_run {
        let v: Value = c
            .post(
                &format!("/v1/apps/{}/uninstall", enc(app)),
                Some(json!({ "delete_data": delete_data, "dry_run": true })),
            )
            .await?;
        dry_out(g, &v);
        if !g.json {
            println!("Would: {what}");
        }
        return Ok(ExitCode::Ok);
    }
    confirm(g, &what)?;
    let r: JobRef = c
        .post(
            &format!("/v1/apps/{}/uninstall", enc(app)),
            Some(json!({ "delete_data": delete_data })),
        )
        .await?;
    follow(c, g, r).await
}

async fn app(c: &Client, g: &GlobalArgs, cmd: &AppCmd) -> Option<Result<ExitCode, Error>> {
    Some(match cmd {
        AppCmd::Start { app } | AppCmd::Restart { app } | AppCmd::Stop { app } => {
            let verb = match cmd {
                AppCmd::Start { .. } => "start",
                AppCmd::Stop { .. } => "stop",
                _ => "restart",
            };
            async {
                if g.dry_run {
                    let out = json!({ "would": verb, "app": app });
                    dry_out(g, &out);
                    if !g.json {
                        println!("Would {verb} {app}.");
                    }
                    return Ok(ExitCode::Ok);
                }
                if verb == "stop" {
                    confirm(g, &format!("Stop {app} (it stays stopped until you start it)"))?;
                }
                let r: JobRef = c.post(&format!("/v1/apps/{}/{verb}", enc(app)), None).await?;
                follow(c, g, r).await
            }
            .await
        }
        AppCmd::Apply {
            app,
            file,
            sets,
            message,
        } => {
            async {
                let compose = read_file(file)?;
                let sets = parse_sets(sets)?;
                let body = json!({ "compose": compose, "sets": sets, "message": message });
                let mut preview = body.clone();
                preview["dry_run"] = json!(true);
                let plan: ApplyPlan = c
                    .post(&format!("/v1/apps/{}/apply", enc(app)), Some(preview))
                    .await?;
                if !g.json {
                    print_plan(&plan);
                }
                if !plan.errors.is_empty() {
                    if g.json {
                        dry_out(g, &plan);
                    }
                    return Err(Error::Usage(plan.errors.join("; ")));
                }
                if plan.unchanged {
                    if g.json {
                        dry_out(g, &plan);
                    } else {
                        println!("Nothing to change.");
                    }
                    return Ok(ExitCode::Ok);
                }
                if g.dry_run {
                    dry_out(g, &plan);
                    return Ok(ExitCode::Ok);
                }
                confirm(g, &format!("Apply this change to {app}"))?;
                let r: JobRef = c
                    .post(&format!("/v1/apps/{}/apply", enc(app)), Some(body))
                    .await?;
                follow(c, g, r).await
            }
            .await
        }
        AppCmd::Update {
            app: Some(app),
            check: false,
            ..
        } => {
            async {
                let plan: UpdatePlan = c
                    .post(
                        &format!("/v1/apps/{}/update", enc(app)),
                        Some(json!({ "dry_run": true })),
                    )
                    .await?;
                if !g.json {
                    print_update_plan(&plan);
                }
                if plan.services.is_empty() || g.dry_run {
                    dry_out(g, &plan);
                    return Ok(ExitCode::Ok);
                }
                confirm(g, &format!("Update {app}"))?;
                let r: JobRef = c
                    .post(&format!("/v1/apps/{}/update", enc(app)), Some(json!({})))
                    .await?;
                follow(c, g, r).await
            }
            .await
        }
        AppCmd::Update {
            app: None,
            check: false,
            ..
        } => Err(Error::Usage(
            "updates are applied one app at a time: `hearth app update <app>` (use --all only with --check)"
                .into(),
        )),
        AppCmd::History { app } => {
            async {
                let list: Vec<HistoryEntry> = c.get(&format!("/v1/apps/{}/history", enc(app))).await?;
                if g.json {
                    dry_out(g, &list);
                } else {
                    let mut rows = vec![vec!["REV".into(), "WHEN".into(), "BY".into(), "WHAT".into()]];
                    for h in &list {
                        rows.push(vec![
                            h.revision.to_string(),
                            ago(h.time),
                            h.actor.clone(),
                            h.message.clone(),
                        ]);
                    }
                    render::table(&rows);
                }
                Ok(ExitCode::Ok)
            }
            .await
        }
        AppCmd::Revert { app, revision } => {
            async {
                let plan: ApplyPlan = c
                    .post(
                        &format!("/v1/apps/{}/revert", enc(app)),
                        Some(json!({ "revision": revision, "dry_run": true })),
                    )
                    .await?;
                if !g.json {
                    print_plan(&plan);
                }
                if plan.unchanged {
                    if !g.json {
                        println!("Revision {revision} is what's there now.");
                    }
                    dry_out(g, &plan);
                    return Ok(ExitCode::Ok);
                }
                if g.dry_run {
                    dry_out(g, &plan);
                    return Ok(ExitCode::Ok);
                }
                confirm(g, &format!("Revert {app} to revision {revision}"))?;
                let r: JobRef = c
                    .post(
                        &format!("/v1/apps/{}/revert", enc(app)),
                        Some(json!({ "revision": revision })),
                    )
                    .await?;
                follow(c, g, r).await
            }
            .await
        }
        _ => return None,
    })
}

fn ago(t: i64) -> String {
    let d = crate::daemon::db::now() - t;
    match d {
        d if d < 120 => format!("{d}s ago"),
        d if d < 7200 => format!("{}m ago", d / 60),
        d if d < 172_800 => format!("{}h ago", d / 3600),
        d => format!("{}d ago", d / 86400),
    }
}
