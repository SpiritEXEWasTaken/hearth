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
    // 2: jobs (audit log), desired app state, settings
    "CREATE TABLE jobs (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        app TEXT NOT NULL,
        actor TEXT NOT NULL,
        status TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        finished_at INTEGER,
        error TEXT,
        events TEXT NOT NULL
     );
     CREATE INDEX jobs_app ON jobs (app, started_at);
     CREATE TABLE app_state (
        app TEXT PRIMARY KEY,
        desired TEXT NOT NULL,
        updated_at INTEGER NOT NULL
     );
     CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );",
    // 3: CasaOS migration log
    "CREATE TABLE migrations (
        app TEXT PRIMARY KEY,
        status TEXT NOT NULL,
        fixes TEXT NOT NULL,
        updated_at INTEGER NOT NULL
     );",
    // 4: web UI accounts and sessions
    "CREATE TABLE users (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL UNIQUE COLLATE NOCASE,
        hash TEXT NOT NULL,
        role TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_login_at INTEGER
     );
     CREATE TABLE sessions (
        hash TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        csrf TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL,
        ip TEXT,
        user_agent TEXT
     );",
    // 5: HTTPS ports for apps (Hearth proxies https://<server>:<port> to the app's own port)
    "CREATE TABLE app_https (
        app TEXT PRIMARY KEY,
        port INTEGER NOT NULL UNIQUE,
        target_port INTEGER NOT NULL,
        created_at INTEGER NOT NULL
     );",
];

/// An app served over HTTPS by Hearth.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppHttps {
    pub app: String,
    /// The HTTPS port Hearth listens on.
    pub port: u16,
    /// The app's own (plain HTTP) port on this machine.
    pub target_port: u16,
}

/// A web UI account.
#[derive(Debug, Clone, serde::Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    /// "admin" or "viewer".
    pub role: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

/// A signed-in browser session, looked up by its cookie.
#[derive(Debug, Clone)]
pub struct Session {
    pub user: User,
    pub csrf: String,
    pub last_seen_at: i64,
}

/// Settings with their defaults.
pub const SETTINGS: &[(&str, &str, &str)] = &[
    (
        "image_retention",
        "1",
        "rollback images kept per app service after a successful update",
    ),
    ("backup_retention", "5", "backups kept per app"),
    (
        "health_timeout",
        "300",
        "seconds an app may take to become healthy after a change",
    ),
    (
        "security_updates",
        "off",
        "install security updates every day at HH:MM (local time), or off; Hearth never reboots",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desired {
    Running,
    Stopped,
}

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

    // ----- users and sessions -----

    pub fn count_users(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
    }

    pub fn count_admins(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| {
                r.get(0)
            })
    }

    pub fn create_user(&self, id: &str, username: &str, hash: &str, role: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, username, hash, role, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, username, hash, role, now()],
        )?;
        Ok(())
    }

    fn user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
        Ok(User {
            id: r.get(0)?,
            username: r.get(1)?,
            role: r.get(2)?,
            created_at: r.get(3)?,
            last_login_at: r.get(4)?,
        })
    }

    pub fn list_users(&self) -> rusqlite::Result<Vec<User>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, username, role, created_at, last_login_at FROM users ORDER BY created_at")?;
        let rows = stmt.query_map([], Self::user_row)?;
        rows.collect()
    }

    pub fn get_user(&self, id_or_name: &str) -> rusqlite::Result<Option<User>> {
        self.conn
            .query_row(
                "SELECT id, username, role, created_at, last_login_at FROM users
                 WHERE id = ?1 OR username = ?1",
                [id_or_name],
                Self::user_row,
            )
            .optional()
    }

    /// The user and their password hash, by username.
    pub fn user_with_hash(&self, username: &str) -> rusqlite::Result<Option<(User, String)>> {
        self.conn
            .query_row(
                "SELECT id, username, role, created_at, last_login_at, hash FROM users WHERE username = ?1",
                [username],
                |r| Ok((Self::user_row(r)?, r.get(5)?)),
            )
            .optional()
    }

    pub fn password_hash(&self, user_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT hash FROM users WHERE id = ?1", [user_id], |r| r.get(0))
            .optional()
    }

    pub fn set_password(&self, user_id: &str, hash: &str) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .execute("UPDATE users SET hash = ?1 WHERE id = ?2", params![hash, user_id])?
            > 0)
    }

    pub fn set_role(&self, user_id: &str, role: &str) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .execute("UPDATE users SET role = ?1 WHERE id = ?2", params![role, user_id])?
            > 0)
    }

    pub fn delete_user(&self, user_id: &str) -> rusqlite::Result<bool> {
        self.conn
            .execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        Ok(self.conn.execute("DELETE FROM users WHERE id = ?1", [user_id])? > 0)
    }

    pub fn record_login(&self, user_id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE users SET last_login_at = ?1 WHERE id = ?2",
            params![now(), user_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session(
        &self,
        token: &str,
        user_id: &str,
        csrf: &str,
        expires_at: i64,
        ip: Option<&str>,
        user_agent: Option<&str>,
    ) -> rusqlite::Result<()> {
        let t = now();
        self.conn.execute(
            "INSERT INTO sessions (hash, user_id, csrf, created_at, expires_at, last_seen_at, ip, user_agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6, ?7)",
            params![hash_token(token), user_id, csrf, t, expires_at, ip, user_agent],
        )?;
        Ok(())
    }

    /// The session for a cookie value, if it exists and hasn't expired.
    pub fn session(&self, token: &str) -> rusqlite::Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT u.id, u.username, u.role, u.created_at, u.last_login_at, s.csrf, s.last_seen_at
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.hash = ?1 AND s.expires_at > ?2",
                params![hash_token(token), now()],
                |r| {
                    Ok(Session {
                        user: Self::user_row(r)?,
                        csrf: r.get(5)?,
                        last_seen_at: r.get(6)?,
                    })
                },
            )
            .optional()
    }

    /// Slides the session's expiry forward.
    pub fn touch_session(&self, token: &str, expires_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET last_seen_at = ?1, expires_at = ?2 WHERE hash = ?3",
            params![now(), expires_at, hash_token(token)],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, token: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE hash = ?1", [hash_token(token)])?;
        Ok(())
    }

    /// Signs a user out everywhere, except (optionally) the session making the change.
    pub fn delete_user_sessions(&self, user_id: &str, keep_token: Option<&str>) -> rusqlite::Result<()> {
        let keep = keep_token.map(hash_token).unwrap_or_default();
        self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND hash != ?2",
            params![user_id, keep],
        )?;
        Ok(())
    }

    pub fn purge_expired_sessions(&self) -> rusqlite::Result<usize> {
        self.conn
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", [now()])
    }

    // ----- app HTTPS ports -----

    pub fn list_app_https(&self) -> rusqlite::Result<Vec<AppHttps>> {
        let mut stmt = self
            .conn
            .prepare("SELECT app, port, target_port FROM app_https ORDER BY port")?;
        let rows = stmt.query_map([], |r| {
            Ok(AppHttps {
                app: r.get(0)?,
                port: r.get(1)?,
                target_port: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_app_https(&self, h: &AppHttps) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO app_https (app, port, target_port, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(app) DO UPDATE SET port = ?2, target_port = ?3",
            params![h.app, h.port, h.target_port, now()],
        )?;
        Ok(())
    }

    pub fn remove_app_https(&self, app: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.execute("DELETE FROM app_https WHERE app = ?1", [app])? > 0)
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

impl Db {
    // ----- jobs -----

    pub fn save_job(&self, job: &hearth_core::model::Job, actor: &str) -> rusqlite::Result<()> {
        let status = serde_json::to_value(job.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        self.conn.execute(
            "INSERT OR REPLACE INTO jobs (id, kind, app, actor, status, started_at, finished_at, error, events)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                job.id,
                job.kind,
                job.app,
                actor,
                status,
                job.started_at,
                job.finished_at,
                job.error,
                serde_json::to_string(&job.events).unwrap_or_default()
            ],
        )?;
        // Keep the audit log bounded.
        self.conn.execute(
            "DELETE FROM jobs WHERE id NOT IN (SELECT id FROM jobs ORDER BY started_at DESC LIMIT 1000)",
            [],
        )?;
        Ok(())
    }

    // ----- desired state -----

    pub fn desired(&self, app: &str) -> rusqlite::Result<Option<Desired>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT desired FROM app_state WHERE app = ?1", [app], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.map(|s| {
            if s == "stopped" {
                Desired::Stopped
            } else {
                Desired::Running
            }
        }))
    }

    pub fn set_desired(&self, app: &str, d: Desired) -> rusqlite::Result<()> {
        let v = match d {
            Desired::Running => "running",
            Desired::Stopped => "stopped",
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO app_state (app, desired, updated_at) VALUES (?1, ?2, ?3)",
            params![app, v, now()],
        )?;
        Ok(())
    }

    pub fn forget_app(&self, app: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM app_state WHERE app = ?1", [app])?;
        Ok(())
    }

    // ----- migration log -----

    pub fn record_migration(&self, app: &str, status: &str, fixes: &[String]) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO migrations (app, status, fixes, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![app, status, fixes.join(" "), now()],
        )?;
        Ok(())
    }

    // ----- settings -----

    pub fn setting(&self, key: &str) -> String {
        let stored: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten();
        stored.unwrap_or_else(|| {
            SETTINGS
                .iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, d, _)| d.to_string())
                .unwrap_or_default()
        })
    }

    pub fn setting_u64(&self, key: &str) -> u64 {
        self.setting(key).parse().unwrap_or(0)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        if !SETTINGS.iter().any(|(k, _, _)| *k == key) {
            return Err(format!(
                "unknown setting '{key}' (known: {})",
                SETTINGS.iter().map(|(k, _, _)| *k).collect::<Vec<_>>().join(", ")
            ));
        }
        if key == "security_updates" {
            if !super::system::valid_schedule(value) {
                return Err("security_updates must be 'off' or a time like 04:17".into());
            }
        } else if value.parse::<u64>().is_err() {
            return Err(format!("{key} must be a whole number"));
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|e| e.to_string())?;
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

    #[test]
    fn desired_state_and_settings() {
        let db = db();
        assert_eq!(db.desired("x").ok().flatten(), None);
        assert!(db.set_desired("x", Desired::Stopped).is_ok());
        assert_eq!(db.desired("x").ok().flatten(), Some(Desired::Stopped));
        assert_eq!(db.setting_u64("image_retention"), 1, "default");
        assert!(db.set_setting("image_retention", "3").is_ok());
        assert_eq!(db.setting_u64("image_retention"), 3);
        assert!(db.set_setting("nope", "1").is_err());
        assert!(db.set_setting("backup_retention", "many").is_err());
    }
}
