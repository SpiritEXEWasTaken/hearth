# Hearth

A lightweight home-server manager: Docker Compose apps, a curated app store, OS updates on several distros,
files, storage, users and a web terminal. It's one static Rust binary, has a web UI and an Android app, and
uses under 30 MB of RAM when idle.

> **Status: early development (milestone M0).** Not usable yet.

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
| `crates/hearth-core` | Shared types: errors, exit codes, JSON output |
| `crates/hearth` | The `hearth` binary: CLI, daemon (`hearth daemon`) and root helper (`hearth helper`) |
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

## Building

```sh
cargo test --workspace
cargo build --release -p hearth                                   # native
cargo zigbuild --release --target x86_64-unknown-linux-musl -p hearth   # static, as in CI
```

## Licence

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
