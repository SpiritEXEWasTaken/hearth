//! The daemon ↔ helper protocol.
//!
//! The helper runs as root and accepts only these typed operations, as one JSON object per line.
//! There is no "run this command" or raw Docker API passthrough: every operation is listed here,
//! and arguments are validated by the helper before use.

use crate::model::DockerEndpoint;
use serde::{Deserialize, Serialize};

/// Maximum log lines a single request may ask for.
pub const MAX_LOG_TAIL: u32 = 10_000;
/// Maximum bytes of log output returned by one request.
pub const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Docker endpoints that exist on this host (root daemon, rootless sockets under /run/user).
    ListEndpoints,
    /// `GET /containers/json?all=true`
    ListContainers {
        endpoint: DockerEndpoint,
    },
    /// `GET /containers/{id}/json`
    InspectContainer {
        endpoint: DockerEndpoint,
        id: String,
    },
    /// `GET /images/{id}/json`
    InspectImage {
        endpoint: DockerEndpoint,
        id: String,
    },
    /// `GET /containers/{id}/logs`, demultiplexed into lines.
    ContainerLogs {
        endpoint: DockerEndpoint,
        id: String,
        tail: u32,
        /// Unix seconds; only lines after this time.
        since: Option<i64>,
    },
    /// Names of CasaOS apps (directories under /var/lib/casaos/apps).
    ListCasaosApps,
    /// The CasaOS app's compose file, verbatim. The daemon redacts secrets before showing it to anyone.
    ReadCasaosApp {
        name: String,
    },

    // ----- Hearth-managed apps (root Docker, files under /var/lib/hearth/apps/<app>) -----
    /// Names of Hearth apps.
    ListHearthApps,
    /// `compose.yaml` and (if asked) `.env` of a Hearth app.
    ReadAppFiles {
        app: String,
        include_env: bool,
    },
    /// Writes `compose.yaml` and `.env` atomically and records a history revision.
    /// Returns the new revision number.
    WriteAppFiles {
        app: String,
        compose: String,
        env: String,
        message: String,
        actor: String,
    },
    /// Revision list (no file contents).
    ListHistory {
        app: String,
    },
    /// One revision's files.
    ReadRevision {
        app: String,
        revision: u32,
    },
    /// Moves the app directory to /var/lib/hearth/removed/<app>-<time> (nothing is deleted).
    RemoveAppDir {
        app: String,
    },
    /// Runs `docker compose` for a Hearth app. Streams progress lines.
    Compose {
        app: String,
        action: ComposeAction,
        /// Limit to these services (empty: all).
        services: Vec<String>,
    },
    /// `docker compose config --format json`: the fully resolved project.
    ComposeConfig {
        app: String,
    },
    /// Validates compose text (with its .env) without writing anything: `docker compose config -q`.
    ValidateCompose {
        app: String,
        compose: String,
        env: String,
    },
    /// `POST /images/{source}/tag`
    TagImage {
        source: String,
        repo: String,
        tag: String,
    },
    /// `DELETE /images/{name}` (untag; the image is removed when nothing else uses it).
    RemoveImage {
        name: String,
    },
    /// `GET /images/json` (root Docker).
    ListImages,
    /// Deletes data on uninstall: the project's named volumes, and bind sources under the
    /// Hearth data root. Other paths are reported, never deleted.
    DeleteAppData {
        app: String,
        volumes: Vec<String>,
        paths: Vec<String>,
    },
    /// Writes a backup archive of the app (files, named volumes, bind-mounted data).
    CreateBackup {
        app: String,
        /// Container paths to leave out (e.g. a media library).
        exclude_targets: Vec<String>,
    },
    ListBackups {
        app: String,
    },
    /// Restores data and files from a backup archive (the app must be stopped).
    RestoreBackup {
        app: String,
        file: String,
    },
    /// Deletes old backups beyond `keep`.
    PruneBackups {
        app: String,
        keep: u32,
    },
    /// Migration: archives /var/lib/casaos, /etc/casaos, the CasaOS unit files and the settings of
    /// every container and image (as JSON) into /var/backups/hearth/casaos/.
    BackupCasaos,
    /// `pg_dumpall` of a Postgres service of a compose project (any app, CasaOS or Hearth),
    /// streamed into /var/backups/hearth/<app>/<app>-<service>-db-<time>.sql.gz.
    DumpPostgres {
        app: String,
        service: String,
    },
    /// Hands an app back to CasaOS: `casaos-cli app-management install -f <CasaOS's own file>`
    /// (the app's containers must be removed first). CasaOS creates them asynchronously; the
    /// daemon waits and checks.
    CasaosApply {
        app: String,
    },

    // ----- M4: system updates (package manager, Flatpak) -----
    /// Distro, package backends and whether a reboot is needed.
    SystemInfo,
    /// Available updates from every backend. With `refresh`, the package lists are downloaded
    /// first (apt update / dnf makecache / Flatpak summaries); this can take a minute.
    SystemUpdates {
        refresh: bool,
    },
    /// Installs updates. `ids` are `SystemUpdate::id`s from the current list; empty means all
    /// (or all security updates with `security_only`). Streams progress lines.
    SystemUpgrade {
        ids: Vec<String>,
        security_only: bool,
    },

    // ----- M4b: packages -----
    /// Searches package names. `backend`: `native` (the distro's packages), `flatpak`, or
    /// `all`.
    PackageSearch {
        terms: Vec<String>,
        backend: String,
    },
    /// Details of one package.
    PackageInfo {
        name: String,
        backend: String,
    },
    /// Installs or removes packages. With `simulate`, nothing changes and the result is a
    /// `PackagePlan`; otherwise progress is streamed and the result is a `PackageResult`.
    PackageChange {
        action: PackageAction,
        names: Vec<String>,
        backend: String,
        simulate: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAction {
    Install,
    Remove,
}

impl PackageAction {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageAction::Install => "install",
            PackageAction::Remove => "remove",
        }
    }
}

/// A package name or Flatpak app id Hearth passes to a package manager: letters, digits and
/// `._+-:@/`, never starting with `-`.
pub fn is_valid_package_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-:@/".contains(c))
}

/// What `docker compose` should do. Each maps to one fixed command line in the helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComposeAction {
    Pull,
    /// `up -d --pull missing`; with `no_start`, `up --no-start` (create only). `no_deps` adds
    /// `--no-deps`.
    Up {
        no_start: bool,
        no_deps: bool,
        force_recreate: bool,
    },
    Start,
    Stop,
    Restart,
    /// `down`; never removes volumes unless `volumes` is true.
    Down {
        volumes: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok {
        data: serde_json::Value,
    },
    Err {
        message: String,
    },
    /// A progress line from a long operation; more messages follow until `Ok` or `Err`.
    Progress {
        line: String,
    },
}

/// One saved revision of an app's files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub revision: u32,
    pub time: i64,
    pub actor: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    pub file: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// Data of a `ContainerLogs` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Logs {
    pub lines: Vec<LogRecord>,
    /// True when output was cut at `MAX_LOG_BYTES`.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub stream: String,
    pub line: String,
}

/// Validates a Docker object id or name: letters, digits, `_`, `.`, `-`, and `:`/`/`/`@` for image
/// references. Rejects anything that could change the request path.
pub fn is_valid_docker_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-:/@".contains(c))
}

/// Validates a Hearth app name: it becomes the compose project name and a directory name,
/// so lowercase letters, digits, `-` and `_`, starting with a letter or digit.
pub fn is_valid_hearth_app_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Validates a compose service name.
pub fn is_valid_service_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// Validates a backup file name (as returned by `ListBackups`).
pub fn is_valid_backup_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('.')
        && s.ends_with(".tar.gz")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// Validates a CasaOS app (directory) name.
pub fn is_valid_app_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert!(is_valid_package_name("vim"));
        assert!(is_valid_package_name("libstdc++6"));
        assert!(is_valid_package_name("org.mozilla.firefox"));
        assert!(is_valid_package_name("python3.13:amd64"));
        assert!(!is_valid_package_name("--noconfirm"));
        assert!(!is_valid_package_name("vim; rm -rf /"));
        assert!(!is_valid_package_name(""));
    }

    #[test]
    fn request_wire_format_is_tagged() {
        let r = Request::InspectContainer {
            endpoint: DockerEndpoint::Rootless { uid: 1000 },
            id: "jellyfin".into(),
        };
        let s = serde_json::to_string(&r).unwrap_or_default();
        assert_eq!(
            s,
            r#"{"op":"inspect_container","endpoint":{"kind":"rootless","uid":1000},"id":"jellyfin"}"#
        );
    }

    #[test]
    fn docker_ref_validation() {
        assert!(is_valid_docker_ref("jellyfin"));
        assert!(is_valid_docker_ref("sha256:abc123"));
        assert!(is_valid_docker_ref("ghcr.io/immich-app/immich-server:v3"));
        assert!(!is_valid_docker_ref(""));
        assert!(!is_valid_docker_ref("../../etc"));
        assert!(!is_valid_docker_ref("a?all=1"));
        assert!(!is_valid_docker_ref("a b"));
        assert!(!is_valid_docker_ref("a#b"));
    }

    #[test]
    fn app_name_validation() {
        assert!(is_valid_app_name("affectionate_romain"));
        assert!(is_valid_app_name("big-bear-playit"));
        assert!(!is_valid_app_name("../etc"));
        assert!(!is_valid_app_name("a/b"));
        assert!(!is_valid_app_name(".hidden"));
        assert!(!is_valid_app_name(""));
    }
}
