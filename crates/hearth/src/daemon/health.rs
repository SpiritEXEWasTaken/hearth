//! The health gate: after a change, wait until the changed services are really working.
//!
//! A service passes when its container is running and either
//! 1. its Docker healthcheck reports `healthy`, or
//! 2. (no healthcheck) the app's web port answers with a non-error status, if this is the web
//!    service, or
//! 3. (neither) it stays running for `stable` seconds without restarting.
//!
//! It fails fast when a container exits, dies, reports `unhealthy` or restarts in a loop.

use super::State;
use super::jobs::JobHandle;
use hearth_core::model::DockerEndpoint;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct Web {
    pub service: String,
    pub port: u16,
    pub path: String,
    /// A readiness URL that must answer 2xx (e.g. Jellyfin's /health). Without it, any
    /// non-error answer from `path` counts.
    pub health: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GateSpec {
    pub app: String,
    /// Services to check (must not be empty).
    pub services: Vec<String>,
    pub web: Option<Web>,
    pub timeout: Duration,
    pub stable: Duration,
}

#[derive(Default)]
struct Seen {
    first_running: Option<Instant>,
    restart_baseline: Option<u64>,
    passed: bool,
}

/// Returns Ok when every service passed, else the reason (with recent logs of the failing service).
pub async fn wait_healthy(state: &State, job: &JobHandle, spec: &GateSpec) -> Result<(), String> {
    let start = Instant::now();
    let mut seen: HashMap<String, Seen> = HashMap::new();
    job.log(format!(
        "checking health of {} (up to {} s)",
        spec.services.join(", "),
        spec.timeout.as_secs()
    ));
    loop {
        let containers = containers_by_service(state, &spec.app).await?;
        let mut all_ok = true;
        for svc in &spec.services {
            let s = seen.entry(svc.clone()).or_default();
            if s.passed {
                continue;
            }
            let Some(id) = containers.get(svc) else {
                all_ok = false;
                if start.elapsed() > Duration::from_secs(30) {
                    return Err(format!("{svc}: no container was created"));
                }
                continue;
            };
            let info = state
                .helper
                .inspect_container(&DockerEndpoint::Root, id)
                .await
                .unwrap_or(Value::Null);
            let st = |p: &str| {
                info.pointer(p)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let status = st("/State/Status");
            let health = info
                .pointer("/State/Health/Status")
                .and_then(Value::as_str)
                .map(str::to_string);
            let restarts = info.get("RestartCount").and_then(Value::as_u64).unwrap_or(0);
            let baseline = *s.restart_baseline.get_or_insert(restarts);

            if matches!(status.as_str(), "exited" | "dead") {
                let code = info
                    .pointer("/State/ExitCode")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1);
                return Err(with_logs(state, id, format!("{svc} stopped (exit code {code})")).await);
            }
            if restarts >= baseline + 2 {
                return Err(with_logs(
                    state,
                    id,
                    format!("{svc} keeps restarting ({} restarts)", restarts - baseline),
                )
                .await);
            }
            if status != "running" {
                all_ok = false;
                continue;
            }
            match health.as_deref() {
                Some("healthy") => {
                    s.passed = true;
                    job.log(format!("{svc}: healthy (healthcheck)"));
                }
                Some("unhealthy") => {
                    return Err(with_logs(state, id, format!("{svc} reports unhealthy")).await);
                }
                Some(_) => all_ok = false, // starting
                None => {
                    let web = spec.web.as_ref().filter(|w| &w.service == svc);
                    if let Some(w) = web {
                        let (path, strict) = match &w.health {
                            Some(hp) => (hp.as_str(), true),
                            None => (w.path.as_str(), false),
                        };
                        if let Some(code) = probe(w.port, path).await
                            && (if strict {
                                (200..300).contains(&code)
                            } else {
                                code < 500
                            })
                        {
                            s.passed = true;
                            job.log(format!(
                                "{svc}: http://127.0.0.1:{}{path} answers (HTTP {code})",
                                w.port
                            ));
                            continue;
                        }
                        all_ok = false;
                    } else {
                        let first = *s.first_running.get_or_insert_with(Instant::now);
                        if first.elapsed() >= spec.stable {
                            s.passed = true;
                            job.log(format!("{svc}: running steadily for {} s", spec.stable.as_secs()));
                        } else {
                            all_ok = false;
                        }
                    }
                }
            }
        }
        if all_ok && seen.values().filter(|s| s.passed).count() == spec.services.len() {
            return Ok(());
        }
        if start.elapsed() > spec.timeout {
            let pending: Vec<String> = spec
                .services
                .iter()
                .filter(|s| !seen.get(*s).is_some_and(|x| x.passed))
                .cloned()
                .collect();
            let first = pending.first().and_then(|s| containers.get(s)).cloned();
            let msg = format!(
                "not healthy after {} s: {}",
                spec.timeout.as_secs(),
                pending.join(", ")
            );
            return Err(match first {
                Some(id) => with_logs(state, &id, msg).await,
                None => msg,
            });
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// service name → container id, for the app's containers in root Docker.
pub async fn containers_by_service(state: &State, app: &str) -> Result<HashMap<String, String>, String> {
    let list = state.helper.containers(&DockerEndpoint::Root).await?;
    let mut out = HashMap::new();
    for c in list {
        let labels = c.get("Labels");
        let project = labels
            .and_then(|l| l.get("com.docker.compose.project"))
            .and_then(Value::as_str);
        if project != Some(app) {
            continue;
        }
        let Some(svc) = labels
            .and_then(|l| l.get("com.docker.compose.service"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(id) = c.get("Id").and_then(Value::as_str) {
            out.insert(svc.to_string(), id.to_string());
        }
    }
    Ok(out)
}

async fn with_logs(state: &State, id: &str, msg: String) -> String {
    match state.helper.logs(&DockerEndpoint::Root, id, 15, None).await {
        Ok(l) if !l.lines.is_empty() => {
            let lines: Vec<String> = l.lines.iter().map(|r| format!("    {}", r.line)).collect();
            format!("{msg}\n  last log lines:\n{}", lines.join("\n"))
        }
        _ => msg,
    }
}

/// A minimal HTTP/1.0 GET to 127.0.0.1:port; returns the status code.
pub async fn probe(port: u16, path: &str) -> Option<u16> {
    let fut = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let req = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nUser-Agent: hearth-health\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.ok()?;
        parse_status(&buf[..n])
    };
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .ok()
        .flatten()
}

fn parse_status(head: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(head).ok()?;
    let mut parts = text.split_whitespace();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line() {
        assert_eq!(parse_status(b"HTTP/1.1 302 Found\r\n"), Some(302));
        assert_eq!(parse_status(b"HTTP/1.0 200 OK"), Some(200));
        assert_eq!(parse_status(b"SSH-2.0-OpenSSH"), None);
    }

    #[tokio::test]
    async fn probe_reads_status_from_a_local_server() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(e) => panic!("{e}"),
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n").await;
            }
        });
        assert_eq!(probe(port, "/").await, Some(401));
        assert_eq!(probe(1, "/").await, None, "closed port");
    }
}
