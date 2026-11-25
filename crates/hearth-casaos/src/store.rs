//! CasaOS app store layouts.
//!
//! - **v1 source** (CasaOS official before 2026, Big Bear): `Apps/<Folder>/docker-compose.yml`,
//!   optionally inside one top-level folder (how GitHub zip downloads are extracted, e.g.
//!   `big-bear-casaos-master/Apps/...`).
//! - **v2 build output**: `index.json` plus `apps/<id>/docker-compose.yml`.
//!
//! Reading is tolerant: one broken template is reported in `errors` and skipped.

use crate::CasaosError;
use crate::compose::ComposeFile;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreLayout {
    V1Source,
    V2Dist,
}

#[derive(Debug, Clone)]
pub struct StoreTemplate {
    /// Folder name under `Apps/` (v1) or `apps/` (v2).
    pub folder: String,
    pub path: PathBuf,
    pub compose: ComposeFile,
}

#[derive(Debug, Clone)]
pub struct StoreRead {
    pub layout: StoreLayout,
    /// The directory that actually holds `Apps/` or `index.json`.
    pub root: PathBuf,
    /// `store_id` from `store-config.json`, if present.
    pub store_id: Option<String>,
    pub templates: Vec<StoreTemplate>,
    /// One message per template that couldn't be read.
    pub errors: Vec<String>,
}

/// Reads a store from an extracted directory.
pub fn read_store(dir: &Path) -> Result<StoreRead, CasaosError> {
    let (root, layout) = locate(dir).ok_or_else(|| CasaosError::Invalid {
        path: dir.display().to_string(),
        message: "no Apps/ directory or index.json found".into(),
    })?;
    let apps_dir = match layout {
        StoreLayout::V1Source => root.join("Apps"),
        StoreLayout::V2Dist => root.join("apps"),
    };
    let store_id = fs::read_to_string(root.join("store-config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("store_id").and_then(|s| s.as_str()).map(str::to_string));

    let mut entries: Vec<PathBuf> = fs::read_dir(&apps_dir)
        .map_err(|source| CasaosError::Io {
            path: apps_dir.display().to_string(),
            source,
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();

    let mut templates = Vec::new();
    let mut errors = Vec::new();
    for app_dir in entries {
        let folder = app_dir
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = app_dir.join("docker-compose.yml");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match ComposeFile::parse(&text, &path.display().to_string()) {
            Ok(compose) => templates.push(StoreTemplate {
                folder,
                path,
                compose,
            }),
            Err(e) => errors.push(e.to_string()),
        }
    }
    Ok(StoreRead {
        layout,
        root,
        store_id,
        templates,
        errors,
    })
}

fn locate(dir: &Path) -> Option<(PathBuf, StoreLayout)> {
    let check = |d: &Path| -> Option<StoreLayout> {
        if d.join("Apps").is_dir() {
            Some(StoreLayout::V1Source)
        } else if d.join("index.json").is_file() && d.join("apps").is_dir() {
            Some(StoreLayout::V2Dist)
        } else {
            None
        }
    };
    if let Some(l) = check(dir) {
        return Some((dir.to_path_buf(), l));
    }
    // One level down (extracted zips have a single top folder).
    let subdirs: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    subdirs.into_iter().find_map(|d| check(&d).map(|l| (d, l)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(p, s) {
            panic!("write {}: {e}", p.display());
        }
    }

    #[test]
    fn v1_nested_like_a_github_zip() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        let root = tmp.path().join("big-bear-casaos-master");
        write(
            &root.join("store-config.json"),
            r#"{"version":2,"store_id":"com.example.store"}"#,
        );
        write(
            &root.join("Apps/jellyfin/docker-compose.yml"),
            "name: j\nservices:\n  app:\n    image: linuxserver/jellyfin:10.8.12\nx-casaos:\n  main: app\n  title:\n    en_US: Jellyfin\n",
        );
        write(&root.join("Apps/broken/docker-compose.yml"), "services: [");
        let r = match read_store(tmp.path()) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(r.layout, StoreLayout::V1Source);
        assert_eq!(r.store_id.as_deref(), Some("com.example.store"));
        assert_eq!(r.templates.len(), 1);
        assert_eq!(r.templates[0].compose.title().as_deref(), Some("Jellyfin"));
        assert_eq!(r.errors.len(), 1, "broken template is reported, not fatal");
    }

    #[test]
    fn v2_dist() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        write(&tmp.path().join("index.json"), "[]");
        write(
            &tmp.path().join("apps/com.example.app/docker-compose.yml"),
            "name: app\nservices:\n  app:\n    image: nginx\nx-casaos:\n  id: com.example.app\n  title: App\n",
        );
        let r = match read_store(tmp.path()) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(r.layout, StoreLayout::V2Dist);
        assert_eq!(
            r.templates[0].compose.x_casaos().and_then(|x| x.id).as_deref(),
            Some("com.example.app")
        );
    }

    #[test]
    fn not_a_store() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        assert!(read_store(tmp.path()).is_err());
    }
}
