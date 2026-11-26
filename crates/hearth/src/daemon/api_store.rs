//! Store API (M6a): the catalog, entry pages with host checks, installs, and links between
//! installed apps and store entries.

use super::State;
use super::api::ApiError;
use super::auth::Principal;
use super::store::{self, Answers, HostFacts, InstallPlan};
use axum::extract::{Path, State as AxState};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hearth_core::Error;
use hearth_core::model::{JobRef, scopes};
use hearth_core::output::Envelope;
use hearth_store::catalog::{Category, ImageInfo, IndexApp};
use hearth_store::model::Entry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

type ApiResult<T> = Result<Json<Envelope<T>>, ApiError>;

fn ok<T: Serialize>(data: T) -> ApiResult<T> {
    Ok(Json(Envelope::success(data)))
}

fn can_write(p: &Principal) -> Result<(), ApiError> {
    p.require(scopes::APPS_WRITE).map_err(|_| {
        ApiError(Error::Unauthorized(
            "installing apps needs the 'apps:write' scope (or an admin account)".into(),
        ))
    })
}

pub fn routes() -> Router<State> {
    Router::new()
        .route("/v1/store", get(list))
        .route("/v1/store/refresh", post(refresh))
        .route("/v1/store/apps/{id}", get(entry))
        .route("/v1/store/apps/{id}/plan", post(plan))
        .route("/v1/store/apps/{id}/install", post(install))
        .route("/v1/store/icons/{id}", get(icon))
        .route("/v1/store/files/{id}/{*path}", get(file))
        .route("/v1/apps/{id}/store", get(app_store).post(app_link))
        .route("/v1/apps/{id}/credentials", get(credentials))
}

#[derive(Serialize)]
struct StoreApp {
    #[serde(flatten)]
    app: IndexApp,
    /// Names of installed apps from this entry.
    installed: Vec<String>,
}

#[derive(Serialize)]
struct StoreList {
    source: Option<String>,
    generated: Option<u64>,
    refreshed_at: Option<i64>,
    error: Option<String>,
    categories: Vec<Category>,
    apps: Vec<StoreApp>,
    host: HostFacts,
}

/// installed app name → store entry
async fn installed_map(st: &State) -> BTreeMap<String, String> {
    let (apps, _) = super::apps::list(st).await;
    apps.iter()
        .filter_map(|a| {
            store::linked_entry(st, &a.summary.id, a.store_app_id.as_deref())
                .map(|e| (a.summary.id.clone(), e))
        })
        .collect()
}

async fn list(AxState(st): AxState<State>, p: Principal) -> ApiResult<StoreList> {
    p.require(scopes::READ)?;
    let installed = installed_map(&st).await;
    let (refreshed_at, error) = st.store.status();
    let index = st.store.index();
    let host = store::host_facts(&st, Vec::new()).await;
    let Some(index) = index else {
        return ok(StoreList {
            source: None,
            generated: None,
            refreshed_at,
            error: error.or_else(|| Some("the store catalog hasn't been downloaded yet".into())),
            categories: Vec::new(),
            apps: Vec::new(),
            host,
        });
    };
    let apps = index
        .apps
        .iter()
        .map(|a| StoreApp {
            app: a.clone(),
            installed: installed
                .iter()
                .filter(|(_, e)| *e == &a.id)
                .map(|(n, _)| n.clone())
                .collect(),
        })
        .collect();
    ok(StoreList {
        source: Some(index.source.clone()),
        generated: Some(index.generated),
        refreshed_at,
        error,
        categories: index.categories.clone(),
        apps,
        host,
    })
}

async fn refresh(AxState(st): AxState<State>, p: Principal) -> ApiResult<Value> {
    can_write(&p)?;
    let (apps, downloaded) = st.store.refresh().await.map_err(Error::Other)?;
    ok(json!({ "apps": apps, "downloaded": downloaded }))
}

#[derive(Serialize)]
struct EntryView {
    app: IndexApp,
    entry: Entry,
    images: Vec<ImageInfo>,
    installed: Vec<String>,
    /// The install plan with default answers (host checks included).
    plan: InstallPlan,
}

async fn entry(AxState(st): AxState<State>, p: Principal, Path(id): Path<String>) -> ApiResult<EntryView> {
    p.require(scopes::READ)?;
    let (app, bundle) = st.store.bundle(&id).ok_or_else(|| Error::NotFound {
        what: "store app",
        name: id.clone(),
    })?;
    let plan = store::plan(&st, &id, &Answers::default())
        .await
        .map_err(Error::Other)?;
    let installed = installed_map(&st)
        .await
        .into_iter()
        .filter(|(_, e)| e == &id)
        .map(|(n, _)| n)
        .collect();
    ok(EntryView {
        app,
        entry: bundle.entry,
        images: bundle.images,
        installed,
        plan,
    })
}

async fn plan(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(a): Json<Answers>,
) -> ApiResult<InstallPlan> {
    p.require(scopes::READ)?;
    ok(store::plan(&st, &id, &a).await.map_err(Error::Usage)?)
}

#[derive(Deserialize)]
struct InstallBody {
    #[serde(flatten)]
    answers: Answers,
    #[serde(default)]
    dry_run: bool,
}

async fn install(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(b): Json<InstallBody>,
) -> Result<Response, ApiError> {
    p.require(scopes::READ)?;
    if b.dry_run {
        let plan = store::plan(&st, &id, &b.answers).await.map_err(Error::Usage)?;
        return Ok(Json(Envelope::success(plan)).into_response());
    }
    can_write(&p)?;
    let plan = store::plan(&st, &id, &b.answers).await.map_err(Error::Usage)?;
    if !plan.ok {
        let errs: Vec<String> = plan
            .checks
            .iter()
            .filter(|c| c.level == "error")
            .map(|c| c.message.clone())
            .collect();
        return Err(Error::Usage(errs.join("; ")).into());
    }
    let st2 = st.clone();
    let actor = p.actor();
    let mut answers = b.answers;
    answers.name = Some(plan.name.clone());
    let actor2 = actor.clone();
    let job = st
        .jobs
        .start(&st, "install", &plan.name, &actor, move |job| async move {
            store::install(&st2, &job, &id, answers, &actor2).await
        });
    Ok(Json(Envelope::success(JobRef { job })).into_response())
}

fn image_response(ctype: String, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox".to_string(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}

async fn icon(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    p.require(scopes::READ)?;
    let (ctype, bytes) = st.store.icon(&id).ok_or_else(|| Error::NotFound {
        what: "store icon",
        name: id.clone(),
    })?;
    Ok(image_response(ctype, bytes))
}

async fn file(
    AxState(st): AxState<State>,
    p: Principal,
    Path((id, path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    p.require(scopes::READ)?;
    let (ctype, bytes) = st.store.file(&id, &path).await.map_err(Error::Other)?;
    Ok(image_response(ctype, bytes))
}

#[derive(Serialize)]
struct AppStoreView {
    /// The store entry this app belongs to.
    entry: Option<String>,
    /// `installed` (from the store) or `linked` (linked by you).
    how: Option<String>,
    name: Option<String>,
    notice: Option<String>,
    after_install: Vec<String>,
    /// A store entry that looks like this app, when it isn't linked yet.
    suggestion: Option<Suggestion>,
    /// Sign-in details Hearth generated (labels only; values need apps:write).
    credentials: Vec<String>,
}

#[derive(Serialize)]
struct Suggestion {
    id: String,
    name: String,
}

async fn app_store(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<AppStoreView> {
    p.require(scopes::READ)?;
    let (apps, _) = super::apps::list(&st).await;
    let app = apps
        .into_iter()
        .find(|a| a.summary.id == id)
        .ok_or_else(|| Error::NotFound {
            what: "app",
            name: id.clone(),
        })?;
    let from_compose = app.store_app_id.clone().filter(|s| s.contains('.'));
    let entry = store::linked_entry(&st, &id, from_compose.as_deref());
    let how = entry.as_ref().map(|_| {
        if from_compose.is_some() {
            "installed"
        } else {
            "linked"
        }
        .to_string()
    });
    let bundle = entry.as_ref().and_then(|e| st.store.bundle(e)).map(|(_, b)| b);
    let suggestion = if entry.is_none() {
        let images: Vec<String> = app.summary.images.iter().map(|i| i.reference.clone()).collect();
        store::suggest_entry(&st, &images).and_then(|sid| {
            st.store.bundle(&sid).map(|(_, b)| Suggestion {
                id: sid,
                name: b.entry.name,
            })
        })
    } else {
        None
    };
    let credentials = if from_compose.is_some() {
        shown_fields(&st, &id)
            .await
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    } else {
        Vec::new()
    };
    ok(AppStoreView {
        entry,
        how,
        name: bundle.as_ref().map(|b| b.entry.name.clone()),
        notice: bundle.as_ref().and_then(|b| b.entry.notice.clone()),
        after_install: bundle.map(|b| b.entry.after_install).unwrap_or_default(),
        suggestion,
        credentials,
    })
}

/// `x-hearth.store.show`: (label, env variable) pairs.
async fn shown_fields(st: &State, app: &str) -> Vec<(String, String)> {
    let Ok(v) = st
        .helper
        .call(&hearth_core::proto::Request::ReadAppFiles {
            app: app.to_string(),
            include_env: false,
        })
        .await
    else {
        return Vec::new();
    };
    let text = v.get("compose").and_then(Value::as_str).unwrap_or_default();
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).unwrap_or_default();
    doc.get("x-hearth")
        .and_then(|x| x.get("store"))
        .and_then(|s| s.get("show"))
        .and_then(|s| s.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|m| {
                    Some((
                        m.get("label")?.as_str()?.to_string(),
                        m.get("env")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct LinkBody {
    /// A store entry id, or null to unlink.
    entry: Option<String>,
}

async fn app_link(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(b): Json<LinkBody>,
) -> ApiResult<Value> {
    can_write(&p)?;
    let (apps, _) = super::apps::list(&st).await;
    let app = apps
        .iter()
        .find(|a| a.summary.id == id)
        .ok_or_else(|| Error::NotFound {
            what: "app",
            name: id.clone(),
        })?;
    if app.store_app_id.as_deref().is_some_and(|s| s.contains('.')) {
        return Err(
            Error::Usage("this app was installed from the store; its link can't change".into()).into(),
        );
    }
    if let Some(e) = &b.entry
        && st.store.bundle(e).is_none()
    {
        return Err(Error::Usage(format!("the store has no app '{e}'")).into());
    }
    st.with_db(|db| db.set_store_link(&id, b.entry.as_deref()))
        .map_err(|e| Error::Other(e.to_string()))?;
    tracing::info!(app = %id, entry = ?b.entry, by = %p.actor(), "store link changed");
    ok(json!({ "app": id, "entry": b.entry }))
}

#[derive(Serialize)]
struct Credential {
    label: String,
    value: String,
}

async fn credentials(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Vec<Credential>> {
    can_write(&p)?;
    let fields = shown_fields(&st, &id).await;
    if fields.is_empty() {
        return ok(Vec::new());
    }
    let (_, env_text) = super::engine::read_files(&st, &id).await.map_err(Error::Other)?;
    let env = crate::compose_norm::parse_env(&env_text);
    tracing::info!(app = %id, by = %p.actor(), "sign-in details shown");
    ok(fields
        .into_iter()
        .filter_map(|(label, var)| {
            env.get(&var).map(|v| Credential {
                label,
                value: v.clone(),
            })
        })
        .collect())
}
