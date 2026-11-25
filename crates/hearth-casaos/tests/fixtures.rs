//! Parser tests against real CasaOS and Big Bear templates.

use hearth_casaos::{ComposeFile, read_store};
use std::path::Path;

fn fixture(name: &str) -> ComposeFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => panic!("{}: {e}", path.display()),
    };
    match ComposeFile::parse(&text, name) {
        Ok(c) => c,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn casaos_v1_jellyfin_has_pi_devices_and_needs_casaos_variables() {
    let c = fixture("casaos-v1-jellyfin.yml");
    let x = c.x_casaos().unwrap_or_default();
    assert_eq!(x.main.as_deref(), Some("jellyfin"));
    assert_eq!(x.title.as_deref(), Some("Jellyfin"));
    assert_eq!(x.port_map.as_deref(), Some("8097"));
    assert_eq!(c.images(), vec!["linuxserver/jellyfin:10.8.12".to_string()]);

    // A real-world failure: Raspberry Pi-only devices in a template used on an x86 host.
    let missing = c.missing_devices(|p| p == "/dev/dri");
    assert!(missing.contains(&"/dev/vcsm".to_string()), "{missing:?}");
    assert!(!missing.contains(&"/dev/dri".to_string()));

    // And the other one: variables only CasaOS fills in.
    let vars = c.external_variables();
    for v in ["PUID", "PGID", "TZ", "AppID"] {
        assert!(vars.contains(v), "{v} missing from {vars:?}");
    }
}

#[test]
fn casaos_v2_template_has_stable_id() {
    let c = fixture("casaos-v2-jellyfin.yml");
    let x = c.x_casaos().unwrap_or_default();
    assert_eq!(x.id.as_deref(), Some("org.icewhale.jellyfin"));
    assert!(x.architectures.contains(&"amd64".to_string()));
    assert!(x.title.is_some());
}

#[test]
fn bigbear_immich_multi_service_with_digest_pins() {
    let c = fixture("bigbear-immich.yml");
    let services = c.services();
    assert!(services.len() >= 4, "{}", services.len());
    let x = c.x_casaos().unwrap_or_default();
    assert_eq!(x.id.as_deref(), Some("com.bigbeartechworld.immich"));
    // YAML comments after the image are not part of the value.
    assert!(c.images().iter().all(|i| !i.contains('#')), "{:?}", c.images());
    assert!(c.images().iter().any(|i| i.contains("@sha256:")));
    let main = c.main_service().unwrap_or_default();
    assert!(main.image.unwrap_or_default().contains("immich-server"));
}

#[test]
fn bigbear_jellyfin_long_syntax() {
    let c = fixture("bigbear-jellyfin.yml");
    let s = c.main_service().unwrap_or_default();
    assert!(s.ports.iter().any(|p| p.target == "7359" && p.protocol == "udp"));
    assert!(s.volumes.iter().all(|v| v.kind == "bind"));
}

/// Bulk check over real stores. Set `HEARTH_CASAOS_STORES` to a `:`-separated list of
/// extracted store directories, e.g. `/var/lib/casaos/appstore/default:/tmp/CasaOS-AppStore`.
/// Every template must parse; this keeps the reader honest against hundreds of real files.
#[test]
fn bulk_real_stores() {
    let Ok(dirs) = std::env::var("HEARTH_CASAOS_STORES") else {
        eprintln!("HEARTH_CASAOS_STORES not set; skipping bulk store test");
        return;
    };
    let mut total = 0;
    let mut failures = Vec::new();
    for dir in dirs.split(':').filter(|d| !d.is_empty()) {
        let read = match read_store(Path::new(dir)) {
            Ok(r) => r,
            Err(e) => panic!("{dir}: {e}"),
        };
        total += read.templates.len();
        failures.extend(read.errors);
        for t in &read.templates {
            if t.compose.main_service().is_none() {
                failures.push(format!("{}: no main service", t.path.display()));
            }
            let _ = t.compose.external_variables();
            let _ = t.compose.x_casaos();
        }
    }
    eprintln!("read {total} templates, {} problems", failures.len());
    for f in &failures {
        eprintln!("  {f}");
    }
    assert!(total > 0);
    // Upstream stores contain a few genuinely broken files; allow at most 1%.
    assert!(
        failures.len() * 100 <= total,
        "{} of {total} templates failed",
        failures.len()
    );
}
