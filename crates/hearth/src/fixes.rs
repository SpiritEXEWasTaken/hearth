//! Known fixes for real-world app templates. Each fix is opt-in, explains itself, and is
//! recorded in the file's `x-hearth.decisions` when applied.

use serde_yaml_ng::{Mapping, Value};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    pub id: String,
    pub description: String,
    /// Applying it changes the container, so the app is recreated once.
    pub needs_restart: bool,
}

/// Jellyfin's client discovery (7359) and DLNA (1900) are UDP. Templates that publish them as TCP
/// break discovery from TVs and apps.
const JELLYFIN_UDP_PORTS: &[&str] = &["7359", "1900"];
/// Raspberry Pi firmware libraries; meaningless (and often an empty folder Docker created) elsewhere.
const RPI_PATHS: &[&str] = &["/opt/vc/lib", "/opt/vc"];

fn is_jellyfin(image: &str) -> bool {
    let name = image.split(['@', ':']).next().unwrap_or(image);
    name.ends_with("/jellyfin") || name == "jellyfin"
}

/// The fixes that apply to this compose text on this host.
pub fn available(compose: &str, is_raspberry_pi: bool) -> Vec<Fix> {
    let Ok(doc) = serde_yaml_ng::from_str::<Value>(compose) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let services = doc
        .get("services")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    for (name, svc) in &services {
        let name = name.as_str().unwrap_or_default();
        let image = svc.get("image").and_then(Value::as_str).unwrap_or_default();
        if is_jellyfin(image) {
            let wrong: Vec<String> = ports(svc)
                .into_iter()
                .filter(|(target, proto)| JELLYFIN_UDP_PORTS.contains(&target.as_str()) && proto != "udp")
                .map(|(t, _)| t)
                .collect();
            if !wrong.is_empty() {
                out.push(Fix {
                    id: "jellyfin-udp".into(),
                    description: format!(
                        "{name}: publish ports {} as UDP (Jellyfin's client discovery and DLNA use UDP; TCP breaks finding the server from TVs and apps)",
                        wrong.join(", ")
                    ),
                    needs_restart: true,
                });
            }
        }
        if !is_raspberry_pi && binds(svc).iter().any(|s| RPI_PATHS.contains(&s.as_str())) {
            out.push(Fix {
                id: "drop-rpi-paths".into(),
                description: format!(
                    "{name}: remove the Raspberry Pi-only mount /opt/vc/lib (this host isn't a Raspberry Pi)"
                ),
                needs_restart: true,
            });
        }
    }
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Applies the fixes with the given ids. Returns the new compose text and one decision per change.
pub fn apply(
    compose: &str,
    ids: &[String],
    is_raspberry_pi: bool,
    today: &str,
) -> Result<(String, Vec<String>), String> {
    let mut doc: Value = serde_yaml_ng::from_str(compose).map_err(|e| e.to_string())?;
    let mut decisions = Vec::new();
    let known = ["jellyfin-udp", "drop-rpi-paths"];
    if let Some(bad) = ids.iter().find(|i| !known.contains(&i.as_str())) {
        return Err(format!("unknown fix '{bad}' (known: {})", known.join(", ")));
    }
    let header = compose
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) {
        for (name, svc) in services.iter_mut() {
            let name = name.as_str().unwrap_or_default().to_string();
            let image = svc
                .get("image")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let Some(m) = svc.as_mapping_mut() else { continue };
            if ids.iter().any(|i| i == "jellyfin-udp") && is_jellyfin(&image) {
                for t in set_udp(m, JELLYFIN_UDP_PORTS) {
                    decisions.push(format!(
                        "published port {t} of {name} as UDP (Jellyfin discovery/DLNA)"
                    ));
                }
            }
            if ids.iter().any(|i| i == "drop-rpi-paths") && !is_raspberry_pi {
                for p in drop_binds(m, RPI_PATHS) {
                    decisions.push(format!("removed the Raspberry Pi-only mount {p} from {name}"));
                }
            }
        }
    }
    if !decisions.is_empty()
        && let Some(root) = doc.as_mapping_mut()
    {
        let xh = root
            .entry("x-hearth".into())
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Some(xh) = xh.as_mapping_mut() {
            let list = xh
                .entry("decisions".into())
                .or_insert_with(|| Value::Sequence(Vec::new()));
            if let Some(seq) = list.as_sequence_mut() {
                for d in &decisions {
                    seq.push(Value::String(format!("{today}: {d}")));
                }
            }
        }
    }
    let body = serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())?;
    let text = if header.is_empty() {
        body
    } else {
        format!("{header}\n{body}")
    };
    Ok((text, decisions))
}

/// (target, protocol) of each published port.
fn ports(svc: &Value) -> Vec<(String, String)> {
    svc.get("ports")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Value::Mapping(m) => Some((
                scalar(m.get("target")?)?,
                m.get("protocol")
                    .and_then(Value::as_str)
                    .unwrap_or("tcp")
                    .to_string(),
            )),
            other => {
                let s = scalar(other)?;
                let (spec, proto) = s
                    .rsplit_once('/')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .unwrap_or((s, "tcp".into()));
                Some((spec.rsplit(':').next()?.to_string(), proto))
            }
        })
        .collect()
}

fn set_udp(svc: &mut Mapping, targets: &[&str]) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(seq) = svc.get_mut("ports").and_then(Value::as_sequence_mut) else {
        return changed;
    };
    for p in seq.iter_mut() {
        match p {
            Value::Mapping(m) => {
                let t = m.get("target").and_then(scalar).unwrap_or_default();
                let proto = m.get("protocol").and_then(Value::as_str).unwrap_or("tcp");
                if targets.contains(&t.as_str()) && proto != "udp" {
                    m.insert("protocol".into(), "udp".into());
                    changed.push(t);
                }
            }
            other => {
                let Some(s) = scalar(other) else { continue };
                let (spec, proto) = s
                    .rsplit_once('/')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .unwrap_or((s.clone(), "tcp".into()));
                let t = spec.rsplit(':').next().unwrap_or_default().to_string();
                if targets.contains(&t.as_str()) && proto != "udp" {
                    *other = Value::String(format!("{spec}/udp"));
                    changed.push(t);
                }
            }
        }
    }
    changed
}

fn binds(svc: &Value) -> Vec<String> {
    svc.get("volumes")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|v| match v {
            Value::Mapping(m) => m.get("source").and_then(Value::as_str).map(str::to_string),
            Value::String(s) => s.split(':').next().map(str::to_string),
            _ => None,
        })
        .collect()
}

fn drop_binds(svc: &mut Mapping, paths: &[&str]) -> Vec<String> {
    let mut removed = Vec::new();
    if let Some(seq) = svc.get_mut("volumes").and_then(Value::as_sequence_mut) {
        seq.retain(|v| {
            let src = match v {
                Value::Mapping(m) => m
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Value::String(s) => s.split(':').next().unwrap_or_default().to_string(),
                _ => String::new(),
            };
            if paths.contains(&src.as_str()) {
                removed.push(src);
                false
            } else {
                true
            }
        });
        if seq.is_empty() {
            svc.remove("volumes");
        }
    }
    removed
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// True on Raspberry Pi hardware (where /opt/vc and /dev/vchiq are real).
pub fn host_is_raspberry_pi() -> bool {
    std::path::Path::new("/dev/vchiq").exists()
        || std::fs::read_to_string("/proc/device-tree/model")
            .map(|m| m.contains("Raspberry Pi"))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JELLYFIN: &str = "# header\nservices:\n  jellyfin:\n    image: linuxserver/jellyfin:latest\n    ports:\n    - target: 8096\n      published: '8097'\n      protocol: tcp\n    - target: 7359\n      published: '7359'\n      protocol: tcp\n    - 1901:1900\n    volumes:\n    - type: bind\n      source: /DATA/AppData/jellyfin/config\n      target: /config\n    - type: bind\n      source: /opt/vc/lib\n      target: /opt/vc/lib\n";

    #[test]
    fn finds_jellyfin_fixes_on_x86() {
        let f = available(JELLYFIN, false);
        let ids: Vec<&str> = f.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["jellyfin-udp", "drop-rpi-paths"]);
        assert!(f[0].description.contains("7359, 1900"));
        // On a Pi the /opt/vc mount is legitimate.
        let ids: Vec<String> = available(JELLYFIN, true).into_iter().map(|f| f.id).collect();
        assert_eq!(ids, vec!["jellyfin-udp"]);
    }

    #[test]
    fn applies_fixes_and_records_them() {
        let (text, decisions) = match apply(
            JELLYFIN,
            &["jellyfin-udp".into(), "drop-rpi-paths".into()],
            false,
            "2026-11-25",
        ) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert!(text.starts_with("# header\n"));
        assert!(text.contains("1901:1900/udp"), "{text}");
        assert!(text.contains("protocol: udp"));
        assert!(
            text.contains("target: 8096\n      published: '8097'\n      protocol: tcp"),
            "8096 stays TCP: {text}"
        );
        assert!(!text.contains("source: /opt/vc/lib"));
        assert_eq!(decisions.len(), 3);
        assert!(text.contains("2026-11-25: published port 7359 of jellyfin as UDP"));
        // Nothing left to fix afterwards.
        assert!(available(&text, false).is_empty());
    }

    #[test]
    fn other_apps_and_unknown_fixes() {
        let c = "services:\n  web:\n    image: nginx\n    ports:\n    - 1900:1900\n";
        assert!(
            available(c, false).is_empty(),
            "only Jellyfin's ports are touched"
        );
        assert!(apply(c, &["nope".into()], false, "x").is_err());
    }
}
