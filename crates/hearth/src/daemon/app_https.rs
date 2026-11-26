//! API for app HTTPS ports: show, switch on/off, test, and the Home Assistant proxy fix.

use super::State;
use super::api::ApiError;
use super::auth::Principal;
use super::db::AppHttps;
use super::engine::StateAction;
use super::proxy::{SUGGEST_FROM, SUGGEST_TO};
use axum::extract::{Path, State as AxState};
use axum::routing::{get, post};
use axum::{Json, Router};
use hearth_core::Error;
use hearth_core::model::{AppDetail, JobRef, scopes};
use hearth_core::output::Envelope;
use hearth_core::proto::Request;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

type ApiResult<T> = Result<Json<Envelope<T>>, ApiError>;

fn ok<T: Serialize>(data: T) -> ApiResult<T> {
    Ok(Json(Envelope::success(data)))
}

pub fn routes() -> Router<State> {
    Router::new()
        .route("/v1/apps/{id}/https", get(show).put(set))
        .route("/v1/apps/{id}/https/test", post(test))
        .route("/v1/apps/{id}/fix/ha-proxy", post(ha_fix))
}

#[derive(Debug, Serialize)]
pub struct HttpsView {
    pub enabled: bool,
    pub port: Option<u16>,
    /// The app's own port that Hearth forwards to.
    pub target_port: Option<u16>,
    /// Addresses to open, best first (Tailscale names, then LAN addresses).
    pub urls: Vec<String>,
    /// Listening right now.
    pub running: bool,
    /// Why HTTPS can't be turned on for this app, if so.
    pub unavailable: Option<String>,
    /// A free port to use when turning it on.
    pub suggested_port: Option<u16>,
    /// The app looks like Home Assistant (it needs the proxy fix).
    pub home_assistant: bool,
}

/// `http://host:8123/` → 8123, for plain-HTTP web addresses only.
pub fn http_port(web_url: &str) -> Result<u16, String> {
    let rest = web_url
        .strip_prefix("http://")
        .ok_or("the app already uses HTTPS itself (or has no plain web address)")?;
    let hostport = rest.split('/').next().unwrap_or(rest);
    let port = match hostport.rsplit_once(':') {
        Some((_, p)) if !p.ends_with(']') => p.parse::<u16>().map_err(|_| "odd port in the web address")?,
        _ => 80,
    };
    Ok(port)
}

async fn find(st: &State, id: &str) -> Result<AppDetail, Error> {
    let (apps, _) = super::apps::list(st).await;
    apps.into_iter()
        .find(|a| a.summary.id == id)
        .ok_or_else(|| Error::NotFound {
            what: "app",
            name: id.to_string(),
        })
}

fn is_home_assistant(app: &AppDetail) -> bool {
    app.summary
        .images
        .iter()
        .any(|i| i.reference.contains("home-assistant/home-assistant"))
}

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn suggest(st: &State) -> Option<u16> {
    let used: Vec<u16> = st
        .with_db(|db| db.list_app_https())
        .unwrap_or_default()
        .iter()
        .map(|h| h.port)
        .collect();
    (SUGGEST_FROM..=SUGGEST_TO).find(|p| !used.contains(p) && port_free(*p))
}

pub fn urls(st: &State, port: u16) -> Vec<String> {
    let info = st.certs.info();
    let mut out: Vec<String> = info
        .tailscale_names
        .iter()
        .map(|n| format!("https://{n}:{port}"))
        .collect();
    for n in info.local_names {
        if n == "localhost" || n == "127.0.0.1" || n == "::1" {
            continue;
        }
        let host = if n.contains(':') { format!("[{n}]") } else { n };
        out.push(format!("https://{host}:{port}"));
    }
    out
}

async fn view(st: &State, id: &str) -> Result<HttpsView, Error> {
    let app = find(st, id).await?;
    let current = st
        .with_db(|db| db.list_app_https())
        .map_err(|e| Error::Other(e.to_string()))?
        .into_iter()
        .find(|h| h.app == id);
    let unavailable = match app.summary.web_url.as_deref() {
        None => Some("this app has no web page".to_string()),
        Some(u) => http_port(u).err(),
    };
    Ok(HttpsView {
        enabled: current.is_some(),
        port: current.as_ref().map(|h| h.port),
        target_port: current
            .as_ref()
            .map(|h| h.target_port)
            .or_else(|| app.summary.web_url.as_deref().and_then(|u| http_port(u).ok())),
        urls: current.as_ref().map(|h| urls(st, h.port)).unwrap_or_default(),
        running: current.as_ref().is_some_and(|h| st.proxies.is_running(h.port)),
        unavailable,
        suggested_port: if current.is_none() { suggest(st) } else { None },
        home_assistant: is_home_assistant(&app),
    })
}

async fn show(AxState(st): AxState<State>, p: Principal, Path(id): Path<String>) -> ApiResult<HttpsView> {
    p.require(scopes::READ)?;
    ok(view(&st, &id).await?)
}

#[derive(Deserialize)]
struct SetBody {
    enabled: bool,
    #[serde(default)]
    port: Option<u16>,
}

async fn set(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(b): Json<SetBody>,
) -> ApiResult<HttpsView> {
    p.require(scopes::APPS_WRITE)?;
    let app = find(&st, &id).await?;
    if !b.enabled {
        st.with_db(|db| db.remove_app_https(&id))
            .map_err(|e| Error::Other(e.to_string()))?;
        st.proxies.sync(&st);
        tracing::info!(app = %id, by = %p.actor(), "app HTTPS turned off");
        return ok(view(&st, &id).await?);
    }
    let web = app
        .summary
        .web_url
        .as_deref()
        .ok_or_else(|| Error::Usage("this app has no web page".into()))?;
    let target = http_port(web).map_err(Error::Usage)?;
    let existing = st
        .with_db(|db| db.list_app_https())
        .map_err(|e| Error::Other(e.to_string()))?;
    let mine = existing.iter().find(|h| h.app == id);
    let port = match b.port.or(mine.map(|h| h.port)) {
        Some(p) => p,
        None => suggest(&st)
            .ok_or_else(|| Error::Usage(format!("no free port between {SUGGEST_FROM} and {SUGGEST_TO}")))?,
    };
    if port < 1024 || port == target {
        return Err(Error::Usage("pick a port from 1024 up that isn't the app's own port".into()).into());
    }
    if let Some(other) = existing.iter().find(|h| h.port == port && h.app != id) {
        return Err(Error::Usage(format!("port {port} is already used for {}", other.app)).into());
    }
    let changing = mine.is_none_or(|h| h.port != port);
    if changing && !port_free(port) {
        return Err(Error::Usage(format!("port {port} is already in use on this server")).into());
    }
    let h = AppHttps {
        app: id.clone(),
        port,
        target_port: target,
    };
    st.with_db(|db| db.set_app_https(&h))
        .map_err(|e| Error::Other(e.to_string()))?;
    if let Some(problem) = st.proxies.sync(&st).remove(&id) {
        return Err(Error::Other(problem).into());
    }
    tracing::info!(app = %id, port, target, by = %p.actor(), "app HTTPS turned on");
    ok(view(&st, &id).await?)
}

#[derive(Debug, Serialize)]
struct TestResult {
    ok: bool,
    status: Option<u16>,
    message: String,
    /// A fix Hearth can apply ("ha-proxy").
    fix: Option<String>,
}

/// Asks the app the way the proxy does (with forwarding headers) and explains the answer.
async fn test(AxState(st): AxState<State>, p: Principal, Path(id): Path<String>) -> ApiResult<TestResult> {
    p.require(scopes::READ)?;
    let v = view(&st, &id).await?;
    let (Some(port), Some(target)) = (v.port, v.target_port) else {
        return Err(Error::Usage("HTTPS isn't on for this app".into()).into());
    };
    if !v.running {
        return ok(TestResult {
            ok: false,
            status: None,
            message: format!(
                "Hearth isn't listening on port {port}; turn HTTPS off and on again, or pick another port"
            ),
            fix: None,
        });
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;
    let resp = client
        .get(format!("http://127.0.0.1:{target}/"))
        .header("x-forwarded-for", "192.0.2.10")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", format!("{}:{port}", st.hostname))
        .send()
        .await;
    let r = match resp {
        Err(e) => TestResult {
            ok: false,
            status: None,
            message: format!("the app doesn't answer on port {target}: {e}"),
            fix: None,
        },
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().await.unwrap_or_default();
            if v.home_assistant && status == 400 {
                TestResult {
                    ok: false,
                    status: Some(status),
                    message: "Home Assistant refuses requests through a proxy until it's told to trust it."
                        .into(),
                    fix: Some("ha-proxy".into()),
                }
            } else if status >= 500 || status == 400 {
                TestResult {
                    ok: false,
                    status: Some(status),
                    message: format!(
                        "the app answered {status}: {}",
                        body.chars().take(200).collect::<String>().trim()
                    ),
                    fix: None,
                }
            } else {
                TestResult {
                    ok: true,
                    status: Some(status),
                    message: "Works: the app answers through Hearth's HTTPS address.".into(),
                    fix: None,
                }
            }
        }
    };
    ok(r)
}

#[derive(Deserialize, Default)]
struct DryRun {
    #[serde(default)]
    dry_run: bool,
}

/// Makes Home Assistant trust Hearth's proxy, then restarts it.
async fn ha_fix(
    AxState(st): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(b): Json<DryRun>,
) -> ApiResult<Value> {
    p.require(scopes::READ)?;
    super::engine::require_hearth_app(&st, &id).await?;
    if b.dry_run {
        let v = st
            .helper
            .call(&Request::HaTrustProxy {
                app: id.clone(),
                dry_run: true,
            })
            .await
            .map_err(Error::Other)?;
        return ok(v);
    }
    p.require(scopes::APPS_WRITE)?;
    let (st2, app) = (st.clone(), id.clone());
    let job = st.jobs.start(&st, "fix", &id, &p.actor(), move |job| async move {
        let ask = |dry_run: bool| Request::HaTrustProxy {
            app: app.clone(),
            dry_run,
        };
        let plan = st2.helper.call(&ask(true)).await?;
        let action = plan.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        match action.as_str() {
            "present" => {
                job.log("Home Assistant already trusts Hearth's proxy; nothing to change");
                return Ok(plan);
            }
            "manual" => {
                let why = plan.get("reason").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| {
                    format!(
                        "configuration.yaml already has an http: section, so Hearth won't edit it. Add these lines under it:\n{}",
                        plan.get("lines").and_then(Value::as_str).unwrap_or("")
                    )
                });
                return Err(why.into());
            }
            _ => {}
        }
        let v = if action == "add-storage" {
            // HA writes its storage when it stops, so it must be stopped while the file changes.
            job.log("stopping Home Assistant to change its network settings");
            let stopped = super::engine::set_running(&st2, &job, &app, StateAction::Stop).await;
            // Only change the file once HA is down; start it again whatever happened.
            let r = match &stopped {
                Ok(_) => st2.helper.call(&ask(false)).await,
                Err(e) => Err(format!("Home Assistant didn't stop, so nothing was changed: {}", e.message)),
            };
            job.log("starting Home Assistant");
            super::engine::set_running(&st2, &job, &app, StateAction::Start).await?;
            r?
        } else {
            job.log("adding the proxy settings to Home Assistant's configuration.yaml");
            let r = st2.helper.call(&ask(false)).await?;
            job.log("restarting Home Assistant");
            super::engine::set_running(&st2, &job, &app, StateAction::Restart).await?;
            r
        };
        if let Some(b) = v.get("backup").and_then(Value::as_str) {
            job.log(format!("the old settings are kept as {b}"));
        }
        Ok(v)
    });
    ok(json!(JobRef { job }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_from_web_addresses() {
        assert_eq!(http_port("http://jarvis:8123/"), Ok(8123));
        assert_eq!(http_port("http://jarvis/"), Ok(80));
        assert_eq!(http_port("http://[::1]:9000/x"), Ok(9000));
        assert!(http_port("https://jarvis:8443/").is_err());
    }
}
