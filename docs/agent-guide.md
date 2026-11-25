# Hearth agent guide

How an AI agent (or a script) should drive the `hearth` CLI safely. A copy of this guide ships as a
Claude Code skill in `packaging/claude-skill/hearth/SKILL.md`.

## Ground rules

1. **Always pass `--json`** and parse stdout. It holds exactly one JSON document:
   `{"ok": true, "schema": 1, "data": …}` or
   `{"ok": false, "schema": 1, "error": {"kind", "message", "exit_code"}}`.
   Progress messages go to stderr; ignore them or show them to the person.
2. **Branch on the exit code**, not on message text:

   | Code | Meaning | What to do |
   |---|---|---|
   | 0 | OK | continue |
   | 1 | error | report `error.message`; don't retry blindly |
   | 2 | bad usage | fix the command (`hearth <cmd> --help`) |
   | 3 | not found | check the id with `hearth apps list --json` |
   | 4 | needs confirmation | **stop and ask the person**; only then re-run with `--yes` |
   | 5 | not authorized | the token lacks a scope: ask the person, never work around it |
   | 6 | change failed, rolled back | report it, including the logs Hearth returns |
   | 7 | change failed, rollback failed | **urgent**: tell the person straight away |
   | 10 | updates available (`--check`) | not an error; report what's available |
3. **Look before you change.** Every command that changes state accepts `--dry-run`. Run it first,
   show the person the planned change (diffs included), and only run the real command once they
   agree. Never add `--yes` on your own initiative.
4. **Read-only is always safe.** Use these freely: `status`, `stats`, `apps list`, `app info`,
   `app logs`, `app update --check`.
5. **Never touch apps marked `"read_only": true`** unless the person explicitly asks. This includes
   rootless apps (`"endpoint": {"kind": "rootless"}`) and CasaOS apps that haven't been adopted.
6. **Never apply updates in bulk** (`--all`, system upgrades) without asking, even when
   `--check` shows updates. Report them instead.
7. **Major versions are the person's decision.** `newer_major` (e.g. Postgres 14 → 17, Immich
   v3 → v4) often needs a migration. Report it; never switch tags yourself.
8. **Secrets stay hidden.** Compose output from Hearth has passwords replaced with `***`. Don't try
   to recover them, and never print the API token.

## Authentication

- Members of the `hearth` group can read everything through `/run/hearth/hearth.sock`.
- Changes need a token with the right scope. The token goes in `~/.config/hearth/token` (mode 600)
  or the `HEARTH_TOKEN` environment variable. An agent token normally has `read` + `apps:write`;
  `system:write` (OS updates and packages) is granted only if the person wants that.
- Only root can create tokens: `sudo hearth token create --name claude --scope read --scope apps:write`.

## Commands

Read-only commands are always safe to run:

```sh
hearth status --json                      # host stats + every app (state, health, memory, versions)
hearth stats --json                       # CPU, memory, swap, PSI, OOM kills, disks
hearth apps list --json [--check]         # --check asks registries first (takes a few seconds)
hearth app info <app> --json [--compose]  # containers, images, findings; compose with secrets hidden
hearth app logs <app> --json [--service s] [--tail N] [--since SECONDS]
hearth app update <app>|--all --check --json   # exit 10 when updates are available
hearth app history <app> --json           # every change to the app's files
hearth backups <app> --json
hearth images list --json
hearth job list --json / hearth job show <id> --json
```

Changes need an `apps:write` token. Always run `--dry-run --json` first, show the person the plan
(the diff, decisions, warnings), and add `--yes` only after they agree:

```sh
hearth install --name <app> -f compose.yml [--set KEY=VALUE]... [--no-start]
hearth app apply <app> -f compose.yml [-m "why"]   # diff → recreate changed services → health check → rollback on failure
                                                   # (pulls only missing images; newer builds come from `app update`)
hearth app update <app>                            # same tag, newer build; keeps a rollback image
hearth app start|stop|restart <app>                # stop means stopped, also after reboots
hearth app revert <app> <revision>
hearth backup <app> [--exclude CONTAINER_PATH]... [--no-stop]
hearth restore <app> <backup-file>                 # current data is moved aside, not deleted
hearth uninstall <app> [--delete-data]             # data is KEPT unless --delete-data
hearth images prune [--keep N] [--unused]
```

Rules for changes:

- One app at a time. Never loop `update` over every app without asking.
- `--delete-data` and `restore` are destructive: only use them when the person asked for exactly that.
- Exit code 6 means the change failed and Hearth put the previous version back. The app is fine;
  report the error and the log lines in it. Exit code 7 means the rollback failed too: tell the
  person at once and don't try more changes to that app.
- `--background` returns a job id immediately; follow it with `hearth job wait <id> --json`.
- Read-only apps (CasaOS apps before migration, rootless apps) refuse changes with exit code 5
  and `error.kind = "read_only"`. That's expected; don't work around it.

## System updates

System updates (the distro's packages and Flatpak; see `docs/system-updates.md`). Reading is always
safe; installing needs a token with `system:write`, which the person grants only if they want that:

```sh
hearth system info --json                 # distro, backends, "reboot.needed" and its reasons
hearth system updates --json [--check] [--security]   # exit 10 when there are updates
hearth system upgrade <id|name>... [--security] --dry-run --json   # the plan, then the same with --yes
```

Rules for system updates:

- **Never install system updates without the person's OK**, even security ones, and never
  `--all` on your own. Report what `hearth system updates` shows instead.
- **Hearth never reboots, and neither should you.** When `reboot.needed` is true, tell the person
  and the reasons; rebooting some servers needs someone present.
- A kernel update means a reboot will be needed; say so before installing.
- On Arch, choosing one pacman update installs all of them: show the person the plan's notes.

## Migrating from CasaOS

`hearth migrate casaos --json` is read-only. `backup`, `adopt <app> [--fix ID]` and `undo <app>`
are changes: show the person the report first, and adopt one app at a time. See
`docs/migration-casaos.md`.

## Reading versions

Each app's `images[]` entry has:

- `reference`: the image as written (`linuxserver/jellyfin:latest`)
- `running_version` / `running_digest`: what's running now
- `latest.update_available`: the same tag now points to a different image (a normal update)
- `latest.newer_in_track`: a newer tag on the same line (`v3.4.1` → `v3.4.2`), shown as a suggestion
- `latest.newer_major`: a new major version, for information only
- `latest.error`: why the registry check failed (rate limits, network), if it did

`update_available` never means "the store has a different version": Hearth compares registry
digests for *your* tag, so it can't suggest a downgrade.
