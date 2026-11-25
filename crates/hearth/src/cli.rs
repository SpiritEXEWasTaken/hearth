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
    /// Install an app from a compose file (it's normalised and checked against this host first).
    Install {
        /// The app's name (lowercase letters, digits, '-', '_').
        #[arg(long)]
        name: String,
        /// The compose file.
        #[arg(long, short = 'f')]
        file: PathBuf,
        /// Values for variables in the file (KEY=VALUE). Repeat for several.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        sets: Vec<String>,
        /// Create the containers but don't start them.
        #[arg(long)]
        no_start: bool,
    },
    /// Remove an app. Its data is kept unless you add --delete-data.
    Uninstall {
        app: String,
        /// Also delete its named volumes and data folders under /DATA/AppData/<app>.
        #[arg(long)]
        delete_data: bool,
    },
    /// Back up an app's files and data (stops it briefly unless --no-stop).
    Backup {
        app: String,
        /// Leave out a container path (e.g. a big media folder). Repeat for several.
        #[arg(long = "exclude", value_name = "CONTAINER_PATH")]
        exclude: Vec<String>,
        /// Don't stop the app while backing up (faster, but databases may be inconsistent).
        #[arg(long)]
        no_stop: bool,
    },
    /// List an app's backups.
    Backups { app: String },
    /// Restore an app from one of its backups (current data is moved aside, not deleted).
    Restore { app: String, file: String },
    /// Docker images: list, or clean up.
    #[command(subcommand)]
    Images(ImagesCmd),
    /// Show running and recent jobs, or follow one.
    #[command(subcommand)]
    Job(JobCmd),
    /// Show or change settings (image_retention, backup_retention, health_timeout).
    Settings {
        /// `KEY VALUE` to change a setting (needs root or an admin token).
        #[arg(num_args = 0..=2)]
        set: Vec<String>,
    },
    /// Move apps from another manager to Hearth.
    #[command(subcommand)]
    Migrate(MigrateCmd),
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
    /// Update an app's images (same tag, newer build), with a health check and automatic
    /// rollback. With --check: only look (exit code 10 means updates are available).
    Update {
        /// The app to update.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        app: Option<String>,
        /// All apps (only with --check; updates are applied one app at a time, by name).
        #[arg(long)]
        all: bool,
        /// Only check, change nothing.
        #[arg(long)]
        check: bool,
    },
    /// Start an app (and keep it running after reboots).
    Start { app: String },
    /// Stop an app and keep it stopped, also after reboots, until you start it.
    Stop { app: String },
    /// Restart an app.
    Restart { app: String },
    /// Apply a changed compose file: shows the diff, recreates what changed, checks health,
    /// rolls back if it fails.
    Apply {
        app: String,
        #[arg(long, short = 'f')]
        file: PathBuf,
        #[arg(long = "set", value_name = "KEY=VALUE")]
        sets: Vec<String>,
        /// A note for the history.
        #[arg(long, short = 'm')]
        message: Option<String>,
    },
    /// The app's file history (every change is a revision).
    History { app: String },
    /// Go back to an earlier revision of the app's files.
    Revert { app: String, revision: u32 },
}

#[derive(Debug, Subcommand)]
pub enum MigrateCmd {
    /// CasaOS: without a sub-command, a read-only report of what the migration would do.
    Casaos {
        #[command(subcommand)]
        action: Option<CasaosCmd>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CasaosCmd {
    /// Back up CasaOS (data, config, units, container settings) and dump every Postgres database.
    Backup,
    /// Take over an app's files. Nothing restarts, unless you add fixes (each recreates it once,
    /// with a health check and automatic rollback).
    Adopt {
        app: String,
        /// A fix from the report (e.g. jellyfin-udp, drop-rpi-paths). Repeat for several.
        #[arg(long = "fix")]
        fixes: Vec<String>,
    },
    /// Hand an adopted app back to CasaOS.
    Undo { app: String },
}

#[derive(Debug, Subcommand)]
pub enum ImagesCmd {
    /// All images in the root Docker, with size and whether they're used.
    List,
    /// Remove old rollback images (beyond the retention setting) and unused untagged images.
    Prune {
        /// Rollback images to keep per app service.
        #[arg(long)]
        keep: Option<u64>,
        /// Also remove tagged images that no container uses.
        #[arg(long)]
        unused: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum JobCmd {
    /// Running and recent jobs.
    List,
    /// Follow a job until it finishes (exit code reflects its result).
    Wait { id: String },
    /// A job's full log and result.
    Show { id: String },
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
    /// Value for PUID in compose files that expect one.
    #[arg(long, default_value_t = 1000)]
    pub puid: u32,
    /// Value for PGID in compose files that expect one.
    #[arg(long, default_value_t = 1000)]
    pub pgid: u32,
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
    #[arg(long, default_value = "/usr/bin/docker")]
    pub docker_bin: PathBuf,
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
