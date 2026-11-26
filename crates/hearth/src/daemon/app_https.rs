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
        .route(
            "/v1/domain",
            get(domain_show).put(domain_set).delete(domain_remove),
        )
        .route("/v1/domain/renew", post(domain_renew))
}

// ----- your own domain -----

async fn domain_show(AxState(st): AxState<State>, p: Principal) -> ApiResult<super::domain::DomainStatus> {
    p.require(scopes::READ)?;
    ok(super::domain::status(&st))
}

#[derive(Deserialize)]
struct DomainBody {
    name: String,
    /// Kept as is when left out (changing only the address).
    #[serde(default)]
    token: Option<String>,
    #[serde(default = "default_address")]
    address: String,
}

fn default_address() -> String {
    "tailscale".into()
}

async fn domain_set(
    AxState(st): AxState<State>,
    p: Principal,
    Json(b): Json<DomainBody>,
) -> ApiResult<JobRef> {
    p.require(scopes::ADMIN)?;
    let name = super::domain::valid_name(&b.name).map_err(Error::Usage)?;
    if b.address != "tailscale" && b.address != "lan" {
        return Err(Error::Usage("address must be 'tailscale' or 'lan'".into()).into());
    }
    let token = match b.token.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => super::domain::valid_token(t).map_err(Error::Usage)?,
        None => super::domain::load_config(&st)
            .filter(|c| c.name == name)
            .map(|c| c.token)
            .ok_or_else(|| Error::Usage("enter the DuckDNS token".into()))?,
    };
    if super::domain::pick_ip(&b.address).is_none() {
        return Err(Error::Usage(if b.address == "tailscale" {
            "this server has no Tailscale address; is Tailscale running?".into()
        } else {
            "this server has no home network address".into()
        })
        .into());
    }
    let cfg = super::domain::DomainConfig {
        name,
        token,
        address: b.address,
    };
    let changed_name = super::domain::load_config(&st).is_none_or(|c| c.name != cfg.name);
    if changed_name {
        // Another name: the old certificates don't fit any more.
        let _ = super::domain::save_config(&st, None);
    }
    super::domain::save_config(&st, Some(&cfg)).map_err(Error::Other)?;
    st.forget_domain();
    tracing::info!(name = %cfg.name, address = %cfg.address, by = %p.actor(), "domain set");
    let st2 = st.clone();
    let job = st
        .jobs
        .start(&st, "domain", "https", &p.actor(), move |job| async move {
            super::domain::run(&st2, &job, changed_name).await
        });
    ok(JobRef { job })
}

async fn domain_renew(AxState(st): AxState<State>, p: Principal) -> ApiResult<JobRef> {
    p.require(scopes::ADMIN)?;
    if super::domain::load_config(&st).is_none() {
        return Err(Error::Usage("no domain is set up".into()).into());
    }
    let st2 = st.clone();
    let job = st
        .jobs
        .start(&st, "domain", "https", &p.actor(), move |job| async move {
            super::domain::run(&st2, &job, true).await
        });
    ok(JobRef { job })
}

async fn domain_remove(AxState(st): AxState<State>, p: Principal) -> ApiResult<Value> {
    p.require(scopes::ADMIN)?;
    super::domain::save_config(&st, None).map_err(Error::Other)?;
    st.certs.set_domain(Vec::new());
    st.forget_domain();
    tracing::info!(by = %p.actor(), "domain removed");
    ok(json!({ "removed": true }))
}

/// A subdomain made from the app's title: "Home Assistant" → "home-assistant".
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').chars().take(40).collect::<String>();
    if out.is_empty() { "app".into() } else { out }
}

fn valid_subdomain(s: &str) -> Result<String, Error> {
    let s = s.trim().to_ascii_lowercase();
    let ok = !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-');
    if ok {
        Ok(s)
    } else {
        Err(Error::Usage(
            "a subdomain is letters, digits and '-' (like ha or home-assistant)".into(),
        ))
    }
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
    pub subdomain: Option<String>,
    /// A subdomain to offer when your own domain is set up and the app has none.
    pub suggested_subdomain: Option<String>,
    /// Your own domain, if set up (`<name>.duckdns.org`).
    pub domain: Option<String>,
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

pub fn urls(st: &State, port: u16, subdomain: Option<&str>) -> Vec<String> {
    let info = st.certs.info();
    let mut out: Vec<String> = Vec::new();
    if let (Some(sub), Some(base)) = (subdomain, st.domain_base()) {
        out.push(format!("https://{sub}.{base}/"));
    }
    out.extend(info.tailscale_names.iter().map(|n| format!("https://{n}:{port}")));
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
        urls: current
            .as_ref()
            .map(|h| urls(st, h.port, h.subdomain.as_deref()))
            .unwrap_or_default(),
        running: current.as_ref().is_some_and(|h| st.proxies.is_running(h.port)),
        unavailable,
        suggested_port: if current.is_none() { suggest(st) } else { None },
        home_assistant: is_home_assistant(&app),
        subdomain: current.as_ref().and_then(|h| h.subdomain.clone()),
        suggested_subdomain: st
            .domain_base()
            .filter(|_| current.as_ref().is_none_or(|h| h.subdomain.is_none()))
            .map(|_| slug(&app.summary.title)),
        domain: st.domain_base(),
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
    /// `<subdomain>.<your domain>`; "" removes it, left out keeps it.
    #[serde(default)]
    subdomain: Option<String>,
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
    let subdomain = match b.subdomain.as_deref().map(str::trim) {
        Some("") => None,
        Some(sd) => {
            let sd = valid_subdomain(sd)?;
            if let Some(other) = existing
                .iter()
                .find(|h| h.subdomain.as_deref() == Some(sd.as_str()) && h.app != id)
            {
                return Err(Error::Usage(format!("{sd} is already used for {}", other.app)).into());
            }
            Some(sd)
        }
        None => mine.and_then(|h| h.subdomain.clone()),
    };
    let h = AppHttps {
        app: id.clone(),
        port,
        target_port: target,
        subdomain,
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
    fn slugs() {
        assert_eq!(slug("Home Assistant"), "home-assistant");
        assert_eq!(slug("  Immich!! "), "immich");
        assert_eq!(slug("??"), "app");
        assert!(valid_subdomain("ha").is_ok());
        assert!(valid_subdomain("a.b").is_err());
    }

    #[test]
    fn ports_from_web_addresses() {
        assert_eq!(http_port("http://jarvis:8123/"), Ok(8123));
        assert_eq!(http_port("http://jarvis/"), Ok(80));
        assert_eq!(http_port("http://[::1]:9000/x"), Ok(9000));
        assert!(http_port("https://jarvis:8443/").is_err());
    }
}
