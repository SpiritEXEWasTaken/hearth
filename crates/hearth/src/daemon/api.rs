//! The daemon's HTTP API (served on the Unix socket in this milestone).
//!
//! Every response uses the `--json` envelope: `{"ok":true,"schema":1,"data":…}` or
//! `{"ok":false,"schema":1,"error":{"kind","message","exit_code"}}`, so the CLI can pass it
//! through unchanged and map errors to exit codes.

use super::State;
use super::auth::Principal;
use super::{apps, stats, versions};
use axum::extract::{Path, Query, State as AxState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hearth_core::model::{AppDetail, AppSummary, HostStats, LogLine, NewToken, Status, TokenInfo, scopes};
use hearth_core::output::Envelope;
use hearth_core::{Error, ExitCode};
use serde::Deserialize;

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.exit_code() {
            ExitCode::Usage => StatusCode::BAD_REQUEST,
            ExitCode::NotFound => StatusCode::NOT_FOUND,
            ExitCode::NeedsConfirmation => StatusCode::CONFLICT,
            ExitCode::Unauthorized => StatusCode::FORBIDDEN,
            _ if matches!(self.0, Error::NotImplemented(_)) => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(Envelope::failure(&self.0))).into_response()
    }
}

type ApiResult<T> = Result<Json<Envelope<T>>, ApiError>;

fn ok<T: serde::Serialize>(data: T) -> ApiResult<T> {
    Ok(Json(Envelope::success(data)))
}

pub fn router(state: State) -> Router {
    Router::new()
        .route("/v1/ping", get(ping))
        .route("/v1/status", get(status))
        .route("/v1/stats", get(host_stats))
        .route("/v1/apps", get(list_apps))
        .route("/v1/apps/{id}", get(app_detail))
        .route("/v1/apps/{id}/logs", get(app_logs))
        .route("/v1/apps/{id}/compose", get(app_compose))
        .route("/v1/versions/check", post(check_versions))
        .route("/v1/tokens", get(list_tokens).post(create_token))
        .route("/v1/tokens/{id}", delete(revoke_token))
        .merge(super::api_changes::routes())
        .merge(super::system::routes())
        .with_state(state)
}

async fn ping(p: Principal) -> ApiResult<serde_json::Value> {
    ok(serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "uid": p.uid, "scopes": p.scopes }))
}

async fn status(AxState(state): AxState<State>, p: Principal) -> ApiResult<Status> {
    p.require(scopes::READ)?;
    let (host, (apps, warnings)) = tokio::join!(stats::collect(), apps::list(&state));
    ok(Status {
        host,
        apps: apps.into_iter().map(|a| a.summary).collect(),
        warnings,
    })
}

async fn host_stats(p: Principal) -> ApiResult<HostStats> {
    p.require(scopes::READ)?;
    ok(stats::collect().await)
}

async fn list_apps(AxState(state): AxState<State>, p: Principal) -> ApiResult<Vec<AppSummary>> {
    p.require(scopes::READ)?;
    let (apps, _) = apps::list(&state).await;
    ok(apps.into_iter().map(|a| a.summary).collect())
}

async fn find_app(state: &State, id: &str) -> Result<AppDetail, Error> {
    let (apps, _) = apps::list(state).await;
    apps.into_iter()
        .find(|a| a.summary.id == id)
        .ok_or_else(|| Error::NotFound {
            what: "app",
            name: id.to_string(),
        })
}

async fn app_detail(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<AppDetail> {
    p.require(scopes::READ)?;
    ok(find_app(&state, &id).await?)
}

#[derive(Deserialize)]
struct LogsQuery {
    tail: Option<u32>,
    service: Option<String>,
    since: Option<i64>,
}

async fn app_logs(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Vec<LogLine>> {
    p.require(scopes::READ)?;
    let app = find_app(&state, &id).await?;
    let tail = q.tail.unwrap_or(200);
    let mut out = Vec::new();
    let mut matched = false;
    for c in &app.summary.containers {
        let service = c.service.clone().unwrap_or_else(|| c.name.clone());
        if q.service.as_deref().is_some_and(|s| s != service && s != c.name) {
            continue;
        }
        matched = true;
        let logs = state
            .helper
            .logs(&app.summary.endpoint, &c.id, tail, q.since)
            .await
            .map_err(Error::Other)?;
        out.extend(logs.lines.into_iter().map(|l| LogLine {
            service: service.clone(),
            stream: l.stream,
            line: l.line,
        }));
    }
    if !matched {
        return Err(Error::NotFound {
            what: "service",
            name: q.service.unwrap_or_default(),
        }
        .into());
    }
    ok(out)
}

async fn app_compose(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<String> {
    p.require(scopes::READ)?;
    let app = find_app(&state, &id).await?;
    let text = match app.summary.managed_by {
        hearth_core::model::ManagedBy::Casaos => state.helper.casaos_app(&id).await.map_err(Error::Other)?,
        hearth_core::model::ManagedBy::Hearth => {
            let v = state
                .helper
                .call(&hearth_core::proto::Request::ReadAppFiles {
                    app: id.clone(),
                    include_env: false,
                })
                .await
                .map_err(Error::Other)?;
            v.get("compose")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        _ => return Err(Error::NotImplemented("compose files of apps Hearth doesn't manage").into()),
    };
    // Secrets are always hidden here; nobody needs them through this endpoint.
    ok(hearth_core::redact::redact_compose_text(&text))
}

#[derive(Deserialize)]
struct CheckQuery {
    app: Option<String>,
}

async fn check_versions(
    AxState(state): AxState<State>,
    p: Principal,
    Query(q): Query<CheckQuery>,
) -> ApiResult<Vec<AppSummary>> {
    // Checking only reads from registries, so read access is enough.
    p.require(scopes::READ)?;
    versions::check(&state, q.app.as_deref()).await.map_err(|e| {
        if e.starts_with("app not found") {
            Error::NotFound {
                what: "app",
                name: q.app.clone().unwrap_or_default(),
            }
        } else {
            Error::Other(e)
        }
    })?;
    let (apps, _) = apps::list(&state).await;
    ok(apps
        .into_iter()
        .map(|a| a.summary)
        .filter(|a| q.app.as_deref().is_none_or(|id| a.id == id))
        .collect())
}

async fn list_tokens(AxState(state): AxState<State>, p: Principal) -> ApiResult<Vec<TokenInfo>> {
    p.require(scopes::ADMIN)?;
    let list = state
        .with_db(|db| db.list_tokens())
        .map_err(|e| Error::Other(e.to_string()))?;
    ok(list)
}

#[derive(Deserialize)]
struct CreateToken {
    name: String,
    scopes: Vec<String>,
}

async fn create_token(
    AxState(state): AxState<State>,
    p: Principal,
    Json(req): Json<CreateToken>,
) -> ApiResult<NewToken> {
    p.require(scopes::ADMIN)?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(Error::Usage("token name must be 1–64 characters".into()).into());
    }
    if req.scopes.is_empty() {
        return Err(Error::Usage("give at least one scope".into()).into());
    }
    if let Some(bad) = req.scopes.iter().find(|s| !scopes::is_valid(s)) {
        return Err(Error::Usage(format!(
            "unknown scope '{bad}' (valid: {})",
            scopes::ALL.join(", ")
        ))
        .into());
    }
    let secret = format!("hth_{}", random_hex(32)?);
    let id = random_hex(6)?;
    state
        .with_db(|db| db.create_token(&id, name, &secret, &req.scopes))
        .map_err(|e| Error::Other(e.to_string()))?;
    tracing::info!(token_id = %id, name, scopes = ?req.scopes, by_uid = ?p.uid, "token created");
    ok(NewToken {
        id,
        name: name.to_string(),
        scopes: req.scopes,
        token: secret,
    })
}

async fn revoke_token(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    p.require(scopes::ADMIN)?;
    let removed = state
        .with_db(|db| db.revoke_token(&id))
        .map_err(|e| Error::Other(e.to_string()))?;
    if !removed {
        return Err(Error::NotFound {
            what: "token",
            name: id,
        }
        .into());
    }
    tracing::info!(token_id = %id, by_uid = ?p.uid, "token revoked");
    ok(serde_json::json!({ "revoked": id }))
}

fn random_hex(bytes: usize) -> Result<String, Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Error::Other(format!("random: {e}")))?;
    Ok(hex::encode(buf))
}
