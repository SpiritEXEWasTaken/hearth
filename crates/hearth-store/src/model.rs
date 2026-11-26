//! The store entry format: `apps/<id>/app.yaml` in the store repository.
//!
//! Unknown fields are rejected, so a typo in an entry fails the store's CI instead of being
//! silently ignored. `schema/app.schema.json` in the store repo describes the same format for
//! editors; this file is the authority.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Store categories. The store page shows them in this order.
pub const CATEGORIES: &[(&str, &str)] = &[
    ("media", "Media"),
    ("photos", "Photos"),
    ("files", "Files & sync"),
    ("home", "Smart home"),
    ("security", "Passwords & security"),
    ("network", "Network & DNS"),
    ("documents", "Documents"),
    ("ai", "AI"),
    ("gaming", "Gaming"),
    ("productivity", "Productivity"),
    ("monitoring", "Monitoring"),
    ("development", "Development"),
    ("downloads", "Downloads"),
    ("communication", "Communication"),
    ("other", "Other"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Reverse-domain id, e.g. `org.jellyfin.jellyfin`. Never changes once published.
    pub id: String,
    pub name: String,
    /// One line, shown on store cards.
    pub summary: String,
    /// A few short paragraphs in plain text (blank line between paragraphs).
    pub description: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub upstream: Upstream,
    /// SPDX licence of the app itself.
    pub license: String,
    /// File in the entry folder (`icon.svg` or `icon.png`).
    pub icon: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<Screenshot>,
    /// Default app name on this server (`jellyfin`); the user can change it.
    pub app_name: String,
    /// The app's web page, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSpec>,
    pub requirements: Requirements,
    /// Shown before installing and on the app's page, e.g. "No movies, TV shows or music are
    /// provided."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Ports published on the host. Every published port in the compose file must be listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortSpec>,
    /// Values for `.env`: fixed values, generated secrets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, EnvSpec>,
    /// Install menu.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OptionSpec>,
    /// Folders under `${APP_DATA}` that Hearth creates before the first start, with their owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_dirs: Vec<DataDir>,
    /// Image tags that aren't a track (e.g. Immich's exact Postgres build), with the reason.
    /// Keys are image names without tag (`ghcr.io/immich-app/postgres`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_reasons: BTreeMap<String, String>,
    /// Container paths left out of backups (caches, thumbnails).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backup_exclude: Vec<String>,
    /// Things to know after installing, shown on the app's page (plain text lines).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_install: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub website: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Screenshot {
    /// File in the entry folder (`screenshots/1.webp`).
    pub file: String,
    pub caption: String,
    /// Where it came from: `own` (taken by the store maintainers) or the upstream URL.
    pub source: String,
    /// Licence of the image when it came from upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum HttpsNeed {
    /// Works fine over plain HTTP on the home network.
    #[default]
    No,
    /// Some features (camera, microphone, install as an app, notifications) need HTTPS.
    Recommended,
    /// The app (or its phone apps) won't work without HTTPS.
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebSpec {
    pub service: String,
    /// The `ports` entry (by its env variable) that serves the web page. Omit for host networking,
    /// where `port` is the fixed port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default = "slash")]
    pub path: String,
    /// Readiness URL path used by the health gate (defaults to `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default)]
    pub https: HttpsNeed,
    /// Show a tile on the dashboard (false for API-only apps such as Ollama).
    #[serde(default = "yes")]
    pub dashboard: bool,
}

fn slash() -> String {
    "/".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuUse {
    /// The app doesn't use a GPU.
    #[default]
    None,
    /// Can use one (faster), works without.
    Optional,
    /// Works without, but slowly.
    Recommended,
    /// Won't work without.
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    /// RAM the app needs to run at all (all its containers, MB).
    pub ram_mb: u64,
    /// RAM for comfortable use (MB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_recommended_mb: Option<u64>,
    /// A typical size of the app's own data (database, config, caches; not your media), MB.
    pub data_mb: u64,
    /// CPU architectures every image supports (`amd64`, `arm64`, `arm`). CI checks the registry.
    pub architectures: Vec<String>,
    #[serde(default)]
    pub gpu: GpuUse,
    /// Uses host networking (sees the LAN directly: discovery, mDNS).
    #[serde(default)]
    pub host_network: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    /// The `.env` variable the compose file uses for the host port (`WEB_PORT`). Omit for host
    /// networking, where the port is fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Default host port.
    pub port: u16,
    #[serde(default)]
    pub protocol: Protocol,
    /// What it's for, in words ("Web page", "DNS").
    pub label: String,
    /// Can another host port be used if this one is taken? (No for DNS: devices expect 53.)
    #[serde(default = "yes")]
    pub movable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvSpec {
    /// A fixed value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Generate a random value at install: `password` (24 letters/digits), `hex32` (64 hex chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate: Option<String>,
    /// Show this value to the user once after installing, with this label (e.g. an admin password).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_as: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OptionKind {
    /// Pick one of `choices`.
    Choice,
    /// GPU acceleration: like `choice`, but choices with `needs` are only offered when the host
    /// has that hardware, and `auto` picks the best one found.
    Gpu,
    /// On/off.
    Switch,
    /// A folder on the host, stored in `env`.
    Folder,
    /// A line of text, stored in `env`.
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OptionSpec {
    pub id: String,
    pub kind: OptionKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Choice/gpu: a choice id (`auto` for gpu). Switch: `true`/`false`. Folder/text: the value.
    pub default: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
    /// Switch: what "on" and "off" do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<Effect>,
    /// Folder/text: the `.env` variable that gets the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Hardware this choice needs: `dri` (Intel/AMD GPU), `nvidia`, `amd` (AMD GPU, ROCm),
    /// `intel` (Intel GPU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
    #[serde(flatten)]
    pub effect: Effect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    /// Variant overlays (`variants/<name>.yaml`) merged onto the base compose, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// `.env` values set by this choice.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DataDir {
    /// Path below `${APP_DATA}` (`config`, `library`).
    pub path: String,
    /// Owner: `root` or `user` (the PUID/PGID user apps run as).
    #[serde(default = "root")]
    pub owner: String,
}

fn root() -> String {
    "root".into()
}

impl Entry {
    pub fn category_name(&self) -> &str {
        CATEGORIES
            .iter()
            .find(|(id, _)| *id == self.category)
            .map(|(_, n)| *n)
            .unwrap_or("Other")
    }

    pub fn option(&self, id: &str) -> Option<&OptionSpec> {
        self.options.iter().find(|o| o.id == id)
    }

    /// Every variant name used by any option.
    pub fn variant_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for o in &self.options {
            let effects = o
                .choices
                .iter()
                .map(|c| &c.effect)
                .chain(o.on.iter())
                .chain(o.off.iter());
            for e in effects {
                for name in &e.variants {
                    if !v.contains(name) {
                        v.push(name.clone());
                    }
                }
            }
        }
        v
    }
}

/// Is this a valid entry id? Reverse-domain: lowercase letters, digits, `-`, at least two dots
/// separated parts.
pub fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && id.len() <= 100
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && !p.starts_with('-')
        })
}
