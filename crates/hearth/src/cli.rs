//! Command-line interface definition.
//!
//! Rules every command follows (see the agent guide):
//! - never prompts when stdin is not a terminal: it fails with exit code 4 unless `--yes` is given;
//! - `--json` prints exactly one JSON document to stdout; progress goes to stderr;
//! - `--dry-run` is accepted by every command that changes state and changes nothing.

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "hearth",
    version,
    about = "Lightweight home-server manager",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Print one machine-readable JSON document to stdout.
    #[arg(long, global = true, env = "HEARTH_JSON")]
    pub json: bool,

    /// Assume "yes" to confirmations (required for changes when not run from a terminal).
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Show what would change without changing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Start the job and return its ID instead of waiting for it to finish.
    #[arg(long, global = true)]
    pub background: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the Hearth version.
    Version,
    /// Host and apps summary.
    Status,
    /// Run the Hearth daemon (used by the systemd unit).
    #[command(hide = true)]
    Daemon,
    /// Run the privileged helper (used by the systemd unit).
    #[command(hide = true)]
    Helper,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_work_after_subcommand() {
        let cli = Cli::try_parse_from(["hearth", "version", "--json", "--yes"]);
        let cli = match cli {
            Ok(c) => c,
            Err(e) => panic!("parse failed: {e}"),
        };
        assert!(cli.global.json);
        assert!(cli.global.yes);
        assert!(!cli.global.dry_run);
    }
}
