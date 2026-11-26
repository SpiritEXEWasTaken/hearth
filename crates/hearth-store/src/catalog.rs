//! The published catalog (GitHub Pages) and loading the store repository.
//!
//! ```text
//! index.json            Index: every app's card data, plus the sha256 of its bundle and icon
//! index.json.minisig    minisign signature of index.json (Hearth refuses the catalog without it)
//! apps/<id>.json        Bundle: entry, compose, variants, image facts, screenshot hashes
//! icons/<id>.<ext>
//! files/<id>/screenshots/…
//! ```
//!
//! Only `index.json` is signed; everything else is checked against the hashes in it (bundles
//! and icons) or in the bundle (screenshots), so one signature covers the whole catalog.

use crate::model::{CATEGORIES, Entry, GpuUse, HttpsNeed};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Index {
    pub format: u32,
    /// Unix time the catalog was built. Hearth refuses a catalog older than the one it has.
    pub generated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub source: String,
    pub categories: Vec<Category>,
    pub apps: Vec<IndexApp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexApp {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub app_name: String,
    /// Path of the icon in the catalog, and its sha256.
    pub icon: String,
    pub icon_sha256: String,
    /// Path of the bundle in the catalog, and its sha256.
    pub bundle: String,
    pub sha256: String,
    pub badges: Badges,
}

/// What the store cards and entry pages show at a glance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Badges {
    pub ram_mb: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_recommended_mb: Option<u64>,
    pub data_mb: u64,
    /// Download size of the default install's images, per architecture (MB, compressed).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub download_mb: BTreeMap<String, u64>,
    pub architectures: Vec<String>,
    pub gpu: GpuUse,
    pub https: HttpsNeed,
    pub host_network: bool,
    pub notice: bool,
    pub screenshots: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bundle {
    pub entry: Entry,
    pub compose: String,
    #[serde(default)]
    pub variants: BTreeMap<String, String>,
    /// Screenshot path → sha256 (files are at `files/<id>/<path>`).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Registry facts about every image the entry can use (filled by CI).
    #[serde(default)]
    pub images: Vec<ImageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageInfo {
    pub image: String,
    pub platforms: Vec<String>,
    /// Compressed download size per architecture, MB.
    #[serde(default)]
    pub size_mb: BTreeMap<String, u64>,
    /// In the default install (base compose, default options)?
    #[serde(default)]
    pub default: bool,
}

/// One entry as found in the store repository.
#[derive(Debug, Clone)]
pub struct Source {
    pub dir: PathBuf,
    pub entry: Entry,
    pub compose: String,
    pub variants: BTreeMap<String, String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Read every `apps/<id>/` folder of the store repository.
pub fn load_repo(root: &Path) -> Result<Vec<Source>, Vec<String>> {
    let apps = root.join("apps");
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&apps) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => return Err(vec![format!("{}: {e}", apps.display())]),
    };
    dirs.sort();
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).map_err(|e| format!("{name}/{f}: {e}"));
        let entry: Entry = match read("app.yaml")
            .and_then(|t| serde_yaml_ng::from_str(&t).map_err(|e| format!("{name}/app.yaml: {e}")))
        {
            Ok(e) => e,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let compose = match read("compose.yaml") {
            Ok(c) => c,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let mut variants = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("variants")) {
            for f in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
                if let (Some(stem), Some("yaml")) = (
                    f.file_stem().and_then(|s| s.to_str()),
                    f.extension().and_then(|s| s.to_str()),
                ) {
                    match std::fs::read_to_string(&f) {
                        Ok(t) => {
                            variants.insert(stem.to_string(), t);
                        }
                        Err(e) => errors.push(format!("{name}/variants/{stem}.yaml: {e}")),
                    }
                }
            }
        }
        out.push(Source {
            dir,
            entry,
            compose,
            variants,
        });
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

fn ext_of(p: &str) -> &str {
    p.rsplit('.').next().unwrap_or("")
}

/// Write the catalog to `out`. `images` holds registry facts per entry id (empty when not
/// checked). Returns the index (not yet signed).
pub fn build(
    sources: &[Source],
    images: &BTreeMap<String, Vec<ImageInfo>>,
    out: &Path,
    generated: u64,
    commit: Option<String>,
    source_url: &str,
) -> std::io::Result<Index> {
    std::fs::create_dir_all(out.join("apps"))?;
    std::fs::create_dir_all(out.join("icons"))?;
    let mut apps = Vec::new();
    for s in sources {
        let e = &s.entry;
        let icon_bytes = std::fs::read(s.dir.join(&e.icon))?;
        let icon_path = format!("icons/{}.{}", e.id, ext_of(&e.icon));
        std::fs::write(out.join(&icon_path), &icon_bytes)?;
        let mut files = BTreeMap::new();
        for shot in &e.screenshots {
            let bytes = std::fs::read(s.dir.join(&shot.file))?;
            let dest = out.join("files").join(&e.id).join(&shot.file);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, &bytes)?;
            files.insert(shot.file.clone(), sha256_hex(&bytes));
        }
        let imgs = images.get(&e.id).cloned().unwrap_or_default();
        let mut download_mb: BTreeMap<String, u64> = BTreeMap::new();
        for i in imgs.iter().filter(|i| i.default) {
            for (arch, mb) in &i.size_mb {
                *download_mb.entry(arch.clone()).or_default() += mb;
            }
        }
        let bundle = Bundle {
            entry: e.clone(),
            compose: s.compose.clone(),
            variants: s.variants.clone(),
            files,
            images: imgs,
        };
        let bundle_bytes = serde_json::to_vec(&bundle)?;
        let bundle_path = format!("apps/{}.json", e.id);
        std::fs::write(out.join(&bundle_path), &bundle_bytes)?;
        apps.push(IndexApp {
            id: e.id.clone(),
            name: e.name.clone(),
            summary: e.summary.clone(),
            category: e.category.clone(),
            tags: e.tags.clone(),
            app_name: e.app_name.clone(),
            icon: icon_path,
            icon_sha256: sha256_hex(&icon_bytes),
            bundle: bundle_path,
            sha256: sha256_hex(&bundle_bytes),
            badges: Badges {
                ram_mb: e.requirements.ram_mb,
                ram_recommended_mb: e.requirements.ram_recommended_mb,
                data_mb: e.requirements.data_mb,
                download_mb,
                architectures: e.requirements.architectures.clone(),
                gpu: e.requirements.gpu,
                https: e.web.as_ref().map(|w| w.https).unwrap_or_default(),
                host_network: e.requirements.host_network,
                notice: e.notice.is_some(),
                screenshots: e.screenshots.len(),
            },
        });
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    let index = Index {
        format: FORMAT,
        generated,
        commit,
        source: source_url.to_string(),
        categories: CATEGORIES
            .iter()
            .filter(|(id, _)| apps.iter().any(|a| a.category == *id))
            .map(|(id, name)| Category {
                id: id.to_string(),
                name: name.to_string(),
            })
            .collect(),
        apps,
    };
    std::fs::write(out.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
    Ok(index)
}

/// Check `index.json` against its minisign signature with any of the trusted public keys
/// (base64 minisign public keys, the second line of a `.pub` file), then parse it.
pub fn verify_index(index: &[u8], signature: &str, trusted: &[&str]) -> Result<Index, String> {
    let sig = minisign_verify::Signature::decode(signature)
        .map_err(|e| format!("the catalog signature is unreadable: {e}"))?;
    let mut ok = false;
    for key in trusted {
        if let Ok(pk) = minisign_verify::PublicKey::from_base64(key.trim())
            && pk.verify(index, &sig, false).is_ok()
        {
            ok = true;
            break;
        }
    }
    if !ok {
        return Err("the catalog's signature doesn't match any trusted store key: refusing it".into());
    }
    let idx: Index =
        serde_json::from_slice(index).map_err(|e| format!("the signed catalog is unreadable: {e}"))?;
    if idx.format != FORMAT {
        return Err(format!(
            "the catalog uses format {} and this Hearth reads format {FORMAT}: update Hearth",
            idx.format
        ));
    }
    Ok(idx)
}

/// Parse a bundle after checking it against the hash from the signed index.
pub fn verify_bundle(bytes: &[u8], app: &IndexApp) -> Result<Bundle, String> {
    if sha256_hex(bytes) != app.sha256 {
        return Err(format!(
            "{}: the download doesn't match the signed catalog",
            app.id
        ));
    }
    let b: Bundle = serde_json::from_slice(bytes).map_err(|e| format!("{}: {e}", app.id))?;
    if b.entry.id != app.id {
        return Err(format!("{}: bundle is for '{}'", app.id, b.entry.id));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Signed with a throwaway test key (password "test"), not a store key.
    const PK: &str = "RWQQK7PgQ9Jp/fMFkHovT+/O5InGXDZ7Msod1SH2y7kBjaiR+3FEHD7y";
    const INDEX: &str = r#"{"format":1,"generated":1795700000,"source":"test","categories":[],"apps":[]}"#;
    const SIG: &str = "untrusted comment: hearth store
RUQQK7PgQ9Jp/Tw4Zd+pPuUi6Y8luW/Rk+1hLWEyxjO5GbD8FasLpp6B9CQnmlogw4yz5lLFiCfu7XZiPwaoeMhzZW/kb/uxCQA=
trusted comment: timestamp:1795703007 file:index.json
ntC5/tAOw7wKekS5hQQZoHo4JqPFFzWhLk0QODa0K4wyC3Twu385GPzTyhNBdV8FaWcf7cj0OEAXJnMbEk9qCg==
";

    #[test]
    fn signed_index_is_accepted() {
        let idx = verify_index(INDEX.as_bytes(), SIG, &[crate::TRUSTED_KEYS[0], PK]).expect("valid");
        assert_eq!(idx.generated, 1_795_700_000);
    }

    #[test]
    fn changed_index_or_wrong_key_is_refused() {
        let changed = INDEX.replace("1795700000", "1795700001");
        assert!(verify_index(changed.as_bytes(), SIG, &[PK]).is_err());
        assert!(verify_index(INDEX.as_bytes(), SIG, crate::TRUSTED_KEYS).is_err());
        assert!(verify_index(INDEX.as_bytes(), "garbage", &[PK]).is_err());
    }
}
