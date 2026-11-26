//! `hearth helper`: the small privileged process.
//!
//! It runs as root (it needs the root Docker socket, users' rootless Docker sockets, the
//! root-only CasaOS files and Hearth's app folders) and answers only the typed operations in
//! [`hearth_core::proto::Request`]. Changes are limited to Hearth-managed apps (root Docker,
//! `/var/lib/hearth/apps`); everything else is read-only. Connections are accepted only from root and from the
//! Hearth daemon's user, checked with the kernel's peer credentials (SO_PEERCRED).

use crate::{helper_apps, helper_units, sysupdate, unixhttp};
use hearth_core::model::DockerEndpoint;
use hearth_core::proto::{self, LogRecord, Logs, Request, Response};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST_LINE: u64 = 64 * 1024;
const MAX_COMPOSE_BYTES: u64 = 2 * 1024 * 1024;
const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct HelperConfig {
    pub socket: PathBuf,
    /// The daemon's uid; it (and root) may connect.
    pub daemon_uid: Option<u32>,
    pub docker_socket: PathBuf,
    /// Directory holding per-user runtime dirs (`/run/user`), for rootless Docker sockets.
    pub user_runtime_dir: PathBuf,
    pub casaos_apps_dir: PathBuf,
    /// The daemon's group: app folders and compose files are readable by it.
    pub daemon_gid: Option<u32>,
    pub apps_dir: PathBuf,
    pub removed_dir: PathBuf,
    pub data_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub docker_bin: PathBuf,
}

impl Default for HelperConfig {
    fn default() -> Self {
        HelperConfig {
            socket: "/run/hearth-helper/helper.sock".into(),
            daemon_uid: None,
            docker_socket: "/run/docker.sock".into(),
            user_runtime_dir: "/run/user".into(),
            casaos_apps_dir: "/var/lib/casaos/apps".into(),
            daemon_gid: None,
            apps_dir: "/var/lib/hearth/apps".into(),
            removed_dir: "/var/lib/hearth/removed".into(),
            data_dir: "/var/lib/hearth/data".into(),
            backups_dir: "/var/backups/hearth".into(),
            docker_bin: "/usr/bin/docker".into(),
        }
    }
}

pub async fn run(cfg: HelperConfig) -> std::io::Result<()> {
    if let Some(dir) = cfg.socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&cfg.socket);
    let listener = UnixListener::bind(&cfg.socket)?;
    // Only the socket's owner may connect; ownership goes to the daemon user.
    std::fs::set_permissions(&cfg.socket, std::fs::Permissions::from_mode(0o600))?;
    if let Some(uid) = cfg.daemon_uid {
        rustix::fs::chown(&cfg.socket, Some(rustix::fs::Uid::from_raw(uid)), None)
            .map_err(std::io::Error::from)?;
    }
    tracing::info!(socket = %cfg.socket.display(), daemon_uid = ?cfg.daemon_uid, "helper listening");
    let cfg = Arc::new(cfg);
    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(stream, &cfg).await {
                tracing::debug!("helper connection ended: {e}");
            }
        });
    }
}

async fn serve_conn(stream: UnixStream, cfg: &HelperConfig) -> std::io::Result<()> {
    let cred = stream.peer_cred()?;
    let uid = cred.uid();
    if uid != 0 && Some(uid) != cfg.daemon_uid {
        tracing::warn!(uid, "rejected helper connection from unauthorised uid");
        return Ok(());
    }
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    loop {
        let mut line = Vec::new();
        let n = (&mut reader)
            .take(MAX_REQUEST_LINE)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Ok(());
        }
        let resp = match serde_json::from_slice::<Request>(&line) {
            Ok(req) => {
                log_request(&req, uid);
                let mut out = Out { w: &mut w, ok: true };
                let resp = handle(req, cfg, &mut out).await;
                if !out.ok {
                    return Ok(()); // the daemon went away mid-operation
                }
                resp
            }
            Err(e) => Response::Err {
                message: format!("bad request: {e}"),
            },
        };
        write_response(&mut w, &resp).await?;
    }
}

async fn write_response(w: &mut OwnedWriteHalf, resp: &Response) -> std::io::Result<()> {
    let mut out =
        serde_json::to_vec(resp).unwrap_or_else(|_| b"{\"status\":\"err\",\"message\":\"encode\"}".to_vec());
    out.push(b'\n');
    w.write_all(&out).await
}

/// Streams progress lines to the daemon during a long operation.
pub struct Out<'a> {
    w: &'a mut OwnedWriteHalf,
    ok: bool,
}

impl Out<'_> {
    pub async fn progress(&mut self, line: &str) {
        if !self.ok {
            return;
        }
        let resp = Response::Progress {
            line: line.chars().take(2000).collect(),
        };
        if write_response(self.w, &resp).await.is_err() {
            self.ok = false;
        }
    }
}

/// Runs `work`, forwarding the lines it sends to the daemon as progress while it runs.
async fn with_progress<F, Fut, T>(out: &mut Out<'_>, work: F) -> T
where
    F: FnOnce(sysupdate::Tx) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let fut = work(tx);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => {
                while let Ok(line) = rx.try_recv() {
                    out.progress(&line).await;
                }
                return r;
            }
            Some(line) = rx.recv() => out.progress(&line).await,
        }
    }
}

/// Changes are logged at info level (they end up in the journal as an audit trail).
fn log_request(req: &Request, uid: u32) {
    match req {
        Request::WriteAppFiles {
            app, message, actor, ..
        } => {
            tracing::info!(uid, app, actor, message, "write app files")
        }
        Request::Compose {
            app,
            action,
            services,
        } => {
            tracing::info!(uid, app, ?action, ?services, "docker compose")
        }
        Request::RemoveAppDir { app } => tracing::info!(uid, app, "remove app folder"),
        Request::TagImage { source, repo, tag } => tracing::info!(uid, source, repo, tag, "tag image"),
        Request::RemoveImage { name } => tracing::info!(uid, name, "remove image"),
        Request::DeleteAppData { app, volumes, paths } => {
            tracing::info!(uid, app, ?volumes, ?paths, "delete app data")
        }
        Request::CreateBackup { app, .. } => tracing::info!(uid, app, "create backup"),
        Request::RestoreBackup { app, file } => tracing::info!(uid, app, file, "restore backup"),
        Request::PruneBackups { app, keep } => tracing::info!(uid, app, keep, "prune backups"),
        Request::BackupCasaos => tracing::info!(uid, "back up CasaOS"),
        Request::DumpPostgres { app, service } => tracing::info!(uid, app, service, "dump postgres"),
        Request::CasaosApply { app } => tracing::info!(uid, app, "hand back to CasaOS"),
        Request::SystemUpgrade { ids, security_only } => {
            tracing::info!(uid, ?ids, security_only, "system upgrade")
        }
        Request::SystemUpdates { refresh: true } => tracing::info!(uid, "refresh package lists"),
        Request::WriteAppDefinition {
            app, actor, message, ..
        } => {
            tracing::info!(uid, app, actor, message, "write app definition")
        }
        Request::UnitAction { unit, user, action } => {
            tracing::info!(uid, unit, ?user, ?action, "unit action")
        }
        Request::InstallNativeUnit { app, .. } => tracing::info!(uid, app, "install native unit"),
        Request::RemoveNativeUnit { app } => tracing::info!(uid, app, "remove native unit"),
        Request::PackageChange {
            action,
            names,
            backend,
            simulate: false,
        } => tracing::info!(uid, action = action.as_str(), ?names, backend, "package change"),
        other => tracing::debug!(?other, uid, "helper request"),
    }
}

async fn handle(req: Request, cfg: &HelperConfig, out: &mut Out<'_>) -> Response {
    let result: Result<Value, String> = match req {
        Request::Ping => Ok(Value::String("pong".into())),
        Request::ListEndpoints => Ok(serde_json::to_value(list_endpoints(cfg)).unwrap_or(Value::Null)),
        Request::ListContainers { endpoint } => {
            docker_json(cfg, &endpoint, "/containers/json?all=true").await
        }
        Request::InspectContainer { endpoint, id } => {
            if !proto::is_valid_docker_ref(&id) {
                Err("invalid container id".into())
            } else {
                docker_json(cfg, &endpoint, &format!("/containers/{id}/json")).await
            }
        }
        Request::InspectImage { endpoint, id } => {
            if !proto::is_valid_docker_ref(&id) {
                Err("invalid image id".into())
            } else {
                docker_json(cfg, &endpoint, &format!("/images/{id}/json")).await
            }
        }
        Request::ContainerLogs {
            endpoint,
            id,
            tail,
            since,
        } => container_logs(cfg, &endpoint, &id, tail, since).await,
        Request::ListCasaosApps => list_casaos_apps(&cfg.casaos_apps_dir),
        Request::ReadCasaosApp { name } => read_casaos_app(&cfg.casaos_apps_dir, &name).await,
        Request::ListHearthApps => helper_apps::list_apps(cfg),
        Request::ReadAppFiles { app, include_env } => helper_apps::read_files(cfg, &app, include_env),
        Request::WriteAppFiles {
            app,
            compose,
            env,
            message,
            actor,
        } => helper_apps::write_files(cfg, &app, &compose, &env, &message, &actor),
        Request::ListHistory { app } => helper_apps::list_history(cfg, &app),
        Request::ReadRevision { app, revision } => helper_apps::read_revision(cfg, &app, revision),
        Request::RemoveAppDir { app } => helper_apps::remove_app_dir(cfg, &app),
        Request::Compose {
            app,
            action,
            services,
        } => helper_apps::compose(cfg, &app, &action, &services, out).await,
        Request::ComposeConfig { app } => helper_apps::compose_config(cfg, &app).await,
        Request::ValidateCompose { app, compose, env } => {
            helper_apps::validate(cfg, &app, &compose, &env).await
        }
        Request::TagImage { source, repo, tag } => helper_apps::tag_image(cfg, &source, &repo, &tag).await,
        Request::RemoveImage { name } => helper_apps::remove_image(cfg, &name).await,
        Request::ListImages => helper_apps::list_images(cfg).await,
        Request::DeleteAppData { app, volumes, paths } => {
            helper_apps::delete_data(cfg, &app, &volumes, &paths, out).await
        }
        Request::CreateBackup { app, exclude_targets } => {
            helper_apps::create_backup(cfg, &app, &exclude_targets, out).await
        }
        Request::ListBackups { app } => helper_apps::list_backups(cfg, &app),
        Request::RestoreBackup { app, file } => helper_apps::restore_backup(cfg, &app, &file, out).await,
        Request::PruneBackups { app, keep } => helper_apps::prune_backups(cfg, &app, keep),
        Request::BackupCasaos => helper_apps::backup_casaos(cfg, out).await,
        Request::DumpPostgres { app, service } => helper_apps::dump_postgres(cfg, &app, &service, out).await,
        Request::CasaosApply { app } => helper_apps::casaos_apply(cfg, &app, out).await,
        Request::SystemInfo => Ok(serde_json::to_value(sysupdate::info().await).unwrap_or(Value::Null)),
        Request::SystemUpdates { refresh } => {
            let r = with_progress(out, |tx| async move { sysupdate::updates(refresh, &tx).await }).await;
            Ok(serde_json::to_value(r).unwrap_or(Value::Null))
        }
        Request::SystemUpgrade { ids, security_only } => with_progress(out, |tx| async move {
            sysupdate::upgrade(&ids, security_only, &tx).await
        })
        .await
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        Request::ListAppDefinitions => helper_apps::list_definitions(cfg),
        Request::WriteAppDefinition {
            app,
            yaml,
            message,
            actor,
        } => helper_apps::write_definition(cfg, &app, &yaml, &message, &actor),
        Request::UnitStatus { unit, user } => helper_units::status(&unit, user.as_deref()).await,
        Request::UnitAction { unit, user, action } => {
            helper_units::action(&unit, user.as_deref(), action).await
        }
        Request::UnitLogs {
            unit,
            user,
            tail,
            since,
        } => helper_units::logs(&unit, user.as_deref(), tail, since).await,
        Request::InstallNativeUnit { app, unit_text } => {
            helper_units::install_native(cfg, &app, &unit_text).await
        }
        Request::RemoveNativeUnit { app } => helper_units::remove_native(&app).await,
        Request::PackageSearch { terms, backend } => sysupdate::package_search(&terms, &backend)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        Request::PackageInfo { name, backend } => sysupdate::package_info(&name, &backend)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        Request::PackageChange {
            action,
            names,
            backend,
            simulate,
        } => {
            with_progress(out, |tx| async move {
                sysupdate::package_change(action, &names, &backend, simulate, &tx).await
            })
            .await
        }
    };
    match result {
        Ok(data) => Response::Ok { data },
        Err(message) => Response::Err { message },
    }
}

fn socket_for(cfg: &HelperConfig, endpoint: &DockerEndpoint) -> PathBuf {
    match endpoint {
        DockerEndpoint::Root => cfg.docker_socket.clone(),
        DockerEndpoint::Rootless { uid } => cfg.user_runtime_dir.join(uid.to_string()).join("docker.sock"),
    }
}

fn list_endpoints(cfg: &HelperConfig) -> Vec<DockerEndpoint> {
    let mut out = Vec::new();
    if cfg.docker_socket.exists() {
        out.push(DockerEndpoint::Root);
    }
    if let Ok(dir) = std::fs::read_dir(&cfg.user_runtime_dir) {
        let mut uids: Vec<u32> = dir
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .filter(|uid: &u32| {
                cfg.user_runtime_dir
                    .join(uid.to_string())
                    .join("docker.sock")
                    .exists()
            })
            .collect();
        uids.sort_unstable();
        out.extend(uids.into_iter().map(|uid| DockerEndpoint::Rootless { uid }));
    }
    out
}

async fn docker_get(
    cfg: &HelperConfig,
    endpoint: &DockerEndpoint,
    path: &str,
    max: usize,
) -> Result<unixhttp::Response, String> {
    let sock = socket_for(cfg, endpoint);
    let resp = unixhttp::request(&sock, http::Method::GET, path, &[], None, max, DOCKER_TIMEOUT)
        .await
        .map_err(|e| format!("docker ({}) unreachable: {e}", endpoint.label()))?;
    if resp.status >= 300 {
        let msg = serde_json::from_slice::<Value>(&resp.body)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&resp.body).into_owned());
        return Err(format!("docker ({}) {}: {msg}", endpoint.label(), resp.status));
    }
    Ok(resp)
}

async fn docker_json(cfg: &HelperConfig, endpoint: &DockerEndpoint, path: &str) -> Result<Value, String> {
    let resp = docker_get(cfg, endpoint, path, 16 * 1024 * 1024).await?;
    serde_json::from_slice(&resp.body).map_err(|e| format!("docker returned invalid JSON: {e}"))
}

async fn container_logs(
    cfg: &HelperConfig,
    endpoint: &DockerEndpoint,
    id: &str,
    tail: u32,
    since: Option<i64>,
) -> Result<Value, String> {
    if !proto::is_valid_docker_ref(id) {
        return Err("invalid container id".into());
    }
    let tail = tail.clamp(1, proto::MAX_LOG_TAIL);
    let info = docker_json(cfg, endpoint, &format!("/containers/{id}/json")).await?;
    let tty = info
        .pointer("/Config/Tty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut path = format!("/containers/{id}/logs?stdout=true&stderr=true&tail={tail}");
    if let Some(s) = since {
        path.push_str(&format!("&since={s}"));
    }
    // Ask for a bit more than we return so a cut never lands mid-frame silently.
    let resp = docker_get(cfg, endpoint, &path, proto::MAX_LOG_BYTES * 2).await?;
    let logs = if tty {
        split_tty(&resp.body)
    } else {
        demux(&resp.body)
    };
    serde_json::to_value(logs).map_err(|e| e.to_string())
}

/// Splits Docker's multiplexed log stream (8-byte frame headers) into lines.
pub fn demux(mut data: &[u8]) -> Logs {
    let mut logs = Logs::default();
    let mut bytes = 0usize;
    let mut partial: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    while data.len() >= 8 {
        let stream = data[0];
        let size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let end = (8 + size).min(data.len());
        let payload = &data[8..end];
        data = &data[end..];
        let idx = usize::from(stream == 2);
        partial[idx].extend_from_slice(payload);
        while let Some(pos) = partial[idx].iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = partial[idx].drain(..=pos).collect();
            bytes += line.len();
            if bytes > proto::MAX_LOG_BYTES {
                logs.truncated = true;
                return logs;
            }
            logs.lines.push(LogRecord {
                stream: if idx == 1 { "stderr" } else { "stdout" }.into(),
                line: String::from_utf8_lossy(&line[..line.len() - 1])
                    .trim_end_matches('\r')
                    .to_string(),
            });
        }
    }
    for (idx, rest) in partial.iter().enumerate() {
        if !rest.is_empty() {
            logs.lines.push(LogRecord {
                stream: if idx == 1 { "stderr" } else { "stdout" }.into(),
                line: String::from_utf8_lossy(rest).into_owned(),
            });
        }
    }
    logs
}

fn split_tty(data: &[u8]) -> Logs {
    let text = String::from_utf8_lossy(data);
    let mut logs = Logs::default();
    let mut bytes = 0;
    for l in text.lines() {
        bytes += l.len() + 1;
        if bytes > proto::MAX_LOG_BYTES {
            logs.truncated = true;
            break;
        }
        logs.lines.push(LogRecord {
            stream: "stdout".into(),
            line: l.trim_end_matches('\r').to_string(),
        });
    }
    logs
}

fn list_casaos_apps(dir: &Path) -> Result<Value, String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Array(vec![])),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("docker-compose.yml").is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| proto::is_valid_app_name(n))
        .collect();
    names.sort();
    Ok(serde_json::to_value(names).unwrap_or(Value::Null))
}

async fn read_casaos_app(dir: &Path, name: &str) -> Result<Value, String> {
    if !proto::is_valid_app_name(name) {
        return Err("invalid app name".into());
    }
    let app_dir = dir.join(name);
    match std::fs::symlink_metadata(&app_dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", app_dir.display())),
        Err(e) => return Err(format!("{}: {e}", app_dir.display())),
    }
    let path = app_dir.join("docker-compose.yml");
    // CasaOS app directories are world-writable (0777), so refuse symlinks atomically with
    // O_NOFOLLOW and check the opened file itself: a local user must not be able to make the
    // helper read another root-only file.
    let fd = rustix::fs::open(
        &path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let file = std::fs::File::from(fd);
    let meta = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > MAX_COMPOSE_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(&file, MAX_COMPOSE_BYTES), &mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Value::String(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![stream, 0, 0, 0];
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn demux_splits_streams_and_joins_partial_lines() {
        let mut data = frame(1, b"hello\nwor");
        data.extend(frame(2, b"oops\n"));
        data.extend(frame(1, b"ld\n"));
        let logs = demux(&data);
        let lines: Vec<(&str, &str)> = logs
            .lines
            .iter()
            .map(|l| (l.stream.as_str(), l.line.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![("stdout", "hello"), ("stderr", "oops"), ("stdout", "world")]
        );
        assert!(!logs.truncated);
    }

    #[test]
    fn demux_keeps_trailing_partial_line() {
        let logs = demux(&frame(1, b"no newline"));
        assert_eq!(logs.lines.len(), 1);
        assert_eq!(logs.lines[0].line, "no newline");
    }

    #[test]
    fn casaos_reader_refuses_symlinks_and_bad_names() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        let app = tmp.path().join("evil");
        let _ = std::fs::create_dir_all(&app);
        let _ = std::os::unix::fs::symlink("/etc/hostname", app.join("docker-compose.yml"));
        let rt = match tokio::runtime::Builder::new_current_thread().build() {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        let r = rt.block_on(read_casaos_app(tmp.path(), "evil"));
        assert!(r.is_err(), "symlink must be refused");
        let r = rt.block_on(read_casaos_app(tmp.path(), "../etc"));
        assert!(r.is_err());
    }
}
