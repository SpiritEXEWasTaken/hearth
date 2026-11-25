//! API endpoints that change things (milestone M2). Every change needs the `apps:write` scope,
//! returns a job reference at once, and can be previewed with `"dry_run": true` (read scope).

use super::State;
use super::api::ApiError;
use super::auth::Principal;
use super::engine::{self, InstallRequest, StateAction};
use super::jobs::JobError;
use axum::extract::{Path, Query, State as AxState};
use axum::routing::{get, post};
use axum::{Json, Router};
use hearth_core::model::{ApplyPlan, HistoryEntry, ImageEntry, Job, JobRef, UpdatePlan, scopes};
use hearth_core::output::Envelope;
use hearth_core::proto::{self, Request, Revision};
use hearth_core::{Error, model::DockerEndpoint};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

type ApiResult<T> = Result<Json<Envelope<T>>, ApiError>;

fn ok<T: serde::Serialize>(data: T) -> ApiResult<T> {
    Ok(Json(Envelope::success(data)))
}

fn actor(p: &Principal) -> String {
    match (&p.token_id, p.uid) {
        (Some(t), _) => format!("token:{t}"),
        (None, Some(uid)) => format!("uid:{uid}"),
        _ => "unknown".into(),
    }
}

fn can_write(p: &Principal) -> Result<(), ApiError> {
    p.require(scopes::APPS_WRITE).map_err(|_| {
        ApiError(Error::Unauthorized(
            "changing apps needs the 'apps:write' scope: create a token with `sudo hearth token create --name NAME --scope read --scope apps:write`".into(),
        ))
    })
}

pub fn routes() -> Router<State> {
    Router::new()
        .route("/v1/apps", post(install))
        .route("/v1/apps/{id}/uninstall", post(uninstall))
        .route("/v1/apps/{id}/start", post(start))
        .route("/v1/apps/{id}/stop", post(stop))
        .route("/v1/apps/{id}/restart", post(restart))
        .route("/v1/apps/{id}/apply", post(apply))
        .route("/v1/apps/{id}/update", post(update))
        .route("/v1/apps/{id}/history", get(history))
        .route("/v1/apps/{id}/revert", post(revert))
        .route("/v1/apps/{id}/backup", post(backup))
        .route("/v1/apps/{id}/backups", get(backups))
        .route("/v1/apps/{id}/restore", post(restore))
        .route("/v1/images", get(images))
        .route("/v1/images/prune", post(prune_images))
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job))
        .route("/v1/settings", get(get_settings).put(put_setting))
}

// ----- install / uninstall -----

#[derive(Deserialize)]
struct InstallBody {
    app: String,
    compose: String,
    #[serde(default)]
    sets: BTreeMap<String, String>,
    #[serde(default = "yes")]
    start: bool,
    #[serde(default)]
    dry_run: bool,
}

fn yes() -> bool {
    true
}

async fn install(AxState(st): AxState<State>, p: Principal, Json(b): Json<InstallBody>) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    if !proto::is_valid_hearth_app_name(&b.app) {
        return Err(Error::Usage(format!(
            "invalid app name '{}': use lowercase letters, digits, '-' and '_' (max 63)",
            b.app
        ))
        .into());
    }
    if b.dry_run {
        let (plan, _) = engine::plan_apply(&st, &b.app, &b.compose, &b.sets, None).await;
        return ok(json!(plan));
    }
    can_write(&p)?;
    let st2 = st.clone();
    let req = InstallRequest {
        app: b.app.clone(),
        compose: b.compose,
        sets: b.sets,
        start: b.start,
        actor: actor(&p),
    };
    let id = st
        .jobs
        .start(&st, "install", &b.app, &actor(&p), move |job| async move {
            engine::install(&st2, &job, req).await
        });
    ok(json!(JobRef { job: id }))
}

#[derive(Deserialize, Default)]
struct UninstallBody {
    #[serde(default)]
    delete_data: bool,
    #[serde(default)]
    dry_run: bool,
}

async fn uninstall(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<UninstallBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    if b.dry_run {
        return ok(json!({ "would_uninstall": app, "delete_data": b.delete_data }));
    }
    can_write(&p)?;
    let st2 = st.clone();
    let app2 = app.clone();
    let id = st
        .jobs
        .start(&st, "uninstall", &app, &actor(&p), move |job| async move {
            engine::uninstall(&st2, &job, &app2, b.delete_data).await
        });
    ok(json!(JobRef { job: id }))
}

// ----- start / stop / restart -----

async fn state_change(st: State, p: Principal, app: String, action: StateAction) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    can_write(&p)?;
    let kind = match action {
        StateAction::Start => "start",
        StateAction::Stop => "stop",
        StateAction::Restart => "restart",
    };
    let st2 = st.clone();
    let app2 = app.clone();
    let id = st.jobs.start(&st, kind, &app, &actor(&p), move |job| async move {
        engine::set_running(&st2, &job, &app2, action).await
    });
    ok(json!(JobRef { job: id }))
}

async fn start(AxState(st): AxState<State>, p: Principal, Path(app): Path<String>) -> ApiResult<Value> {
    state_change(st, p, app, StateAction::Start).await
}
async fn stop(AxState(st): AxState<State>, p: Principal, Path(app): Path<String>) -> ApiResult<Value> {
    state_change(st, p, app, StateAction::Stop).await
}
async fn restart(AxState(st): AxState<State>, p: Principal, Path(app): Path<String>) -> ApiResult<Value> {
    state_change(st, p, app, StateAction::Restart).await
}

// ----- apply / revert / update -----

#[derive(Deserialize)]
struct ApplyBody {
    compose: String,
    #[serde(default)]
    sets: BTreeMap<String, String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

async fn current_files(st: &State, app: &str) -> Result<(String, String), Error> {
    let v = st
        .helper
        .call(&Request::ReadAppFiles {
            app: app.to_string(),
            include_env: true,
        })
        .await
        .map_err(Error::Other)?;
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

async fn apply(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<ApplyBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    let (cur_compose, cur_env) = current_files(&st, &app).await?;
    let (plan, n) = engine::plan_apply(&st, &app, &b.compose, &b.sets, Some((&cur_compose, &cur_env))).await;
    if b.dry_run {
        return ok(json!(plan));
    }
    can_write(&p)?;
    if !plan.errors.is_empty() {
        return Err(Error::Usage(plan.errors.join("; ")).into());
    }
    let env_text = crate::compose_norm::render_env(&n.env).map_err(Error::Usage)?;
    let message = b.message.unwrap_or_else(|| "applied a new compose file".into());
    let (st2, app2, who) = (st.clone(), app.clone(), actor(&p));
    let id = st
        .jobs
        .start(&st, "apply", &app, &actor(&p), move |job| async move {
            engine::apply_files(&st2, &job, &app2, &n.compose, &env_text, &message, &who).await
        });
    ok(json!(JobRef { job: id }))
}

#[derive(Deserialize)]
struct RevertBody {
    revision: u32,
    #[serde(default)]
    dry_run: bool,
}

async fn revert(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<RevertBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    let rev: Revision = st
        .helper
        .call_as(&Request::ReadRevision {
            app: app.clone(),
            revision: b.revision,
        })
        .await
        .map_err(|e| Error::NotFound {
            what: "revision",
            name: format!("{}: {e}", b.revision),
        })?;
    let (cur_compose, cur_env) = current_files(&st, &app).await?;
    let (new_compose, new_env) = (rev.compose.unwrap_or_default(), rev.env.unwrap_or_default());
    if b.dry_run {
        let plan = ApplyPlan {
            app: app.clone(),
            compose_diff: engine::unified_diff(&cur_compose, &new_compose),
            unchanged: cur_compose == new_compose && cur_env == new_env,
            env_changes: if cur_env == new_env {
                vec![]
            } else {
                vec![".env differs".into()]
            },
            ..Default::default()
        };
        return ok(json!(plan));
    }
    can_write(&p)?;
    let message = format!("reverted to revision {}", b.revision);
    let (st2, app2, who) = (st.clone(), app.clone(), actor(&p));
    let id = st
        .jobs
        .start(&st, "revert", &app, &actor(&p), move |job| async move {
            engine::apply_files(&st2, &job, &app2, &new_compose, &new_env, &message, &who).await
        });
    ok(json!(JobRef { job: id }))
}

#[derive(Deserialize, Default)]
struct DryRun {
    #[serde(default)]
    dry_run: bool,
}

async fn update(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<DryRun>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    if b.dry_run {
        let plan: UpdatePlan = engine::plan_update(&st, &app).await?;
        return ok(json!(plan));
    }
    can_write(&p)?;
    let (st2, app2, who) = (st.clone(), app.clone(), actor(&p));
    let id = st
        .jobs
        .start(&st, "update", &app, &actor(&p), move |job| async move {
            engine::update(&st2, &job, &app2, &who).await
        });
    ok(json!(JobRef { job: id }))
}

async fn history(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
) -> ApiResult<Vec<HistoryEntry>> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    let revs: Vec<Revision> = st
        .helper
        .call_as(&Request::ListHistory { app })
        .await
        .map_err(Error::Other)?;
    ok(revs
        .into_iter()
        .map(|r| HistoryEntry {
            revision: r.revision,
            time: r.time,
            actor: r.actor,
            message: r.message,
        })
        .collect())
}

// ----- backups -----

#[derive(Deserialize)]
struct BackupBody {
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default = "yes")]
    stop: bool,
}

async fn backup(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<BackupBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    can_write(&p)?;
    let (st2, app2) = (st.clone(), app.clone());
    let id = st
        .jobs
        .start(&st, "backup", &app, &actor(&p), move |job| async move {
            engine::backup(&st2, &job, &app2, &b.exclude, b.stop).await
        });
    ok(json!(JobRef { job: id }))
}

async fn backups(AxState(st): AxState<State>, p: Principal, Path(app): Path<String>) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    let v = st
        .helper
        .call(&Request::ListBackups { app })
        .await
        .map_err(Error::Other)?;
    ok(v)
}

#[derive(Deserialize)]
struct RestoreBody {
    file: String,
    #[serde(default)]
    dry_run: bool,
}

async fn restore(
    AxState(st): AxState<State>,
    p: Principal,
    Path(app): Path<String>,
    Json(b): Json<RestoreBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    engine::require_hearth_app(&st, &app).await?;
    if !proto::is_valid_backup_name(&b.file) {
        return Err(Error::Usage("invalid backup file name".into()).into());
    }
    if b.dry_run {
        return ok(json!({ "would_restore": b.file, "app": app }));
    }
    can_write(&p)?;
    let (st2, app2) = (st.clone(), app.clone());
    let id = st
        .jobs
        .start(&st, "restore", &app, &actor(&p), move |job| async move {
            engine::restore(&st2, &job, &app2, &b.file).await
        });
    ok(json!(JobRef { job: id }))
}

// ----- images -----

async fn image_entries(st: &State) -> Result<Vec<ImageEntry>, Error> {
    let images = st.helper.call(&Request::ListImages).await.map_err(Error::Other)?;
    let containers = st
        .helper
        .containers(&DockerEndpoint::Root)
        .await
        .map_err(Error::Other)?;
    let used: HashSet<String> = containers
        .iter()
        .filter_map(|c| c.get("ImageID").and_then(Value::as_str).map(str::to_string))
        .collect();
    Ok(images
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            let id = i
                .get("Id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tags: Vec<String> = i
                .get("RepoTags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .filter(|t| t != "<none>:<none>")
                        .collect()
                })
                .unwrap_or_default();
            ImageEntry {
                in_use: used.contains(&id),
                rollback: tags.iter().any(|t| t.starts_with("hearth-rollback/")),
                size_bytes: i.get("Size").and_then(Value::as_u64).unwrap_or(0),
                created: i.get("Created").and_then(Value::as_i64).unwrap_or(0),
                id,
                tags,
            }
        })
        .collect())
}

async fn images(AxState(st): AxState<State>, p: Principal) -> ApiResult<Vec<ImageEntry>> {
    p.require(scopes::READ)?;
    ok(image_entries(&st).await?)
}

#[derive(Deserialize, Default)]
struct PruneBody {
    /// Rollback images to keep per app service (default: the image_retention setting).
    keep: Option<u64>,
    /// Also remove tagged images that no container uses.
    #[serde(default)]
    unused: bool,
    #[serde(default)]
    dry_run: bool,
}

/// What a prune removes: extra rollback images, dangling images, and (with `unused`) tagged
/// images no container uses. Images used by any container are never removed.
fn prune_targets(entries: &[ImageEntry], keep: usize, unused: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in entries {
        for t in &e.tags {
            if let Some((repo, _)) = t.rsplit_once(':')
                && repo.starts_with("hearth-rollback/")
            {
                by_repo.entry(repo.to_string()).or_default().push(t.clone());
            }
        }
    }
    for (_, mut tags) in by_repo {
        tags.sort();
        tags.reverse();
        for t in tags.into_iter().skip(keep) {
            out.push((t.clone(), format!("old rollback image {t}")));
        }
    }
    for e in entries.iter().filter(|e| !e.in_use) {
        if e.tags.is_empty() {
            out.push((
                e.id.clone(),
                format!("unused untagged image {}", &e.id[..e.id.len().min(19)]),
            ));
        } else if unused && !e.rollback {
            for t in &e.tags {
                out.push((t.clone(), format!("unused image {t}")));
            }
        }
    }
    out
}

async fn prune_images(
    AxState(st): AxState<State>,
    p: Principal,
    Json(b): Json<PruneBody>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    let keep = b
        .keep
        .unwrap_or_else(|| st.with_db(|db| db.setting_u64("image_retention"))) as usize;
    let entries = image_entries(&st).await?;
    let targets = prune_targets(&entries, keep, b.unused);
    if b.dry_run {
        return ok(json!({ "would_remove": targets.iter().map(|(_, d)| d).collect::<Vec<_>>() }));
    }
    can_write(&p)?;
    let st2 = st.clone();
    let id = st
        .jobs
        .start(&st, "prune-images", "images", &actor(&p), move |job| async move {
            let mut removed = 0;
            for (name, desc) in targets {
                match st2.helper.call(&Request::RemoveImage { name }).await {
                    Ok(_) => {
                        removed += 1;
                        job.log(format!("removed {desc}"));
                    }
                    Err(e) => job.log(format!("kept {desc}: {e}")),
                }
            }
            Ok::<Value, JobError>(json!({ "removed": removed }))
        });
    ok(json!(JobRef { job: id }))
}

// ----- jobs and settings -----

#[derive(Deserialize)]
struct JobQuery {
    #[serde(default)]
    after: usize,
}

async fn get_job(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Query(q): Query<JobQuery>,
) -> ApiResult<Job> {
    p.require(scopes::READ)?;
    match st.jobs.get(&id, q.after) {
        Some(j) => ok(j),
        None => Err(ApiError(Error::NotFound {
            what: "job",
            name: id,
        })),
    }
}

async fn list_jobs(AxState(st): AxState<State>, p: Principal) -> ApiResult<Vec<Job>> {
    p.require(scopes::READ)?;
    ok(st.jobs.list())
}

async fn get_settings(AxState(st): AxState<State>, p: Principal) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    let mut out = serde_json::Map::new();
    for (k, _, desc) in super::db::SETTINGS {
        out.insert(
            k.to_string(),
            json!({ "value": st.with_db(|db| db.setting(k)), "description": desc }),
        );
    }
    ok(Value::Object(out))
}

#[derive(Deserialize)]
struct SettingBody {
    key: String,
    value: String,
}

async fn put_setting(
    AxState(st): AxState<State>,
    p: Principal,
    Json(b): Json<SettingBody>,
) -> ApiResult<Value> {
    p.require(scopes::ADMIN)?;
    st.with_db(|db| db.set_setting(&b.key, &b.value))
        .map_err(Error::Usage)?;
    ok(json!({ b.key: b.value }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(id: &str, tags: &[&str], in_use: bool) -> ImageEntry {
        ImageEntry {
            id: id.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            size_bytes: 0,
            created: 0,
            in_use,
            rollback: tags.iter().any(|t| t.starts_with("hearth-rollback/")),
        }
    }

    #[test]
    fn prune_keeps_newest_rollbacks_and_never_touches_used_images() {
        let entries = vec![
            img("sha256:a", &["hearth-rollback/web-app:20261101-000000"], false),
            img("sha256:b", &["hearth-rollback/web-app:20261120-000000"], false),
            img("sha256:c", &["hearth-rollback/web-app:20261125-000000"], false),
            img("sha256:d", &[], false),
            img("sha256:e", &[], true),
            img("sha256:f", &["nginx:1"], false),
            img("sha256:g", &["nginx:2"], true),
        ];
        let t: Vec<String> = prune_targets(&entries, 1, false)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            t,
            vec![
                "hearth-rollback/web-app:20261120-000000",
                "hearth-rollback/web-app:20261101-000000",
                "sha256:d"
            ]
        );
        let t: Vec<String> = prune_targets(&entries, 1, true)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(t.contains(&"nginx:1".to_string()));
        assert!(
            !t.contains(&"nginx:2".to_string()),
            "used images are never removed"
        );
    }
}
