# Web UI

Hearth's web UI is built into the `hearth` binary and served by the daemon over HTTPS. There is no
separate web server.

## Addresses and certificates

The daemon listens on port **443** (`--web-listen`, default `[::]:443`; `off` disables it). Port 80 is
added once CasaOS is gone (it will redirect to HTTPS). Which certificate a browser gets depends on the
name it asked for:

| Address | Certificate | Browser warning? |
|---|---|---|
| `https://<host>.<tailnet>.ts.net` | Real Let's Encrypt certificate from Tailscale | No, on any device in the tailnet |
| `https://<LAN IP>`, `https://<host>`, `https://<host>.local` | Hearth's own CA | Until the CA is installed on that device |

- **Tailscale:** the helper asks tailscaled's LocalAPI for the certificate (only root may), and the
  daemon reloads it twice a day; tailscaled renews it. It needs HTTPS certificates turned on in the
  Tailscale admin console (DNS page).
- **Hearth's CA** lives in `/var/lib/hearth/tls/` (`ca.key` never leaves the server). The server
  certificate is re-issued automatically when the machine's names or addresses change, or a month
  before it expires. Download the CA at `https://<server>/ca.crt` (Settings → HTTPS shows how to install
  it on Android, Windows and iPhone, plus its fingerprint). `hearth web status` prints the same.

Coming in M5c: HTTPS for every app (Hearth as the front door), Let's Encrypt with your own domain or
DuckDNS (DNS-01, no open ports), and the Home Assistant proxy fix.

## Accounts

- **First run:** the UI asks for a one-time **setup code** before creating the admin, so nobody else on
  the network can claim the server first. It's printed by `install.sh`, in the journal, and by
  `hearth web setup-code` (on the server only).
- **Roles:** *admin* (everything) and *read-only* (see apps and open them; no changes, no users or
  tokens). Admins add users in Settings → Users. The last admin can't be removed or demoted.
- **Forgotten password:** `sudo hearth web user-password NAME` (reads the new one from the terminal).
  Other commands: `hearth web users`, `user-add NAME --role admin|viewer`, `user-role`, `user-remove`.

## Security

- Passwords: argon2id, one hash at a time (a burst of sign-ins can't exhaust memory).
- Sessions: a random `__Host-hearth` cookie (Secure, HttpOnly, SameSite=Strict), 14 days after last
  use; only its SHA-256 is stored. Every change from the browser must also send the session's CSRF
  token in the `X-Hearth-CSRF` header.
- Sign-in: after 5 failures per address or username, waits grow from 30 s to 15 min. Failures are
  logged with the client address.
- Network clients never get the rights that local users get on the Unix socket: they need a session or
  an API token.
- Headers: a strict Content Security Policy (scripts only from Hearth, by hash), no framing, no
  referrer. App icons are fetched once by the server and cached, so the UI loads nothing from other
  sites.
- The daemon's systemd unit gains only `CAP_NET_BIND_SERVICE` (to use port 443).

## Pages

- **Home:** CPU, memory, storage and app widgets; a tile per app with a web page (opens it, using
  the address you reached Hearth by); other apps and services listed below.
- **Apps:** search and filters (with a web page, updates, problems); per app: Overview (containers,
  versions, service state), Logs (live, searchable, per part), Settings (the compose file with a diff
  review before applying), Updates, Backups, History (go back to any revision). Start, stop and
  restart show the change's progress live, and failed changes report the automatic rollback.
- **System** (M5b): *Updates* lists OS and Flatpak updates (security ones first), with "install the
  security updates", "install all" or a selection, always through a confirmation that shows exactly
  what will be installed; the automatic daily security updates switch (off by default); a "restart
  needed" notice (Hearth never restarts the server). *Packages* searches the package manager and
  Flathub (best matches first), shows details, and installs or removes after showing the full plan
  (dependencies included). Removing something the server needs to stay reachable (SSH, Docker, the
  kernel…) needs an extra tick. The home page shows a line when system updates are waiting.
- **Settings:** your password, users, HTTPS, API tokens.

The container console moves to M7 with the web terminal.

## Development

```sh
cd ui && npm ci
HEARTH_URL=https://127.0.0.1:9443 npm run dev    # UI with hot reload, API proxied to a running Hearth
npm run check && npm run build                   # what CI runs; cargo embeds ui/build
```

End-to-end tests: see [ui/tests/README.md](../ui/tests/README.md) (CI runs them as `ci/e2e.sh`). The
System tests (`tests/04-system.spec.ts`) really install packages and updates, so they only run against a
throwaway VM: `HEARTH_SYSTEM_URL=https://<vm> HEARTH_SYSTEM_SETUP_CODE=<code> npx playwright test --project system`.
