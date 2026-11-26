# App kinds

Besides Docker Compose apps, Hearth manages four other kinds of app. They all appear in
`hearth apps list` with their state, memory and health, and use the same `hearth app …` commands.

**Only apps with a web page are on the main app list (the dashboard).** Services and programs
without one are listed below it ("Not on the dashboard"), and you manage them the same way.
`--dashboard` / `--no-dashboard` override this for one app.

| Kind | What it is | Added with |
|---|---|---|
| `compose` | A Docker Compose project | `hearth install`, or migrated from CasaOS |
| `service` | An existing systemd unit, system or user (smbd, tailscaled, a user's `claude-remote-control`) | `hearth add service <unit> [--user NAME]` |
| `native` | A program Hearth runs as a hardened systemd service it writes | `hearth add native <name> --exec "/path/prog args" [--user NAME]` |
| `link` | A shortcut to something Hearth doesn't run (router, another PC); its address is checked | `hearth add link <name> --url <address>` |
| `package` | An installed package or Flatpak app as one app, with its service if it has one | `hearth add package <name> [--flatpak] [--unit UNIT]` |

Common options: `--title`, `--icon`, `--port`/`--path` (a web page on this host) or `--url`,
`--dashboard`/`--no-dashboard`. Each app is an `app.yaml` in `/var/lib/hearth/apps/<name>/`, with
the same history as compose apps. Change the title, address or placement later with
`hearth app set <name> …`.

## What the commands do

| | service | native | link | package |
|---|---|---|---|---|
| state | the unit's state | the unit's state | reachable or not | its unit's state, or installed / not installed |
| health | HTTP check of its web page, if it has one | same | – | same as service |
| memory | the unit's cgroup | same | – | its unit |
| `app logs` | the journal (`journalctl -u`, or the user's journal) | same | – | its unit's journal |
| `app start` / `stop` | `systemctl enable --now` / `disable --now` (a stopped app stays stopped after a reboot) | same | – | its unit |
| `app restart` | `systemctl restart` | same | – | its unit |
| `uninstall` | Hearth forgets it; **the service is left as it is** | stops it, unlinks its unit | forgets it | forgets it (the package stays) |

Nothing is deleted: removed app folders go to `/var/lib/hearth/removed/`.

## Native apps

`hearth add native` shows the unit file before writing it. The unit:

- runs as the user you give (`--user`, default: you; running as `root` needs a token with
  `system:write` as well as `apps:write`);
- has `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=full` and kernel/cgroup protections;
- restarts on failure (`--restart always|no` to change), with an optional `--memory-max`;
- reads `--env-file` if given and starts in `--workdir`;
- lives in `/var/lib/hearth/units/hearth-native-<name>.service` and is linked into systemd.

The command line is passed literally (`%` and `$` aren't expanded by systemd).

## Permissions

Reading needs `read`. Adding, changing, starting, stopping and removing need `apps:write`.
