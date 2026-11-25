//! Compose normalisation: turn any compose file (store template, CasaOS app, pasted file) into
//! one that plain `docker compose` can run on its own, on *this* host.
//!
//! - Variables only a manager would fill in (`$PUID`, `$PGID`, `$TZ`, `$AppID`) get real values
//!   in `.env`; any other variable without a value is an error.
//! - Inline secrets move to `.env` and are referenced as `${VAR}`, so the compose file can be
//!   shown to read-only users.
//! - Devices that don't exist here are removed, and so are bind mounts of missing *system*
//!   paths (e.g. the Raspberry Pi's `/opt/vc/lib`). Missing data folders are kept (Docker creates
//!   them) with a warning.
//! - `x-casaos` becomes `x-hearth` (title, icon, web port).
//!
//! Every change is listed in `decisions` and written into `x-hearth.decisions`, so the file
//! explains itself later.

use hearth_casaos::compose::find_variables;
use hearth_core::redact::is_secret_key;
use serde_yaml_ng::{Mapping, Value};
use std::collections::BTreeMap;

/// Host paths under which a missing bind source is just "data that doesn't exist yet".
const DATA_ROOTS: &[&str] = &[
    "/DATA",
    "/srv",
    "/mnt",
    "/media",
    "/home",
    "/var/lib/hearth",
    "/opt/hearth",
    "/data",
];

pub struct Options<'a> {
    pub app: &'a str,
    pub puid: u32,
    pub pgid: u32,
    pub tz: &'a str,
    /// Values the user passed with `--set KEY=VALUE` (win over everything).
    pub sets: &'a BTreeMap<String, String>,
    /// The app's current `.env` (kept unless overridden).
    pub existing_env: &'a BTreeMap<String, String>,
    pub path_exists: &'a dyn Fn(&str) -> bool,
    /// Today's date for decision notes (`YYYY-MM-DD`).
    pub today: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Normalized {
    pub compose: String,
    pub env: BTreeMap<String, String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

pub const HEADER: &str =
    "# Managed by Hearth. Plain `docker compose` works too: values come from .env next to this file.\n";

pub fn normalize(input: &str, opts: &Options) -> Normalized {
    let mut out = Normalized::default();
    let mut doc: Value = match serde_yaml_ng::from_str(input) {
        Ok(v) => v,
        Err(e) => {
            out.errors.push(format!("not valid YAML: {e}"));
            return out;
        }
    };
    let Some(root) = doc.as_mapping_mut() else {
        out.errors
            .push("not a compose file (top level isn't a mapping)".into());
        return out;
    };
    if root
        .get("services")
        .and_then(Value::as_mapping)
        .is_none_or(Mapping::is_empty)
    {
        out.errors.push("no services".into());
        return out;
    }

    // Project name = app name, so `docker compose` in the folder targets the same containers.
    let old_name = root.get("name").and_then(Value::as_str).map(str::to_string);
    if old_name.as_deref() != Some(opts.app) {
        root.insert("name".into(), opts.app.into());
        if let Some(old) = old_name {
            out.decisions
                .push(format!("set the project name to '{}' (was '{old}')", opts.app));
        }
    }

    let mut env: BTreeMap<String, String> = opts.existing_env.clone();
    env.extend(opts.sets.iter().map(|(k, v)| (k.clone(), v.clone())));

    let x_casaos = root.remove("x-casaos");
    let services = root.get_mut("services").and_then(Value::as_mapping_mut);
    let mut main_service = None;
    if let Some(services) = services {
        main_service = services.keys().next().and_then(Value::as_str).map(str::to_string);
        move_secrets(services, &mut env, &mut out);
        for (name, svc) in services.iter_mut() {
            let name = name.as_str().unwrap_or_default().to_string();
            if let Some(m) = svc.as_mapping_mut() {
                m.remove("x-casaos");
                drop_missing_devices(&name, m, opts, &mut out);
                drop_missing_system_binds(&name, m, opts, &mut out);
            }
        }
    }

    // Variables: fill what a manager used to fill in; anything else needs a value from the user.
    let services_text = root
        .get("services")
        .map(|s| serde_yaml_ng::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    for var in find_variables(&services_text) {
        if env.contains_key(&var) {
            continue;
        }
        let default = match var.as_str() {
            "PUID" => Some(opts.puid.to_string()),
            "PGID" => Some(opts.pgid.to_string()),
            "TZ" => Some(opts.tz.to_string()),
            "AppID" => Some(opts.app.to_string()),
            // CasaOS's built-in defaults; kept so existing apps keep working, but flagged.
            "DefaultUserName" => Some("admin".to_string()),
            "DefaultPassword" => Some("casaos".to_string()),
            _ => None,
        };
        match default {
            Some(v) if var.starts_with("Default") => {
                out.decisions.push(format!(
                    "set {var} in .env to CasaOS's built-in default (everyone knows it: change it in the app)"
                ));
                env.insert(var, v);
            }
            Some(v) => {
                out.decisions.push(format!(
                    "set {var}={v} in .env (it used to be filled in by the app manager)"
                ));
                env.insert(var, v);
            }
            None => out
                .errors
                .push(format!("variable {var} has no value: pass --set {var}=VALUE")),
        }
    }

    // x-casaos → x-hearth (keep an existing x-hearth, add decisions to it).
    let mut xh = root
        .remove("x-hearth")
        .and_then(|v| v.as_mapping().cloned())
        .unwrap_or_default();
    if let Some(xc) = x_casaos.as_ref().and_then(Value::as_mapping) {
        convert_x_casaos(xc, &mut xh, main_service.as_deref());
    }
    add_known_health(root, &mut xh);
    if !out.decisions.is_empty() {
        let mut list = xh
            .get("decisions")
            .and_then(Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        for d in &out.decisions {
            list.push(Value::String(format!("{}: {d}", opts.today)));
        }
        xh.insert("decisions".into(), Value::Sequence(list));
    }
    if !xh.is_empty() {
        root.insert("x-hearth".into(), Value::Mapping(xh));
    }

    let body = serde_yaml_ng::to_string(&doc).unwrap_or_default();
    out.compose = format!("{HEADER}{body}");
    out.env = env;
    out
}

/// Readiness URLs of well-known apps, for a stricter health gate than "the port answers".
fn known_health_path(image: &str) -> Option<&'static str> {
    let name = image.split(['@', ':']).next().unwrap_or(image);
    if name.ends_with("/jellyfin") || name == "jellyfin" {
        Some("/health")
    } else if name.ends_with("/home-assistant") || name.ends_with("/homeassistant") {
        Some("/manifest.json")
    } else if name.ends_with("/immich-server") {
        Some("/api/server/ping")
    } else {
        None
    }
}

/// Adds `x-hearth.web.health` when the web service is a known app and none is set.
fn add_known_health(root: &Mapping, xh: &mut Mapping) {
    let Some(web) = xh.get_mut("web").and_then(Value::as_mapping_mut) else {
        return;
    };
    if web.contains_key("health") {
        return;
    }
    let service = web.get("service").and_then(Value::as_str).map(str::to_string);
    let image = root
        .get("services")
        .and_then(Value::as_mapping)
        .and_then(|s| match &service {
            Some(name) => s.get(name.as_str()),
            None => s.values().next(),
        })
        .and_then(|svc| svc.get("image"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(p) = known_health_path(image) {
        web.insert("health".into(), p.into());
    }
}

fn convert_x_casaos(xc: &Mapping, xh: &mut Mapping, first_service: Option<&str>) {
    let s = |k: &str| {
        xc.get(k).and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    if !xh.contains_key("title") {
        let title = xc.get("title").and_then(|t| match t {
            Value::Mapping(m) => ["custom", "en_US", "en_us"]
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()))
                .map(str::to_string),
            Value::String(s) => Some(s.clone()),
            _ => None,
        });
        if let Some(t) = title {
            xh.insert("title".into(), t.into());
        }
    }
    if !xh.contains_key("icon")
        && let Some(i) = s("icon")
    {
        xh.insert("icon".into(), i.into());
    }
    if !xh.contains_key("web")
        && let Some(port) = s("port_map")
    {
        let mut web = Mapping::new();
        web.insert("port".into(), port.into());
        web.insert("path".into(), s("index").unwrap_or_else(|| "/".into()).into());
        web.insert(
            "scheme".into(),
            s("scheme").unwrap_or_else(|| "http".into()).into(),
        );
        if let Some(main) = s("main").or_else(|| first_service.map(str::to_string)) {
            web.insert("service".into(), main.into());
        }
        xh.insert("web".into(), Value::Mapping(web));
    }
    if !xh.contains_key("source") {
        let mut src = Mapping::new();
        if let Some(id) = s("store_app_id").or_else(|| s("id")) {
            src.insert("casaos_store_app_id".into(), id.into());
        }
        if !src.is_empty() {
            xh.insert("source".into(), Value::Mapping(src));
        }
    }
}

/// Moves literal secret values out of `environment:` into `.env`.
/// The same key with the same value in several services becomes one variable.
fn move_secrets(services: &mut Mapping, env: &mut BTreeMap<String, String>, out: &mut Normalized) {
    // Pass 1: find literal secrets and decide variable names.
    let mut found: Vec<(String, String, String)> = Vec::new(); // (service, key, value)
    for (name, svc) in services.iter() {
        let name = name.as_str().unwrap_or_default();
        for (k, v) in env_entries(svc.get("environment")) {
            if let Some(v) = v
                && is_secret_key(&k)
                && !v.contains("${")
                && !v.starts_with('$')
                && !v.is_empty()
            {
                found.push((name.to_string(), k, v));
            }
        }
    }
    let mut var_for: BTreeMap<(String, String), String> = BTreeMap::new();
    for (svc, key, value) in &found {
        let same_value_everywhere = found
            .iter()
            .filter(|(_, k, _)| k == key)
            .all(|(_, _, v)| v == value);
        let var = if same_value_everywhere {
            key.clone()
        } else {
            format!("{}_{}", svc.to_ascii_uppercase().replace(['-', '.'], "_"), key)
        };
        var_for.insert((svc.clone(), key.clone()), var);
    }
    // Pass 2: rewrite.
    let mut moved: Vec<String> = Vec::new();
    for (name, svc) in services.iter_mut() {
        let name = name.as_str().unwrap_or_default().to_string();
        let Some(envv) = svc.as_mapping_mut().and_then(|m| m.get_mut("environment")) else {
            continue;
        };
        match envv {
            Value::Mapping(m) => {
                for (k, v) in m.iter_mut() {
                    let key = k.as_str().unwrap_or_default().to_string();
                    if let Some(var) = var_for.get(&(name.clone(), key.clone())) {
                        let literal = scalar(v).unwrap_or_default();
                        env.entry(var.clone()).or_insert(literal);
                        *v = Value::String(format!("${{{var}}}"));
                        if !moved.contains(var) {
                            moved.push(var.clone());
                        }
                    }
                }
            }
            Value::Sequence(seq) => {
                for item in seq.iter_mut() {
                    let Some(s) = item.as_str().map(str::to_string) else {
                        continue;
                    };
                    let Some((k, v)) = s.split_once('=') else { continue };
                    if let Some(var) = var_for.get(&(name.clone(), k.to_string())) {
                        env.entry(var.clone()).or_insert_with(|| v.to_string());
                        *item = Value::String(format!("{k}=${{{var}}}"));
                        if !moved.contains(var) {
                            moved.push(var.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for var in moved {
        out.decisions.push(format!("moved the secret {var} into .env"));
    }
}

fn drop_missing_devices(service: &str, svc: &mut Mapping, opts: &Options, out: &mut Normalized) {
    let Some(devs) = svc.get_mut("devices").and_then(Value::as_sequence_mut) else {
        return;
    };
    devs.retain(|d| {
        let host = match d {
            Value::String(s) => s.split(':').next().unwrap_or_default().to_string(),
            Value::Mapping(m) => m
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            _ => return true,
        };
        if host.is_empty() || (opts.path_exists)(&host) {
            return true;
        }
        out.decisions.push(format!(
            "removed device {host} from {service}: it doesn't exist on this host"
        ));
        false
    });
    if devs.is_empty() {
        svc.remove("devices");
    }
}

fn drop_missing_system_binds(service: &str, svc: &mut Mapping, opts: &Options, out: &mut Normalized) {
    let Some(vols) = svc.get_mut("volumes").and_then(Value::as_sequence_mut) else {
        return;
    };
    let mut warnings = Vec::new();
    vols.retain(|v| {
        let source = match v {
            Value::String(s) => {
                let src = s.split(':').next().unwrap_or_default();
                if !src.starts_with('/') || !s.contains(':') {
                    return true; // named or anonymous volume
                }
                src.to_string()
            }
            Value::Mapping(m) => {
                if m.get("type").and_then(Value::as_str) != Some("bind") {
                    return true;
                }
                m.get("source")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }
            _ => return true,
        };
        if source.is_empty() || source.contains('$') || (opts.path_exists)(&source) {
            return true;
        }
        let is_data = DATA_ROOTS
            .iter()
            .any(|r| source == *r || source.starts_with(&format!("{r}/")));
        if is_data {
            warnings.push(format!(
                "{service}: {source} doesn't exist yet; Docker will create it as an empty folder"
            ));
            return true;
        }
        out.decisions.push(format!(
            "removed the bind mount {source} from {service}: it doesn't exist on this host"
        ));
        false
    });
    out.warnings.extend(warnings);
    if vols.is_empty() {
        svc.remove("volumes");
    }
}

fn env_entries(v: Option<&Value>) -> Vec<(String, Option<String>)> {
    match v {
        Some(Value::Mapping(m)) => m
            .iter()
            .filter_map(|(k, v)| Some((k.as_str()?.to_string(), scalar(v))))
            .collect(),
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(Value::as_str)
            .map(|s| match s.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (s.to_string(), None),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ----- .env files -----

/// Parses a `.env` file (KEY=VALUE lines, `#` comments, optional quotes).
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('\'') && v.ends_with('\'')) || (v.starts_with('"') && v.ends_with('"')))
        {
            v[1..v.len() - 1].to_string()
        } else {
            v.to_string()
        };
        out.insert(k.trim().to_string(), v);
    }
    out
}

/// Writes a `.env` file. Values with special characters are single-quoted (taken literally by
/// Compose). Returns an error for values that can't be written safely.
pub fn render_env(env: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::from(
        "# Values for compose.yaml (kept out of it so it can be shown safely). Managed by Hearth.\n",
    );
    for (k, v) in env {
        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || k.is_empty() {
            return Err(format!("invalid variable name '{k}'"));
        }
        if v.contains('\n') {
            return Err(format!("the value of {k} contains a line break"));
        }
        let plain = v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@+,".contains(c));
        if plain {
            out.push_str(&format!("{k}={v}\n"));
        } else if !v.contains('\'') {
            out.push_str(&format!("{k}='{v}'\n"));
        } else {
            return Err(format!(
                "the value of {k} contains a single quote; set it some other way"
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts<'a>(
        sets: &'a BTreeMap<String, String>,
        existing: &'a BTreeMap<String, String>,
        exists: &'a dyn Fn(&str) -> bool,
    ) -> Options<'a> {
        Options {
            app: "jellyfin",
            puid: 1000,
            pgid: 1000,
            tz: "America/New_York",
            sets,
            existing_env: existing,
            path_exists: exists,
            today: "2026-11-25",
        }
    }

    const CASAOS_JELLYFIN: &str = r#"
name: jellyfin
services:
  jellyfin:
    image: linuxserver/jellyfin:latest
    devices:
      - /dev/dri:/dev/dri
      - /dev/vcsm:/dev/vcsm
    environment:
      PUID: $PUID
      PGID: $PGID
      TZ: $TZ
    volumes:
      - type: bind
        source: /DATA/AppData/$AppID/config
        target: /config
      - type: bind
        source: /opt/vc/lib
        target: /opt/vc/lib
      - type: bind
        source: /DATA/Media
        target: /Media
    x-casaos:
      envs: []
x-casaos:
  main: jellyfin
  port_map: "8097"
  title:
    custom: ""
    en_US: Jellyfin
  store_app_id: jellyfin
"#;

    #[test]
    fn casaos_jellyfin_becomes_self_contained() {
        let empty = BTreeMap::new();
        let exists = |p: &str| p == "/dev/dri";
        let n = normalize(CASAOS_JELLYFIN, &opts(&empty, &empty, &exists));
        assert!(n.errors.is_empty(), "{:?}", n.errors);
        assert_eq!(n.env.get("PUID").map(String::as_str), Some("1000"));
        assert_eq!(n.env.get("TZ").map(String::as_str), Some("America/New_York"));
        assert_eq!(n.env.get("AppID").map(String::as_str), Some("jellyfin"));
        assert!(!n.compose.contains("- /dev/vcsm:/dev/vcsm"), "{}", n.compose);
        assert!(n.compose.contains("/dev/dri"));
        assert!(!n.compose.contains("source: /opt/vc/lib"), "system path dropped");
        assert!(n.compose.contains("/DATA/Media"), "missing data folders are kept");
        assert!(!n.compose.contains("x-casaos"));
        assert!(n.compose.contains("x-hearth"));
        assert!(n.compose.contains("title: Jellyfin"));
        assert!(
            n.compose.contains("health: /health"),
            "known readiness URL: {}",
            n.compose
        );
        assert!(
            n.compose.contains("port: '8097'") || n.compose.contains("port: \"8097\""),
            "{}",
            n.compose
        );
        assert!(n.compose.starts_with(HEADER));
        assert!(n.decisions.iter().any(|d| d.contains("/dev/vcsm")));
        assert!(n.decisions.iter().any(|d| d.contains("/opt/vc/lib")));
        // The decisions are recorded in the file itself.
        assert!(n.compose.contains("2026-11-25: removed device /dev/vcsm"));
        assert!(!n.warnings.is_empty(), "missing /DATA paths are warned about");
    }

    #[test]
    fn shared_secrets_become_one_variable() {
        let input = "services:\n  db:\n    image: postgres\n    environment:\n      POSTGRES_PASSWORD: s3cret\n  app:\n    image: app\n    environment:\n      - DB_PASSWORD=s3cret\n      - DB_HOST=db\n  ml:\n    image: ml\n    environment:\n      DB_PASSWORD: s3cret\n";
        let empty = BTreeMap::new();
        let exists = |_: &str| true;
        let n = normalize(input, &opts(&empty, &empty, &exists));
        assert!(n.errors.is_empty(), "{:?}", n.errors);
        assert!(!n.compose.contains("s3cret"), "{}", n.compose);
        assert!(n.compose.contains("DB_PASSWORD=${DB_PASSWORD}"));
        assert!(n.compose.contains("POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}"));
        assert_eq!(n.env.get("DB_PASSWORD").map(String::as_str), Some("s3cret"));
        assert_eq!(n.env.get("POSTGRES_PASSWORD").map(String::as_str), Some("s3cret"));
        assert!(n.compose.contains("DB_HOST=db"), "non-secrets stay inline");
    }

    #[test]
    fn different_secret_values_get_per_service_names() {
        let input = "services:\n  a:\n    image: x\n    environment:\n      API_KEY: one\n  b-svc:\n    image: y\n    environment:\n      API_KEY: two\n";
        let empty = BTreeMap::new();
        let exists = |_: &str| true;
        let n = normalize(input, &opts(&empty, &empty, &exists));
        assert_eq!(n.env.get("A_API_KEY").map(String::as_str), Some("one"));
        assert_eq!(n.env.get("B_SVC_API_KEY").map(String::as_str), Some("two"));
    }

    #[test]
    fn unknown_variables_are_errors_unless_set() {
        let input =
            "services:\n  a:\n    image: x:${VERSION}\n    environment:\n      X: ${WITH_DEFAULT:-1}\n";
        let empty = BTreeMap::new();
        let exists = |_: &str| true;
        let n = normalize(input, &opts(&empty, &empty, &exists));
        assert_eq!(
            n.errors,
            vec!["variable VERSION has no value: pass --set VERSION=VALUE".to_string()]
        );
        let mut sets = BTreeMap::new();
        sets.insert("VERSION".to_string(), "1.2".to_string());
        let n = normalize(input, &opts(&sets, &empty, &exists));
        assert!(n.errors.is_empty());
        assert_eq!(n.env.get("VERSION").map(String::as_str), Some("1.2"));
    }

    #[test]
    fn idempotent_on_its_own_output() {
        let empty = BTreeMap::new();
        let exists = |p: &str| p == "/dev/dri";
        let first = normalize(CASAOS_JELLYFIN, &opts(&empty, &empty, &exists));
        let second = normalize(&first.compose, &opts(&empty, &first.env, &exists));
        assert!(second.decisions.is_empty(), "{:?}", second.decisions);
        assert_eq!(first.compose, second.compose);
        assert_eq!(first.env, second.env);
    }

    #[test]
    fn casaos_default_credentials_are_filled_but_flagged() {
        let input = "services:\n  a:\n    image: x\n    environment:\n      USER: $DefaultUserName\n";
        let empty = BTreeMap::new();
        let exists = |_: &str| true;
        let n = normalize(input, &opts(&empty, &empty, &exists));
        assert!(n.errors.is_empty(), "{:?}", n.errors);
        assert_eq!(n.env.get("DefaultUserName").map(String::as_str), Some("admin"));
        assert!(n.decisions.iter().any(|d| d.contains("change it")));
    }

    #[test]
    fn rejects_non_compose() {
        let empty = BTreeMap::new();
        let exists = |_: &str| true;
        assert!(
            !normalize("a: [", &opts(&empty, &empty, &exists))
                .errors
                .is_empty()
        );
        assert!(
            !normalize("services: {}\n", &opts(&empty, &empty, &exists))
                .errors
                .is_empty()
        );
    }

    #[test]
    fn env_roundtrip_and_quoting() {
        let mut env = BTreeMap::new();
        env.insert("A".to_string(), "plain-value_1".to_string());
        env.insert("B".to_string(), "has space $and dollar".to_string());
        let text = render_env(&env).unwrap_or_default();
        assert!(text.contains("A=plain-value_1\n"));
        assert!(text.contains("B='has space $and dollar'\n"));
        assert_eq!(parse_env(&text), env);
        env.insert("C".to_string(), "it's".to_string());
        assert!(render_env(&env).is_err());
    }
}
