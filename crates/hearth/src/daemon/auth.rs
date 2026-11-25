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

#[derive(Debug, Clone, Copy)]
pub struct Peer {
    pub uid: Option<u32>,
}

impl Connected<IncomingStream<'_, UnixListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        Peer {
            uid: stream.io().peer_cred().ok().map(|c| c.uid()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub uid: Option<u32>,
    pub token_id: Option<String>,
    pub scopes: Vec<String>,
}

impl Principal {
    pub fn allows(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == scopes::ADMIN)
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
            .unwrap_or(Peer { uid: None });
        let auth = parts.headers.get("authorization").and_then(|v| v.to_str().ok());
        Ok(principal(state, peer, auth)?)
    }
}
