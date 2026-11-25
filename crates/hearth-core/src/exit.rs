//! Process exit codes.
//!
//! These values are a public contract: scripts and AI agents branch on them, so a code's
//! meaning never changes once released. New codes may be added; existing ones are never reused.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExitCode {
    /// Success.
    Ok = 0,
    /// Generic failure (see the error message / JSON `error.kind`).
    Error = 1,
    /// Invalid arguments or usage.
    Usage = 2,
    /// The named app, package, backup, etc. does not exist.
    NotFound = 3,
    /// The action needs confirmation: re-run with `--yes` (never prompts without a terminal).
    NeedsConfirmation = 4,
    /// Not authenticated, or the token lacks the needed scope.
    Unauthorized = 5,
    /// The change was applied, the health check failed, and it was rolled back successfully.
    RolledBack = 6,
    /// The health check failed AND the rollback failed: manual attention needed.
    RollbackFailed = 7,
    /// `--check` found updates (not an error).
    UpdatesAvailable = 10,
}

impl ExitCode {
    pub const fn code(self) -> u8 {
        self as u8
    }

    pub const fn is_success(self) -> bool {
        matches!(self, ExitCode::Ok | ExitCode::UpdatesAvailable)
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(c: ExitCode) -> Self {
        std::process::ExitCode::from(c.code())
    }
}

#[cfg(test)]
mod tests {
    use super::ExitCode::*;

    #[test]
    fn codes_are_stable() {
        // Changing any of these breaks scripts and agents. Add new codes instead.
        let table = [
            (Ok, 0),
            (Error, 1),
            (Usage, 2),
            (NotFound, 3),
            (NeedsConfirmation, 4),
            (Unauthorized, 5),
            (RolledBack, 6),
            (RollbackFailed, 7),
            (UpdatesAvailable, 10),
        ];
        for (code, value) in table {
            assert_eq!(code.code(), value, "{code:?}");
        }
    }

    #[test]
    fn success_codes() {
        assert!(Ok.is_success());
        assert!(UpdatesAvailable.is_success());
        assert!(!RolledBack.is_success());
    }
}
