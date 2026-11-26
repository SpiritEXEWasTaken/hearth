//! The API data model, shared by the daemon (producer) and the CLI, web UI and Android app (consumers).
//!
//! Field names are part of the `--json` contract: add fields freely, never rename or remove them.

use serde::{Deserialize, Serialize};

/// `GET /v1/status`: host summary plus every app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub host: HostStats,
    pub apps: Vec<AppSummary>,
    /// Problems worth showing (e.g. "rootless Docker for uid 1000 not reachable").
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostStats {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
    pub load: [f64; 3],
    pub cpu_count: u32,
    /// CPU busy percentage over a short sample (0–100).
    pub cpu_percent: f64,
    pub memory: MemoryStats,
    /// Memory pressure (PSI "some" avg60, percent of time stalled), if the kernel exposes it.
    pub memory_pressure_avg60: Option<f64>,
    /// Out-of-memory kills since boot.
    pub oom_kills: Option<u64>,
    pub disks: Vec<DiskStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskStats {
    pub mount: String,
    pub device: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

/// Which Docker daemon an app runs in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockerEndpoint {
    /// The system (root) daemon, `/run/docker.sock`.
    Root,
    /// A user's rootless daemon, `/run/user/<uid>/docker.sock`.
    Rootless { uid: u32 },
}

impl DockerEndpoint {
    pub fn label(&self) -> String {
        match self {
            DockerEndpoint::Root => "root".into(),
            DockerEndpoint::Rootless { uid } => format!("rootless:{uid}"),
        }
    }
}

/// Who manages an app today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBy {
    /// Managed by Hearth.
    Hearth,
    /// A CasaOS app (compose project under /var/lib/casaos/apps), not yet adopted.
    Casaos,
    /// A compose project Hearth doesn't manage.
    Compose,
    /// A single container that isn't part of a compose project.
    Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppState {
    Running,
    /// Some containers running, some not.
    Partial,
    Stopped,
    /// Created/restarting/dead or otherwise not settled.
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    /// Stable id: the compose project name (or container name for single containers).
    pub id: String,
    pub title: String,
    /// What kind of app: a Docker Compose project, a systemd service, a program Hearth runs, a
    /// link, or an installed package.
    #[serde(default)]
    pub kind: AppKind,
    /// Shown in the main app list (the dashboard). True for apps with a web page; services and
    /// programs without one are managed in their own section.
    #[serde(default)]
    pub dashboard: bool,
    pub managed_by: ManagedBy,
    /// The Docker daemon, for Docker apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<DockerEndpoint>,
    /// The systemd unit, for service, native and package apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitStatus>,
    /// Hearth can change this app (false for rootless/external apps until you opt in).
    pub read_only: bool,
    pub state: AppState,
    /// "healthy", "unhealthy", "starting", or None when no container has a healthcheck.
    pub health: Option<String>,
    pub memory_bytes: Option<u64>,
    pub web_url: Option<String>,
    /// Hearth serves the app over HTTPS on this port (https://<server>:<port>).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https_port: Option<u16>,
    /// `<subdomain>.<your domain>` also reaches the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https_subdomain: Option<String>,
    pub icon: Option<String>,
    pub containers: Vec<ContainerSummary>,
    pub images: Vec<ImageVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    /// Compose service name, if any.
    pub service: Option<String>,
    /// Docker state: running, exited, created, restarting, paused, dead.
    pub state: String,
    /// Docker's human status, e.g. "Up 3 hours (healthy)".
    pub status: String,
    pub health: Option<String>,
    pub image: String,
    pub memory_bytes: Option<u64>,
    pub restart_count: Option<u64>,
    pub started_at: Option<String>,
}

/// Version information for one service's image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageVersion {
    pub service: String,
    /// The image reference as written in the compose file, e.g. `linuxserver/jellyfin:latest`.
    pub reference: String,
    /// The manifest (index) digest the running image was pulled as.
    pub running_digest: Option<String>,
    /// Human-readable running version from image labels, e.g. "10.11.2".
    pub running_version: Option<String>,
    /// Result of the last registry check, if any.
    pub latest: Option<LatestInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestInfo {
    /// Unix seconds of the check.
    pub checked_at: i64,
    /// Digest the same tag points to now.
    pub digest: Option<String>,
    /// Human-readable version of `digest`, from its image labels.
    pub version: Option<String>,
    /// True when the tag now points to a different image than the one running.
    pub update_available: bool,
    /// A newer tag on the same track (e.g. `v3.4.2` when running `v3.4.1`). Never applied automatically.
    pub newer_in_track: Option<String>,
    /// A newer major version tag (e.g. `v4` when running `v3`). Informational only.
    pub newer_major: Option<String>,
    /// Why the check failed, if it did.
    pub error: Option<String>,
}

/// `GET /v1/apps/{id}`: one app with details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDetail {
    #[serde(flatten)]
    pub summary: AppSummary,
    /// Compose project working directory, if known.
    pub working_dir: Option<String>,
    /// Compose files used, if known.
    pub config_files: Vec<String>,
    /// Variables the compose file needs from outside (e.g. `PUID`, `AppID`): it won't run with
    /// plain `docker compose` unless these are provided. Empty for self-contained files.
    pub external_variables: Vec<String>,
    /// Devices listed in the compose file that don't exist on this host.
    pub missing_devices: Vec<String>,
    /// CasaOS store id, if the app came from a CasaOS store.
    pub store_app_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub service: String,
    /// "stdout" or "stderr".
    pub stream: String,
    pub line: String,
}

/// A newly created API token. The secret is shown exactly once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewToken {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Token scopes. A token can only do what its scopes allow; `admin` implies all.
pub mod scopes {
    pub const READ: &str = "read";
    pub const APPS_WRITE: &str = "apps:write";
    pub const SYSTEM_WRITE: &str = "system:write";
    pub const TERMINAL: &str = "terminal";
    pub const ADMIN: &str = "admin";
    pub const ALL: &[&str] = &[READ, APPS_WRITE, SYSTEM_WRITE, TERMINAL, ADMIN];

    pub fn is_valid(s: &str) -> bool {
        ALL.contains(&s)
    }
}

// ----- M2: jobs, plans, history, images -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Succeeded,
    Failed,
    /// The change failed its health check and was rolled back successfully.
    RolledBack,
    /// The change failed and so did the rollback: manual attention needed.
    RollbackFailed,
}

impl JobStatus {
    pub fn is_done(self) -> bool {
        self != JobStatus::Running
    }
}

/// A change running (or finished) in the daemon. Poll `GET /v1/jobs/{id}?after=N` for new events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// What it does: "install", "start", "stop", "restart", "apply", "update", "revert", "backup", "restore", "uninstall".
    pub kind: String,
    pub app: String,
    pub status: JobStatus,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Progress lines, in order. With `?after=N` only lines from index N on are returned.
    pub events: Vec<String>,
    /// Index of the first event in `events` (for incremental polling).
    pub events_from: usize,
    /// The job's outcome, when done.
    pub result: Option<serde_json::Value>,
    /// The error, when it failed.
    pub error: Option<String>,
}

/// Returned by every change request: the job that carries it out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRef {
    pub job: String,
}

/// What an update would do (`--dry-run`, and the confirmation prompt).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdatePlan {
    pub app: String,
    pub services: Vec<ServiceUpdate>,
    /// A backup will be taken first (a database image changes).
    pub backup_first: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceUpdate {
    pub service: String,
    pub reference: String,
    pub running_digest: Option<String>,
    pub running_version: Option<String>,
    pub new_digest: Option<String>,
    pub new_version: Option<String>,
    /// Suggestions only; never applied by `update`.
    pub newer_in_track: Option<String>,
    pub newer_major: Option<String>,
}

/// What applying a new compose file would do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyPlan {
    pub app: String,
    /// Unified diff of compose.yaml (secrets hidden).
    pub compose_diff: String,
    /// Names of `.env` variables added, changed or removed (values never shown).
    pub env_changes: Vec<String>,
    /// Decisions Hearth made while normalising the file (dropped devices, moved secrets…).
    pub decisions: Vec<String>,
    /// Problems that block the change.
    pub errors: Vec<String>,
    /// Warnings that don't block it.
    pub warnings: Vec<String>,
    /// True when nothing would change.
    pub unchanged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub revision: u32,
    pub time: i64,
    pub actor: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntry {
    pub id: String,
    pub tags: Vec<String>,
    pub size_bytes: u64,
    pub created: i64,
    /// Used by at least one container.
    pub in_use: bool,
    /// A Hearth rollback image (`hearth-rollback/...`).
    pub rollback: bool,
}

// ----- M4: system updates -----

/// A package backend Hearth found on this host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    /// `packagekit`, `pacman` or `flatpak`.
    pub kind: String,
    /// Whether it answers right now.
    pub available: bool,
    /// E.g. PackageKit's own backend (`apt`, `dnf5`, `zypp`), or the tool's version.
    pub detail: String,
    /// Why it isn't available, or a hint (e.g. "install packagekit").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RebootStatus {
    pub needed: bool,
    /// Why, in words (e.g. "kernel 6.12.118 is installed, 6.12.116 is running").
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// `ID` from /etc/os-release (`debian`, `fedora`, `opensuse-tumbleweed`, `cachyos`…).
    pub distro_id: String,
    /// `PRETTY_NAME` from /etc/os-release.
    pub distro_name: String,
    pub kernel: String,
    pub backends: Vec<BackendInfo>,
    pub reboot: RebootStatus,
}

/// One available update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemUpdate {
    /// `<backend>:<native id>`: what `hearth system upgrade <id>` takes.
    pub id: String,
    pub backend: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    pub new_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// From the distro's security channel (Debian/Ubuntu `-security`, Fedora/openSUSE advisories).
    /// Arch and Flatpak have no such channel, so this is always false there.
    pub security: bool,
    /// `low`, `normal`, `important`, `critical`, `security`, `bugfix`, `enhancement` or `unknown`.
    pub severity: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemUpdates {
    /// Unix time of this list; 0 = never checked.
    pub checked_at: i64,
    pub updates: Vec<SystemUpdate>,
    /// Backends that failed to answer, with the reason. The rest of the list is still valid.
    pub errors: Vec<String>,
    pub reboot: RebootStatus,
}

/// What `hearth system upgrade` would install.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpgradePlan {
    pub updates: Vec<SystemUpdate>,
    /// E.g. "Arch installs all updates together (partial upgrades aren't supported)".
    pub notes: Vec<String>,
    /// When the list was made (`hearth system updates --check` refreshes it).
    pub checked_at: i64,
}

/// What an upgrade did, per backend.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpgradeResult {
    pub installed: Vec<String>,
    pub failed: Vec<String>,
    pub errors: Vec<String>,
    pub reboot: RebootStatus,
}

// ----- M4b: packages -----

/// A package (or Flatpak app) found by a search, or part of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEntry {
    /// `packagekit`, `pacman` or `flatpak`.
    pub backend: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Repository, or Flatpak remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub installed: bool,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDetail {
    #[serde(flatten)]
    pub entry: PackageEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Download or installed size, as the backend reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// What installing or removing packages would do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackagePlan {
    /// `install` or `remove`.
    pub action: String,
    pub backend: String,
    /// What was asked for.
    pub names: Vec<String>,
    /// Everything that would be installed (dependencies included).
    pub install: Vec<PackageEntry>,
    /// Everything that would be removed (packages that depend on it included).
    pub remove: Vec<PackageEntry>,
    /// Packages that would be upgraded on the way.
    pub update: Vec<PackageEntry>,
    pub warnings: Vec<String>,
    /// Problems that block the change.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageResult {
    pub action: String,
    pub names: Vec<String>,
    pub reboot: RebootStatus,
}

// ----- M4c: app kinds -----

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    /// A Docker Compose project.
    #[default]
    Compose,
    /// An existing systemd unit (system or user), e.g. smbd or tailscaled.
    Service,
    /// A program Hearth runs as a hardened systemd unit it writes itself.
    Native,
    /// A shortcut to something Hearth doesn't run (a router, another PC); only checked for
    /// reachability.
    Link,
    /// An app installed through the package manager or Flatpak.
    Package,
}

impl AppKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AppKind::Compose => "compose",
            AppKind::Service => "service",
            AppKind::Native => "native",
            AppKind::Link => "link",
            AppKind::Package => "package",
        }
    }
}

/// A systemd unit's state, as `systemctl show` reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnitStatus {
    pub name: String,
    /// Owner of a user unit; None for system units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `loaded`, `not-found`, `masked`…
    pub load_state: String,
    /// `active`, `inactive`, `failed`, `activating`…
    pub active_state: String,
    pub sub_state: String,
    /// `enabled`, `disabled`, `static`, `linked`…
    pub unit_file_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_pid: Option<u32>,
    #[serde(default)]
    pub description: String,
}

/// `app.yaml` of a service, native, link or package app (compose apps use compose.yaml).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDefinition {
    pub kind: AppKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Override: show in the main app list or not. Default: when it has a web page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<bool>,
    /// A full web address (required for links).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A web page on this host: port and path (instead of `url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The systemd unit (service, package; native apps get `hearth-native-<id>.service`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Service: the owner of a user unit. Native: the user the program runs as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Native: the command line (absolute path first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<String>,
    /// Native: e.g. `512M`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_max: Option<String>,
    /// Native: `on-failure` (default), `always` or `no`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// Package: the package name or Flatpak app id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Package: `native` or `flatpak`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_backend: Option<String>,
}

impl AppDefinition {
    /// The web address: `url`, or `http://<host>:<port><path>`.
    pub fn web_url(&self, host: &str) -> Option<String> {
        if let Some(u) = &self.url {
            return Some(u.clone());
        }
        self.port.map(|p| {
            let path = self.path.clone().unwrap_or_else(|| "/".into());
            let path = if path.starts_with('/') {
                path
            } else {
                format!("/{path}")
            };
            format!("http://{host}:{p}{path}")
        })
    }

    pub fn on_dashboard(&self, host: &str) -> bool {
        self.dashboard.unwrap_or_else(|| self.web_url(host).is_some())
    }

    /// The systemd unit this app is about, if any.
    pub fn unit_name(&self, id: &str) -> Option<String> {
        match self.kind {
            AppKind::Native => Some(format!("hearth-native-{id}.service")),
            AppKind::Service | AppKind::Package => self.unit.clone(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn web_url_and_dashboard() {
        let mut d = AppDefinition {
            kind: AppKind::Service,
            title: "Samba".into(),
            unit: Some("smbd.service".into()),
            ..Default::default()
        };
        assert_eq!(d.web_url("jarvis"), None);
        assert!(!d.on_dashboard("jarvis"));
        d.port = Some(8080);
        d.path = Some("admin".into());
        assert_eq!(d.web_url("jarvis").as_deref(), Some("http://jarvis:8080/admin"));
        assert!(d.on_dashboard("jarvis"));
        d.dashboard = Some(false);
        assert!(!d.on_dashboard("jarvis"));
        let n = AppDefinition {
            kind: AppKind::Native,
            title: "x".into(),
            ..Default::default()
        };
        assert_eq!(n.unit_name("bot").as_deref(), Some("hearth-native-bot.service"));
    }

    #[test]
    fn old_app_summaries_still_parse() {
        // JSON from M3 (no kind/dashboard, endpoint as an object) reads as a compose app.
        let v = serde_json::json!({
            "id": "jellyfin", "title": "Jellyfin", "managed_by": "hearth",
            "endpoint": {"kind": "root"}, "read_only": false, "state": "running",
            "health": null, "memory_bytes": null, "web_url": null, "icon": null,
            "containers": [], "images": []
        });
        let s: Result<AppSummary, _> = serde_json::from_value(v);
        assert!(s.is_ok_and(|s| s.kind == AppKind::Compose && s.endpoint == Some(DockerEndpoint::Root)));
    }
}
