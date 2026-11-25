//! The `--json` output envelope.
//!
//! Every command prints exactly one JSON document to stdout when `--json` is given:
//! `{"ok": true, "data": ...}` on success or `{"ok": false, "error": {...}}` on failure.
//! Progress and logs go to stderr so stdout stays parseable.

use crate::error::{Error, ErrorBody};
use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    pub schema: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl<T: Serialize> Envelope<T> {
    pub fn success(data: T) -> Self {
        Envelope {
            ok: true,
            schema: SCHEMA_VERSION,
            data: Some(data),
            error: None,
        }
    }
}

impl Envelope<()> {
    pub fn failure(err: &Error) -> Self {
        Envelope {
            ok: false,
            schema: SCHEMA_VERSION,
            data: None,
            error: Some(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_shape() {
        let v = serde_json::to_value(Envelope::success(json!({"apps": 3}))).unwrap_or_default();
        assert_eq!(v, json!({"ok": true, "schema": 1, "data": {"apps": 3}}));
    }

    #[test]
    fn failure_shape() {
        let e = Error::Unauthorized("token lacks apps:write".into());
        let v = serde_json::to_value(Envelope::failure(&e)).unwrap_or_default();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["kind"], json!("unauthorized"));
        assert_eq!(v["error"]["exit_code"], json!(5));
        assert!(v.get("data").is_none());
    }
}
