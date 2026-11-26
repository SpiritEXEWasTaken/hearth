//! Migration from CasaOS.
//!
//! - `report`: read-only. For each CasaOS app: what adopting would do, the file diff, decisions,
//!   and the optional fixes (each needs one restart).
//! - `backup`: archives CasaOS itself and dumps every Postgres database of its apps.
//! - `adopt`: Hearth takes over the app's files. Running containers are left alone (no restart):
//!   start/stop/restart work on them as they are, and the first change recreates them under
//!   Hearth's health gate. With fixes, that recreate happens now, with automatic rollback.
//! - `undo`: hands the app back (Hearth's files are moved aside; CasaOS's were never edited).
//!
//! CasaOS's own files are only ever read.

use super::db::Desired;
use super::engine;
use super::jobs::{JobError, JobHandle};
use super::{State, apps};
use crate::compose_norm;
use crate::fixes::{self, Fix};
use hearth_core::model::{DockerEndpoint, ManagedBy};
use hearth_core::proto::{BackupInfo, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub casaos: CasaosInfo,
    pub apps: Vec<AppReport>,
    /// Apps Hearth sees that CasaOS doesn't manage (left alone by the migration).
    pub not_casaos: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CasaosInfo {
    pub installed: bool,
    pub version: Option<String>,
    /// systemd state of each CasaOS service.
    pub services: BTreeMap<String, String>,
    pub app_stores: Vec<String>,
    pub app_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppReport {
    pub app: String,
    pub title: String,
    /// "casaos" (not adopted yet) or "adopted".
    pub status: String,
    pub containers: Vec<String>,
    /// Adopting never restarts anything.
    pub adopt_restarts: bool,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    /// Optional fixes (`--fix ID`); each recreates the app once.
    pub fixes: Vec<Fix>,
    /// Postgres services whose database the backup step dumps.
    pub databases: Vec<String>,
    /// CasaOS's file → Hearth's file (secrets hidden).
    pub compose_diff: String,
    /// Names of the variables that go into .env.
    pub env_vars: Vec<String>,
}

const CASAOS_SERVICES: &[&str] = &[
    "casaos",
    "casaos-gateway",
    "casaos-app-management",
    "casaos-local-storage",
    "casaos-message-bus",
    "casaos-user-service",
];

fn casaos_info() -> CasaosInfo {
    let mut info = CasaosInfo::default();
    if let Ok(t) = std::fs::read_to_string("/var/lib/casaos/baseinfo.conf") {
        info.installed = true;
        info.version = serde_json::from_str::<Value>(&t)
            .ok()
            .and_then(|v| v.get("v").and_then(Value::as_str).map(str::to_string));
    }
    if let Ok(t) = std::fs::read_to_string("/etc/casaos/app-management.conf") {
        info.installed = true;
        info.app_stores = t
            .lines()
            .filter_map(|l| l.trim().strip_prefix("appstore"))
            .filter_map(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()))
            .collect();
    }
    if let Ok(t) = std::fs::read_to_string("/var/lib/casaos/1/app_order.json") {
        info.app_order = serde_json::from_str::<Value>(&t)
            .ok()
            .and_then(|v| v.get("data").cloned())
            .and_then(|d| serde_json::from_value(d).ok())
            .unwrap_or_default();
    }
    for s in CASAOS_SERVICES {
        let state = std::process::Command::new("systemctl")
            .args(["is-active", &format!("{s}.service")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        info.services.insert(s.to_string(), state);
    }
    info
}

/// What Hearth would write for a CasaOS app (before fixes).
async fn plan_app(st: &State, app: &str) -> Result<(AppReport, compose_norm::Normalized, String), String> {
    let raw = st.helper.casaos_app(app).await?;
    let today = engine::today();
    let empty = BTreeMap::new();
    let exists = |p: &str| std::path::Path::new(p).exists();
    let n = compose_norm::normalize(
        &raw,
        &compose_norm::Options {
            app,
            puid: st.host.puid,
            pgid: st.host.pgid,
            tz: &st.host.tz,
            sets: &empty,
            existing_env: &empty,
            path_exists: &exists,
            today: &today,
        },
    );
    let databases = postgres_services(&n.compose);
    let report = AppReport {
        app: app.to_string(),
        title: hearth_casaos::ComposeFile::parse(&raw, app)
            .ok()
            .and_then(|c| c.title())
            .unwrap_or_else(|| app.to_string()),
        status: "casaos".into(),
        containers: Vec::new(),
        adopt_restarts: false,
        decisions: n.decisions.clone(),
        warnings: n.warnings.clone(),
        errors: n.errors.clone(),
        fixes: fixes::available(&n.compose, fixes::host_is_raspberry_pi()),
        databases,
        compose_diff: engine::unified_diff(&raw, &n.compose),
        env_vars: n.env.keys().cloned().collect(),
    };
    Ok((report, n, today))
}

fn postgres_services(compose: &str) -> Vec<String> {
    hearth_casaos::ComposeFile::parse(compose, "compose.yaml")
        .map(|c| {
            c.services()
                .into_iter()
                .filter(|s| {
                    s.image.as_deref().is_some_and(|i| {
                        i.contains("postgres") || i.contains("pgvecto") || i.contains("vectorchord")
                    })
                })
                .map(|s| s.name)
                .collect()
        })
        .unwrap_or_default()
}

pub async fn report(st: &State) -> Result<Report, String> {
    let casaos = casaos_info();
    let (all, _) = apps::list(st).await;
    let adopted: Vec<String> = engine::hearth_apps(st).await.unwrap_or_default();
    let mut out = Report {
        casaos,
        apps: Vec::new(),
        not_casaos: Vec::new(),
        notes: Vec::new(),
    };
    for a in &all {
        let s = &a.summary;
        let is_casaos_dir = a
            .working_dir
            .as_deref()
            .is_some_and(|w| w.starts_with(apps::CASAOS_APPS_DIR));
        if s.kind != hearth_core::model::AppKind::Compose {
            continue;
        }
        if s.endpoint != Some(DockerEndpoint::Root) || !(s.managed_by == ManagedBy::Casaos || is_casaos_dir) {
            out.not_casaos.push(format!(
                "{} ({:?}, {})",
                s.id,
                s.managed_by,
                s.endpoint.as_ref().map(|e| e.label()).unwrap_or_default()
            ));
            continue;
        }
        let containers: Vec<String> = s
            .containers
            .iter()
            .map(|c| format!("{} ({})", c.name, c.state))
            .collect();
        if adopted.contains(&s.id) {
            let fixes = match st
                .helper
                .call(&Request::ReadAppFiles {
                    app: s.id.clone(),
                    include_env: false,
                })
                .await
            {
                Ok(v) => fixes::available(
                    v.get("compose").and_then(Value::as_str).unwrap_or_default(),
                    fixes::host_is_raspberry_pi(),
                ),
                Err(_) => Vec::new(),
            };
            out.apps.push(AppReport {
                app: s.id.clone(),
                title: s.title.clone(),
                status: "adopted".into(),
                containers,
                adopt_restarts: false,
                decisions: Vec::new(),
                warnings: Vec::new(),
                errors: Vec::new(),
                fixes,
                databases: Vec::new(),
                compose_diff: String::new(),
                env_vars: Vec::new(),
            });
            continue;
        }
        match plan_app(st, &s.id).await {
            Ok((mut r, _, _)) => {
                r.containers = containers;
                out.apps.push(r);
            }
            Err(e) => out
                .notes
                .push(format!("{}: cannot read its CasaOS file: {e}", s.id)),
        }
    }
    out.notes.push(
        "Adopting an app doesn't restart it. Its containers stay as CasaOS created them; the first change you make \
         (an update, a fix, an apply) recreates them once, with a health check and automatic rollback."
            .into(),
    );
    out.notes.push(
        "CasaOS keeps running during the migration. Don't change adopted apps from CasaOS's dashboard."
            .into(),
    );
    Ok(out)
}

pub async fn backup(st: &State, job: &JobHandle) -> Result<Value, JobError> {
    let j = job.clone();
    let casaos: BackupInfo = serde_json::from_value(
        st.helper
            .call_stream(&Request::BackupCasaos, move |l| j.log(format!("  {l}")))
            .await?,
    )
    .map_err(|e| e.to_string())?;
    let mut dumps = Vec::new();
    let report = report(st).await?;
    for a in &report.apps {
        let services = if a.status == "casaos" {
            a.databases.clone()
        } else {
            match st
                .helper
                .call(&Request::ReadAppFiles {
                    app: a.app.clone(),
                    include_env: false,
                })
                .await
            {
                Ok(v) => postgres_services(v.get("compose").and_then(Value::as_str).unwrap_or_default()),
                Err(_) => Vec::new(),
            }
        };
        for svc in services {
            let j = job.clone();
            let r = st
                .helper
                .call_stream(
                    &Request::DumpPostgres {
                        app: a.app.clone(),
                        service: svc.clone(),
                    },
                    move |l| j.log(format!("  {l}")),
                )
                .await;
            match r {
                Ok(v) => dumps.push(v),
                Err(e) => {
                    return Err(JobError::failed(format!(
                        "database dump of {}/{svc} failed: {e}",
                        a.app
                    )));
                }
            }
        }
    }
    Ok(json!({ "casaos": casaos, "database_dumps": dumps }))
}

pub async fn adopt(
    st: &State,
    job: &JobHandle,
    app: &str,
    fix_ids: &[String],
    actor: &str,
) -> Result<Value, JobError> {
    if engine::hearth_apps(st).await?.iter().any(|a| a == app) {
        if fix_ids.is_empty() {
            return Err(JobError::failed(format!("{app} is already adopted")));
        }
        // Already adopted: just apply the fixes.
        return apply_fixes(st, job, app, fix_ids, actor).await;
    }
    let (all, _) = apps::list(st).await;
    let Some(found) = all.iter().find(|a| a.summary.id == app) else {
        return Err(JobError::failed(format!("no app named {app}")));
    };
    let is_casaos = found
        .working_dir
        .as_deref()
        .is_some_and(|w| w.starts_with(apps::CASAOS_APPS_DIR));
    if !is_casaos || found.summary.endpoint != Some(DockerEndpoint::Root) {
        return Err(JobError::failed(format!("{app} isn't a CasaOS app")));
    }
    let (report, n, _) = plan_app(st, app).await?;
    if !report.errors.is_empty() {
        return Err(JobError::failed(report.errors.join("; ")));
    }
    for d in &report.decisions {
        job.log(format!("decision: {d}"));
    }
    for w in &report.warnings {
        job.log(format!("warning: {w}"));
    }
    let env_text = compose_norm::render_env(&n.env)?;
    let running = found.summary.containers.iter().any(|c| c.state == "running");
    st.with_db(|db| {
        let _ = db.set_desired(
            app,
            if running {
                Desired::Running
            } else {
                Desired::Stopped
            },
        );
    });
    let rev = st
        .helper
        .call(&Request::WriteAppFiles {
            app: app.to_string(),
            compose: n.compose.clone(),
            env: env_text,
            message: "adopted from CasaOS (containers left running as they were)".into(),
            actor: actor.to_string(),
        })
        .await?;
    st.with_db(|db| db.record_migration(app, "adopted", &[]).ok());
    job.log(format!(
        "adopted {app}: Hearth now manages /var/lib/hearth/apps/{app} (revision {}); nothing was restarted",
        rev.get("revision").and_then(Value::as_u64).unwrap_or(1)
    ));
    if fix_ids.is_empty() {
        if !report.fixes.is_empty() {
            job.log(format!(
                "fixes available (each recreates the app once): {}",
                report
                    .fixes
                    .iter()
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Ok(json!({ "app": app, "adopted": true, "restarted": false }));
    }
    apply_fixes(st, job, app, fix_ids, actor).await
}

async fn apply_fixes(
    st: &State,
    job: &JobHandle,
    app: &str,
    fix_ids: &[String],
    actor: &str,
) -> Result<Value, JobError> {
    let v = st
        .helper
        .call(&Request::ReadAppFiles {
            app: app.to_string(),
            include_env: true,
        })
        .await?;
    let compose = v
        .get("compose")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let env = v
        .get("env")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let today = engine::today();
    let (fixed, decisions) = fixes::apply(&compose, fix_ids, fixes::host_is_raspberry_pi(), &today)?;
    if decisions.is_empty() {
        job.log("the fixes are already applied");
        return Ok(json!({ "app": app, "fixed": [] }));
    }
    for d in &decisions {
        job.log(format!("fix: {d}"));
    }
    let result = engine::apply_files(
        st,
        job,
        app,
        &fixed,
        &env,
        &format!("fixes: {}", fix_ids.join(", ")),
        actor,
    )
    .await?;
    st.with_db(|db| db.record_migration(app, "adopted", fix_ids).ok());
    Ok(json!({ "app": app, "fixed": decisions, "result": result }))
}

pub async fn undo(st: &State, job: &JobHandle, app: &str) -> Result<Value, JobError> {
    if !engine::hearth_apps(st).await?.iter().any(|a| a == app) {
        return Err(JobError::failed(format!("{app} isn't adopted")));
    }
    // CasaOS finds its apps through the containers' compose labels, so containers that Hearth
    // recreated must be recreated by CasaOS again before it can manage them.
    let labels = project_working_dirs(st, app).await?;
    let recreated_by_hearth = labels.iter().any(|w| w.starts_with(apps::HEARTH_APPS_DIR));
    if recreated_by_hearth {
        let v = st
            .helper
            .call(&Request::ReadAppFiles {
                app: app.to_string(),
                include_env: false,
            })
            .await?;
        let hearth_compose = v
            .get("compose")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // CasaOS only labels containers as its own when it creates them, so: take Hearth's
        // containers down (data stays: it lives in bind mounts and volumes), let CasaOS install
        // the app from its own file, and bring Hearth's back if that fails.
        job.log("removing Hearth's containers so CasaOS can create its own (data is kept)");
        let down = Request::Compose {
            app: app.to_string(),
            action: hearth_core::proto::ComposeAction::Down { volumes: false },
            services: Vec::new(),
        };
        let j = job.clone();
        st.helper
            .call_stream(&down, move |l| j.log(format!("  {l}")))
            .await?;
        job.log("asking CasaOS to install the app from its own file");
        let j = job.clone();
        let handed = st
            .helper
            .call_stream(&Request::CasaosApply { app: app.to_string() }, move |l| {
                j.log(format!("  {l}"))
            })
            .await;
        if let Err(e) = handed {
            return Err(restore_hearth(st, job, app, &format!("casaos-cli failed: {e}")).await);
        }
        // casaos-cli returns before CasaOS has done anything: wait until it really has.
        job.log("waiting for CasaOS to recreate the containers (up to 10 minutes)");
        let start = std::time::Instant::now();
        loop {
            let dirs = project_working_dirs(st, app).await?;
            if !dirs.is_empty() && dirs.iter().all(|w| w.starts_with(apps::CASAOS_APPS_DIR)) {
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(600) {
                return Err(
                    restore_hearth(st, job, app, "CasaOS didn't create the app within 10 minutes").await,
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let services: Vec<String> = super::health::containers_by_service(st, app)
            .await?
            .into_keys()
            .collect();
        let spec = super::health::GateSpec {
            app: app.to_string(),
            services,
            web: engine::web_of(&hearth_compose),
            timeout: std::time::Duration::from_secs(
                st.with_db(|db| db.setting_u64("health_timeout")).max(30),
            ),
            stable: std::time::Duration::from_secs(15),
        };
        super::health::wait_healthy(st, job, &spec)
            .await
            .map_err(|e| JobError::failed(format!("CasaOS recreated the app but it isn't healthy: {e}")))?;
    }
    let moved = st
        .helper
        .call(&Request::RemoveAppDir { app: app.to_string() })
        .await?;
    st.with_db(|db| {
        let _ = db.forget_app(app);
        let _ = db.record_migration(app, "undone", &[]);
    });
    job.log(format!(
        "Hearth no longer manages {app}; its files were moved to {}. CasaOS manages it again.",
        moved.get("moved_to").and_then(Value::as_str).unwrap_or("?")
    ));
    Ok(json!({ "app": app, "undone": true, "recreated_by_casaos": recreated_by_hearth }))
}

/// Hand-back failed: bring Hearth's own version of the app back up.
async fn restore_hearth(st: &State, job: &JobHandle, app: &str, why: &str) -> JobError {
    job.log(format!("{why}; bringing Hearth's version back"));
    let up = Request::Compose {
        app: app.to_string(),
        action: hearth_core::proto::ComposeAction::Up {
            no_start: false,
            no_deps: false,
            force_recreate: false,
        },
        services: Vec::new(),
    };
    let j = job.clone();
    match st.helper.call_stream(&up, move |l| j.log(format!("  {l}"))).await {
        Ok(_) => JobError::rolled_back(format!(
            "{why}. Hearth still manages {app} and it is running again."
        )),
        Err(e) => {
            JobError::rollback_failed(format!("{why}. Bringing Hearth's version back also failed: {e}"))
        }
    }
}

/// The compose working-dir label of each container of a project.
async fn project_working_dirs(st: &State, app: &str) -> Result<Vec<String>, String> {
    let list = st.helper.containers(&DockerEndpoint::Root).await?;
    Ok(list
        .iter()
        .filter(|c| {
            c.pointer("/Labels/com.docker.compose.project")
                .and_then(Value::as_str)
                == Some(app)
        })
        .map(|c| {
            c.pointer("/Labels/com.docker.compose.project.working_dir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect())
}
