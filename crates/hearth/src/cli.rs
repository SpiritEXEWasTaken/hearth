//! Command-line interface definition.
//!
//! Rules every command follows (see docs/agent-guide.md):
//! - never prompts when stdin is not a terminal: it fails with exit code 4 unless `--yes` is given;
//! - `--json` prints exactly one JSON document to stdout; progress goes to stderr;
//! - `--dry-run` is accepted by every command that changes state and changes nothing.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

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

    /// The daemon's socket.
    #[arg(
        long,
        global = true,
        env = "HEARTH_SOCKET",
        default_value = "/run/hearth/hearth.sock"
    )]
    pub socket: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the Hearth version.
    Version,
    /// Host summary and every app with its state, memory and versions.
    Status,
    /// Host statistics: CPU, memory, swap, disks, memory pressure.
    Stats,
    /// Commands about all apps.
    #[command(subcommand)]
    Apps(AppsCmd),
    /// Commands about one app.
    #[command(subcommand)]
    App(AppCmd),
    /// API tokens (for Claude, scripts and the Android app). Needs root or an admin token.
    #[command(subcommand)]
    Token(TokenCmd),
    /// Run the Hearth daemon (used by the systemd unit).
    #[command(hide = true)]
    Daemon(DaemonArgs),
    /// Run the privileged helper (used by the systemd unit).
    #[command(hide = true)]
    Helper(HelperArgs),
}

#[derive(Debug, Subcommand)]
pub enum AppsCmd {
    /// List apps: state, health, memory, running version, latest version.
    List {
        /// Ask the registries for the latest versions first (otherwise the last check is shown).
        #[arg(long)]
        check: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppCmd {
    /// Details of one app: containers, images, versions, CasaOS findings.
    Info {
        app: String,
        /// Also print the app's compose file (secrets hidden).
        #[arg(long)]
        compose: bool,
    },
    /// Recent log lines of an app's containers.
    Logs {
        app: String,
        /// Only this service (or container name).
        #[arg(long)]
        service: Option<String>,
        /// Number of lines per container.
        #[arg(long, default_value_t = 200)]
        tail: u32,
        /// Only lines from the last N seconds.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Check for (and later apply) updates. Exit code 10 means updates are available.
    Update {
        /// The app to update.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        app: Option<String>,
        /// All apps.
        #[arg(long)]
        all: bool,
        /// Only check, change nothing.
        #[arg(long)]
        check: bool,
    },
    /// Start an app (milestone M2).
    Start { app: String },
    /// Stop an app and keep it stopped (milestone M2).
    Stop { app: String },
    /// Restart an app (milestone M2).
    Restart { app: String },
}

#[derive(Debug, Subcommand)]
pub enum TokenCmd {
    /// Create a token. The secret is printed once; store it (e.g. in ~/.config/hearth/token).
    Create {
        #[arg(long)]
        name: String,
        /// read, apps:write, system:write, terminal, admin. Repeat for several.
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
    },
    /// List tokens (secrets are never shown again).
    List,
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[arg(long, default_value = "/run/hearth-helper/helper.sock")]
    pub helper_socket: PathBuf,
    #[arg(long, default_value = "/var/lib/hearth")]
    pub state_dir: PathBuf,
    /// Seconds between background registry checks.
    #[arg(long, default_value_t = 6 * 3600)]
    pub check_interval: u64,
}

#[derive(Debug, Args)]
pub struct HelperArgs {
    #[arg(long, default_value = "/run/hearth-helper/helper.sock")]
    pub helper_socket: PathBuf,
    /// The user the daemon runs as; only it (and root) may use the helper.
    #[arg(long, default_value = "hearth")]
    pub daemon_user: String,
    #[arg(long, default_value = "/run/docker.sock")]
    pub docker_socket: PathBuf,
    #[arg(long, default_value = "/var/lib/casaos/apps")]
    pub casaos_apps_dir: PathBuf,
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
        let cli = match Cli::try_parse_from(["hearth", "version", "--json", "--yes"]) {
            Ok(c) => c,
            Err(e) => panic!("parse failed: {e}"),
        };
        assert!(cli.global.json);
        assert!(cli.global.yes);
        assert!(!cli.global.dry_run);
    }

    #[test]
    fn update_needs_app_or_all() {
        assert!(Cli::try_parse_from(["hearth", "app", "update", "--check"]).is_err());
        assert!(Cli::try_parse_from(["hearth", "app", "update", "jellyfin", "--check"]).is_ok());
        assert!(Cli::try_parse_from(["hearth", "app", "update", "--all", "--check"]).is_ok());
        assert!(Cli::try_parse_from(["hearth", "app", "update", "jellyfin", "--all"]).is_err());
    }
}
