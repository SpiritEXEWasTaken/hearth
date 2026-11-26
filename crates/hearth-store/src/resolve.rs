//! Turning a store entry plus the user's install answers into a compose file and `.env` values.

use crate::merge;
use crate::model::{Effect, Entry, OptionKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hardware facts that install options depend on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Hardware {
    /// `/dev/dri` render node present (Intel or AMD GPU, or another VAAPI device).
    pub dri: bool,
    /// Vendor of the first render node: `intel`, `amd`, `nvidia` or `other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dri_vendor: Option<String>,
    /// `/dev/kfd` present (AMD ROCm compute).
    pub kfd: bool,
    /// Nvidia driver present (`/dev/nvidiactl`).
    pub nvidia: bool,
    /// Docker knows the `nvidia` runtime (nvidia-container-toolkit installed).
    pub nvidia_runtime: bool,
    /// Pretend every kind of hardware is present (store checks render every choice).
    #[serde(skip)]
    pub assume_all: bool,
}

impl Hardware {
    /// Does the host have what a choice `needs`?
    pub fn everything() -> Hardware {
        Hardware {
            assume_all: true,
            ..Default::default()
        }
    }

    pub fn has(&self, need: &str) -> bool {
        if self.assume_all {
            return true;
        }
        match need {
            "dri" => self.dri,
            "intel" => self.dri && self.dri_vendor.as_deref() == Some("intel"),
            "amd" => self.dri && self.kfd && self.dri_vendor.as_deref() == Some("amd"),
            "nvidia" => self.nvidia && self.nvidia_runtime,
            _ => false,
        }
    }
}

/// The finished install plan for an entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Resolved {
    /// Final answers, one per option (defaults filled in, `auto` replaced).
    pub answers: BTreeMap<String, String>,
    pub variants: Vec<String>,
    /// `.env` values from options and fixed `env:` entries (secrets are generated later).
    pub env: BTreeMap<String, String>,
    pub errors: Vec<String>,
}

/// Choose the GPU choice for `auto`: the first choice (in the entry's order) whose hardware is
/// present, after the CPU-only one; CPU-only if nothing fits.
fn auto_gpu(choices: &[crate::model::Choice], hw: &Hardware) -> Option<String> {
    choices
        .iter()
        .find(|c| c.needs.as_deref().is_some_and(|n| hw.has(n)))
        .or_else(|| choices.iter().find(|c| c.needs.is_none()))
        .map(|c| c.id.clone())
}

pub fn resolve(entry: &Entry, answers: &BTreeMap<String, String>, hw: &Hardware) -> Resolved {
    let mut r = Resolved::default();
    for (k, v) in &entry.env {
        if let Some(val) = &v.value {
            r.env.insert(k.clone(), val.clone());
        }
    }
    for unknown in answers.keys().filter(|k| entry.option(k).is_none()) {
        r.errors
            .push(format!("{} has no install option '{unknown}'", entry.name));
    }
    for o in &entry.options {
        let given = answers.get(&o.id).cloned();
        let mut value = given.clone().unwrap_or_else(|| o.default.clone());
        match o.kind {
            OptionKind::Choice | OptionKind::Gpu => {
                if o.kind == OptionKind::Gpu && value == "auto" {
                    value = auto_gpu(&o.choices, hw).unwrap_or_default();
                }
                match o.choices.iter().find(|c| c.id == value) {
                    Some(c) => {
                        if let Some(need) = &c.needs
                            && given.is_some()
                            && !hw.has(need)
                        {
                            r.errors.push(format!(
                                "{}: '{}' needs {} on this server, and it wasn't found",
                                o.label,
                                c.label,
                                need_words(need)
                            ));
                        }
                        apply(&c.effect, &mut r);
                    }
                    None => r.errors.push(format!(
                        "{}: '{value}' isn't one of: {}",
                        o.label,
                        o.choices
                            .iter()
                            .map(|c| c.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            OptionKind::Switch => {
                let on = match value.as_str() {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => {
                        r.errors.push(format!("{}: use on or off", o.label));
                        false
                    }
                };
                value = if on { "true" } else { "false" }.into();
                if let Some(e) = if on { &o.on } else { &o.off } {
                    apply(e, &mut r);
                }
            }
            OptionKind::Folder | OptionKind::Text => {
                if let Some(var) = &o.env {
                    if o.kind == OptionKind::Folder
                        && !(value.starts_with('/') || value.starts_with("${APP_DATA}"))
                    {
                        r.errors
                            .push(format!("{}: give a full path starting with /", o.label));
                    }
                    if value.contains('\n') {
                        r.errors.push(format!("{}: must be one line", o.label));
                    }
                    r.env.insert(var.clone(), value.clone());
                }
            }
        }
        r.answers.insert(o.id.clone(), value);
    }
    r
}

fn apply(e: &Effect, r: &mut Resolved) {
    r.variants.extend(e.variants.iter().cloned());
    r.env.extend(e.env.iter().map(|(k, v)| (k.clone(), v.clone())));
}

pub fn need_words(need: &str) -> &'static str {
    match need {
        "dri" => "an Intel or AMD graphics chip (/dev/dri)",
        "intel" => "an Intel graphics chip",
        "amd" => "an AMD graphics card with ROCm (/dev/kfd)",
        "nvidia" => "an Nvidia card with the Nvidia container toolkit",
        _ => "hardware",
    }
}

/// Base compose + the chosen variants, as YAML text.
pub fn render_compose(
    base: &str,
    variants: &BTreeMap<String, String>,
    chosen: &[String],
) -> Result<String, String> {
    let mut texts = Vec::new();
    for name in chosen {
        let t = variants
            .get(name)
            .ok_or_else(|| format!("the entry has no variant '{name}'"))?;
        texts.push(t.as_str());
    }
    let doc = merge::merge_texts(base, &texts)?;
    serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())
}

/// Every combination of option values that changes variants (for CI: each must be valid).
pub fn combinations(entry: &Entry) -> Vec<BTreeMap<String, String>> {
    let mut out: Vec<BTreeMap<String, String>> = vec![BTreeMap::new()];
    for o in &entry.options {
        let values: Vec<String> = match o.kind {
            OptionKind::Choice | OptionKind::Gpu => o.choices.iter().map(|c| c.id.clone()).collect(),
            OptionKind::Switch => vec!["true".into(), "false".into()],
            _ => continue,
        };
        let mut next = Vec::new();
        for base in &out {
            for v in &values {
                let mut m = base.clone();
                m.insert(o.id.clone(), v.clone());
                next.push(m);
            }
        }
        out = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        serde_yaml_ng::from_str(
            r#"
id: org.example.app
name: Example
summary: s
description: d
category: media
upstream: {website: "https://e.org", source: "https://github.com/e/e"}
license: MIT
icon: icon.svg
app_name: example
requirements: {ram_mb: 100, data_mb: 10, architectures: [amd64]}
env:
  FIXED: {value: "1"}
  SECRET: {generate: password}
options:
  - id: gpu
    kind: gpu
    label: Hardware acceleration
    default: auto
    choices:
      - {id: none, label: None}
      - {id: nvidia, label: Nvidia, needs: nvidia, variants: [nvidia]}
      - {id: vaapi, label: Intel/AMD, needs: dri, variants: [vaapi], env: {DRIVER: va}}
  - id: ml
    kind: switch
    label: Machine learning
    default: "true"
    off: {variants: [no-ml]}
  - id: media
    kind: folder
    label: Media folder
    default: /srv/media
    env: MEDIA
"#,
        )
        .expect("entry")
    }

    #[test]
    fn auto_gpu_picks_present_hardware() {
        let e = entry();
        let hw = Hardware {
            dri: true,
            dri_vendor: Some("intel".into()),
            ..Default::default()
        };
        let r = resolve(&e, &BTreeMap::new(), &hw);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.answers["gpu"], "vaapi");
        assert_eq!(r.variants, vec!["vaapi"]);
        assert_eq!(r.env["DRIVER"], "va");
        assert_eq!(r.env["FIXED"], "1");
        assert_eq!(r.env["MEDIA"], "/srv/media");
        assert!(!r.env.contains_key("SECRET"));

        let r = resolve(&e, &BTreeMap::new(), &Hardware::default());
        assert_eq!(r.answers["gpu"], "none");
        assert!(r.variants.is_empty());
    }

    #[test]
    fn explicit_answers_are_checked() {
        let e = entry();
        let mut a = BTreeMap::new();
        a.insert("gpu".to_string(), "nvidia".to_string());
        a.insert("ml".to_string(), "off".to_string());
        a.insert("bogus".to_string(), "x".to_string());
        let r = resolve(&e, &a, &Hardware::default());
        assert_eq!(r.variants, vec!["nvidia", "no-ml"]);
        assert_eq!(r.errors.len(), 2, "{:?}", r.errors);
        assert!(r.errors.iter().any(|e| e.contains("Nvidia")));
        assert!(r.errors.iter().any(|e| e.contains("bogus")));
    }

    #[test]
    fn combinations_cover_choices_and_switches() {
        assert_eq!(combinations(&entry()).len(), 6);
    }
}
