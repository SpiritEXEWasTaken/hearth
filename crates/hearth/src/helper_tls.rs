//! Helper side of HTTPS: certificates for this machine's Tailscale names.
//!
//! tailscaled's LocalAPI issues (and renews) Let's Encrypt certificates for `<host>.<tailnet>.ts.net`
//! when HTTPS is enabled for the tailnet. Only root or the Tailscale operator may ask for them, so
//! the daemon gets them through the helper.

use crate::unixhttp;
use hearth_core::proto::TailscaleCert;
use std::path::Path;
use std::time::Duration;

const SOCKETS: &[&str] = &[
    "/run/tailscale/tailscaled.sock",
    "/var/run/tailscale/tailscaled.sock",
];
const HOST: &str = "local-tailscaled.sock";

async fn get(socket: &Path, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let resp = unixhttp::request(
        socket,
        http::Method::GET,
        path,
        &[
            ("host", HOST.to_string()),
            ("sec-tailscale", "localapi".to_string()),
        ],
        None,
        1024 * 1024,
        timeout,
    )
    .await
    .map_err(|e| format!("tailscaled: {e}"))?;
    if resp.status != 200 {
        return Err(format!(
            "tailscaled answered {} for {path}: {}",
            resp.status,
            String::from_utf8_lossy(&resp.body).trim()
        ));
    }
    Ok(resp.body.to_vec())
}

/// Certificates for every name tailscaled can issue one for. Ok(empty) when Tailscale isn't
/// installed or running.
pub async fn tailscale_certs() -> Result<Vec<TailscaleCert>, String> {
    let Some(socket) = SOCKETS.iter().map(Path::new).find(|p| p.exists()) else {
        return Ok(Vec::new());
    };
    let status = get(socket, "/localapi/v0/status?peers=false", Duration::from_secs(10)).await?;
    let status: serde_json::Value =
        serde_json::from_slice(&status).map_err(|e| format!("tailscale status: {e}"))?;
    let domains: Vec<String> = status
        .get("CertDomains")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for domain in domains {
        if !valid_domain(&domain) {
            continue;
        }
        // Issuing a new certificate can take a while the first time (ACME DNS-01 via Tailscale).
        let body = get(
            socket,
            &format!("/localapi/v0/cert/{domain}?type=pair"),
            Duration::from_secs(120),
        )
        .await?;
        let text = String::from_utf8(body).map_err(|_| "certificate isn't text".to_string())?;
        let (key_pem, cert_pem) =
            split_pair(&text).ok_or("tailscaled's answer has no key and certificate")?;
        out.push(TailscaleCert {
            domain,
            cert_pem,
            key_pem,
        });
    }
    Ok(out)
}

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Splits tailscaled's "key then certificate chain" PEM into (key, chain).
fn split_pair(text: &str) -> Option<(String, String)> {
    let mut key = String::new();
    let mut certs = String::new();
    let mut current = String::new();
    for line in text.lines() {
        current.push_str(line);
        current.push('\n');
        if line.starts_with("-----END ") {
            if line.contains("PRIVATE KEY") {
                key.push_str(&current);
            } else if line.contains("CERTIFICATE") {
                certs.push_str(&current);
            }
            current.clear();
        }
    }
    (!key.is_empty() && !certs.is_empty()).then_some((key, certs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_key_and_chain() {
        let t = "-----BEGIN EC PRIVATE KEY-----\nAAA\n-----END EC PRIVATE KEY-----\n\
                 -----BEGIN CERTIFICATE-----\nBBB\n-----END CERTIFICATE-----\n\
                 -----BEGIN CERTIFICATE-----\nCCC\n-----END CERTIFICATE-----\n";
        let (k, c) = split_pair(t).expect("test");
        assert!(k.contains("AAA") && !k.contains("BBB"));
        assert!(c.contains("BBB") && c.contains("CCC") && !c.contains("AAA"));
        assert!(split_pair("nothing").is_none());
        assert!(valid_domain("jarvis.tail559c11.ts.net"));
        assert!(!valid_domain("../etc"));
    }
}
