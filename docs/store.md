# App store

Hearth installs apps from its own store, [hearth-store](https://github.com/SpiritEXEWasTaken/hearth-store),
published at https://spiritexewastaken.github.io/hearth-store/ (that page is also a public app browser).

## Using it

- **Web UI:** *Store* in the menu: search, categories, and badges for each app (RAM, disk space, CPU type, GPU,
  HTTPS). An app's page shows screenshots, a notice of what isn't included ("No movies, TV shows or music are
  provided"), the install options, and the **checks for this server**, which update as you change options.
- **CLI:** `hearth store list [words] [--category C]`, `hearth store show <app>`,
  `hearth store install <app> [--name N] [--option id=value]… [--port VAR=N]… [--https|--no-https]`
  (with `--dry-run --json` for the plan), `hearth store refresh`, `hearth store credentials <app>`,
  `hearth store link <app> <entry>` / `unlink <app>`.
- `<app>` can be the store id (`org.jellyfin.jellyfin`), the app name (`jellyfin`) or the display name.

## Install options

One entry per app; GPU builds, editions and optional parts are options of that entry:

- **GPU** options only offer what this server has (`/dev/dri` Intel/AMD, `/dev/kfd` for ROCm, Nvidia with
  Docker's Nvidia runtime) and `auto` picks the best one. Choices whose images don't exist for this CPU
  are refused.
- **Switches** (Immich's machine learning, Paperless's Office support), **choices** (copyparty's edition,
  OCR language), **folders** (where your media is: default under the app's data folder, other folders must
  exist) and **ports** (a taken port moves to the next free one, except ports devices need, like DNS 53).

## Host checks

| Check | Blocks the install when… |
|---|---|
| CPU type | the app or a chosen image has no build for this CPU |
| RAM | never (a warning when the server has less than the app needs, or too little is free now) |
| Disk | the images plus the app's own data don't fit where Docker keeps images (warning when tight) |
| Ports | a fixed port (DNS 53, discovery) is taken; on Debian/Ubuntu port 53 is usually systemd-resolved's stub, and the message says how to turn it off |
| Folders | a chosen folder doesn't exist |
| Name | an app with that name exists (the store suggests `name-2`) |
| HTTPS | never: apps that need HTTPS (Vaultwarden) get it turned on after installing |

## What an install does

1. Resolves the options into variants (compose overlays) and `.env` values; generates passwords and keys.
2. Creates the data folders under `/var/lib/hearth/data/<app>` (owned by root or the app user, per entry).
3. Hands the compose file to the normal install (health gate, automatic undo on failure). The file's
   `x-hearth` block records the store id, the chosen options and which generated values to show.
4. Turns on HTTPS for the app if asked or needed.

Generated sign-in details (e.g. Paperless's admin password) stay in the app's `.env`. Admins see them on the
app's page under **Show sign-in details** or with `hearth store credentials <app>`.

Apps installed before the store (or by hand) can be **linked** to their entry: the app's page suggests it
when it runs the entry's main image. Linking only shows the entry's notes; nothing in the app changes.

## Trust

- The catalog's `index.json` is signed with minisign (Ed25519). Hearth trusts two public keys, built in: the
  store CI's key, and an offline backup key used only to replace a leaked CI key.
- Bundles, icons and screenshots are checked against hashes in the signed index; an older catalog than the
  one Hearth has is refused (no replay of old catalogs).
- The catalog is cached in `/var/lib/hearth/store/` and refreshed twice a day, so the store page works
  offline and the UI loads nothing from other sites. Hidden settings for tests: `HEARTH_STORE_URL`,
  `HEARTH_STORE_PUBKEY` (an extra trusted key).
- Entries are checked by the store's CI with the `hearth-store` tool (`crates/hearth-store`, feature `tool`):
  format, no duplicates, no pinned tags, every option combination through `docker compose config`, images
  and CPU types in the registry. See the store's CONTRIBUTING.md.
