//! The daemon's SQLite state: API tokens and the registry check cache.

use hearth_core::model::TokenInfo;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Db {
    conn: Connection,
}

/// Cached result of a registry check for one image reference.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageCheck {
    pub reference: String,
    pub digest: Option<String>,
    pub version: Option<String>,
    pub newer_in_track: Option<String>,
    pub newer_major: Option<String>,
    pub error: Option<String>,
    pub checked_at: i64,
}

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "CREATE TABLE tokens (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        hash TEXT NOT NULL UNIQUE,
        scopes TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER
     );
     CREATE TABLE image_checks (
        reference TEXT PRIMARY KEY,
        digest TEXT,
        version TEXT,
        newer_in_track TEXT,
        newer_major TEXT,
        error TEXT,
        checked_at INTEGER NOT NULL
     );
     CREATE TABLE digest_versions (
        digest TEXT PRIMARY KEY,
        version TEXT
     );
     CREATE TABLE tag_lists (
        repository TEXT PRIMARY KEY,
        tags TEXT NOT NULL,
        fetched_at INTEGER NOT NULL
     );",
];

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Keep SQLite's page cache small: this runs on low-RAM servers.
        conn.pragma_update(None, "cache_size", -512)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", (i + 1) as i64)?;
        }
        Ok(Db { conn })
    }

    // ----- tokens -----

    pub fn create_token(&self, id: &str, name: &str, token: &str, scopes: &[String]) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO tokens (id, name, hash, scopes, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, hash_token(token), scopes.join(" "), now()],
        )?;
        Ok(())
    }

    /// Returns the token's scopes if it exists, and records its use.
    pub fn check_token(&self, token: &str) -> rusqlite::Result<Option<(String, Vec<String>)>> {
        let hash = hash_token(token);
        let found: Option<(String, String)> = self
            .conn
            .query_row("SELECT id, scopes FROM tokens WHERE hash = ?1", [&hash], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((id, _)) = &found {
            self.conn.execute(
                "UPDATE tokens SET last_used_at = ?1 WHERE id = ?2",
                params![now(), id],
            )?;
        }
        Ok(found.map(|(id, s)| (id, s.split_whitespace().map(str::to_string).collect())))
    }

    pub fn list_tokens(&self) -> rusqlite::Result<Vec<TokenInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, scopes, created_at, last_used_at FROM tokens ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            let scopes: String = r.get(2)?;
            Ok(TokenInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                scopes: scopes.split_whitespace().map(str::to_string).collect(),
                created_at: r.get(3)?,
                last_used_at: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn revoke_token(&self, id: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.execute("DELETE FROM tokens WHERE id = ?1", [id])? > 0)
    }

    // ----- registry cache -----

    pub fn image_check(&self, reference: &str) -> rusqlite::Result<Option<ImageCheck>> {
        self.conn
            .query_row(
                "SELECT reference, digest, version, newer_in_track, newer_major, error, checked_at
                 FROM image_checks WHERE reference = ?1",
                [reference],
                |r| {
                    Ok(ImageCheck {
                        reference: r.get(0)?,
                        digest: r.get(1)?,
                        version: r.get(2)?,
                        newer_in_track: r.get(3)?,
                        newer_major: r.get(4)?,
                        error: r.get(5)?,
                        checked_at: r.get(6)?,
                    })
                },
            )
            .optional()
    }

    pub fn save_image_check(&self, c: &ImageCheck) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO image_checks
             (reference, digest, version, newer_in_track, newer_major, error, checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                c.reference,
                c.digest,
                c.version,
                c.newer_in_track,
                c.newer_major,
                c.error,
                c.checked_at
            ],
        )?;
        Ok(())
    }

    /// `Some(version)` if the digest was described before (the version itself may be unknown).
    pub fn digest_version(&self, digest: &str) -> rusqlite::Result<Option<Option<String>>> {
        self.conn
            .query_row(
                "SELECT version FROM digest_versions WHERE digest = ?1",
                [digest],
                |r| r.get(0),
            )
            .optional()
    }

    pub fn save_digest_version(&self, digest: &str, version: Option<&str>) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO digest_versions (digest, version) VALUES (?1, ?2)",
            params![digest, version],
        )?;
        Ok(())
    }

    pub fn tag_list(&self, repository: &str, max_age_secs: i64) -> rusqlite::Result<Option<Vec<String>>> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT tags, fetched_at FROM tag_lists WHERE repository = ?1",
                [repository],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row
            .filter(|(_, at)| now() - at <= max_age_secs)
            .and_then(|(t, _)| serde_json::from_str(&t).ok()))
    }

    pub fn save_tag_list(&self, repository: &str, tags: &[String]) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tag_lists (repository, tags, fetched_at) VALUES (?1, ?2, ?3)",
            params![
                repository,
                serde_json::to_string(tags).unwrap_or_else(|_| "[]".into()),
                now()
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        match Db::open_in_memory() {
            Ok(d) => d,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn tokens_are_stored_hashed_and_revocable() {
        let db = db();
        let scopes = vec!["read".to_string(), "apps:write".to_string()];
        assert!(db.create_token("abc123", "claude", "hth_secret", &scopes).is_ok());
        let found = db.check_token("hth_secret").ok().flatten();
        assert_eq!(found, Some(("abc123".to_string(), scopes)));
        assert_eq!(db.check_token("hth_wrong").ok().flatten(), None);

        // The raw token is never stored.
        let raw: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tokens WHERE hash LIKE '%secret%'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        assert_eq!(raw, 0);

        let list = db.list_tokens().unwrap_or_default();
        assert_eq!(list.len(), 1);
        assert!(list[0].last_used_at.is_some());
        assert_eq!(db.revoke_token("abc123").ok(), Some(true));
        assert_eq!(db.check_token("hth_secret").ok().flatten(), None);
    }

    #[test]
    fn image_check_roundtrip_and_tag_cache_expiry() {
        let db = db();
        let c = ImageCheck {
            reference: "docker.io/linuxserver/jellyfin:latest".into(),
            digest: Some("sha256:abc".into()),
            version: Some("10.11.2".into()),
            checked_at: 42,
            ..Default::default()
        };
        assert!(db.save_image_check(&c).is_ok());
        assert_eq!(db.image_check(&c.reference).ok().flatten(), Some(c));

        assert!(
            db.save_tag_list("docker.io/library/nginx", &["1".into(), "2".into()])
                .is_ok()
        );
        assert_eq!(
            db.tag_list("docker.io/library/nginx", 60)
                .ok()
                .flatten()
                .map(|t| t.len()),
            Some(2)
        );
        assert_eq!(
            db.tag_list("docker.io/library/nginx", -1).ok().flatten(),
            None,
            "expired"
        );
    }

    #[test]
    fn migrations_are_idempotent() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        let p = tmp.path().join("h.db");
        assert!(Db::open(&p).is_ok());
        assert!(Db::open(&p).is_ok(), "re-opening must not re-run migrations");
    }
}
