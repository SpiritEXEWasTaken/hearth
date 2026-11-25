//! Registry checks: what each running image's tag points to now, its version, and newer tags.
//!
//! Results are cached in SQLite. Requests are kept low for Docker Hub's limits: one `HEAD` per
//! image per check; manifest/config fetches only for digests not seen before; tag lists at
//! most once a day per repository, and only for versioned tags.

use super::State;
use super::apps::{self, check_key};
use super::db::{ImageCheck, now};
use hearth_registry::{ImageRef, Platform, TagTrack, track};
use std::time::Duration;

const TAG_LIST_MAX_AGE: i64 = 24 * 3600;

/// Checks every image used by the named app (or all apps), sequentially.
/// Returns the number of references checked.
pub async fn check(state: &State, app: Option<&str>) -> Result<usize, String> {
    let _guard = state.check_lock.lock().await;
    let (apps, _) = apps::list(state).await;
    let mut refs: Vec<String> = Vec::new();
    let mut found = app.is_none();
    for a in &apps {
        if let Some(want) = app
            && a.summary.id != want
        {
            continue;
        }
        found = true;
        for img in &a.summary.images {
            if !refs.contains(&img.reference) {
                refs.push(img.reference.clone());
            }
        }
    }
    if !found {
        return Err(format!("app not found: {}", app.unwrap_or_default()));
    }
    for (i, r) in refs.iter().enumerate() {
        if i > 0 {
            // Be polite to registries.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        check_reference(state, r).await;
    }
    Ok(refs.len())
}

async fn check_reference(state: &State, reference: &str) {
    let Some(key) = check_key(reference) else {
        tracing::debug!(reference, "not a registry reference, skipping");
        return;
    };
    let mut result = ImageCheck {
        reference: key.clone(),
        checked_at: now(),
        ..Default::default()
    };
    match ImageRef::parse(reference) {
        Err(e) => result.error = Some(e.to_string()),
        Ok(r) => {
            let r = r.with_tag(&r.tag);
            match state.registry.tag_digest(&r).await {
                Err(e) => result.error = Some(e.to_string()),
                Ok(digest) => {
                    result.version = version_for(state, &r, &digest).await;
                    result.digest = Some(digest);
                    if TagTrack::parse(&r.tag).is_some() {
                        match tags_for(state, &r).await {
                            Ok(tags) => {
                                let rep = track::analyse(&r.tag, &tags);
                                result.newer_in_track = rep.newer_in_track;
                                result.newer_major = rep.newer_major;
                            }
                            Err(e) => result.error = Some(format!("tag list: {e}")),
                        }
                    }
                }
            }
        }
    }
    if let Some(e) = &result.error {
        tracing::info!(reference = %key, "registry check failed: {e}");
    }
    state.with_db(|db| {
        if let Err(e) = db.save_image_check(&result) {
            tracing::warn!("cannot save image check: {e}");
        }
    });
}

pub async fn version_for(state: &State, r: &ImageRef, digest: &str) -> Option<String> {
    if let Some(cached) = state.with_db(|db| db.digest_version(digest).ok().flatten()) {
        return cached;
    }
    let version = match state.registry.describe(r, &Platform::host()).await {
        Ok(remote) => remote.version,
        Err(e) => {
            tracing::info!(reference = %r, "cannot read image config: {e}");
            return None; // don't cache failures
        }
    };
    state.with_db(|db| db.save_digest_version(digest, version.as_deref()).ok());
    version
}

async fn tags_for(state: &State, r: &ImageRef) -> Result<Vec<String>, String> {
    let name = r.name();
    if let Some(t) = state.with_db(|db| db.tag_list(&name, TAG_LIST_MAX_AGE).ok().flatten()) {
        return Ok(t);
    }
    let tags = state.registry.list_tags(r).await.map_err(|e| e.to_string())?;
    state.with_db(|db| db.save_tag_list(&name, &tags).ok());
    Ok(tags)
}

/// Background loop: check all images every `interval`, starting after `initial_delay`.
pub async fn scheduler(state: State, initial_delay: Duration, interval: Duration) {
    tokio::time::sleep(initial_delay).await;
    loop {
        match check(&state, None).await {
            Ok(n) => tracing::info!("scheduled version check done ({n} images)"),
            Err(e) => tracing::warn!("scheduled version check failed: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}
