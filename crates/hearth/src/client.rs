//! The CLI's connection to the daemon.

use crate::unixhttp;
use hearth_core::{Error, ExitCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Client {
    socket: PathBuf,
    token: Option<String>,
}

impl Client {
    pub fn new(socket: &Path) -> Client {
        Client {
            socket: socket.to_path_buf(),
            token: load_token(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.call(http::Method::GET, path, None).await
    }

    pub async fn post<T: DeserializeOwned>(&self, path: &str, body: Option<Value>) -> Result<T, Error> {
        self.call(http::Method::POST, path, body).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.call(http::Method::DELETE, path, None).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: http::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, Error> {
        let mut headers = Vec::new();
        if let Some(t) = &self.token {
            headers.push(("authorization", format!("Bearer {t}")));
        }
        let body = body.map(|b| serde_json::to_vec(&b).unwrap_or_default());
        let resp = unixhttp::request(
            &self.socket,
            method,
            path,
            &headers,
            body,
            64 * 1024 * 1024,
            Duration::from_secs(300),
        )
        .await
        .map_err(|e| connect_error(&self.socket, e))?;
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| Error::Other(format!("daemon returned invalid JSON ({}): {e}", resp.status)))?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            serde_json::from_value(v.get("data").cloned().unwrap_or(Value::Null))
                .map_err(|e| Error::Other(format!("unexpected daemon data: {e}")))
        } else {
            let kind = v
                .pointer("/error/kind")
                .and_then(Value::as_str)
                .unwrap_or("other");
            let message = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            Err(Error::Remote {
                kind: kind.to_string(),
                message,
                exit: exit_for_kind(kind),
            })
        }
    }
}

fn exit_for_kind(kind: &str) -> ExitCode {
    match kind {
        "usage" => ExitCode::Usage,
        "not_found" => ExitCode::NotFound,
        "needs_confirmation" => ExitCode::NeedsConfirmation,
        "unauthorized" => ExitCode::Unauthorized,
        "rolled_back" => ExitCode::RolledBack,
        "rollback_failed" => ExitCode::RollbackFailed,
        _ => ExitCode::Error,
    }
}

fn connect_error(socket: &Path, e: std::io::Error) -> Error {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::Other(format!(
            "the Hearth daemon isn't running (nothing at {}). Start it with: sudo systemctl start hearth",
            socket.display()
        )),
        std::io::ErrorKind::PermissionDenied => Error::Unauthorized(format!(
            "no access to {}: add your user to the 'hearth' group (sudo usermod -aG hearth $USER) and log in again",
            socket.display()
        )),
        _ => Error::Other(format!("cannot talk to the daemon at {}: {e}", socket.display())),
    }
}

/// `HEARTH_TOKEN`, else `~/.config/hearth/token`.
fn load_token() -> Option<String> {
    if let Ok(t) = std::env::var("HEARTH_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    std::fs::read_to_string(base.join("hearth/token"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}
