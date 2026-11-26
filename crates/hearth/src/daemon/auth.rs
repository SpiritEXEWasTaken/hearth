//! Who is calling: kernel peer credentials on the Unix socket, plus an optional API token.
//!
//! - root (uid 0) on the socket is an admin;
//! - any other local caller got through the socket's permissions (owner or `hearth` group) and
//!   may read;
//! - an `Authorization: Bearer <token>` header adds the token's scopes; a bad token is refused.

use super::State;
use axum::extract::connect_info::Connected;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::serve::IncomingStream;
use hearth_core::Error;
use hearth_core::model::scopes;
use tokio::net::UnixListener;

/// The connection a request came in on: the Unix socket (with the caller's uid) or the network
/// (with the client's address; never any uid-based rights).
#[derive(Debug, Clone, Copy)]
pub struct Peer {
    pub uid: Option<u32>,
    pub ip: Option<std::net::IpAddr>,
}

impl Connected<IncomingStream<'_, UnixListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        Peer {
            uid: stream.io().peer_cred().ok().map(|c| c.uid()),
            ip: None,
        }
    }
}

/// A signed-in web UI user.
#[derive(Debug, Clone)]
pub struct WebUser {
    pub id: String,
    pub username: String,
    pub role: String,
    /// The session cookie value (to sign out, or keep this session when changing a password).
    pub session_token: String,
    pub csrf: String,
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub uid: Option<u32>,
    pub token_id: Option<String>,
    pub user: Option<WebUser>,
    pub scopes: Vec<String>,
}

impl Principal {
    pub fn allows(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == scopes::ADMIN)
    }

    /// Who made a change, for the job log.
    pub fn actor(&self) -> String {
        match (&self.user, &self.token_id, self.uid) {
            (Some(u), _, _) => format!("user:{}", u.username),
            (None, Some(t), _) => format!("token:{t}"),
            (None, None, Some(uid)) => format!("uid:{uid}"),
            _ => "unknown".into(),
        }
    }

    /// True for callers on the server itself (the Unix socket), never for network clients.
    pub fn is_local(&self) -> bool {
        self.uid.is_some()
    }

    pub fn require(&self, scope: &str) -> Result<(), Error> {
        if self.allows(scope) {
            Ok(())
        } else {
            Err(Error::Unauthorized(format!("this needs the '{scope}' scope")))
        }
    }
}

pub fn principal(state: &State, peer: Peer, authorization: Option<&str>) -> Result<Principal, Error> {
    let mut p = Principal {
        uid: peer.uid,
        token_id: None,
        user: None,
        scopes: Vec::new(),
    };
    match peer.uid {
        Some(0) => p.scopes.push(scopes::ADMIN.into()),
        Some(_) => p.scopes.push(scopes::READ.into()),
        None => {}
    }
    if let Some(h) = authorization {
        let token = h
            .strip_prefix("Bearer ")
            .ok_or_else(|| Error::Unauthorized("Authorization must be 'Bearer <token>'".into()))?
            .trim();
        match state.with_db(|db| db.check_token(token)) {
            Ok(Some((id, token_scopes))) => {
                p.token_id = Some(id);
                for s in token_scopes {
                    if !p.scopes.contains(&s) {
                        p.scopes.push(s);
                    }
                }
            }
            Ok(None) => return Err(Error::Unauthorized("invalid or revoked token".into())),
            Err(e) => return Err(Error::Other(format!("token check failed: {e}"))),
        }
    }
    Ok(p)
}

impl FromRequestParts<State> for Principal {
    type Rejection = super::api::ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &State) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<Peer>>()
            .map(|c| c.0)
            .unwrap_or(Peer { uid: None, ip: None });
        let auth = parts.headers.get("authorization").and_then(|v| v.to_str().ok());
        let mut p = principal(state, peer, auth)?;
        // A browser session (cookie) counts only when no token was given.
        if p.token_id.is_none()
            && let Some(token) = super::web_auth::session_cookie(&parts.headers)
        {
            match super::web_auth::session_user(state, &token) {
                Some(user) => {
                    // Changes from a browser must carry the session's CSRF token in a header,
                    // which other sites can't read or set.
                    let safe = matches!(parts.method, http::Method::GET | http::Method::HEAD);
                    if !safe {
                        let sent = parts.headers.get("x-hearth-csrf").and_then(|v| v.to_str().ok());
                        if sent != Some(user.csrf.as_str()) {
                            return Err(Error::Unauthorized(
                                "missing or wrong CSRF token; reload the page".into(),
                            )
                            .into());
                        }
                    }
                    let role_scopes: &[&str] = if user.role == "admin" {
                        &[scopes::ADMIN]
                    } else {
                        &[scopes::READ]
                    };
                    for s in role_scopes {
                        if !p.scopes.iter().any(|x| x == s) {
                            p.scopes.push((*s).to_string());
                        }
                    }
                    p.user = Some(user);
                }
                None if p.uid.is_none() => {
                    return Err(Error::Unauthorized("not signed in (the session has expired)".into()).into());
                }
                None => {}
            }
        }
        Ok(p)
    }
}

/// The connection's [`Peer`] as an extractor (missing info means "unknown network client").
pub struct PeerInfo(pub Peer);

impl<S: Send + Sync> FromRequestParts<S> for PeerInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(PeerInfo(
            parts
                .extensions
                .get::<ConnectInfo<Peer>>()
                .map(|c| c.0)
                .unwrap_or(Peer { uid: None, ip: None }),
        ))
    }
}
