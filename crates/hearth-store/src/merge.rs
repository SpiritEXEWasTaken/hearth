//! Merging variant overlays onto a base compose file.
//!
//! Follows Compose's own merge rules for the parts store entries use, so a variant reads like a
//! normal `docker compose -f base.yaml -f variant.yaml` override file:
//! - mappings merge key by key, the overlay's scalars win;
//! - `environment`, `labels`, `extra_hosts`, `sysctls` (list or map form) merge by key;
//! - `volumes` and `devices` of a service merge by container path (the overlay wins);
//! - `ports` and other lists are joined without duplicates;
//! - `command`, `entrypoint` and `healthcheck.test` are replaced.
//!
//! One extension: a top-level `x-remove-services: [name, …]` in an overlay drops services
//! (and their `depends_on` entries), e.g. Immich without machine learning.

use serde_yaml_ng::{Mapping, Value};

const BY_KEY: &[&str] = &["environment", "labels", "extra_hosts", "sysctls", "annotations"];
const BY_TARGET: &[&str] = &["volumes", "devices"];
const REPLACE: &[&str] = &["command", "entrypoint", "test"];

pub fn merge_texts(base: &str, overlays: &[&str]) -> Result<Value, String> {
    let mut doc: Value = serde_yaml_ng::from_str(base).map_err(|e| format!("base compose: {e}"))?;
    for (i, o) in overlays.iter().enumerate() {
        let ov: Value = serde_yaml_ng::from_str(o).map_err(|e| format!("variant {}: {e}", i + 1))?;
        merge_overlay(&mut doc, ov)?;
    }
    Ok(doc)
}

pub fn merge_overlay(doc: &mut Value, mut overlay: Value) -> Result<(), String> {
    let Some(ov) = overlay.as_mapping_mut() else {
        return Err("a variant must be a mapping".into());
    };
    let remove: Vec<String> = ov
        .remove("x-remove-services")
        .and_then(|v| v.as_sequence().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let Some(root) = doc.as_mapping_mut() else {
        return Err("the base compose must be a mapping".into());
    };
    merge_map(root, ov.clone(), &[]);
    if !remove.is_empty()
        && let Some(services) = root.get_mut("services").and_then(Value::as_mapping_mut)
    {
        for r in &remove {
            if services.remove(r.as_str()).is_none() {
                return Err(format!("x-remove-services: no service '{r}'"));
            }
        }
        for (_, svc) in services.iter_mut() {
            if let Some(dep) = svc.get_mut("depends_on") {
                match dep {
                    Value::Sequence(s) => {
                        s.retain(|v| !v.as_str().is_some_and(|n| remove.iter().any(|r| r == n)))
                    }
                    Value::Mapping(m) => {
                        for r in &remove {
                            m.remove(r.as_str());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn merge_map(base: &mut Mapping, over: Mapping, path: &[String]) {
    for (k, v) in over {
        let key = k.as_str().unwrap_or_default().to_string();
        let mut sub = path.to_vec();
        sub.push(key.clone());
        let in_service = path.len() == 2 && path[0] == "services";
        match base.get_mut(&k) {
            None => {
                base.insert(k, v);
            }
            Some(existing) => {
                if REPLACE.contains(&key.as_str()) {
                    *existing = v;
                } else if in_service && BY_KEY.contains(&key.as_str()) {
                    let mut m = to_map(existing);
                    for (kk, vv) in to_map(&v) {
                        m.insert(kk, vv);
                    }
                    *existing = Value::Mapping(m);
                } else if in_service && BY_TARGET.contains(&key.as_str()) {
                    merge_by_target(existing, v);
                } else {
                    match (existing, v) {
                        (Value::Mapping(a), Value::Mapping(b)) => merge_map(a, b, &sub),
                        (Value::Sequence(a), Value::Sequence(b)) => {
                            for item in b {
                                if !a.contains(&item) {
                                    a.push(item);
                                }
                            }
                        }
                        (slot, v) => *slot = v,
                    }
                }
            }
        }
    }
}

/// `KEY=VALUE` lists and mappings both become a mapping.
fn to_map(v: &Value) -> Mapping {
    match v {
        Value::Mapping(m) => m.clone(),
        Value::Sequence(s) => {
            let mut m = Mapping::new();
            for item in s {
                if let Some(text) = item.as_str() {
                    let (k, val) = match text.split_once('=') {
                        Some((k, v)) => (k, Value::String(v.to_string())),
                        None => (text, Value::Null),
                    };
                    m.insert(Value::String(k.to_string()), val);
                }
            }
            m
        }
        _ => Mapping::new(),
    }
}

/// The container path of a short (`src:dst[:mode]`) or long (`target:`) volume/device entry.
fn target_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let parts: Vec<&str> = s.split(':').collect();
            Some(parts.get(1).unwrap_or(&parts[0]).to_string())
        }
        Value::Mapping(m) => m.get("target").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn merge_by_target(existing: &mut Value, over: Value) {
    let (Value::Sequence(a), Value::Sequence(b)) = (&mut *existing, over.clone()) else {
        *existing = over;
        return;
    };
    for item in b {
        let t = target_of(&item);
        match a.iter().position(|x| t.is_some() && target_of(x) == t) {
            Some(i) => a[i] = item,
            None => a.push(item),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(s: &str) -> Value {
        serde_yaml_ng::from_str(s).expect("yaml")
    }

    #[test]
    fn merges_like_compose() {
        let base = r#"
services:
  web:
    image: x/web:latest
    environment:
      - A=1
      - B=2
    volumes:
      - ./config:/config
      - ./cache:/cache
    ports: ["8080:80"]
    command: ["serve"]
  ml:
    image: x/ml:latest
  db:
    image: postgres:16
    depends_on: [ml]
"#;
        let gpu = r#"
services:
  web:
    environment:
      B: "3"
      C: "4"
    volumes:
      - /fast/cache:/cache
    devices: ["/dev/dri:/dev/dri"]
    ports: ["8080:80", "7359:7359/udp"]
    command: ["serve", "--gpu"]
"#;
        let noml = "x-remove-services: [ml]\n";
        let out = merge_texts(base, &[gpu, noml]).expect("merge");
        let web = &out["services"]["web"];
        assert_eq!(web["environment"], y("{A: '1', B: '3', C: '4'}"));
        assert_eq!(web["volumes"], y("['./config:/config', '/fast/cache:/cache']"));
        assert_eq!(web["devices"], y("['/dev/dri:/dev/dri']"));
        assert_eq!(web["ports"], y("['8080:80', '7359:7359/udp']"));
        assert_eq!(web["command"], y("['serve', '--gpu']"));
        assert!(out["services"].get("ml").is_none());
        assert_eq!(out["services"]["db"]["depends_on"], y("[]"));
        assert!(out.get("x-remove-services").is_none());
    }

    #[test]
    fn removing_an_unknown_service_fails() {
        let e = merge_texts("services: {a: {image: x}}\n", &["x-remove-services: [b]\n"]);
        assert!(e.is_err());
    }

    #[test]
    fn deep_merges_deploy() {
        let base = "services: {a: {image: x, deploy: {resources: {limits: {memory: 1g}}}}}\n";
        let nv = "services: {a: {deploy: {resources: {reservations: {devices: [{driver: nvidia, count: 1, capabilities: [gpu]}]}}}}}\n";
        let out = merge_texts(base, &[nv]).expect("merge");
        let r = &out["services"]["a"]["deploy"]["resources"];
        assert_eq!(r["limits"]["memory"], y("1g"));
        assert_eq!(r["reservations"]["devices"][0]["driver"], y("nvidia"));
    }
}
