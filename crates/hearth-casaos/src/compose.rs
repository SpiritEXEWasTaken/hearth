//! A CasaOS compose file, read tolerantly.
//!
//! CasaOS files are written by many hands, so every accessor copes with both the short and long
//! Compose syntaxes, numbers where strings are expected, and missing fields. The original YAML
//! is kept as-is (`raw`) so later steps can transform it without losing anything.

use crate::CasaosError;
use serde_yaml_ng::{Mapping, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct ComposeFile {
    /// The parsed document, untouched.
    pub raw: Value,
    /// The original text (needed to find `$VARIABLES`, which parsing keeps as plain strings).
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub image: Option<String>,
    pub container_name: Option<String>,
    /// Device mappings as written, e.g. `/dev/dri:/dev/dri`.
    pub devices: Vec<String>,
    /// Environment as (key, value) pairs; value `None` for `- KEY` without `=`.
    pub environment: Vec<(String, Option<String>)>,
    pub ports: Vec<PortSpec>,
    pub volumes: Vec<VolumeSpec>,
    pub network_mode: Option<String>,
    pub privileged: bool,
    pub restart: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    pub published: Option<String>,
    pub target: String,
    /// `tcp` unless stated.
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSpec {
    /// Host path or named volume; `None` for anonymous volumes.
    pub source: Option<String>,
    pub target: String,
    /// `bind`, `volume`, `tmpfs`…
    pub kind: String,
}

/// The top-level `x-casaos` block (installed apps and store templates).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XCasaos {
    /// v2 stores: stable reverse-domain id (`com.bigbeartechworld.jellyfin`).
    pub id: Option<String>,
    /// Installed apps: the store app this was installed from.
    pub store_app_id: Option<String>,
    pub main: Option<String>,
    /// Display title, already resolved (custom title > en_US > any).
    pub title: Option<String>,
    pub icon: Option<String>,
    pub port_map: Option<String>,
    pub scheme: Option<String>,
    pub index: Option<String>,
    pub hostname: Option<String>,
    pub category: Option<String>,
    pub author: Option<String>,
    pub developer: Option<String>,
    pub version: Option<String>,
    pub architectures: Vec<String>,
    pub tagline: Option<String>,
}

impl ComposeFile {
    pub fn parse(text: &str, path: &str) -> Result<ComposeFile, CasaosError> {
        let raw: Value = serde_yaml_ng::from_str(text).map_err(|source| CasaosError::Yaml {
            path: path.to_string(),
            source,
        })?;
        if !raw.is_mapping() {
            return Err(CasaosError::Invalid {
                path: path.to_string(),
                message: "not a YAML mapping".into(),
            });
        }
        if raw.get("services").and_then(Value::as_mapping).is_none() {
            return Err(CasaosError::Invalid {
                path: path.to_string(),
                message: "no services".into(),
            });
        }
        Ok(ComposeFile {
            raw,
            text: text.to_string(),
        })
    }

    /// The compose project name (`name:`), if set.
    pub fn name(&self) -> Option<String> {
        scalar(self.raw.get("name")?)
    }

    pub fn services(&self) -> Vec<Service> {
        let Some(map) = self.raw.get("services").and_then(Value::as_mapping) else {
            return Vec::new();
        };
        map.iter()
            .filter_map(|(k, v)| Some(parse_service(&scalar(k)?, v)))
            .collect()
    }

    pub fn service(&self, name: &str) -> Option<Service> {
        self.services().into_iter().find(|s| s.name == name)
    }

    /// The service CasaOS treats as the app's main one: `x-casaos.main`, else the first.
    pub fn main_service(&self) -> Option<Service> {
        let main = self.x_casaos().and_then(|x| x.main);
        let services = self.services();
        match main {
            Some(m) => services
                .iter()
                .find(|s| s.name == m)
                .cloned()
                .or_else(|| services.first().cloned()),
            None => services.first().cloned(),
        }
    }

    pub fn x_casaos(&self) -> Option<XCasaos> {
        let x = self.raw.get("x-casaos")?.as_mapping()?;
        let s = |k: &str| x.get(k).and_then(scalar).filter(|v| !v.is_empty());
        Some(XCasaos {
            id: s("id"),
            store_app_id: s("store_app_id"),
            main: s("main"),
            title: x.get("title").and_then(localized),
            icon: s("icon"),
            port_map: s("port_map"),
            scheme: s("scheme"),
            index: s("index"),
            hostname: s("hostname"),
            category: s("category"),
            author: s("author"),
            developer: s("developer"),
            version: s("version"),
            architectures: x
                .get("architectures")
                .and_then(Value::as_sequence)
                .map(|seq| seq.iter().filter_map(scalar).collect())
                .unwrap_or_default(),
            tagline: x.get("tagline").and_then(localized),
        })
    }

    /// Variables the file needs from outside: `$VAR` / `${VAR}` without a default
    /// (`${VAR:-x}` and `${VAR-x}` have defaults; `$$` is an escaped dollar).
    /// A non-empty result means plain `docker compose` can't run this file correctly on its own,
    /// unless the variables are put in an `.env` file next to it.
    pub fn external_variables(&self) -> BTreeSet<String> {
        // Only look at the services section: x-casaos descriptions may contain '$' in prose.
        let text = match self.raw.get("services") {
            Some(s) => serde_yaml_ng::to_string(s).unwrap_or_default(),
            None => return BTreeSet::new(),
        };
        find_variables(&text)
    }

    /// Host device paths in `devices:` for which `exists` returns false.
    pub fn missing_devices(&self, exists: impl Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        for svc in self.services() {
            for d in &svc.devices {
                let host = d.split(':').next().unwrap_or(d);
                if !host.is_empty() && !exists(host) && !out.contains(&host.to_string()) {
                    out.push(host.to_string());
                }
            }
        }
        out
    }

    /// Display title: `x-casaos.title` (custom name first), else the project name.
    pub fn title(&self) -> Option<String> {
        self.x_casaos().and_then(|x| x.title).or_else(|| self.name())
    }

    /// All image references used by the services, in file order.
    pub fn images(&self) -> Vec<String> {
        self.services().into_iter().filter_map(|s| s.image).collect()
    }
}

/// Finds `$VAR` and `${VAR}` references without defaults.
pub fn find_variables(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'$') {
            i += 2; // escaped
            continue;
        }
        if b.get(i + 1) == Some(&b'{') {
            let start = i + 2;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let name = &text[start..j];
            // What follows the name decides whether a value must come from outside:
            //   }        ${VAR}            required
            //   ? / :?   ${VAR:?message}   required (error if unset)
            //   - / :-   ${VAR:-default}   has a default
            //   = / :=   ${VAR:=default}   has a default
            //   + / :+   ${VAR:+alt}       never required
            let op = match (b.get(j), b.get(j + 1)) {
                (Some(b':'), Some(c)) => Some(*c),
                (Some(c), _) => Some(*c),
                (None, _) => None,
            };
            let required = matches!(op, Some(b'}') | Some(b'?'));
            if !name.is_empty() && is_valid_var(name) && required {
                out.insert(name.to_string());
            }
            i = j;
        } else {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let name = &text[start..j];
            if !name.is_empty() && is_valid_var(name) {
                out.insert(name.to_string());
            }
            i = j.max(i + 1);
        }
    }
    out
}

fn is_valid_var(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn parse_service(name: &str, v: &Value) -> Service {
    let s = |k: &str| v.get(k).and_then(scalar);
    Service {
        name: name.to_string(),
        image: s("image"),
        container_name: s("container_name"),
        devices: v
            .get("devices")
            .and_then(Value::as_sequence)
            .map(|seq| {
                seq.iter()
                    .filter_map(|d| match d {
                        Value::Mapping(m) => {
                            let src = m.get("source").and_then(scalar)?;
                            let tgt = m.get("target").and_then(scalar).unwrap_or_else(|| src.clone());
                            Some(format!("{src}:{tgt}"))
                        }
                        other => scalar(other),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        environment: parse_env(v.get("environment")),
        ports: v
            .get("ports")
            .and_then(Value::as_sequence)
            .map(|seq| seq.iter().filter_map(parse_port).collect())
            .unwrap_or_default(),
        volumes: v
            .get("volumes")
            .and_then(Value::as_sequence)
            .map(|seq| seq.iter().filter_map(parse_volume).collect())
            .unwrap_or_default(),
        network_mode: s("network_mode"),
        privileged: v.get("privileged").and_then(Value::as_bool).unwrap_or(false),
        restart: s("restart"),
    }
}

fn parse_env(v: Option<&Value>) -> Vec<(String, Option<String>)> {
    match v {
        Some(Value::Mapping(m)) => m
            .iter()
            .filter_map(|(k, v)| Some((scalar(k)?, if v.is_null() { None } else { scalar(v) })))
            .collect(),
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(scalar)
            .map(|item| match item.split_once('=') {
                Some((k, val)) => (k.to_string(), Some(val.to_string())),
                None => (item, None),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_port(v: &Value) -> Option<PortSpec> {
    match v {
        Value::Mapping(m) => Some(PortSpec {
            published: m.get("published").and_then(scalar),
            target: m.get("target").and_then(scalar)?,
            protocol: m.get("protocol").and_then(scalar).unwrap_or_else(|| "tcp".into()),
        }),
        other => {
            // Short syntax: [ip:][published:]target[/protocol]
            let s = scalar(other)?;
            let (spec, protocol) = match s.rsplit_once('/') {
                Some((a, p)) => (a.to_string(), p.to_string()),
                None => (s.clone(), "tcp".to_string()),
            };
            let parts: Vec<&str> = spec.rsplitn(3, ':').collect();
            let target = parts.first()?.to_string();
            let published = parts.get(1).map(|p| p.to_string());
            Some(PortSpec {
                published,
                target,
                protocol,
            })
        }
    }
}

fn parse_volume(v: &Value) -> Option<VolumeSpec> {
    match v {
        Value::Mapping(m) => Some(VolumeSpec {
            source: m.get("source").and_then(scalar),
            target: m.get("target").and_then(scalar)?,
            kind: m.get("type").and_then(scalar).unwrap_or_else(|| "volume".into()),
        }),
        other => {
            let s = scalar(other)?;
            let parts: Vec<&str> = s.splitn(3, ':').collect();
            match parts.as_slice() {
                [target] => Some(VolumeSpec {
                    source: None,
                    target: target.to_string(),
                    kind: "volume".into(),
                }),
                [src, target, ..] => Some(VolumeSpec {
                    kind: if src.starts_with('/') || src.starts_with('.') || src.starts_with('~') {
                        "bind".into()
                    } else {
                        "volume".into()
                    },
                    source: Some(src.to_string()),
                    target: target.to_string(),
                }),
                [] => None,
            }
        }
    }
}

/// A YAML scalar as a string (numbers and booleans included).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A localized field: `{custom: "...", en_US: "...", ...}` or a plain string.
fn localized(v: &Value) -> Option<String> {
    match v {
        Value::Mapping(m) => {
            let get = |k: &str| m.get(k).and_then(scalar).filter(|s| !s.trim().is_empty());
            get("custom")
                .or_else(|| get("en_US"))
                .or_else(|| get("en_us"))
                .or_else(|| get("en_GB"))
                .or_else(|| first_value(m))
        }
        other => scalar(other).filter(|s| !s.trim().is_empty()),
    }
}

fn first_value(m: &Mapping) -> Option<String> {
    m.values().filter_map(scalar).find(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED_HA: &str = r#"
name: affectionate_romain
services:
  homeassistant:
    container_name: homeassistant
    image: ghcr.io/home-assistant/home-assistant:latest
    network_mode: host
    privileged: true
    restart: unless-stopped
    environment:
      TZ: $TZ
    volumes:
      - type: bind
        source: /DATA/AppData/homeassistant/config
        target: /config
      - /etc/localtime:/etc/localtime:ro
x-casaos:
  author: self
  category: self
  hostname: ""
  index: /
  is_uncontrolled: false
  port_map: 8123
  scheme: http
  store_app_id: affectionate_romain
  title:
    custom: Home Assistant
    en_us: homeassistant
"#;

    fn parse(s: &str) -> ComposeFile {
        match ComposeFile::parse(s, "test.yml") {
            Ok(c) => c,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn installed_custom_app() {
        let c = parse(INSTALLED_HA);
        assert_eq!(c.name().as_deref(), Some("affectionate_romain"));
        assert_eq!(c.title().as_deref(), Some("Home Assistant"));
        let x = c.x_casaos().unwrap_or_default();
        assert_eq!(x.port_map.as_deref(), Some("8123"), "numbers are read as strings");
        assert_eq!(x.hostname, None, "empty strings are None");
        assert_eq!(x.store_app_id.as_deref(), Some("affectionate_romain"));
        let s = c.main_service().unwrap_or_default();
        assert_eq!(s.network_mode.as_deref(), Some("host"));
        assert!(s.privileged);
        assert_eq!(s.volumes.len(), 2);
        assert_eq!(s.volumes[1].kind, "bind");
        assert_eq!(
            c.external_variables().into_iter().collect::<Vec<_>>(),
            vec!["TZ".to_string()]
        );
    }

    #[test]
    fn variables_with_defaults_and_escapes() {
        let v = find_variables(
            "a: $PUID\nb: ${PGID}\nc: ${TZ:-UTC}\nd: ${X-1}\ne: $$NOT\nf: ${REQ:?must be set}\ng: ${AppID}/x\n",
        );
        let v: Vec<_> = v.into_iter().collect();
        assert_eq!(v, vec!["AppID", "PGID", "PUID", "REQ"]);
    }

    #[test]
    fn short_and_long_ports() {
        let c = parse(
            "services:\n  a:\n    image: x\n    ports:\n      - \"8097:8096\"\n      - 7359:7359/udp\n      - 127.0.0.1:9000:9000\n      - target: 1900\n        published: \"1901\"\n        protocol: tcp\n",
        );
        let p = c.services()[0].ports.clone();
        assert_eq!(
            p[0],
            PortSpec {
                published: Some("8097".into()),
                target: "8096".into(),
                protocol: "tcp".into()
            }
        );
        assert_eq!(p[1].protocol, "udp");
        assert_eq!(p[2].published.as_deref(), Some("9000"));
        assert_eq!(p[3].target, "1900");
    }

    #[test]
    fn missing_devices_are_reported_once() {
        let c = parse(
            "services:\n  j:\n    image: x\n    devices:\n      - /dev/dri:/dev/dri\n      - /dev/vcsm:/dev/vcsm\n      - /dev/vcsm\n",
        );
        let missing = c.missing_devices(|p| p == "/dev/dri");
        assert_eq!(missing, vec!["/dev/vcsm".to_string()]);
    }

    #[test]
    fn env_list_form_and_null_values() {
        let c = parse("services:\n  a:\n    image: x\n    environment:\n      - A=1\n      - B\n");
        assert_eq!(
            c.services()[0].environment,
            vec![("A".into(), Some("1".into())), ("B".into(), None)]
        );
        let c = parse("services:\n  a:\n    image: x\n    environment:\n      A: 1\n      B:\n");
        assert_eq!(
            c.services()[0].environment,
            vec![("A".into(), Some("1".into())), ("B".into(), None)]
        );
    }

    #[test]
    fn rejects_non_compose() {
        assert!(ComposeFile::parse("just a string", "x").is_err());
        assert!(ComposeFile::parse("version: '3'\n", "x").is_err());
        assert!(ComposeFile::parse("services: [", "x").is_err());
    }

    #[test]
    fn prose_dollars_in_x_casaos_are_ignored() {
        let c = parse(
            "services:\n  a:\n    image: x\nx-casaos:\n  description:\n    en_US: costs $5 per $MONTH\n",
        );
        assert!(c.external_variables().is_empty());
    }
}
