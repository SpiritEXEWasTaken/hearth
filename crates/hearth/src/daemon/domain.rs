//! Your own domain (DuckDNS) with Let's Encrypt certificates, issued through DNS-01 so no port has
//! to be opened. `<name>.duckdns.org` serves Hearth and `<app>.<name>.duckdns.org` serves each app
//! that has a subdomain.
//!
//! - DuckDNS's A record is set to this machine's Tailscale (or LAN) address: the name only helps
//!   devices find jarvis; nothing becomes reachable from the internet.
//! - DuckDNS holds a single TXT record per name, so the main name and the wildcard are two
//!   certificates, issued one after the other.
//! - The first setup checks the whole flow against Let's Encrypt's staging service before asking
//!   the real one, so a mistake can't run into Let's Encrypt's rate limits.
//!
//! Testing aids (environment of the daemon): HEARTH_ACME_DIRECTORY (ACME server), HEARTH_ACME_ROOT
//! (its CA, PEM), HEARTH_DNS_TESTSRV (a pebble-challtestsrv API instead of DuckDNS).

use super::State;
use super::jobs::{JobError, JobHandle};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const RENEW_BEFORE_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainConfig {
    /// The DuckDNS name, without `.duckdns.org`.
    pub name: String,
    pub token: String,
    /// "tailscale" or "lan": which of this machine's addresses the name points to.
    pub address: String,
}

impl DomainConfig {
    pub fn base(&self) -> String {
        format!("{}.duckdns.org", self.name)
    }
}

/// What the UI shows (never the token).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainStatus {
    pub configured: bool,
    pub name: Option<String>,
    pub base: Option<String>,
    pub address: Option<String>,
    pub ip: Option<String>,
    pub certificates: Vec<CertInfo>,
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertInfo {
    pub name: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    ip: Option<String>,
    last_ok: Option<i64>,
    last_error: Option<String>,
}

pub fn valid_name(name: &str) -> Result<String, String> {
    let n = name.trim().trim_end_matches(".duckdns.org").to_ascii_lowercase();
    let ok = !n.is_empty()
        && n.len() <= 63
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !n.starts_with('-')
        && !n.ends_with('-');
    if ok {
        Ok(n)
    } else {
        Err("the DuckDNS name is the part before .duckdns.org: letters, digits and '-'".into())
    }
}

pub fn valid_token(token: &str) -> Result<String, String> {
    let t = token.trim().to_ascii_lowercase();
    let ok = t.len() == 36 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if ok {
        Ok(t)
    } else {
        Err(
            "that doesn't look like a DuckDNS token (it's shown at the top of duckdns.org once you sign in)"
                .into(),
        )
    }
}

fn dir(state: &State) -> PathBuf {
    state.state_dir.join("tls")
}

pub fn load_config(state: &State) -> Option<DomainConfig> {
    let text = std::fs::read_to_string(dir(state).join("domain.json")).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_config(state: &State, cfg: Option<&DomainConfig>) -> Result<(), String> {
    let path = dir(state).join("domain.json");
    match cfg {
        Some(c) => write_private(
            &path,
            &serde_json::to_string_pretty(c).map_err(|e| e.to_string())?,
        ),
        None => {
            for f in [
                "domain.json",
                "domain-status.json",
                "domain-apex.crt",
                "domain-apex.key",
                "domain-wild.crt",
                "domain-wild.key",
            ] {
                let _ = std::fs::remove_file(dir(state).join(f));
            }
            Ok(())
        }
    }
}

fn load_saved(state: &State) -> Saved {
    std::fs::read_to_string(dir(state).join("domain-status.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn store_saved(state: &State, s: &Saved) {
    if let Ok(t) = serde_json::to_string(s) {
        let _ = write_private(&dir(state).join("domain-status.json"), &t);
    }
}

pub fn status(state: &State) -> DomainStatus {
    let Some(cfg) = load_config(state) else {
        return DomainStatus::default();
    };
    let saved = load_saved(state);
    let mut certs = Vec::new();
    for (label, name) in [("apex", cfg.base()), ("wild", format!("*.{}", cfg.base()))] {
        if let Some(exp) = cert_expiry(&dir(state).join(format!("domain-{label}.crt"))) {
            certs.push(CertInfo {
                name,
                expires_at: exp,
            });
        }
    }
    DomainStatus {
        configured: true,
        name: Some(cfg.name.clone()),
        base: Some(cfg.base()),
        address: Some(cfg.address.clone()),
        ip: saved.ip,
        certificates: certs,
        last_ok: saved.last_ok,
        last_error: saved.last_error,
    }
}

/// The domain's certificates for the certificate store: (name pattern, chain PEM, key PEM).
pub fn load_certs(state: &State) -> Vec<(String, String, String)> {
    let Some(cfg) = load_config(state) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (label, name) in [("apex", cfg.base()), ("wild", format!("*.{}", cfg.base()))] {
        let d = dir(state);
        if let (Ok(c), Ok(k)) = (
            std::fs::read_to_string(d.join(format!("domain-{label}.crt"))),
            std::fs::read_to_string(d.join(format!("domain-{label}.key"))),
        ) {
            out.push((name, c, k));
        }
    }
    out
}

/// Unix time a PEM certificate expires.
pub fn cert_expiry(path: &Path) -> Option<i64> {
    let pem = std::fs::read(path).ok()?;
    let (_, p) = x509_parser::pem::parse_x509_pem(&pem).ok()?;
    let cert = p.parse_x509().ok()?;
    Some(cert.validity().not_after.timestamp())
}

/// This machine's address of the chosen kind.
pub fn pick_ip(address: &str) -> Option<IpAddr> {
    let ifs = if_addrs::get_if_addrs().ok()?;
    let v4s: Vec<(String, std::net::Ipv4Addr)> = ifs
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v) => Some((i.name, v)),
            _ => None,
        })
        .collect();
    let tailscale = |v: &std::net::Ipv4Addr| v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]);
    let found = if address == "tailscale" {
        v4s.iter().find(|(_, v)| tailscale(v))
    } else {
        v4s.iter().find(|(n, v)| {
            v.is_private()
                && !n.starts_with("docker")
                && !n.starts_with("br-")
                && !n.starts_with("veth")
                && !tailscale(v)
        })
    };
    found.map(|(_, v)| IpAddr::V4(*v))
}

// ----- DNS provider -----

enum Dns {
    DuckDns { name: String, token: String },
    TestSrv { api: String },
}

impl Dns {
    fn for_config(cfg: &DomainConfig) -> Dns {
        match std::env::var("HEARTH_DNS_TESTSRV") {
            Ok(api) if !api.is_empty() => Dns::TestSrv { api },
            _ => Dns::DuckDns {
                name: cfg.name.clone(),
                token: cfg.token.clone(),
            },
        }
    }

    async fn duck(
        client: &reqwest::Client,
        name: &str,
        token: &str,
        extra: &[(&str, &str)],
    ) -> Result<(), String> {
        let mut q: Vec<(&str, &str)> = vec![("domains", name), ("token", token)];
        q.extend_from_slice(extra);
        let r = client
            .get("https://www.duckdns.org/update")
            .query(&q)
            .send()
            .await
            .map_err(|e| format!("DuckDNS can't be reached: {}", e.without_url()))?;
        let body = r.text().await.unwrap_or_default();
        if body.trim_start().starts_with("OK") {
            Ok(())
        } else {
            Err("DuckDNS said no: check the name and the token".into())
        }
    }

    async fn set_ip(&self, client: &reqwest::Client, base: &str, ip: IpAddr) -> Result<(), String> {
        match self {
            Dns::DuckDns { name, token } => Self::duck(client, name, token, &[("ip", &ip.to_string())]).await,
            Dns::TestSrv { api } => {
                let body = serde_json::json!({ "host": format!("{base}."), "addresses": [ip.to_string()] });
                client
                    .post(format!("{api}/add-a"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let body = serde_json::json!({ "host": format!("*.{base}."), "addresses": [ip.to_string()] });
                client
                    .post(format!("{api}/add-a"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    async fn set_txt(&self, client: &reqwest::Client, base: &str, value: Option<&str>) -> Result<(), String> {
        match self {
            Dns::DuckDns { name, token } => match value {
                Some(v) => Self::duck(client, name, token, &[("txt", v)]).await,
                None => Self::duck(client, name, token, &[("txt", ""), ("clear", "true")]).await,
            },
            Dns::TestSrv { api } => {
                let host = format!("_acme-challenge.{base}.");
                let (path, body) = match value {
                    Some(v) => ("set-txt", serde_json::json!({ "host": host, "value": v })),
                    None => ("clear-txt", serde_json::json!({ "host": host })),
                };
                client
                    .post(format!("{api}/{path}"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

// ----- ACME -----

fn acme_directory(staging: bool) -> String {
    match std::env::var("HEARTH_ACME_DIRECTORY") {
        Ok(d) if !d.is_empty() => d,
        _ if staging => LetsEncrypt::Staging.url().to_string(),
        _ => LetsEncrypt::Production.url().to_string(),
    }
}

async fn account(state: &State, staging: bool) -> Result<Account, String> {
    let file = dir(state).join(if staging {
        "acme-account-staging.json"
    } else {
        "acme-account.json"
    });
    let builder = match std::env::var("HEARTH_ACME_ROOT") {
        Ok(root) if !root.is_empty() => Account::builder_with_root(root),
        _ => Account::builder(),
    }
    .map_err(|e| format!("ACME client: {e}"))?;
    if let Ok(text) = std::fs::read_to_string(&file)
        && let Ok(creds) = serde_json::from_str::<AccountCredentials>(&text)
    {
        return builder
            .from_credentials(creds)
            .await
            .map_err(|e| format!("Let's Encrypt account: {e}"));
    }
    let (acc, creds) = builder
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            acme_directory(staging),
            None,
        )
        .await
        .map_err(|e| format!("creating the Let's Encrypt account: {e}"))?;
    write_private(&file, &serde_json::to_string(&creds).map_err(|e| e.to_string())?)?;
    Ok(acc)
}

/// Issues one certificate for `name` (the main name or `*.<base>`). Returns (chain, key).
async fn issue_one(
    job: &JobHandle,
    acc: &Account,
    dns: &Dns,
    client: &reqwest::Client,
    base: &str,
    name: &str,
) -> Result<(String, String), String> {
    job.log(format!("asking for a certificate for {name}"));
    let ids = [Identifier::Dns(name.to_string())];
    let mut order = acc
        .new_order(&NewOrder::new(&ids))
        .await
        .map_err(|e| format!("Let's Encrypt order: {e}"))?;
    {
        let mut authzs = order.authorizations();
        while let Some(a) = authzs.next().await {
            let mut authz = a.map_err(|e| format!("Let's Encrypt: {e}"))?;
            if authz.status == AuthorizationStatus::Valid {
                continue;
            }
            let mut challenge = authz
                .challenge(ChallengeType::Dns01)
                .ok_or("Let's Encrypt offered no DNS check")?;
            let value = challenge.key_authorization().dns_value();
            job.log("putting the proof in DuckDNS (TXT record)");
            dns.set_txt(client, base, Some(&value)).await?;
            // Give DuckDNS's name servers a moment to answer with the new record.
            tokio::time::sleep(Duration::from_secs(if matches!(dns, Dns::TestSrv { .. }) {
                1
            } else {
                30
            }))
            .await;
            challenge
                .set_ready()
                .await
                .map_err(|e| format!("Let's Encrypt: {e}"))?;
        }
    }
    job.log("waiting for Let's Encrypt to check it");
    let status = order
        .poll_ready(&RetryPolicy::new().timeout(Duration::from_secs(180)))
        .await
        .map_err(|e| format!("Let's Encrypt's check: {e}"))?;
    if status != OrderStatus::Ready {
        let _ = dns.set_txt(client, base, None).await;
        return Err(format!(
            "Let's Encrypt couldn't confirm the name ({status:?}). DuckDNS may be slow; try again in a few minutes"
        ));
    }
    let key = order
        .finalize()
        .await
        .map_err(|e| format!("Let's Encrypt: {e}"))?;
    let chain = order
        .poll_certificate(&RetryPolicy::new().timeout(Duration::from_secs(120)))
        .await
        .map_err(|e| format!("Let's Encrypt: {e}"))?;
    let _ = dns.set_txt(client, base, None).await;
    job.log(format!("got the certificate for {name}"));
    Ok((chain, key))
}

/// Points the name at this machine and gets (or renews) both certificates.
/// `force`: renew even when they're still fresh.
pub async fn run(state: &State, job: &JobHandle, force: bool) -> Result<serde_json::Value, JobError> {
    let result = run_inner(state, job, force).await;
    let mut saved = load_saved(state);
    match &result {
        Ok(_) => {
            saved.last_ok = Some(super::db::now());
            saved.last_error = None;
        }
        Err(e) => saved.last_error = Some(e.clone()),
    }
    if let Some(cfg) = load_config(state)
        && let Some(ip) = pick_ip(&cfg.address)
    {
        saved.ip = Some(ip.to_string());
    }
    store_saved(state, &saved);
    state.certs.set_domain(load_certs(state));
    result.map_err(JobError::failed)
}

async fn run_inner(state: &State, job: &JobHandle, force: bool) -> Result<serde_json::Value, String> {
    let cfg = load_config(state).ok_or("no domain is set up")?;
    let base = cfg.base();
    let dns = Dns::for_config(&cfg);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("hearth/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let ip = pick_ip(&cfg.address).ok_or_else(|| {
        if cfg.address == "tailscale" {
            "this machine has no Tailscale address (is Tailscale running?)".to_string()
        } else {
            "this machine has no home network address".to_string()
        }
    })?;
    job.log(format!("pointing {base} at {ip}"));
    dns.set_ip(&client, &base, ip).await?;

    let now = super::db::now();
    let wanted: Vec<(&str, String)> = [("apex", base.clone()), ("wild", format!("*.{base}"))]
        .into_iter()
        .filter(|(label, _)| {
            force
                || cert_expiry(&dir(state).join(format!("domain-{label}.crt")))
                    .is_none_or(|exp| exp - now < RENEW_BEFORE_DAYS * 86400)
        })
        .collect();
    if wanted.is_empty() {
        job.log("the certificates are still fresh");
        return Ok(serde_json::json!({ "base": base, "renewed": [] }));
    }

    // First time: prove the whole flow on the staging service.
    let first_time =
        !dir(state).join("acme-account.json").exists() && std::env::var("HEARTH_ACME_DIRECTORY").is_err();
    if first_time {
        job.log("first a trial run with Let's Encrypt's test service");
        let acc = account(state, true).await?;
        issue_one(job, &acc, &dns, &client, &base, &base).await?;
        job.log("the trial worked; now the real certificates");
    }

    let acc = account(state, false).await?;
    let mut renewed = Vec::new();
    for (label, name) in wanted {
        let (chain, key) = issue_one(job, &acc, &dns, &client, &base, &name).await?;
        write_private(&dir(state).join(format!("domain-{label}.key")), &key)?;
        write_private(&dir(state).join(format!("domain-{label}.crt")), &chain)?;
        renewed.push(name);
    }
    Ok(serde_json::json!({ "base": base, "renewed": renewed }))
}

/// Called by the certificate refresher: renews when needed, and keeps the A record current.
pub async fn maintain(state: &State) {
    let Some(cfg) = load_config(state) else {
        return;
    };
    let now = super::db::now();
    let due = ["apex", "wild"].iter().any(|l| {
        cert_expiry(&dir(state).join(format!("domain-{l}.crt")))
            .is_none_or(|exp| exp - now < RENEW_BEFORE_DAYS * 86400)
    });
    let ip_changed = pick_ip(&cfg.address).map(|i| i.to_string()) != load_saved(state).ip;
    if due || ip_changed {
        let st = state.clone();
        state
            .jobs
            .start(state, "domain", "https", "schedule", move |job| async move {
                run(&st, &job, false).await
            });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_tokens() {
        assert_eq!(valid_name("MyHome.duckdns.org"), Ok("myhome".into()));
        assert_eq!(valid_name("my-home"), Ok("my-home".into()));
        assert!(valid_name("-x").is_err());
        assert!(valid_name("a.b").is_err());
        assert!(valid_token("a7623b5c-1234-4cde-9f00-0123456789ab").is_ok());
        assert!(valid_token("nope").is_err());
    }
}
