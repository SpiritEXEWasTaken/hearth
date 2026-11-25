//! The error type shared by all Hearth components.

use crate::exit::ExitCode;
use serde::Serialize;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid usage: {0}")]
    Usage(String),

    #[error("{what} not found: {name}")]
    NotFound { what: &'static str, name: String },

    #[error("confirmation required: {0} (re-run with --yes)")]
    NeedsConfirmation(String),

    #[error("not authorized: {0}")]
    Unauthorized(String),

    #[error("health check failed, rolled back: {0}")]
    RolledBack(String),

    #[error("health check failed and rollback FAILED: {0}")]
    RollbackFailed(String),

    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Error::Usage(_) => ExitCode::Usage,
            Error::NotFound { .. } => ExitCode::NotFound,
            Error::NeedsConfirmation(_) => ExitCode::NeedsConfirmation,
            Error::Unauthorized(_) => ExitCode::Unauthorized,
            Error::RolledBack(_) => ExitCode::RolledBack,
            Error::RollbackFailed(_) => ExitCode::RollbackFailed,
            Error::NotImplemented(_) | Error::Io(_) | Error::Other(_) => ExitCode::Error,
        }
    }

    /// Stable, machine-readable kind for the JSON `error.kind` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::NotFound { .. } => "not_found",
            Error::NeedsConfirmation(_) => "needs_confirmation",
            Error::Unauthorized(_) => "unauthorized",
            Error::RolledBack(_) => "rolled_back",
            Error::RollbackFailed(_) => "rollback_failed",
            Error::NotImplemented(_) => "not_implemented",
            Error::Io(_) => "io",
            Error::Other(_) => "other",
        }
    }
}

/// JSON form of an error, as printed with `--json`.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub kind: &'static str,
    pub message: String,
    pub exit_code: u8,
}

impl From<&Error> for ErrorBody {
    fn from(e: &Error) -> Self {
        ErrorBody {
            kind: e.kind(),
            message: e.to_string(),
            exit_code: e.exit_code().code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_maps_to_exit_code_and_kind() {
        let e = Error::NotFound {
            what: "app",
            name: "jellyfin".into(),
        };
        assert_eq!(e.exit_code(), ExitCode::NotFound);
        assert_eq!(e.kind(), "not_found");
        assert_eq!(e.to_string(), "app not found: jellyfin");

        let body = ErrorBody::from(&e);
        assert_eq!(body.exit_code, 3);
    }

    #[test]
    fn confirmation_message_tells_user_how_to_proceed() {
        let e = Error::NeedsConfirmation("update jellyfin".into());
        assert!(e.to_string().contains("--yes"));
        assert_eq!(e.exit_code(), ExitCode::NeedsConfirmation);
    }
}
