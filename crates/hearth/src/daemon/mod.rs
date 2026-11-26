//! `hearth daemon`: the unprivileged service behind the CLI (and later the web UI and app).

mod api;
mod api_changes;
pub mod apps;
mod auth;
pub mod db;
mod engine;
mod health;
mod helper_client;
mod jobs;
mod kinds;
mod migrate;
pub mod stats;
mod system;
mod versions;

use db::Db;
use hearth_registry::RegistryClient;
use helper_client::HelperClient;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket: PathBuf,
    pub helper_socket: PathBuf,
    pub state_dir: PathBuf,
    /// Delay before the first background version check.
    pub first_check_delay: Duration,
    /// Interval between background version checks.
    pub check_interval: Duration,
    /// Values for `PUID`/`PGID` when a compose file expects them.
    pub puid: u32,
    pub pgid: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            socket: "/run/hearth/hearth.sock".into(),
            helper_socket: "/run/hearth-helper/helper.sock".into(),
            state_dir: "/var/lib/hearth".into(),
            first_check_delay: Duration::from_secs(120),
            check_interval: Duration::from_secs(6 * 3600),
            puid: 1000,
            pgid: 1000,
        }
    }
}

#[derive(Clone)]
pub struct State {
    helper: HelperClient,
    db: Arc<Mutex<Db>>,
    registry: Arc<RegistryClient>,
    hostname: String,
    check_lock: Arc<tokio::sync::Mutex<()>>,
    jobs: Arc<jobs::Jobs>,
    host: Arc<HostDefaults>,
    system: Arc<system::SystemState>,
}

/// Values filled into compose files that expect a manager to provide them.
#[derive(Debug, Clone)]
pub struct HostDefaults {
    pub puid: u32,
    pub pgid: u32,
    pub tz: String,
}

/// The host's time zone: /etc/timezone, else the /etc/localtime link, else UTC.
pub fn host_timezone() -> String {
    if let Ok(t) = std::fs::read_to_string("/etc/timezone") {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            let s = p.to_string_lossy().into_owned();
            s.split_once("zoneinfo/").map(|(_, z)| z.to_string())
        })
        .unwrap_or_else(|| "UTC".to_string())
}

impl State {
    /// Runs `f` with the database. Never call across an `.await`.
    pub fn with_db<R>(&self, f: impl FnOnce(&Db) -> R) -> R {
        let guard = self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&guard)
    }
}

pub async fn run(cfg: DaemonConfig) -> Result<(), String> {
    std::fs::create_dir_all(&cfg.state_dir).map_err(|e| format!("{}: {e}", cfg.state_dir.display()))?;
    let db = Db::open(&cfg.state_dir.join("hearth.db")).map_err(|e| format!("database: {e}"))?;
    let registry =
        RegistryClient::new(concat!("hearth/", env!("CARGO_PKG_VERSION"))).map_err(|e| e.to_string())?;
    let state = State {
        helper: HelperClient::new(cfg.helper_socket.clone()),
        db: Arc::new(Mutex::new(db)),
        registry: Arc::new(registry),
        hostname: std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|h| h.trim().to_string())
            .unwrap_or_else(|_| "localhost".into()),
        check_lock: Arc::new(tokio::sync::Mutex::new(())),
        jobs: Arc::new(jobs::Jobs::default()),
        host: Arc::new(HostDefaults {
            puid: cfg.puid,
            pgid: cfg.pgid,
            tz: host_timezone(),
        }),
        system: Arc::new(system::SystemState::load(&cfg.state_dir)),
    };

    if let Some(dir) = cfg.socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let _ = std::fs::remove_file(&cfg.socket);
    let listener =
        tokio::net::UnixListener::bind(&cfg.socket).map_err(|e| format!("{}: {e}", cfg.socket.display()))?;
    // Owner and group (the `hearth` group) may connect; nobody else.
    std::fs::set_permissions(&cfg.socket, std::fs::Permissions::from_mode(0o660))
        .map_err(|e| format!("{}: {e}", cfg.socket.display()))?;

    match state.helper.call(&hearth_core::proto::Request::Ping).await {
        Ok(_) => tracing::info!("helper reachable"),
        Err(e) => tracing::warn!("helper not reachable yet: {e}"),
    }

    // Make Hearth apps match their desired state once Docker has settled after a boot.
    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        engine::reconcile(&st).await;
    });

    tokio::spawn(versions::scheduler(
        state.clone(),
        cfg.first_check_delay,
        cfg.check_interval,
    ));

    tokio::spawn(system::scheduler(state.clone()));

    tracing::info!(socket = %cfg.socket.display(), "daemon listening");
    let app = api::router(state).into_make_service_with_connect_info::<auth::Peer>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return std::future::pending().await,
    };
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    tracing::info!("shutting down");
}
