# Hearth

A lightweight home-server manager: Docker Compose apps, a curated app store, OS updates on several distros,
files, storage, users and a web terminal. It's one static Rust binary, has a web UI and an Android app, and
uses under 30 MB of RAM when idle.

> **Status: early development (milestone M4 done: system updates, packages, app kinds).** Hearth shows every app (CasaOS,
> compose, rootless Docker) with state, health, memory and versions, and manages its own apps
> (install, start/stop that persists, apply, update, revert, backup/restore) with health checks and
> automatic rollback. CasaOS apps can be adopted in place, without restarting them; see
> [docs/migration-casaos.md](docs/migration-casaos.md). System updates work through PackageKit
> (Debian/Ubuntu, Fedora, openSUSE), pacman (Arch/CachyOS) and Flatpak, packages can be searched,
> installed and removed (with a full plan first), and Hearth never reboots by itself; see
> [docs/system-updates.md](docs/system-updates.md). Besides compose apps, systemd services, programs
> Hearth runs, links and packages can be apps; only those with a web page go on the main app list
> ([docs/app-kinds.md](docs/app-kinds.md)).

## Goals

- **Plain files.** Every app is a normal `compose.yaml` plus `.env`. `docker compose up` works without
  Hearth.
- **Safe updates.** Updates are found from registry digests and tags, and never downgrade or silently change
  your tag. You see a diff first. Updates are health-checked and rolled back automatically if they fail.
- **Honest commands.** Commands wait, show progress, verify the result and use clear exit codes. `--json`,
  `--dry-run` and `--yes` work everywhere.
- **Small and least-privileged.** An unprivileged daemon plus a tiny root helper. Reading status needs no
  root.
- **One app, one store entry.** Variants live inside an entry instead of duplicate entries. Store CI rejects
  duplicates.
- **Many distros.** System updates and packages on Debian/Ubuntu, Fedora/RHEL, openSUSE and
  Arch/CachyOS, with Flatpak optional.
- **Migrates from CasaOS** in place, without reinstalling apps.

## Repository layout

| Path | What |
|---|---|
| `crates/hearth-core` | Shared types: errors, exit codes, JSON output, API model, helper protocol, secret redaction |
| `crates/hearth-registry` | OCI registry client: tag digests, version labels, tag tracks |
| `crates/hearth-casaos` | Readers for CasaOS app files, `x-casaos` metadata and store layouts |
| `crates/hearth` | The `hearth` binary: CLI, daemon (`hearth daemon`) and root helper (`hearth helper`) |
| `packaging/` | systemd units, `install.sh`, the Claude Code skill |
| `docs/agent-guide.md` | How AI agents and scripts should use the CLI safely |
| `ci/smoke.sh` | End-to-end test against a real Docker, with the idle-memory budget |
| `ui/` | SvelteKit web UI, compiled to static files embedded in the binary (later milestone) |
| `android/` | Kotlin + Jetpack Compose app (later milestone) |

## CLI exit codes

| Code | Meaning |
|---|---|
| 0 | OK |
| 1 | Error |
| 2 | Bad usage |
| 3 | Not found |
| 4 | Needs confirmation (re-run with `--yes`) |
| 5 | Not authorized |
| 6 | Health check failed, rolled back |
| 7 | Health check failed, rollback also failed |
| 10 | Updates available (with `--check`) |

## How it runs

| Process | Runs as | Does |
|---|---|---|
| `hearth helper` | root | Only a fixed list of typed operations: reading Docker and CasaOS files, running `docker compose` with fixed command lines for Hearth apps, writing app files (with history), tagging and removing images, backups, and system updates (PackageKit over D-Bus; pacman and Flatpak as transient systemd units). Accepts connections only from root and the `hearth` user. |
| `hearth daemon` | `hearth` | The API on `/run/hearth/hearth.sock` (group `hearth`), registry checks, stats, SQLite state |
| `hearth <command>` | you | The CLI. Members of the `hearth` group can read without sudo. Changes need a scoped token. |

Idle memory for helper + daemon is about 10–15 MB, and CI fails above 30 MB.

## Install (from a release binary)

```sh
sudo packaging/install.sh --binary ./hearth --add-user "$USER"
hearth status
```

## Building

```sh
cargo test --workspace
cargo build --release -p hearth                                   # native
cargo zigbuild --release --target x86_64-unknown-linux-musl -p hearth   # static, as in CI (needs zig)
```

## Licence

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
