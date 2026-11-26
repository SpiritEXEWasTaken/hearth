//! Certificates for the web UI.
//!
//! - **Tailscale names** (`<host>.<tailnet>.ts.net`) get the real Let's Encrypt certificate that
//!   tailscaled issues, fetched through the helper and refreshed twice a day.
//! - **Everything else** (LAN IPs, `<host>`, `<host>.local`, localhost) gets a certificate from
//!   Hearth's own CA. The CA is made once and kept in `<state>/tls/`; devices that install
//!   `ca.crt` (offered at `/ca.crt`) trust Hearth without warnings. The server certificate is
//!   re-issued when the machine's names or addresses change, or a month before it expires.
//!
//! The right certificate is picked per connection from the name the browser asked for (SNI).

use super::State;
use hearth_core::proto::{Request, TailscaleCert};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, SanType,
};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SERVER_DAYS: i64 = 365;
const RENEW_BEFORE_DAYS: i64 = 30;

#[derive(Debug)]
pub struct CertStore {
    dir: PathBuf,
    hostname: String,
    local: RwLock<Option<Arc<CertifiedKey>>>,
    tailscale: RwLock<Vec<(String, Arc<CertifiedKey>)>>,
}

/// What the UI shows about HTTPS.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TlsInfo {
    pub tailscale_names: Vec<String>,
    pub local_names: Vec<String>,
    pub ca_fingerprint_sha256: Option<String>,
}

impl CertStore {
    pub fn new(state_dir: &Path, hostname: &str) -> Self {
        CertStore {
            dir: state_dir.join("tls"),
            hostname: hostname.to_string(),
            local: RwLock::new(None),
            tailscale: RwLock::new(Vec::new()),
        }
    }

    pub fn ca_pem(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("ca.crt")).ok()
    }

    pub fn info(&self) -> TlsInfo {
        let names = std::fs::read_to_string(self.dir.join("server.names")).unwrap_or_default();
        TlsInfo {
            tailscale_names: self
                .tailscale
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|(d, _)| d.clone())
                .collect(),
            local_names: names.lines().map(str::to_string).collect(),
            ca_fingerprint_sha256: self.ca_pem().and_then(|p| fingerprint(&p)),
        }
    }

    /// Makes sure the CA and a current server certificate exist, and loads them.
    pub fn refresh_local(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        set_mode(&self.dir, 0o700);
        let (ca_pem, ca_key_pem) = self.ensure_ca()?;
        let names = local_names(&self.hostname);
        let names_text = names.join("\n");
        let cert_path = self.dir.join("server.crt");
        let key_path = self.dir.join("server.key");
        let names_path = self.dir.join("server.names");
        let current = std::fs::read_to_string(&names_path).unwrap_or_default();
        let expires = std::fs::read_to_string(self.dir.join("server.expires"))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let now = super::db::now();
        let needs_new = current != names_text
            || expires - now < RENEW_BEFORE_DAYS * 86400
            || !cert_path.exists()
            || !key_path.exists();
        if needs_new {
            let (cert, key) = issue_server_cert(&ca_pem, &ca_key_pem, &self.hostname, &names)?;
            write_private(&key_path, &key)?;
            // The chain: server certificate, then the CA (so clients can show who issued it).
            write_private(&cert_path, &format!("{cert}{ca_pem}"))?;
            write_private(
                &self.dir.join("server.expires"),
                &(now + SERVER_DAYS * 86400).to_string(),
            )?;
            write_private(&names_path, &names_text)?;
            tracing::info!(names = ?names, "issued a new local HTTPS certificate");
        }
        let ck = certified_key(
            &std::fs::read_to_string(&cert_path).map_err(|e| e.to_string())?,
            &std::fs::read_to_string(&key_path).map_err(|e| e.to_string())?,
        )?;
        *self.local.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(ck));
        Ok(())
    }

    fn ensure_ca(&self) -> Result<(String, String), String> {
        let crt = self.dir.join("ca.crt");
        let key = self.dir.join("ca.key");
        if let (Ok(c), Ok(k)) = (std::fs::read_to_string(&crt), std::fs::read_to_string(&key)) {
            return Ok((c, k));
        }
        let kp = KeyPair::generate().map_err(|e| format!("CA key: {e}"))?;
        let mut p = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("Hearth local CA ({})", self.hostname));
        dn.push(DnType::OrganizationName, "Hearth");
        p.distinguished_name = dn;
        p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        p.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let (nb, na) = validity(20 * 365);
        p.not_before = nb;
        p.not_after = na;
        let cert = p.self_signed(&kp).map_err(|e| format!("CA certificate: {e}"))?;
        let (c, k) = (cert.pem(), kp.serialize_pem());
        write_private(&key, &k)?;
        write_private(&crt, &c)?;
        // The certificate is public: let anyone read it.
        set_mode(&crt, 0o644);
        tracing::info!("created Hearth's local certificate authority");
        Ok((c, k))
    }

    pub fn set_tailscale(&self, certs: Vec<TailscaleCert>) {
        let mut loaded = Vec::new();
        for c in certs {
            match certified_key(&c.cert_pem, &c.key_pem) {
                Ok(ck) => loaded.push((c.domain.to_ascii_lowercase(), Arc::new(ck))),
                Err(e) => tracing::warn!(domain = %c.domain, "bad Tailscale certificate: {e}"),
            }
        }
        *self.tailscale.write().unwrap_or_else(|p| p.into_inner()) = loaded;
    }

    fn pick(&self, sni: Option<&str>) -> Option<Arc<CertifiedKey>> {
        if let Some(name) = sni {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let ts = self.tailscale.read().unwrap_or_else(|p| p.into_inner());
            if let Some((_, ck)) = ts.iter().find(|(d, _)| *d == name) {
                return Some(ck.clone());
            }
        }
        self.local.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.pick(hello.server_name())
    }
}

/// Keeps the certificates current: the local one hourly (addresses can change), Tailscale's
/// twice a day (tailscaled renews it; we pick up the new one).
pub async fn refresher(state: State) {
    let mut tick = 0u32;
    loop {
        if tick.is_multiple_of(12) {
            fetch_tailscale(&state).await;
        }
        let certs = state.certs.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || certs.refresh_local())
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        {
            tracing::warn!("local HTTPS certificate: {e}");
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

pub async fn fetch_tailscale(state: &State) {
    match state
        .helper
        .call_long(&Request::TailscaleCerts, Duration::from_secs(300))
        .await
    {
        Ok(v) => match serde_json::from_value::<Vec<TailscaleCert>>(v) {
            Ok(certs) => {
                let names: Vec<_> = certs.iter().map(|c| c.domain.clone()).collect();
                state.certs.set_tailscale(certs);
                if !names.is_empty() {
                    tracing::info!(?names, "loaded Tailscale HTTPS certificates");
                }
            }
            Err(e) => tracing::warn!("Tailscale certificates: {e}"),
        },
        Err(e) => tracing::warn!("Tailscale certificates: {e}"),
    }
}

fn validity(days: i64) -> (time::OffsetDateTime, time::OffsetDateTime) {
    let now = time::OffsetDateTime::now_utc();
    (now - time::Duration::days(1), now + time::Duration::days(days))
}

/// Names and addresses the local certificate covers.
pub fn local_names(hostname: &str) -> Vec<String> {
    let mut names = vec![
        hostname.to_string(),
        format!("{hostname}.local"),
        "localhost".to_string(),
    ];
    let mut ips: Vec<IpAddr> = vec!["127.0.0.1".parse().expect("ip"), "::1".parse().expect("ip")];
    let ifs = if_addrs::get_if_addrs().unwrap_or_else(|e| {
        tracing::warn!("can't list this machine's addresses for the HTTPS certificate: {e}");
        Vec::new()
    });
    {
        for i in ifs {
            let ip = i.ip();
            // Docker bridges and link-local addresses aren't how anyone reaches the server.
            let skip = i.name.starts_with("docker")
                || i.name.starts_with("br-")
                || i.name.starts_with("veth")
                || match ip {
                    IpAddr::V4(v) => v.is_link_local(),
                    IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
                };
            if !skip && !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips.sort();
    names.extend(ips.iter().map(|i| i.to_string()));
    names
}

fn issue_server_cert(
    ca_pem: &str,
    ca_key_pem: &str,
    hostname: &str,
    names: &[String],
) -> Result<(String, String), String> {
    let ca_key = KeyPair::from_pem(ca_key_pem).map_err(|e| format!("CA key: {e}"))?;
    let issuer = Issuer::from_ca_cert_pem(ca_pem, ca_key).map_err(|e| format!("CA certificate: {e}"))?;
    let kp = KeyPair::generate().map_err(|e| format!("server key: {e}"))?;
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, hostname);
    p.distinguished_name = dn;
    p.subject_alt_names = names
        .iter()
        .filter_map(|n| match n.parse::<IpAddr>() {
            Ok(ip) => Some(SanType::IpAddress(ip)),
            Err(_) => n.clone().try_into().ok().map(SanType::DnsName),
        })
        .collect();
    p.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.use_authority_key_identifier_extension = true;
    let (nb, na) = validity(SERVER_DAYS);
    p.not_before = nb;
    p.not_after = na;
    let cert = p
        .signed_by(&kp, &issuer)
        .map_err(|e| format!("server certificate: {e}"))?;
    Ok((cert.pem(), kp.serialize_pem()))
}

fn certified_key(chain_pem: &str, key_pem: &str) -> Result<CertifiedKey, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("certificate: {e}"))?;
    if chain.is_empty() {
        return Err("no certificate".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| format!("key: {e}"))?;
    let signer =
        rustls::crypto::aws_lc_rs::sign::any_supported_type(&key).map_err(|e| format!("key: {e}"))?;
    Ok(CertifiedKey::new(chain, signer))
}

/// SHA-256 of the first certificate in a PEM, as colon-separated hex (what browsers show).
pub fn fingerprint(pem: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let der = CertificateDer::pem_slice_iter(pem.as_bytes()).next()?.ok()?;
    let d = Sha256::digest(der.as_ref());
    Some(d.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"))
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_ca_and_server_cert_and_picks_by_name() {
        let dir = tempfile::tempdir().expect("test");
        let store = CertStore::new(dir.path(), "testhost");
        store.refresh_local().expect("test");
        let ca = store.ca_pem().expect("test");
        assert!(ca.contains("BEGIN CERTIFICATE"));
        assert!(fingerprint(&ca).expect("test").len() == 95);
        let names = std::fs::read_to_string(dir.path().join("tls/server.names")).expect("test");
        assert!(names.contains("testhost.local") && names.contains("127.0.0.1"));
        let first = std::fs::read_to_string(dir.path().join("tls/server.crt")).expect("test");
        // Nothing changed: the certificate is kept.
        store.refresh_local().expect("test");
        assert_eq!(
            first,
            std::fs::read_to_string(dir.path().join("tls/server.crt")).expect("test")
        );
        assert!(store.pick(Some("192.168.1.5")).is_some());
        assert!(store.pick(None).is_some());

        // A "Tailscale" certificate (here: another one from the same CA) is picked by name.
        let ca_key = std::fs::read_to_string(dir.path().join("tls/ca.key")).expect("test");
        let (c, k) = issue_server_cert(&ca, &ca_key, "x", &["x.tail.ts.net".to_string()]).expect("test");
        store.set_tailscale(vec![TailscaleCert {
            domain: "x.tail.ts.net".into(),
            cert_pem: c,
            key_pem: k,
        }]);
        let ts = store.pick(Some("X.tail.ts.net.")).expect("test");
        let local = store.pick(Some("testhost")).expect("test");
        assert_ne!(ts.cert[0], local.cert[0]);
        assert_eq!(store.info().tailscale_names, vec!["x.tail.ts.net".to_string()]);
    }
}
