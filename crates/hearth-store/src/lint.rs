//! Store checks that need no network: run by the store's CI on every pull request (the registry
//! checks are in the `hearth-store` tool).

use crate::catalog::Source;
use crate::model::{CATEGORIES, OptionKind, valid_id};
use crate::resolve::{Hardware, combinations, render_compose, resolve};
use serde_yaml_ng::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Variables Hearth fills in itself.
pub const HOST_VARS: &[&str] = &["PUID", "PGID", "TZ", "APP_DATA", "RENDER_GID", "VIDEO_GID"];

/// Host paths a store entry may bind-mount besides `${APP_DATA}` and folder options.
const ALLOWED_HOST_PATHS: &[&str] = &[
    "/etc/localtime",
    "/run/dbus",
    "/dev/dri",
    "/dev/bus/usb",
    "/dev/serial",
];

/// Largest screenshot allowed in the repository.
pub const MAX_SCREENSHOT_BYTES: u64 = 800 * 1024;
const MAX_ICON_BYTES: u64 = 200 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct ImageName {
    /// `docker.io/library/postgres` form: registry host + repository, lowercase.
    pub name: String,
    pub tag: String,
    pub digest: Option<String>,
}

/// Split and normalise an image reference (Docker Hub short names get their full form).
pub fn parse_image(reference: &str) -> ImageName {
    let (rest, digest) = match reference.split_once('@') {
        Some((r, d)) => (r, Some(d.to_string())),
        None => (reference, None),
    };
    let (name, tag) = match rest.rsplit_once(':') {
        Some((n, t)) if !t.contains('/') => (n, t.to_string()),
        _ => (rest, "latest".to_string()),
    };
    let mut name = name.to_lowercase();
    let first = name.split('/').next().unwrap_or_default().to_string();
    let has_registry =
        name.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
    if !has_registry {
        if !name.contains('/') {
            name = format!("library/{name}");
        }
        name = format!("docker.io/{name}");
    }
    if let Some(r) = name.strip_prefix("index.docker.io/") {
        name = format!("docker.io/{r}");
    }
    ImageName { name, tag, digest }
}

/// A tag names a version instead of a track when it has `<digit>.<digit>` in it (`10.8`,
/// `14-vectorchord0.4.3`). `latest`, `stable`, `v3`, `16`, `9-alpine` and `rocm` are tracks.
pub fn is_pinned_tag(tag: &str) -> bool {
    let b = tag.as_bytes();
    b.windows(3)
        .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit())
}

/// `${VAR}`, `${VAR:-default}`, `${VAR?err}` and `$VAR` names in a text (`$$` is an escape).
pub fn find_vars(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            if b.get(i + 1) == Some(&b'$') {
                i += 2;
                continue;
            }
            let braced = b.get(i + 1) == Some(&b'{');
            let start = if braced { i + 2 } else { i + 1 };
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start && !b[start].is_ascii_digit() {
                out.insert(text[start..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// `(service, image)` for every service.
pub fn images_of(doc: &Value) -> Vec<(String, String)> {
    doc.get("services")
        .and_then(Value::as_mapping)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.get("image")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The image that identifies the app (its web service's image, else its first service's).
pub fn main_image(s: &Source) -> Option<String> {
    let doc: Value = serde_yaml_ng::from_str(&s.compose).ok()?;
    let imgs = images_of(&doc);
    let svc = s.entry.web.as_ref().map(|w| w.service.clone());
    imgs.iter()
        .find(|(n, _)| Some(n) == svc.as_ref())
        .or_else(|| imgs.first())
        .map(|(_, i)| parse_image(i).name)
}

fn norm_url(u: &str) -> String {
    u.trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .to_lowercase()
}

/// Check every entry. Returns `(entry id or folder, problem)` pairs.
pub fn lint(sources: &[Source]) -> Vec<(String, String)> {
    let mut problems = Vec::new();
    for s in sources {
        let mut p = |m: String| problems.push((s.entry.id.clone(), m));
        lint_one(s, &mut p);
    }
    // No duplicates: the same main image or the same upstream source is the same app.
    let mut seen_img: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_src: BTreeMap<String, String> = BTreeMap::new();
    for s in sources {
        let id = &s.entry.id;
        if let Some(img) = main_image(s) {
            if let Some(other) = seen_img.get(&img) {
                problems.push((
                    id.clone(),
                    format!("same app image as {other} ({img}): make it an install option (variant) of {other} instead of a new entry"),
                ));
            } else {
                seen_img.insert(img, id.clone());
            }
        }
        let src = norm_url(&s.entry.upstream.source);
        if let Some(other) = seen_src.get(&src) {
            problems.push((
                id.clone(),
                format!("same upstream project as {other} ({src}): make it an install option of {other}"),
            ));
        } else {
            seen_src.insert(src, id.clone());
        }
    }
    problems
}

fn lint_one(s: &Source, p: &mut dyn FnMut(String)) {
    let e = &s.entry;
    let folder = s.dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if !valid_id(&e.id) {
        p(format!(
            "id '{}' must be reverse-domain (lowercase, e.g. org.jellyfin.jellyfin)",
            e.id
        ));
    }
    if folder != e.id {
        p(format!("folder is '{folder}' but the id is '{}'", e.id));
    }
    if !CATEGORIES.iter().any(|(c, _)| *c == e.category) {
        p(format!(
            "unknown category '{}' (use one of: {})",
            e.category,
            CATEGORIES.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")
        ));
    }
    let name_ok = !e.app_name.is_empty()
        && e.app_name.len() <= 40
        && e.app_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !name_ok {
        p(format!(
            "app_name '{}' must be lowercase letters, digits and '-'",
            e.app_name
        ));
    }
    for (label, url) in [("website", &e.upstream.website), ("source", &e.upstream.source)] {
        if !url.starts_with("https://") {
            p(format!("upstream.{label} must be an https:// URL"));
        }
    }
    if e.summary.len() > 90 {
        p("summary is longer than 90 characters".into());
    }
    if e.requirements.architectures.is_empty() {
        p("requirements.architectures is empty".into());
    }
    // Files.
    let check_file = |rel: &str, max: u64, exts: &[&str], p: &mut dyn FnMut(String)| {
        if rel.contains("..") || rel.starts_with('/') {
            p(format!("{rel}: must be a path inside the entry folder"));
            return;
        }
        match std::fs::metadata(s.dir.join(rel)) {
            Ok(m) if m.len() > max => p(format!("{rel} is {} KB (max {} KB)", m.len() / 1024, max / 1024)),
            Ok(_) => {}
            Err(_) => p(format!("{rel} is missing")),
        }
        if !exts.iter().any(|x| rel.ends_with(x)) {
            p(format!("{rel}: use one of {}", exts.join(", ")));
        }
    };
    check_file(&e.icon, MAX_ICON_BYTES, &[".svg", ".png"], p);
    if e.icon.ends_with(".svg")
        && let Ok(t) = std::fs::read_to_string(s.dir.join(&e.icon))
    {
        let l = t.to_lowercase();
        if l.contains("<script")
            || l.contains(" on")
                && l.contains("=\"")
                && l.split(" on").skip(1).any(|r| {
                    r.split('=')
                        .next()
                        .is_some_and(|n| n.chars().all(|c| c.is_ascii_alphabetic()) && !n.is_empty())
                })
        {
            p(format!(
                "{}: SVG icons must not contain scripts or event handlers",
                e.icon
            ));
        }
        if l.contains("href=\"http") || l.contains("href='http") {
            p(format!(
                "{}: SVG icons must not load anything from the internet",
                e.icon
            ));
        }
    }
    if e.screenshots.is_empty() {
        p("needs at least one screenshot".into());
    }
    for shot in &e.screenshots {
        check_file(&shot.file, MAX_SCREENSHOT_BYTES, &[".webp", ".png", ".jpg"], p);
        if shot.source != "own" && !shot.source.starts_with("https://") {
            p(format!(
                "{}: source must be 'own' or the https:// URL it came from",
                shot.file
            ));
        }
        if shot.source != "own" && shot.license.is_none() {
            p(format!("{}: upstream screenshots need their licence", shot.file));
        }
    }

    // Compose.
    let doc: Value = match serde_yaml_ng::from_str(&s.compose) {
        Ok(d) => d,
        Err(err) => {
            p(format!("compose.yaml: {err}"));
            return;
        }
    };
    let services: Vec<String> = doc
        .get("services")
        .and_then(Value::as_mapping)
        .map(|m| m.keys().filter_map(|k| k.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if services.is_empty() {
        p("compose.yaml has no services".into());
    }
    if doc.get("x-hearth").is_some() {
        p("compose.yaml must not have x-hearth (Hearth writes it from app.yaml)".into());
    }
    if let Some(w) = &e.web {
        if !services.contains(&w.service) {
            p(format!(
                "web.service '{}' isn't a service in compose.yaml",
                w.service
            ));
        }
        match (&w.port_env, w.port) {
            (Some(env), _) if !e.ports.iter().any(|x| x.env.as_ref() == Some(env)) => {
                p(format!("web.port_env {env} isn't in ports"));
            }
            (None, None) => p("web needs port_env (or port, for host networking)".into()),
            _ => {}
        }
    }

    // Variants referenced ⇔ present.
    let used = e.variant_names();
    for v in &used {
        if !s.variants.contains_key(v) {
            p(format!("variants/{v}.yaml is missing"));
        }
    }
    for v in s.variants.keys() {
        if !used.contains(v) {
            p(format!("variants/{v}.yaml isn't used by any install option"));
        }
    }

    // Options.
    let mut option_ids = BTreeSet::new();
    for o in &e.options {
        if !option_ids.insert(o.id.clone()) {
            p(format!("option '{}' is listed twice", o.id));
        }
        match o.kind {
            OptionKind::Choice | OptionKind::Gpu => {
                if o.choices.len() < 2 {
                    p(format!("option '{}' needs at least two choices", o.id));
                }
                let ok_default = o.choices.iter().any(|c| c.id == o.default)
                    || (o.kind == OptionKind::Gpu && o.default == "auto");
                if !ok_default {
                    p(format!(
                        "option '{}': default '{}' isn't a choice",
                        o.id, o.default
                    ));
                }
                if o.kind == OptionKind::Gpu && !o.choices.iter().any(|c| c.needs.is_none()) {
                    p(format!("gpu option '{}' needs a choice without a GPU", o.id));
                }
                for c in &o.choices {
                    if let Some(n) = &c.needs
                        && !["dri", "intel", "amd", "nvidia"].contains(&n.as_str())
                    {
                        p(format!("option '{}': unknown needs '{n}'", o.id));
                    }
                }
            }
            OptionKind::Switch => {
                if !["true", "false"].contains(&o.default.as_str()) {
                    p(format!(
                        "switch '{}': default must be \"true\" or \"false\"",
                        o.id
                    ));
                }
            }
            OptionKind::Folder | OptionKind::Text => {
                if o.env.is_none() {
                    p(format!(
                        "option '{}' needs env (the variable that gets the value)",
                        o.id
                    ));
                }
            }
        }
    }

    // Variables, images, ports and binds, for every combination of options.
    let mut provided: BTreeSet<String> = HOST_VARS.iter().map(|s| s.to_string()).collect();
    provided.extend(e.env.keys().cloned());
    provided.extend(e.ports.iter().filter_map(|x| x.env.clone()));
    let mut folder_vars = BTreeSet::new();
    for o in &e.options {
        if let Some(v) = &o.env {
            provided.insert(v.clone());
            if o.kind == OptionKind::Folder {
                folder_vars.insert(v.clone());
            }
        }
        for c in &o.choices {
            provided.extend(c.effect.env.keys().cloned());
        }
        for eff in o.on.iter().chain(o.off.iter()) {
            provided.extend(eff.env.keys().cloned());
        }
    }
    for (k, v) in &e.env {
        let n = [v.value.is_some(), v.generate.is_some()]
            .iter()
            .filter(|x| **x)
            .count();
        if n != 1 {
            p(format!("env {k}: give exactly one of value or generate"));
        }
        if let Some(g) = &v.generate
            && !["password", "hex32"].contains(&g.as_str())
        {
            p(format!("env {k}: generate must be password or hex32"));
        }
    }
    let mut pins_used = BTreeSet::new();
    let mut seen_problems = BTreeSet::new();
    let mut port_envs_used = BTreeSet::new();
    let hw = Hardware::everything();
    for combo in combinations(e) {
        let r = resolve(e, &combo, &hw);
        for err in r.errors {
            p(format!("options {combo:?}: {err}"));
        }
        let text = match render_compose(&s.compose, &s.variants, &r.variants) {
            Ok(t) => t,
            Err(err) => {
                p(format!("options {combo:?}: {err}"));
                continue;
            }
        };
        let mut msgs = Vec::new();
        for var in find_vars(&text) {
            if !provided.contains(&var) {
                msgs.push(format!(
                    "${{{var}}} has no value: add it to env, ports or an option"
                ));
            }
        }
        let Ok(doc) = serde_yaml_ng::from_str::<Value>(&text) else {
            continue;
        };
        for (svc, img) in images_of(&doc) {
            if !find_vars(&img).is_empty() {
                msgs.push(format!("{svc}: the image name must not use variables ({img})"));
                continue;
            }
            let n = parse_image(&img);
            let pinned = n.digest.is_some() || is_pinned_tag(&n.tag);
            if pinned {
                if e.pin_reasons.contains_key(&n.name) {
                    pins_used.insert(n.name.clone());
                } else {
                    msgs.push(format!(
                        "{svc}: {img} pins a version; use a track tag (latest, stable, a major like 16 or v3), or explain it in pin_reasons"
                    ));
                }
            }
        }
        for (svc, sv) in doc
            .get("services")
            .and_then(Value::as_mapping)
            .into_iter()
            .flatten()
        {
            let svc = svc.as_str().unwrap_or_default();
            if sv.get("container_name").is_some() {
                msgs.push(format!(
                    "{svc}: remove container_name (Hearth names containers after the app)"
                ));
            }
            if sv.get("network_mode").and_then(Value::as_str) == Some("host") && !e.requirements.host_network
            {
                msgs.push(format!(
                    "{svc}: uses host networking; set requirements.host_network: true"
                ));
            }
            for port in sv.get("ports").and_then(Value::as_sequence).into_iter().flatten() {
                let txt = match port {
                    Value::String(t) => t.clone(),
                    Value::Mapping(m) => m
                        .get("published")
                        .map(|v| serde_yaml_ng::to_string(v).unwrap_or_default().trim().to_string())
                        .unwrap_or_default(),
                    other => serde_yaml_ng::to_string(other).unwrap_or_default(),
                };
                let vars = find_vars(&txt);
                match vars.iter().next() {
                    Some(v) if e.ports.iter().any(|x| x.env.as_deref() == Some(v)) => {
                        port_envs_used.insert(v.clone());
                    }
                    _ => msgs.push(format!(
                        "{svc}: published port '{txt}' must use a host port variable from ports (e.g. ${{WEB_PORT}}:80)"
                    )),
                }
            }
            for vol in sv
                .get("volumes")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
            {
                let src = match vol {
                    Value::String(t) => t.split(':').next().unwrap_or_default().to_string(),
                    Value::Mapping(m) => m
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    _ => String::new(),
                };
                let is_path = src.starts_with('/')
                    || src.starts_with('.')
                    || src.starts_with('$')
                    || src.starts_with('~');
                if !is_path {
                    continue; // named volume
                }
                let ok = src.starts_with("${APP_DATA}")
                    || folder_vars.iter().any(|v| src.starts_with(&format!("${{{v}}}")))
                    || ALLOWED_HOST_PATHS
                        .iter()
                        .any(|a| src == *a || src.starts_with(&format!("{a}/")));
                if !ok {
                    msgs.push(format!(
                        "{svc}: bind mount '{src}' must be under ${{APP_DATA}}, a folder option, or one of {}",
                        ALLOWED_HOST_PATHS.join(", ")
                    ));
                }
            }
        }
        for m in msgs {
            if seen_problems.insert(m.clone()) {
                p(m);
            }
        }
    }
    for k in e.pin_reasons.keys() {
        if !pins_used.contains(k) {
            p(format!("pin_reasons has {k}, which no service pins"));
        }
    }
    for x in &e.ports {
        match &x.env {
            Some(v) if !port_envs_used.contains(v) => p(format!("ports: {v} isn't used in compose.yaml")),
            None if !e.requirements.host_network => {
                p(format!("ports: port {} needs env (or host networking)", x.port))
            }
            _ => {}
        }
    }
    for d in &e.data_dirs {
        if d.path.starts_with('/') || d.path.contains("..") || d.path.is_empty() {
            p(format!(
                "data_dirs: '{}' must be a relative path under APP_DATA",
                d.path
            ));
        }
        if !["root", "user"].contains(&d.owner.as_str()) {
            p(format!("data_dirs: owner of '{}' must be root or user", d.path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_names() {
        let n = parse_image("postgres:16");
        assert_eq!(n.name, "docker.io/library/postgres");
        assert_eq!(n.tag, "16");
        let n = parse_image("ghcr.io/Immich-App/postgres:14-vectorchord0.4.3@sha256:abc");
        assert_eq!(n.name, "ghcr.io/immich-app/postgres");
        assert_eq!(n.digest.as_deref(), Some("sha256:abc"));
        assert_eq!(parse_image("jellyfin/jellyfin").tag, "latest");
        assert_eq!(parse_image("localhost:5000/x").name, "localhost:5000/x");
        assert_eq!(parse_image("localhost:5000/x").tag, "latest");
    }

    #[test]
    fn pins() {
        for t in ["latest", "stable", "v3", "16", "9-alpine", "rocm", "release"] {
            assert!(!is_pinned_tag(t), "{t}");
        }
        for t in ["10.8.12", "v1.2", "14-vectorchord0.4.3", "2025.11"] {
            assert!(is_pinned_tag(t), "{t}");
        }
    }

    #[test]
    fn vars() {
        let v = find_vars("a ${A} $B ${C:-x} $$D ${E?err} $1");
        assert_eq!(v.into_iter().collect::<Vec<_>>(), vec!["A", "B", "C", "E"]);
    }
}
