//! The app engine: every change to a Hearth-managed app.
//!
//! Each operation runs inside a job ([`super::jobs`]), waits for the result, checks health and
//! rolls back on failure. Only apps whose files live in `/var/lib/hearth/apps` can be changed;
//! CasaOS and rootless apps are read-only until they're adopted.

use super::db::{Desired, now};
use super::health::{self, GateSpec, Web};
use super::jobs::{JobError, JobHandle};
use super::{State, apps};
use crate::compose_norm::{self, Normalized};
use hearth_core::Error;
use hearth_core::model::{ApplyPlan, DockerEndpoint, ServiceUpdate, UpdatePlan};
use hearth_core::proto::{self, BackupInfo, ComposeAction, Request};
use hearth_core::redact::redact_compose_text;
use hearth_registry::ImageRef;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

/// Image names that hold databases: a backup is taken before updating them.
const DATABASE_IMAGES: &[&str] = &[
    "postgres",
    "mariadb",
    "mysql",
    "mongo",
    "pgvecto",
    "timescale",
    "couchdb",
];

// ----- small helpers -----

pub async fn hearth_apps(st: &State) -> Result<Vec<String>, String> {
    st.helper.call_as(&Request::ListHearthApps).await
}

/// Errors unless `app` is a Hearth-managed app.
pub async fn require_hearth_app(st: &State, app: &str) -> Result<(), Error> {
    if hearth_apps(st)
        .await
        .map_err(Error::Other)?
        .iter()
        .any(|a| a == app)
    {
        return Ok(());
    }
    let (all, _) = apps::list(st).await;
    match all.iter().find(|a| a.summary.id == app) {
        Some(a) => Err(Error::ReadOnly(match a.summary.managed_by {
            hearth_core::model::ManagedBy::Casaos => format!(
                "'{app}' is a CasaOS app; Hearth can change it once it's migrated (hearth migrate casaos)"
            ),
            _ => format!("'{app}' isn't managed by Hearth (it's shown read-only)"),
        })),
        None => Err(Error::NotFound {
            what: "app",
            name: app.to_string(),
        }),
    }
}

async fn read_files(st: &State, app: &str) -> Result<(String, String), String> {
    let v = st
        .helper
        .call(&Request::ReadAppFiles {
            app: app.to_string(),
            include_env: true,
        })
        .await?;
    Ok((
        v.get("compose")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        v.get("env")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    ))
}

async fn write_files(
    st: &State,
    app: &str,
    compose: &str,
    env: &str,
    message: &str,
    actor: &str,
) -> Result<u64, String> {
    let v = st
        .helper
        .call(&Request::WriteAppFiles {
            app: app.to_string(),
            compose: compose.to_string(),
            env: env.to_string(),
            message: message.to_string(),
            actor: actor.to_string(),
        })
        .await?;
    Ok(v.get("revision").and_then(Value::as_u64).unwrap_or(0))
}

async fn compose(
    st: &State,
    job: &JobHandle,
    app: &str,
    action: ComposeAction,
    services: &[String],
) -> Result<(), String> {
    let j = job.clone();
    st.helper
        .call_stream(
            &Request::Compose {
                app: app.to_string(),
                action,
                services: services.to_vec(),
            },
            move |line| {
                if !is_layer_noise(line) {
                    j.log(format!("  {line}"))
                }
            },
        )
        .await
        .map(|_| ())
}

/// Per-layer pull progress ("39c2ddfd6010 Downloading 3.1MB") is too noisy for a job log.
fn is_layer_noise(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let first = words.next().unwrap_or_default();
    first.len() == 12 && first.chars().all(|c| c.is_ascii_hexdigit())
}

async fn compose_config(st: &State, app: &str) -> Result<Value, String> {
    st.helper
        .call(&Request::ComposeConfig { app: app.to_string() })
        .await
}

fn service_names(config: &Value) -> Vec<String> {
    config
        .get("services")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Services that are new or whose resolved definition changed.
pub fn changed_services(old: &Value, new: &Value) -> Vec<String> {
    let empty = serde_json::Map::new();
    let old_s = old.get("services").and_then(Value::as_object).unwrap_or(&empty);
    let new_s = new.get("services").and_then(Value::as_object).unwrap_or(&empty);
    new_s
        .iter()
        .filter(|(name, def)| old_s.get(*name) != Some(def))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The web endpoint from `x-hearth.web`, for the health gate and the app's link.
pub fn web_of(compose_text: &str) -> Option<Web> {
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(compose_text).ok()?;
    let web = doc.get("x-hearth")?.get("web")?;
    let port = match web.get("port")? {
        serde_yaml_ng::Value::Number(n) => n.as_u64()? as u16,
        serde_yaml_ng::Value::String(s) => s.parse().ok()?,
        _ => return None,
    };
    let service = web
        .get("service")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| {
            doc.get("services")?
                .as_mapping()?
                .keys()
                .next()?
                .as_str()
                .map(str::to_string)
        })?;
    let path = web
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    let health = web.get("health").and_then(|p| p.as_str()).map(str::to_string);
    Some(Web {
        service,
        port,
        path,
        health,
    })
}

/// Container paths listed in `x-hearth.backup.exclude`.
fn backup_excludes(compose_text: &str) -> Vec<String> {
    serde_yaml_ng::from_str::<serde_yaml_ng::Value>(compose_text)
        .ok()
        .and_then(|d| {
            d.get("x-hearth")?
                .get("backup")?
                .get("exclude")?
                .as_sequence()
                .map(|s| s.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        })
        .unwrap_or_default()
}

fn gate_spec(st: &State, app: &str, services: Vec<String>, compose_text: &str) -> GateSpec {
    GateSpec {
        app: app.to_string(),
        services,
        web: web_of(compose_text),
        timeout: Duration::from_secs(st.with_db(|db| db.setting_u64("health_timeout")).max(30)),
        stable: Duration::from_secs(15),
    }
}

fn desired(st: &State, app: &str) -> Desired {
    st.with_db(|db| db.desired(app).ok().flatten())
        .unwrap_or(Desired::Running)
}

fn set_desired(st: &State, app: &str, d: Desired) {
    st.with_db(|db| {
        if let Err(e) = db.set_desired(app, d) {
            tracing::warn!("cannot save desired state: {e}");
        }
    });
}

/// Listening TCP ports and bound UDP ports on the host (from /proc/net).
pub fn ports_in_use() -> HashSet<(String, u16)> {
    let mut out = HashSet::new();
    for (file, proto, listen_only) in [
        ("/proc/net/tcp", "tcp", true),
        ("/proc/net/tcp6", "tcp", true),
        ("/proc/net/udp", "udp", false),
        ("/proc/net/udp6", "udp", false),
    ] {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            if listen_only && f[3] != "0A" {
                continue;
            }
            if let Some(port) = f[1]
                .rsplit(':')
                .next()
                .and_then(|p| u16::from_str_radix(p, 16).ok())
            {
                out.insert((proto.to_string(), port));
            }
        }
    }
    out
}

/// Published host ports of a compose text: (protocol, port).
fn published_ports(compose_text: &str) -> Vec<(String, u16)> {
    let Ok(c) = hearth_casaos::ComposeFile::parse(compose_text, "compose.yaml") else {
        return Vec::new();
    };
    c.services()
        .iter()
        .flat_map(|s| s.ports.clone())
        .filter_map(|p| {
            let published = p.published?;
            let first = published.split('-').next()?.parse().ok()?;
            Some((p.protocol.to_ascii_lowercase(), first))
        })
        .collect()
}

fn env_changes(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in new {
        match old.get(k) {
            None => out.push(format!("added {k}")),
            Some(o) if o != v => out.push(format!("changed {k}")),
            _ => {}
        }
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            out.push(format!("removed {k}"));
        }
    }
    out
}

pub fn unified_diff(old: &str, new: &str) -> String {
    let (old, new) = (redact_compose_text(old), redact_compose_text(new));
    similar::TextDiff::from_lines(&old, &new)
        .unified_diff()
        .context_radius(3)
        .header("compose.yaml (current)", "compose.yaml (new)")
        .to_string()
}

pub fn today() -> String {
    let days = now().div_euclid(86400);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ----- normalisation and plans -----

/// Normalises a compose text for `app` and builds the plan against the current files.
pub async fn plan_apply(
    st: &State,
    app: &str,
    new_compose: &str,
    sets: &BTreeMap<String, String>,
    current: Option<(&str, &str)>,
) -> (ApplyPlan, Normalized) {
    let existing_env = current
        .map(|(_, e)| compose_norm::parse_env(e))
        .unwrap_or_default();
    let today = today();
    let exists = |p: &str| std::path::Path::new(p).exists();
    let n = compose_norm::normalize(
        new_compose,
        &compose_norm::Options {
            app,
            puid: st.host.puid,
            pgid: st.host.pgid,
            tz: &st.host.tz,
            sets,
            existing_env: &existing_env,
            path_exists: &exists,
            today: &today,
        },
    );
    let mut plan = ApplyPlan {
        app: app.to_string(),
        decisions: n.decisions.clone(),
        warnings: n.warnings.clone(),
        errors: n.errors.clone(),
        ..Default::default()
    };
    if n.errors.is_empty() {
        let env_text = match compose_norm::render_env(&n.env) {
            Ok(t) => t,
            Err(e) => {
                plan.errors.push(e);
                String::new()
            }
        };
        if plan.errors.is_empty()
            && let Err(e) = st
                .helper
                .call(&Request::ValidateCompose {
                    app: app.to_string(),
                    compose: n.compose.clone(),
                    env: env_text,
                })
                .await
        {
            plan.errors.push(format!("docker compose rejects the file: {e}"));
        }
        let old_compose = current.map(|(c, _)| c).unwrap_or("");
        plan.compose_diff = unified_diff(old_compose, &n.compose);
        plan.env_changes = env_changes(&existing_env, &n.env);
        plan.unchanged = old_compose == n.compose && plan.env_changes.is_empty();
        // Ports: new published ports must be free (ports the app already uses are fine).
        let in_use = ports_in_use();
        let already: HashSet<(String, u16)> = published_ports(old_compose).into_iter().collect();
        for p in published_ports(&n.compose) {
            if !already.contains(&p) && in_use.contains(&p) {
                plan.errors
                    .push(format!("port {}/{} is already in use on this host", p.1, p.0));
            }
        }
    }
    (plan, n)
}

pub async fn plan_update(st: &State, app: &str) -> Result<UpdatePlan, Error> {
    let config = compose_config(st, app).await.map_err(Error::Other)?;
    let containers = health::containers_by_service(st, app)
        .await
        .map_err(Error::Other)?;
    let mut plan = UpdatePlan {
        app: app.to_string(),
        ..Default::default()
    };
    for svc in service_names(&config) {
        let Some(reference) = config
            .pointer(&format!("/services/{svc}/image"))
            .and_then(Value::as_str)
        else {
            continue; // built locally
        };
        let Some(cid) = containers.get(&svc) else { continue };
        let info = st
            .helper
            .inspect_container(&DockerEndpoint::Root, cid)
            .await
            .map_err(Error::Other)?;
        let image_id = info.get("Image").and_then(Value::as_str).unwrap_or_default();
        let image = st
            .helper
            .inspect_image(&DockerEndpoint::Root, image_id)
            .await
            .unwrap_or(Value::Null);
        let repo_digests: Vec<String> = image
            .get("RepoDigests")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let running_digest = apps::running_digest(reference, &repo_digests, &image);
        let labels: BTreeMap<String, String> = image
            .pointer("/Config/Labels")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let Ok(r) = ImageRef::parse(reference) else {
            continue;
        };
        if r.digest.is_some() {
            continue; // pinned by digest: change the pin with `apply`
        }
        let remote = match st.registry.tag_digest(&r).await {
            Ok(d) => d,
            Err(e) => {
                return Err(Error::Other(format!("{svc}: cannot check {reference}: {e}")));
            }
        };
        if running_digest.as_deref() == Some(remote.as_str()) {
            continue;
        }
        let cached =
            apps::check_key(reference).and_then(|k| st.with_db(|db| db.image_check(&k).ok().flatten()));
        let new_version = super::versions::version_for(st, &r, &remote).await;
        if DATABASE_IMAGES.iter().any(|d| r.repository.contains(d)) {
            plan.backup_first = true;
        }
        plan.services.push(ServiceUpdate {
            service: svc,
            reference: reference.to_string(),
            running_digest,
            running_version: hearth_registry::client::version_from_labels(&labels),
            new_digest: Some(remote),
            new_version,
            newer_in_track: cached.as_ref().and_then(|c| c.newer_in_track.clone()),
            newer_major: cached.and_then(|c| c.newer_major),
        });
    }
    Ok(plan)
}

// ----- operations -----

pub struct InstallRequest {
    pub app: String,
    pub compose: String,
    pub sets: BTreeMap<String, String>,
    pub start: bool,
    pub actor: String,
}

pub async fn install(st: &State, job: &JobHandle, req: InstallRequest) -> Result<Value, JobError> {
    let app = req.app.as_str();
    if hearth_apps(st).await?.iter().any(|a| a == app) {
        return Err(JobError::failed(format!("an app named '{app}' already exists")));
    }
    let (all, _) = apps::list(st).await;
    if all.iter().any(|a| a.summary.id == app) {
        return Err(JobError::failed(format!(
            "a Docker project or container named '{app}' already exists; pick another name"
        )));
    }
    job.log("checking the compose file against this host");
    let (plan, n) = plan_apply(st, app, &req.compose, &req.sets, None).await;
    for d in &plan.decisions {
        job.log(format!("decision: {d}"));
    }
    for w in &plan.warnings {
        job.log(format!("warning: {w}"));
    }
    if !plan.errors.is_empty() {
        return Err(JobError::failed(plan.errors.join("; ")));
    }
    let env_text = compose_norm::render_env(&n.env)?;
    let rev = write_files(st, app, &n.compose, &env_text, "installed", &req.actor).await?;
    job.log(format!("wrote compose.yaml and .env (revision {rev})"));
    set_desired(
        st,
        app,
        if req.start {
            Desired::Running
        } else {
            Desired::Stopped
        },
    );

    let result = async {
        job.log("pulling images");
        compose(st, job, app, ComposeAction::Pull, &[]).await?;
        job.log(if req.start {
            "starting"
        } else {
            "creating containers (not starting)"
        });
        compose(
            st,
            job,
            app,
            ComposeAction::Up {
                no_start: !req.start,
                no_deps: false,
                force_recreate: false,
            },
            &[],
        )
        .await?;
        if req.start {
            let config = compose_config(st, app).await?;
            health::wait_healthy(st, job, &gate_spec(st, app, service_names(&config), &n.compose)).await?;
        }
        Ok::<(), String>(())
    }
    .await;

    if let Err(e) = result {
        job.log(format!("install failed: {e}"));
        job.log("removing the app again");
        let _ = compose(st, job, app, ComposeAction::Down { volumes: true }, &[]).await;
        let _ = st
            .helper
            .call(&Request::RemoveAppDir { app: app.to_string() })
            .await;
        st.with_db(|db| db.forget_app(app).ok());
        return Err(JobError::rolled_back(format!(
            "install failed and was undone: {e}"
        )));
    }
    Ok(json!({ "app": app, "revision": rev, "decisions": plan.decisions, "warnings": plan.warnings }))
}

#[derive(Debug, Clone, Copy)]
pub enum StateAction {
    Start,
    Stop,
    Restart,
}

pub async fn set_running(
    st: &State,
    job: &JobHandle,
    app: &str,
    action: StateAction,
) -> Result<Value, JobError> {
    let (compose_text, _) = read_files(st, app).await?;
    match action {
        StateAction::Stop => {
            set_desired(st, app, Desired::Stopped);
            job.log(format!(
                "stopping {app} (it stays stopped, also after reboots, until you start it)"
            ));
            compose(st, job, app, ComposeAction::Stop, &[]).await?;
            let running: Vec<String> = running_services(st, app).await?;
            if !running.is_empty() {
                return Err(JobError::failed(format!("still running: {}", running.join(", "))));
            }
            Ok(json!({ "app": app, "state": "stopped" }))
        }
        StateAction::Start | StateAction::Restart => {
            set_desired(st, app, Desired::Running);
            let action = if matches!(action, StateAction::Start) {
                job.log(format!("starting {app}"));
                // Containers that already exist are started as they are (`compose start`);
                // only missing ones are created. This never recreates adopted containers.
                let config = compose_config(st, app).await?;
                let existing = health::containers_by_service(st, app).await?;
                if service_names(&config).iter().all(|s| existing.contains_key(s)) {
                    ComposeAction::Start
                } else {
                    ComposeAction::Up {
                        no_start: false,
                        no_deps: false,
                        force_recreate: false,
                    }
                }
            } else {
                job.log(format!("restarting {app}"));
                ComposeAction::Restart
            };
            compose(st, job, app, action, &[]).await?;
            let config = compose_config(st, app).await?;
            health::wait_healthy(
                st,
                job,
                &gate_spec(st, app, service_names(&config), &compose_text),
            )
            .await?;
            Ok(json!({ "app": app, "state": "running" }))
        }
    }
}

async fn running_services(st: &State, app: &str) -> Result<Vec<String>, String> {
    let list = st.helper.containers(&DockerEndpoint::Root).await?;
    Ok(list
        .iter()
        .filter(|c| {
            c.pointer("/Labels/com.docker.compose.project")
                .and_then(Value::as_str)
                == Some(app)
        })
        .filter(|c| c.get("State").and_then(Value::as_str) == Some("running"))
        .filter_map(|c| {
            c.pointer("/Labels/com.docker.compose.service")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect())
}

/// Applies new (already normalised) files with a health gate and automatic rollback.
pub async fn apply_files(
    st: &State,
    job: &JobHandle,
    app: &str,
    new_compose: &str,
    new_env: &str,
    message: &str,
    actor: &str,
) -> Result<Value, JobError> {
    let (old_compose, old_env) = read_files(st, app).await?;
    if old_compose == new_compose && old_env == new_env {
        job.log("nothing changed");
        return Ok(json!({ "app": app, "changed": [] }));
    }
    let old_config = compose_config(st, app).await?;
    let rev = write_files(st, app, new_compose, new_env, message, actor).await?;
    job.log(format!("saved revision {rev}: {message}"));
    let new_config = match compose_config(st, app).await {
        Ok(c) => c,
        Err(e) => {
            write_files(
                st,
                app,
                &old_compose,
                &old_env,
                "rollback: the new file was invalid",
                "hearth",
            )
            .await?;
            return Err(JobError::rolled_back(format!(
                "the new file is invalid, kept the old one: {e}"
            )));
        }
    };
    let mut changed = changed_services(&old_config, &new_config);
    if !changed.is_empty() && has_foreign_containers(st, app).await {
        // Containers created by another manager (e.g. CasaOS before adoption) are all recreated
        // by Compose on the first change, so all of them go through the health gate.
        changed = service_names(&new_config);
        job.log("first change since adoption: all of the app's containers are recreated once");
    }
    if changed.is_empty() {
        job.log("no container needs to change");
        return Ok(json!({ "app": app, "revision": rev, "changed": [] }));
    }
    job.log(format!("services to recreate: {}", changed.join(", ")));
    let running = desired(st, app) == Desired::Running;
    let attempt = async {
        // No `pull` here: `up --pull missing` fetches only images that aren't on the host yet
        // (e.g. a new tag in the file). Newer builds of an existing tag are `app update`'s job.
        compose(
            st,
            job,
            app,
            ComposeAction::Up {
                no_start: !running,
                no_deps: false,
                force_recreate: false,
            },
            &[],
        )
        .await?;
        if running {
            health::wait_healthy(st, job, &gate_spec(st, app, changed.clone(), new_compose)).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    let Err(e) = attempt else {
        return Ok(json!({ "app": app, "revision": rev, "changed": changed }));
    };
    job.log(format!("the change failed: {e}"));
    job.log("rolling back to the previous files");
    let rollback = async {
        write_files(
            st,
            app,
            &old_compose,
            &old_env,
            &format!("rollback after failed revision {rev}"),
            "hearth",
        )
        .await?;
        compose(
            st,
            job,
            app,
            ComposeAction::Up {
                no_start: !running,
                no_deps: false,
                force_recreate: false,
            },
            &[],
        )
        .await?;
        if running {
            health::wait_healthy(st, job, &gate_spec(st, app, changed.clone(), &old_compose)).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    match rollback {
        Ok(()) => Err(JobError::rolled_back(format!(
            "{e}\nThe previous version is running again."
        ))),
        Err(re) => Err(JobError::rollback_failed(format!(
            "{e}\nRollback also failed: {re}"
        ))),
    }
}

/// True if any of the app's containers wasn't created from Hearth's app folder.
async fn has_foreign_containers(st: &State, app: &str) -> bool {
    let Ok(list) = st.helper.containers(&DockerEndpoint::Root).await else {
        return false;
    };
    list.iter()
        .filter(|c| {
            c.pointer("/Labels/com.docker.compose.project")
                .and_then(Value::as_str)
                == Some(app)
        })
        .any(|c| {
            !c.pointer("/Labels/com.docker.compose.project.working_dir")
                .and_then(Value::as_str)
                .is_some_and(|w| w.starts_with(apps::HEARTH_APPS_DIR))
        })
}

fn rollback_repo(app: &str, service: &str) -> String {
    let svc: String = service
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("hearth-rollback/{app}-{svc}")
}

/// Splits a reference as written into (repository, tag) for the Docker tag API.
pub fn split_ref(reference: &str) -> (String, String) {
    let name = reference.split('@').next().unwrap_or(reference);
    let last_slash = name.rfind('/');
    match name.rfind(':') {
        Some(i) if last_slash.is_none_or(|j| i > j) => (name[..i].to_string(), name[i + 1..].to_string()),
        _ => (name.to_string(), "latest".to_string()),
    }
}

fn timestamp() -> String {
    let secs = now();
    let (y, m, d) = civil(secs.div_euclid(86400));
    let r = secs.rem_euclid(86400);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        r / 3600,
        (r % 3600) / 60,
        r % 60
    )
}

pub async fn update(st: &State, job: &JobHandle, app: &str, actor: &str) -> Result<Value, JobError> {
    job.log("checking registries");
    let plan = plan_update(st, app)
        .await
        .map_err(|e| JobError::failed(e.to_string()))?;
    if plan.services.is_empty() {
        job.log("already up to date");
        return Ok(json!({ "app": app, "updated": [] }));
    }
    for s in &plan.services {
        job.log(format!(
            "{}: {} → {}",
            s.service,
            s.running_version
                .clone()
                .unwrap_or_else(|| short(s.running_digest.as_deref())),
            s.new_version
                .clone()
                .unwrap_or_else(|| short(s.new_digest.as_deref()))
        ));
    }
    let (compose_text, _) = read_files(st, app).await?;
    if plan.backup_first {
        job.log("a database image changes: taking a backup first");
        backup(st, job, app, &[], true).await.map_err(|e| {
            JobError::failed(format!(
                "backup before the update failed, nothing was changed: {}",
                e.message
            ))
        })?;
    }
    let services: Vec<String> = plan.services.iter().map(|s| s.service.clone()).collect();
    job.log("pulling new images");
    compose(st, job, app, ComposeAction::Pull, &services).await?;

    // Keep the current images under rollback tags.
    let containers = health::containers_by_service(st, app).await?;
    let stamp = timestamp();
    let mut changed: Vec<(String, String, String)> = Vec::new(); // (service, reference, rollback ref)
    for s in &plan.services {
        let Some(cid) = containers.get(&s.service) else {
            continue;
        };
        let info = st.helper.inspect_container(&DockerEndpoint::Root, cid).await?;
        let old_id = info
            .get("Image")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let new = st
            .helper
            .inspect_image(&DockerEndpoint::Root, &s.reference)
            .await?;
        let new_id = new.get("Id").and_then(Value::as_str).unwrap_or_default();
        if new_id == old_id {
            job.log(format!(
                "{}: the pulled image is the one already running",
                s.service
            ));
            continue;
        }
        let repo = rollback_repo(app, &s.service);
        st.helper
            .call(&Request::TagImage {
                source: old_id.clone(),
                repo: repo.clone(),
                tag: stamp.clone(),
            })
            .await?;
        job.log(format!("{}: kept the current image as {repo}:{stamp}", s.service));
        changed.push((s.service.clone(), s.reference.clone(), format!("{repo}:{stamp}")));
    }
    if changed.is_empty() {
        return Ok(json!({ "app": app, "updated": [] }));
    }
    let svcs: Vec<String> = changed.iter().map(|c| c.0.clone()).collect();
    let running = desired(st, app) == Desired::Running;
    let up = ComposeAction::Up {
        no_start: !running,
        no_deps: true,
        force_recreate: false,
    };
    let attempt = async {
        compose(st, job, app, up.clone(), &svcs).await?;
        if running {
            health::wait_healthy(st, job, &gate_spec(st, app, svcs.clone(), &compose_text)).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = attempt {
        job.log(format!("the update failed: {e}"));
        job.log("rolling back to the previous images");
        let rollback = async {
            for (_, reference, rb) in &changed {
                let (repo, tag) = split_ref(reference);
                st.helper
                    .call(&Request::TagImage {
                        source: rb.clone(),
                        repo,
                        tag,
                    })
                    .await?;
            }
            compose(
                st,
                job,
                app,
                ComposeAction::Up {
                    no_start: !running,
                    no_deps: true,
                    force_recreate: true,
                },
                &svcs,
            )
            .await?;
            if running {
                health::wait_healthy(st, job, &gate_spec(st, app, svcs.clone(), &compose_text)).await?;
            }
            Ok::<(), String>(())
        }
        .await;
        return match rollback {
            Ok(()) => Err(JobError::rolled_back(format!(
                "{e}\nThe previous images are running again."
            ))),
            Err(re) => Err(JobError::rollback_failed(format!(
                "{e}\nRollback also failed: {re}"
            ))),
        };
    }
    // Success: keep only the newest rollback image(s) per service.
    let keep = st.with_db(|db| db.setting_u64("image_retention")) as usize;
    for svc in &svcs {
        prune_rollback_images(st, job, app, svc, keep).await;
    }
    tracing::info!(app, actor, "updated {}", svcs.join(", "));
    let st2 = st.clone();
    let app2 = app.to_string();
    tokio::spawn(async move {
        let _ = super::versions::check(&st2, Some(&app2)).await;
    });
    Ok(json!({ "app": app, "updated": svcs }))
}

fn short(d: Option<&str>) -> String {
    d.map(|d| {
        let h = d.split_once(':').map(|(_, h)| h).unwrap_or(d);
        format!("@{}", &h[..h.len().min(12)])
    })
    .unwrap_or_else(|| "?".into())
}

pub async fn prune_rollback_images(st: &State, job: &JobHandle, app: &str, service: &str, keep: usize) {
    let prefix = format!("{}:", rollback_repo(app, service));
    let Ok(images) = st.helper.call(&Request::ListImages).await else {
        return;
    };
    let mut tags: Vec<String> = images
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|i| {
            i.get("RepoTags")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|t| t.as_str().map(str::to_string))
        .filter(|t| t.starts_with(&prefix))
        .collect();
    tags.sort();
    tags.reverse(); // newest first (timestamps sort)
    for old in tags.into_iter().skip(keep) {
        match st.helper.call(&Request::RemoveImage { name: old.clone() }).await {
            Ok(_) => job.log(format!("removed old rollback image {old}")),
            Err(e) => job.log(format!("kept {old}: {e}")),
        }
    }
}

pub async fn uninstall(st: &State, job: &JobHandle, app: &str, delete_data: bool) -> Result<Value, JobError> {
    let config = compose_config(st, app).await.unwrap_or(Value::Null);
    let mut paths = Vec::new();
    for svc in config
        .get("services")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
    {
        for v in svc.get("volumes").and_then(Value::as_array).into_iter().flatten() {
            if v.get("type").and_then(Value::as_str) == Some("bind")
                && let Some(src) = v.get("source").and_then(Value::as_str)
            {
                paths.push(src.to_string());
            }
        }
    }
    if delete_data {
        // The app's own data folders go too (only these two roots; nothing else is ever deleted).
        paths.push(format!("/DATA/AppData/{app}"));
        paths.push(format!("/var/lib/hearth/data/{app}"));
    }
    job.log(format!(
        "removing {app}'s containers{}",
        if delete_data { " and volumes" } else { "" }
    ));
    compose(st, job, app, ComposeAction::Down { volumes: delete_data }, &[]).await?;
    let mut kept: Vec<String> = Vec::new();
    if delete_data && !paths.is_empty() {
        let v = st
            .helper
            .call(&Request::DeleteAppData {
                app: app.to_string(),
                volumes: Vec::new(),
                paths: paths.clone(),
            })
            .await?;
        kept = v
            .get("kept")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        for k in &kept {
            job.log(format!("kept: {k}"));
        }
    } else if !paths.is_empty() {
        job.log(format!("data folders kept: {}", paths.join(", ")));
    }
    let moved = st
        .helper
        .call(&Request::RemoveAppDir { app: app.to_string() })
        .await?;
    job.log(format!(
        "app files moved to {}",
        moved.get("moved_to").and_then(Value::as_str).unwrap_or("?")
    ));
    st.with_db(|db| db.forget_app(app).ok());
    Ok(json!({ "app": app, "data_deleted": delete_data, "kept": kept, "data_folders": paths }))
}

pub async fn backup(
    st: &State,
    job: &JobHandle,
    app: &str,
    extra_exclude: &[String],
    stop: bool,
) -> Result<Value, JobError> {
    let (compose_text, _) = read_files(st, app).await?;
    let mut exclude = backup_excludes(&compose_text);
    exclude.extend(extra_exclude.iter().cloned());
    let was_running = !running_services(st, app).await?.is_empty();
    if stop && was_running {
        job.log("stopping the app for a consistent backup");
        compose(st, job, app, ComposeAction::Stop, &[]).await?;
    }
    let j = job.clone();
    let result = st
        .helper
        .call_stream(
            &Request::CreateBackup {
                app: app.to_string(),
                exclude_targets: exclude,
            },
            move |l| j.log(format!("  {l}")),
        )
        .await;
    if stop && was_running {
        job.log("starting the app again");
        compose(
            st,
            job,
            app,
            ComposeAction::Up {
                no_start: false,
                no_deps: false,
                force_recreate: false,
            },
            &[],
        )
        .await?;
        let config = compose_config(st, app).await?;
        health::wait_healthy(
            st,
            job,
            &gate_spec(st, app, service_names(&config), &compose_text),
        )
        .await?;
    }
    let info: BackupInfo = serde_json::from_value(result?).map_err(|e| e.to_string())?;
    let keep = st.with_db(|db| db.setting_u64("backup_retention")).max(1) as u32;
    let pruned = st
        .helper
        .call(&Request::PruneBackups {
            app: app.to_string(),
            keep,
        })
        .await?;
    if let Some(r) = pruned
        .get("removed")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
    {
        job.log(format!("removed {} old backup(s) (keeping {keep})", r.len()));
    }
    Ok(json!(info))
}

pub async fn restore(st: &State, job: &JobHandle, app: &str, file: &str) -> Result<Value, JobError> {
    if !proto::is_valid_backup_name(file) {
        return Err(JobError::failed("invalid backup name"));
    }
    job.log(format!("stopping {app}"));
    compose(st, job, app, ComposeAction::Stop, &[]).await?;
    let j = job.clone();
    let restored = st
        .helper
        .call_stream(
            &Request::RestoreBackup {
                app: app.to_string(),
                file: file.to_string(),
            },
            move |l| j.log(format!("  {l}")),
        )
        .await?;
    let previous: Vec<String> = restored
        .get("previous_data")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if !previous.is_empty() {
        job.log(format!(
            "the data from before the restore is kept in: {}",
            previous.join(", ")
        ));
    }
    if desired(st, app) == Desired::Running {
        let (compose_text, _) = read_files(st, app).await?;
        compose(
            st,
            job,
            app,
            ComposeAction::Up {
                no_start: false,
                no_deps: false,
                force_recreate: true,
            },
            &[],
        )
        .await?;
        let config = compose_config(st, app).await?;
        if let Err(e) = health::wait_healthy(
            st,
            job,
            &gate_spec(st, app, service_names(&config), &compose_text),
        )
        .await
        {
            return Err(JobError::failed(format!(
                "restored, but the app isn't healthy: {e}\nThe previous data is kept in: {}",
                previous.join(", ")
            )));
        }
    }
    Ok(restored)
}

/// At start-up: make each Hearth app's actual state match its desired state.
pub async fn reconcile(st: &State) {
    let Ok(apps) = hearth_apps(st).await else { return };
    for app in apps {
        let want = desired(st, &app);
        let running = running_services(st, &app).await.unwrap_or_default();
        let action = match (want, running.is_empty()) {
            (Desired::Stopped, false) => Some(StateAction::Stop),
            (Desired::Running, true) => Some(StateAction::Start),
            _ => None,
        };
        if let Some(a) = action {
            tracing::info!(app, ?a, "reconciling to the desired state");
            let st2 = st.clone();
            let app2 = app.clone();
            st.jobs
                .start(st, "reconcile", &app, "hearth", move |job| async move {
                    set_running(&st2, &job, &app2, a).await
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_services_detects_new_and_modified() {
        let old = json!({"services": {"a": {"image": "x:1"}, "b": {"image": "y"}}});
        let new = json!({"services": {"a": {"image": "x:2"}, "b": {"image": "y"}, "c": {"image": "z"}}});
        assert_eq!(
            changed_services(&old, &new),
            vec!["a".to_string(), "c".to_string()]
        );
        assert!(changed_services(&new, &new).is_empty());
    }

    #[test]
    fn web_from_x_hearth() {
        let c = "services:\n  web:\n    image: nginx\n  db:\n    image: pg\nx-hearth:\n  web:\n    port: '8080'\n    path: /admin\n";
        let w = web_of(c);
        assert_eq!(
            w.as_ref().map(|w| (w.service.as_str(), w.port, w.path.as_str())),
            Some(("web", 8080, "/admin"))
        );
        assert!(web_of("services:\n  a:\n    image: x\n").is_none());
    }

    #[test]
    fn refs_split_for_tagging() {
        assert_eq!(
            split_ref("linuxserver/jellyfin:latest"),
            ("linuxserver/jellyfin".into(), "latest".into())
        );
        assert_eq!(split_ref("nginx"), ("nginx".into(), "latest".into()));
        assert_eq!(
            split_ref("localhost:5000/app"),
            ("localhost:5000/app".into(), "latest".into())
        );
        assert_eq!(
            split_ref("ghcr.io/a/b:v3@sha256:abc"),
            ("ghcr.io/a/b".into(), "v3".into())
        );
    }

    #[test]
    fn layer_noise_filter() {
        assert!(is_layer_noise("39c2ddfd6010 Downloading 3.146MB"));
        assert!(!is_layer_noise("Image nginx:1.27-alpine Pulled"));
        assert!(!is_layer_noise("Container web-web-1 Started"));
    }

    #[test]
    fn rollback_names_are_valid_repos() {
        assert_eq!(
            rollback_repo("immich", "Immich_Server"),
            "hearth-rollback/immich-immich_server"
        );
    }

    #[test]
    fn env_change_list_never_shows_values() {
        let mut a = BTreeMap::new();
        a.insert("A".to_string(), "1".to_string());
        a.insert("B".to_string(), "secret".to_string());
        let mut b = BTreeMap::new();
        b.insert("A".to_string(), "2".to_string());
        b.insert("C".to_string(), "x".to_string());
        let ch = env_changes(&a, &b);
        assert_eq!(ch, vec!["changed A", "added C", "removed B"]);
        assert!(ch.iter().all(|c| !c.contains("secret")));
    }

    #[test]
    fn published_ports_parsing() {
        let c = "services:\n  a:\n    image: x\n    ports:\n      - 8080:80\n      - 7359:7359/udp\n      - 9000-9002:9000-9002\n";
        assert_eq!(
            published_ports(c),
            vec![("tcp".into(), 8080), ("udp".into(), 7359), ("tcp".into(), 9000)]
        );
    }

    #[test]
    fn diff_hides_secrets() {
        let d = unified_diff(
            "services:\n  a:\n    environment:\n      PASSWORD: old\n",
            "services:\n  a:\n    environment:\n      PASSWORD: new\n",
        );
        assert!(!d.contains("old") && !d.contains("new\n"), "{d}");
    }
}
