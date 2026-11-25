---
name: hearth
description: Manage the home server through the Hearth CLI (apps, versions, logs, host stats). Use when asked about server apps, their status, memory, versions, updates or logs, or anything previously done with casaos-cli.
---

# Using Hearth safely

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

## Useful commands (milestone M1: read-only)

```sh
hearth status --json                      # host stats + every app (state, health, memory, versions)
hearth stats --json                       # CPU, memory, swap, PSI, OOM kills, disks
hearth apps list --json [--check]         # --check asks registries first (takes a few seconds)
hearth app info <app> --json [--compose]  # containers, images, CasaOS findings; compose with secrets hidden
hearth app logs <app> --json [--service s] [--tail N] [--since SECONDS]
hearth app update <app>|--all --check --json   # exit 10 when updates are available
```

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
