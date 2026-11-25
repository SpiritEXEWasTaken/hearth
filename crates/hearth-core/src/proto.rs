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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: serde_json::Value },
    Err { message: String },
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
