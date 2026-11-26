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

#[derive(Debug, Clone, Args)]
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
    /// System updates: the distro's packages and Flatpak apps. Hearth never reboots by itself.
    #[command(subcommand)]
    System(SystemCmd),
    /// Packages: search, details, install, remove (the distro's packages, or Flatpak apps).
    #[command(subcommand)]
    Package(PackageCmd),
    /// Add a non-Docker app: an existing systemd service, a program for Hearth to run, a link,
    /// or an installed package. Apps without a web page stay off the main app list.
    #[command(subcommand)]
    Add(AddCmd),
    /// The app store: browse, see what an app needs, install with options.
    #[command(subcommand)]
    Store(StoreCmd),
    /// Move apps from another manager to Hearth.
    #[command(subcommand)]
    Migrate(MigrateCmd),
    /// API tokens (for Claude, scripts and the Android app). Needs root or an admin token.
    #[command(subcommand)]
    Token(TokenCmd),
    /// The web UI: setup code, HTTPS certificates, user accounts.
    #[command(subcommand)]
    Web(WebCmd),
    /// Run the Hearth daemon (used by the systemd unit).
    #[command(hide = true)]
    Daemon(DaemonArgs),
    /// Run the privileged helper (used by the systemd unit).
    #[command(hide = true)]
    Helper(HelperArgs),
}

#[derive(Debug, Subcommand)]
pub enum StoreCmd {
    /// List store apps, optionally matching words or a category.
    List {
        /// Words to search for in names, summaries and tags.
        query: Vec<String>,
        /// Only this category (e.g. media, network).
        #[arg(long)]
        category: Option<String>,
    },
    /// An app's details, and what it needs compared with this server (host checks).
    Show {
        /// Store id, or the app's name (e.g. jellyfin).
        app: String,
    },
    /// Download the latest catalog now (it's also refreshed twice a day).
    Refresh,
    /// Install an app from the store. Shows the plan and host checks first.
    Install {
        /// Store id, or the app's name (e.g. jellyfin).
        app: String,
        /// The app's name on this server (default: the store's, e.g. jellyfin).
        #[arg(long)]
        name: Option<String>,
        /// An install option, e.g. --option gpu=vaapi. Repeat for several; see `hearth store show`.
        #[arg(long = "option", value_name = "ID=VALUE")]
        options: Vec<String>,
        /// A host port, e.g. --port WEB_PORT=8100.
        #[arg(long = "port", value_name = "VAR=PORT")]
        ports: Vec<String>,
        /// Turn on HTTPS for the app after installing (default: only when it needs it).
        #[arg(long, conflicts_with = "no_https")]
        https: bool,
        /// Don't turn on HTTPS, even if the app needs it.
        #[arg(long)]
        no_https: bool,
    },
    /// Link an installed app to its store entry (shows the entry's notes; nothing else changes).
    Link { app: String, entry: String },
    /// Remove a link made with `hearth store link`.
    Unlink { app: String },
    /// Sign-in details Hearth generated for a store app (e.g. an admin password).
    Credentials { app: String },
}

#[derive(Debug, Subcommand)]
pub enum SystemCmd {
    /// Distro, package backends, and whether a reboot is needed (and why).
    Info,
    /// Available updates (from the last check). Exit code 10 when there are any.
    Updates {
        /// Refresh the package lists first (apt update / dnf makecache / Flatpak); takes a while.
        #[arg(long)]
        check: bool,
        /// Only security updates.
        #[arg(long)]
        security: bool,
    },
    /// Install updates (needs a token with 'system:write'). Never reboots.
    Upgrade {
        /// Update ids or package names from `hearth system updates`.
        ids: Vec<String>,
        /// Every available update.
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// Only security updates (all of them, or only those among the given ids).
        #[arg(long)]
        security: bool,
    },
}

/// Options every added app has.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct AddCommon {
    /// The name shown in Hearth.
    #[arg(long)]
    pub title: Option<String>,
    /// An icon URL.
    #[arg(long)]
    pub icon: Option<String>,
    /// Its web page on this host: the port…
    #[arg(long)]
    pub port: Option<u16>,
    /// …and the path (default /).
    #[arg(long)]
    pub path: Option<String>,
    /// Or a full web address.
    #[arg(long)]
    pub url: Option<String>,
    /// Show it in the main app list even without a web page.
    #[arg(long, conflicts_with = "no_dashboard")]
    pub dashboard: bool,
    /// Keep it off the main app list even with a web page.
    #[arg(long)]
    pub no_dashboard: bool,
}

#[derive(Debug, Subcommand)]
pub enum AddCmd {
    /// A shortcut to something Hearth doesn't run (router, another PC); its address is checked.
    Link {
        /// The app's name (lowercase letters, digits, '-', '_').
        name: String,
        #[command(flatten)]
        common: AddCommon,
    },
    /// An existing systemd service, e.g. smbd or tailscaled (a user's unit with --user).
    Service {
        /// The unit, e.g. smbd or smbd.service.
        unit: String,
        /// Owner of a user unit (systemctl --user).
        #[arg(long)]
        user: Option<String>,
        /// The app's name (default: from the unit).
        #[arg(long)]
        id: Option<String>,
        #[command(flatten)]
        common: AddCommon,
    },
    /// A program for Hearth to run as a hardened systemd service.
    Native {
        name: String,
        /// The command line; the program as an absolute path.
        #[arg(long)]
        exec: String,
        /// The user it runs as (default: you).
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        workdir: Option<String>,
        /// A file with KEY=VALUE lines for its environment.
        #[arg(long)]
        env_file: Option<String>,
        /// A memory limit, e.g. 512M.
        #[arg(long)]
        memory_max: Option<String>,
        /// on-failure (default), always or no.
        #[arg(long)]
        restart: Option<String>,
        /// Add it without starting it.
        #[arg(long)]
        no_start: bool,
        #[command(flatten)]
        common: AddCommon,
    },
    /// An installed package or Flatpak app, as an app (with its service, if it has one).
    Package {
        /// The package name or Flatpak app id.
        package: String,
        #[arg(long)]
        flatpak: bool,
        /// Its systemd unit, if it runs as a service.
        #[arg(long)]
        unit: Option<String>,
        #[arg(long)]
        id: Option<String>,
        #[command(flatten)]
        common: AddCommon,
    },
}

#[derive(Debug, Subcommand)]
pub enum PackageCmd {
    /// Search package names (the distro's packages and Flatpak apps).
    Search {
        terms: Vec<String>,
        /// Only Flatpak apps.
        #[arg(long)]
        flatpak: bool,
        /// Only the distro's packages.
        #[arg(long, conflicts_with = "flatpak")]
        native: bool,
    },
    /// Details of one package.
    Info {
        name: String,
        /// A Flatpak app id instead of a distro package.
        #[arg(long)]
        flatpak: bool,
    },
    /// Install packages (needs a token with 'system:write'). Shows everything that would be
    /// installed first.
    Install {
        names: Vec<String>,
        #[arg(long)]
        flatpak: bool,
    },
    /// Remove packages (needs 'system:write'). Shows everything that would be removed first;
    /// removing what the server needs (SSH, Docker, the kernel…) is refused without
    /// --allow-critical.
    Remove {
        names: Vec<String>,
        #[arg(long)]
        flatpak: bool,
        #[arg(long)]
        allow_critical: bool,
    },
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
    /// Serve the app over HTTPS on its own port (https://<server>:<port>), or show its HTTPS
    /// addresses. Hearth forwards to the app's plain-HTTP port.
    Https {
        app: String,
        /// Turn HTTPS on.
        #[arg(long, conflicts_with = "off")]
        on: bool,
        /// Turn HTTPS off.
        #[arg(long)]
        off: bool,
        /// The HTTPS port (default: the first free one from 8443).
        #[arg(long, requires = "on")]
        port: Option<u16>,
        /// Check that the app answers through Hearth's proxy.
        #[arg(long)]
        test: bool,
    },
    /// Change a service/native/link/package app's title, icon, web address or dashboard placement.
    Set {
        app: String,
        #[command(flatten)]
        common: AddCommon,
    },
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

#[derive(Debug, Subcommand)]
pub enum WebCmd {
    /// Show the one-time code for creating the first admin account in the web UI.
    SetupCode,
    /// Web UI addresses and HTTPS certificates.
    Status,
    /// List web UI accounts.
    Users,
    /// Add a web UI account (needs root or an admin token). The password is read from stdin.
    UserAdd {
        username: String,
        /// admin or viewer (read-only).
        #[arg(long, default_value = "viewer")]
        role: String,
    },
    /// Set a new password for an account (e.g. a forgotten admin password). Read from stdin.
    UserPassword { username: String },
    /// Change an account's role (admin or viewer).
    UserRole { username: String, role: String },
    /// Remove a web UI account.
    UserRemove { username: String },
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
    /// Where the HTTPS web UI listens: comma-separated addresses, or "off".
    #[arg(long, env = "HEARTH_WEB_LISTEN", default_value = "[::]:443")]
    pub web_listen: String,
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
    /// Testing aid: where app folders, data and removed apps live (default /var/lib/hearth).
    #[arg(long, default_value = "/var/lib/hearth", hide = true)]
    pub lib_dir: PathBuf,
    /// Testing aid: where backups go (default /var/backups/hearth).
    #[arg(long, default_value = "/var/backups/hearth", hide = true)]
    pub backups_dir: PathBuf,
    /// Testing aid: print this host's system info and update list as JSON, then exit.
    /// `refresh` also downloads the package lists; `upgrade-security` / `upgrade-all` install
    /// updates first (all need root).
    #[arg(long, hide = true, value_name = "list|refresh|upgrade-security|upgrade-all")]
    pub probe_system: Option<String>,
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
