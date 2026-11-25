//! Client for the privileged helper's socket.

use hearth_core::model::DockerEndpoint;
use hearth_core::proto::{Logs, Request, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MAX_RESPONSE: u64 = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
/// Longest silence allowed during a streaming operation (an image layer can take a while).
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct HelperClient {
    socket: PathBuf,
}

impl HelperClient {
    pub fn new(socket: PathBuf) -> Self {
        HelperClient { socket }
    }

    /// A request with a single response (60 s timeout).
    pub async fn call(&self, req: &Request) -> Result<Value, String> {
        tokio::time::timeout(TIMEOUT, self.call_stream(req, |_| {}))
            .await
            .map_err(|_| "helper request timed out".to_string())?
    }

    /// A request that may stream progress lines before its final response. Long operations
    /// (pulls, backups) have no overall timeout, but the helper must say something at least
    /// every `IDLE_TIMEOUT`.
    pub async fn call_stream(
        &self,
        req: &Request,
        mut on_progress: impl FnMut(&str),
    ) -> Result<Value, String> {
        let mut stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| format!("helper unreachable at {}: {e}", self.socket.display()))?;
        let mut line = serde_json::to_vec(req).map_err(|e| e.to_string())?;
        line.push(b'\n');
        stream.write_all(&line).await.map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream);
        loop {
            let mut buf = Vec::new();
            let mut limited = (&mut reader).take(MAX_RESPONSE);
            let read = limited.read_until(b'\n', &mut buf);
            tokio::time::timeout(IDLE_TIMEOUT, read)
                .await
                .map_err(|_| "the helper stopped responding".to_string())?
                .map_err(|e| e.to_string())?;
            if buf.is_empty() {
                return Err("helper closed the connection (not authorised?)".to_string());
            }
            match serde_json::from_slice::<Response>(&buf).map_err(|e| format!("bad helper response: {e}"))? {
                Response::Ok { data } => return Ok(data),
                Response::Err { message } => return Err(message),
                Response::Progress { line } => on_progress(&line),
            }
        }
    }

    pub async fn call_as<T: DeserializeOwned>(&self, req: &Request) -> Result<T, String> {
        let v = self.call(req).await?;
        serde_json::from_value(v).map_err(|e| format!("unexpected helper data: {e}"))
    }

    pub async fn endpoints(&self) -> Result<Vec<DockerEndpoint>, String> {
        self.call_as(&Request::ListEndpoints).await
    }

    pub async fn containers(&self, endpoint: &DockerEndpoint) -> Result<Vec<Value>, String> {
        self.call_as(&Request::ListContainers {
            endpoint: endpoint.clone(),
        })
        .await
    }

    pub async fn inspect_container(&self, endpoint: &DockerEndpoint, id: &str) -> Result<Value, String> {
        self.call(&Request::InspectContainer {
            endpoint: endpoint.clone(),
            id: id.to_string(),
        })
        .await
    }

    pub async fn inspect_image(&self, endpoint: &DockerEndpoint, id: &str) -> Result<Value, String> {
        self.call(&Request::InspectImage {
            endpoint: endpoint.clone(),
            id: id.to_string(),
        })
        .await
    }

    pub async fn logs(
        &self,
        endpoint: &DockerEndpoint,
        id: &str,
        tail: u32,
        since: Option<i64>,
    ) -> Result<Logs, String> {
        self.call_as(&Request::ContainerLogs {
            endpoint: endpoint.clone(),
            id: id.to_string(),
            tail,
            since,
        })
        .await
    }

    pub async fn casaos_app(&self, name: &str) -> Result<String, String> {
        self.call_as(&Request::ReadCasaosApp {
            name: name.to_string(),
        })
        .await
    }
}
