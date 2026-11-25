//! Shared types for Hearth.
//!
//! Everything the CLI, the daemon and the privileged helper agree on lives here:
//! the error type, the process exit codes (a stable contract for scripts and agents),
//! and the machine-readable output envelope used by `--json`.

pub mod error;
pub mod exit;
pub mod output;

pub use error::{Error, Result};
pub use exit::ExitCode;
