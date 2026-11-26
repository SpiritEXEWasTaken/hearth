//! Web UI accounts: first-run setup, sign-in, sessions and user management.
//!
//! - Passwords are hashed with argon2id (one hash at a time, so a burst of logins can't use up the
//!   daemon's memory).
//! - Sessions are a random cookie (`__Host-hearth`: Secure, HttpOnly, SameSite=Strict); only its
//!   SHA-256 is stored. Each session also has a CSRF token that changes need in a header.
//! - Failed sign-ins are slowed down per address and per username.
//! - The first admin can only be created with a one-time setup code that is shown on the server
//!   (`hearth web setup-code`, and the journal), so nobody else on the network can claim it.

use super::State;
use super::api::{ApiError, random_hex};
use super::auth::{Peer, PeerInfo, Principal, WebUser};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{Path, State as AxState};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hearth_core::Error;
use hearth_core::model::scopes;
use hearth_core::output::Envelope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const COOKIE: &str = "__Host-hearth";
/// Sessions last this long after the last use.
const SESSION_SECS: i64 = 14 * 24 * 3600;
/// How often the expiry is pushed forward (avoids a database write on every request).
const TOUCH_EVERY_SECS: i64 = 600;
const FREE_FAILURES: u32 = 5;
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);

pub struct WebAuth {
    setup_code: Mutex<Option<String>>,
    setup_code_file: PathBuf,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    hashing: tokio::sync::Semaphore,
}

impl WebAuth {
    pub fn new(state_dir: &std::path::Path) -> Self {
        WebAuth {
            setup_code: Mutex::new(None),
            setup_code_file: state_dir.join("setup-code"),
            failures: Mutex::new(HashMap::new()),
            hashing: tokio::sync::Semaphore::new(1),
        }
    }

    /// Makes a setup code when there's no account yet (and removes a stale one otherwise).
    pub fn init(&self, users: i64) {
        let mut code = self.setup_code.lock().unwrap_or_else(|p| p.into_inner());
        if users > 0 {
            *code = None;
            let _ = std::fs::remove_file(&self.setup_code_file);
            return;
        }
        let new = new_setup_code();
        if let Err(e) = write_private(&self.setup_code_file, &new) {
            tracing::warn!("couldn't save the setup code: {e}");
        }
        tracing::warn!(
            "no Hearth account yet: open the web UI and enter setup code {new} (also shown by `hearth web setup-code`)"
        );
        *code = Some(new);
    }

    fn current_code(&self) -> Option<String> {
        self.setup_code.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn clear_code(&self) {
        *self.setup_code.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let _ = std::fs::remove_file(&self.setup_code_file);
    }

    /// How long until this key may try again, if it's locked out.
    fn locked_for(&self, key: &str) -> Option<Duration> {
        let f = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let (n, last) = f.get(key)?;
        if *n < FREE_FAILURES {
            return None;
        }
        let wait = lock_duration(*n);
        let passed = last.elapsed();
        (passed < wait).then(|| wait - passed)
    }

    fn fail(&self, key: &str) {
        let mut f = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        if f.len() > 10_000 {
            f.retain(|_, (_, t)| t.elapsed() < MAX_LOCK);
        }
        let e = f.entry(key.to_string()).or_insert((0, Instant::now()));
        e.0 += 1;
        e.1 = Instant::now();
    }

    fn succeed(&self, key: &str) {
        self.failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
    }

    async fn hash(&self, password: String) -> Result<String, Error> {
        let _permit = self
            .hashing
            .acquire()
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        tokio::task::spawn_blocking(move || hash_password(&password))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
    }

    async fn verify(&self, password: String, hash: String) -> bool {
        let Ok(_permit) = self.hashing.acquire().await else {
            return false;
        };
        tokio::task::spawn_blocking(move || verify_password(&password, &hash))
            .await
            .unwrap_or(false)
    }
}

fn lock_duration(failures: u32) -> Duration {
    let extra = failures.saturating_sub(FREE_FAILURES).min(10);
    Duration::from_secs(30u64 << extra).min(MAX_LOCK)
}

/// Eight characters, easy to read and type (no 0/O, 1/l/I).
fn new_setup_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    let s: String = b
        .iter()
        .map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &s[..4], &s[4..])
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o640)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")
}

pub fn hash_password(password: &str) -> Result<String, Error> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| Error::Other(format!("random: {e}")))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| Error::Other(e.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::Other(format!("password hash: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A hash of a random password, checked when the username doesn't exist so that a sign-in takes
/// the same time either way.
fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| hash_password(&random_hex(16).unwrap_or_default()).unwrap_or_default())
}

pub fn valid_username(name: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Usage(
            "usernames are 1–32 characters: letters, digits, '.', '_' or '-'".into(),
        ))
    }
}

pub fn valid_password(pw: &str) -> Result<(), Error> {
    let n = pw.chars().count();
    if n < 8 {
        return Err(Error::Usage("the password needs at least 8 characters".into()));
    }
    if n > 256 {
        return Err(Error::Usage(
            "the password can't be longer than 256 characters".into(),
        ));
    }
    Ok(())
}

fn valid_role(role: &str) -> Result<(), Error> {
    if role == "admin" || role == "viewer" {
        Ok(())
    } else {
        Err(Error::Usage("role must be 'admin' or 'viewer'".into()))
    }
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Other(format!("database: {e}"))
}

/// The session cookie's value, if the request has one.
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all(header::COOKIE) {
        let Ok(v) = v.to_str() else { continue };
        for part in v.split(';') {
            if let Some((k, val)) = part.trim().split_once('=')
                && k == COOKIE
                && !val.is_empty()
            {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// The signed-in user for a session cookie, pushing its expiry forward.
pub fn session_user(state: &State, token: &str) -> Option<WebUser> {
    state.with_db(|db| {
        let s = db.session(token).ok()??;
        let now = super::db::now();
        if now - s.last_seen_at > TOUCH_EVERY_SECS {
            let _ = db.touch_session(token, now + SESSION_SECS);
        }
        Some(WebUser {
            id: s.user.id,
            username: s.user.username,
            role: s.user.role,
            session_token: token.to_string(),
            csrf: s.csrf,
        })
    })
}

fn set_cookie(token: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

#[derive(Serialize)]
struct Me {
    user: super::db::User,
    csrf: String,
    scopes: Vec<String>,
}

fn envelope<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope::success(data))
}

/// Starts a session for `user` and returns the response with its cookie.
fn start_session(
    state: &State,
    user: super::db::User,
    peer: Peer,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let token = random_hex(32)?;
    let csrf = random_hex(16)?;
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(200).collect::<String>());
    let ip = peer.ip.map(|i| i.to_string());
    state
        .with_db(|db| {
            db.create_session(
                &token,
                &user.id,
                &csrf,
                super::db::now() + SESSION_SECS,
                ip.as_deref(),
                ua.as_deref(),
            )?;
            db.record_login(&user.id)?;
            db.purge_expired_sessions()
        })
        .map_err(db_err)?;
    let scopes = if user.role == "admin" {
        vec![scopes::ADMIN.to_string()]
    } else {
        vec![scopes::READ.to_string()]
    };
    let mut resp = envelope(Me { user, csrf, scopes }).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&token, SESSION_SECS));
    Ok(resp)
}

pub fn routes() -> Router<State> {
    Router::new()
        .route("/v1/setup", get(setup_state).post(setup))
        .route("/v1/setup/code", get(setup_code))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/me", get(me))
        .route("/v1/auth/password", post(change_own_password))
        .route("/v1/users", get(list_users).post(create_user))
        .route("/v1/users/{id}", put(update_user).delete(delete_user))
}

async fn setup_state(AxState(state): AxState<State>) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    let users = state.with_db(|db| db.count_users()).map_err(db_err)?;
    Ok(envelope(
        serde_json::json!({ "needed": users == 0, "hostname": state.hostname }),
    ))
}

/// The setup code, for people on the server itself (`hearth web setup-code`).
async fn setup_code(
    AxState(state): AxState<State>,
    p: Principal,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    if !p.is_local() {
        return Err(Error::Unauthorized("the setup code is only shown on the server itself".into()).into());
    }
    let users = state.with_db(|db| db.count_users()).map_err(db_err)?;
    Ok(envelope(serde_json::json!({
        "needed": users == 0,
        "code": if users == 0 { state.web.current_code() } else { None },
    })))
}

#[derive(Deserialize)]
struct SetupReq {
    code: String,
    username: String,
    password: String,
}

async fn setup(
    AxState(state): AxState<State>,
    PeerInfo(peer): PeerInfo,
    headers: HeaderMap,
    Json(req): Json<SetupReq>,
) -> Result<Response, ApiError> {
    let key = format!("setup:{}", peer.ip.map(|i| i.to_string()).unwrap_or_default());
    if let Some(wait) = state.web.locked_for(&key) {
        return Err(Error::Unauthorized(format!(
            "too many wrong codes; try again in {}s",
            wait.as_secs() + 1
        ))
        .into());
    }
    if state.with_db(|db| db.count_users()).map_err(db_err)? > 0 {
        return Err(Error::Usage("Hearth is already set up; sign in instead".into()).into());
    }
    let expected = state.web.current_code();
    let given = req.code.trim().to_ascii_uppercase().replace(' ', "");
    let matches = expected
        .as_deref()
        .is_some_and(|c| c.replace('-', "") == given.replace('-', ""));
    if !matches {
        state.web.fail(&key);
        return Err(Error::Unauthorized(
            "wrong setup code (run `hearth web setup-code` on the server to see it)".into(),
        )
        .into());
    }
    let username = req.username.trim().to_string();
    valid_username(&username)?;
    valid_password(&req.password)?;
    let hash = state.web.hash(req.password).await?;
    let id = random_hex(6)?;
    state
        .with_db(|db| {
            // Checked again under the lock: two setups at once must not make two admins.
            if db.count_users()? > 0 {
                return Ok(false);
            }
            db.create_user(&id, &username, &hash, "admin")?;
            Ok(true)
        })
        .map_err(db_err)?
        .then_some(())
        .ok_or_else(|| Error::Usage("Hearth is already set up; sign in instead".into()))?;
    state.web.clear_code();
    state.web.succeed(&key);
    tracing::info!(username, ip = ?peer.ip, "first admin created");
    let user = state
        .with_db(|db| db.get_user(&id))
        .map_err(db_err)?
        .ok_or_else(|| Error::Other("account vanished".into()))?;
    start_session(&state, user, peer, &headers)
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(
    AxState(state): AxState<State>,
    PeerInfo(peer): PeerInfo,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, ApiError> {
    let username = req.username.trim().to_string();
    let ip_key = format!("ip:{}", peer.ip.map(|i| i.to_string()).unwrap_or_default());
    let user_key = format!("user:{}", username.to_ascii_lowercase());
    for key in [&ip_key, &user_key] {
        if let Some(wait) = state.web.locked_for(key) {
            return Err(Error::Unauthorized(format!(
                "too many failed sign-ins; try again in {}s",
                wait.as_secs() + 1
            ))
            .into());
        }
    }
    let found = state.with_db(|db| db.user_with_hash(&username)).map_err(db_err)?;
    let (user, hash) = match found {
        Some((u, h)) => (Some(u), h),
        None => (None, dummy_hash().to_string()),
    };
    let ok = state.web.verify(req.password, hash).await && user.is_some();
    let Some(user) = user.filter(|_| ok) else {
        state.web.fail(&ip_key);
        state.web.fail(&user_key);
        tracing::warn!(username, ip = ?peer.ip, "failed sign-in");
        return Err(Error::Unauthorized("wrong username or password".into()).into());
    };
    state.web.succeed(&ip_key);
    state.web.succeed(&user_key);
    tracing::info!(username = %user.username, ip = ?peer.ip, "signed in");
    start_session(&state, user, peer, &headers)
}

async fn logout(AxState(state): AxState<State>, p: Principal) -> Result<Response, ApiError> {
    if let Some(u) = &p.user {
        state
            .with_db(|db| db.delete_session(&u.session_token))
            .map_err(db_err)?;
    }
    let mut resp = envelope(serde_json::json!({ "signed_out": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, set_cookie("", 0));
    Ok(resp)
}

async fn me(AxState(state): AxState<State>, p: Principal) -> Result<Json<Envelope<Me>>, ApiError> {
    let Some(u) = &p.user else {
        return Err(Error::Unauthorized("not signed in".into()).into());
    };
    let user = state
        .with_db(|db| db.get_user(&u.id))
        .map_err(db_err)?
        .ok_or_else(|| Error::Unauthorized("not signed in".into()))?;
    Ok(envelope(Me {
        user,
        csrf: u.csrf.clone(),
        scopes: p.scopes.clone(),
    }))
}

#[derive(Deserialize)]
struct PasswordReq {
    old_password: String,
    new_password: String,
}

async fn change_own_password(
    AxState(state): AxState<State>,
    p: Principal,
    Json(req): Json<PasswordReq>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    let Some(u) = &p.user else {
        return Err(Error::Unauthorized("sign in to change your password".into()).into());
    };
    let key = format!("user:{}", u.username.to_ascii_lowercase());
    if let Some(wait) = state.web.locked_for(&key) {
        return Err(Error::Unauthorized(format!(
            "too many wrong passwords; try again in {}s",
            wait.as_secs() + 1
        ))
        .into());
    }
    valid_password(&req.new_password)?;
    let old_hash = state
        .with_db(|db| db.password_hash(&u.id))
        .map_err(db_err)?
        .unwrap_or_default();
    if !state.web.verify(req.old_password, old_hash).await {
        state.web.fail(&key);
        return Err(Error::Unauthorized("the current password is wrong".into()).into());
    }
    let hash = state.web.hash(req.new_password).await?;
    state
        .with_db(|db| {
            db.set_password(&u.id, &hash)?;
            db.delete_user_sessions(&u.id, Some(&u.session_token))
        })
        .map_err(db_err)?;
    tracing::info!(username = %u.username, "password changed");
    Ok(envelope(serde_json::json!({ "changed": true })))
}

async fn list_users(
    AxState(state): AxState<State>,
    p: Principal,
) -> Result<Json<Envelope<Vec<super::db::User>>>, ApiError> {
    p.require(scopes::ADMIN)?;
    Ok(envelope(state.with_db(|db| db.list_users()).map_err(db_err)?))
}

#[derive(Deserialize)]
struct NewUserReq {
    username: String,
    password: String,
    role: String,
}

async fn create_user(
    AxState(state): AxState<State>,
    p: Principal,
    Json(req): Json<NewUserReq>,
) -> Result<Json<Envelope<super::db::User>>, ApiError> {
    p.require(scopes::ADMIN)?;
    let username = req.username.trim().to_string();
    valid_username(&username)?;
    valid_password(&req.password)?;
    valid_role(&req.role)?;
    if state
        .with_db(|db| db.get_user(&username))
        .map_err(db_err)?
        .is_some()
    {
        return Err(Error::Usage(format!("there's already a user called '{username}'")).into());
    }
    let hash = state.web.hash(req.password).await?;
    let id = random_hex(6)?;
    state
        .with_db(|db| db.create_user(&id, &username, &hash, &req.role))
        .map_err(db_err)?;
    // Created from the server (e.g. `hearth web reset` with no accounts): the setup code is no
    // longer needed.
    state.web.clear_code();
    tracing::info!(username, role = %req.role, by = %p.actor(), "user created");
    let user = state
        .with_db(|db| db.get_user(&id))
        .map_err(db_err)?
        .ok_or_else(|| Error::Other("account vanished".into()))?;
    Ok(envelope(user))
}

#[derive(Deserialize)]
struct UpdateUserReq {
    role: Option<String>,
    password: Option<String>,
}

fn find_user(state: &State, id: &str) -> Result<super::db::User, ApiError> {
    state
        .with_db(|db| db.get_user(id))
        .map_err(db_err)?
        .ok_or_else(|| {
            Error::NotFound {
                what: "user",
                name: id.to_string(),
            }
            .into()
        })
}

async fn update_user(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> Result<Json<Envelope<super::db::User>>, ApiError> {
    p.require(scopes::ADMIN)?;
    let user = find_user(&state, &id)?;
    if let Some(role) = &req.role {
        valid_role(role)?;
        if user.role == "admin"
            && role != "admin"
            && state.with_db(|db| db.count_admins()).map_err(db_err)? <= 1
        {
            return Err(
                Error::Usage("this is the last admin; make someone else an admin first".into()).into(),
            );
        }
    }
    let hash = match req.password {
        Some(pw) => {
            valid_password(&pw)?;
            Some(state.web.hash(pw).await?)
        }
        None => None,
    };
    let keep = p.user.as_ref().map(|u| u.session_token.clone());
    state
        .with_db(|db| {
            if let Some(role) = &req.role {
                db.set_role(&user.id, role)?;
            }
            if let Some(h) = &hash {
                db.set_password(&user.id, h)?;
            }
            if req.role.is_some() || hash.is_some() {
                // Sign them out elsewhere so the change applies at once.
                db.delete_user_sessions(&user.id, keep.as_deref())?;
            }
            Ok(())
        })
        .map_err(db_err)?;
    tracing::info!(username = %user.username, role = ?req.role, password = hash.is_some(), by = %p.actor(), "user changed");
    Ok(envelope(find_user(&state, &user.id)?))
}

async fn delete_user(
    AxState(state): AxState<State>,
    p: Principal,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    p.require(scopes::ADMIN)?;
    let user = find_user(&state, &id)?;
    if user.role == "admin" && state.with_db(|db| db.count_admins()).map_err(db_err)? <= 1 {
        return Err(Error::Usage("can't remove the last admin".into()).into());
    }
    state.with_db(|db| db.delete_user(&user.id)).map_err(db_err)?;
    tracing::info!(username = %user.username, by = %p.actor(), "user removed");
    Ok(envelope(serde_json::json!({ "removed": user.username })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify() {
        let h = hash_password("correct horse").expect("test");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn lockout_grows_and_caps() {
        assert_eq!(lock_duration(5), Duration::from_secs(30));
        assert_eq!(lock_duration(6), Duration::from_secs(60));
        assert_eq!(lock_duration(40), MAX_LOCK);
    }

    #[test]
    fn setup_codes_look_right() {
        let c = new_setup_code();
        assert_eq!(c.len(), 9);
        assert_eq!(&c[4..5], "-");
        assert!(!c.contains('0') && !c.contains('O') && !c.contains('1'));
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=b; __Host-hearth=abc123; c=d"),
        );
        assert_eq!(session_cookie(&h).as_deref(), Some("abc123"));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("__Host-hearthx=1"));
        assert_eq!(session_cookie(&h), None);
    }

    #[test]
    fn usernames_and_passwords() {
        assert!(valid_username("gobo").is_ok());
        assert!(valid_username("a b").is_err());
        assert!(valid_username("").is_err());
        assert!(valid_password("short").is_err());
        assert!(valid_password("long enough").is_ok());
    }
}
