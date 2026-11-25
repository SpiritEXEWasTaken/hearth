//! Shared types for Hearth.
//!
//! Everything the CLI, the daemon and the privileged helper agree on lives here:
//! the error type, the process exit codes (a stable contract for scripts and agents),
//! the machine-readable output envelope used by `--json`, the API data model and the
//! daemon ↔ helper protocol.

pub mod error;
pub mod exit;
pub mod model;
pub mod output;
pub mod proto;
pub mod redact;

pub use error::{Error, Result};
pub use exit::ExitCode;
