# Migrating from CasaOS

Hearth takes over CasaOS apps one at a time, without reinstalling them. CasaOS's own files are
only read, never changed, and CasaOS keeps running until you turn it off yourself.

## Steps

1. **Report (read-only).**

   ```sh
   hearth migrate casaos
   hearth migrate casaos adopt <app> --dry-run
   ```

   The report shows, for each app:
   - its containers;
   - the decisions Hearth would make (values moved into `.env`, devices that don't exist here);
   - the optional fixes;
   - the Postgres databases it has.

   The `--dry-run` form shows the full file diff for one app.
2. **Back up.**

   ```sh
   hearth migrate casaos backup
   ```

   This writes two things to `/var/backups/hearth/` (root-only):
   - `casaos/casaos-<time>.tar.gz`: `/var/lib/casaos`, `/etc/casaos`, the CasaOS systemd units, and
     the settings of every container and image as JSON;
   - `<app>/<app>-<service>-db-<time>.sql.gz`: a `pg_dumpall` of every Postgres database.
3. **Adopt.**

   ```sh
   hearth migrate casaos adopt <app>
   ```

   - Hearth writes `/var/lib/hearth/apps/<app>/compose.yaml` and `.env`.
   - **Nothing restarts.** The containers stay exactly as CasaOS created them.
   - Start, stop and restart from Hearth act on those same containers.
   - The first change you make (an update, a fix, an apply) recreates them once, with a health
     check and automatic rollback.
4. **Fixes (optional).**

   ```sh
   hearth migrate casaos adopt <app> --fix <id>
   ```

   Each fix recreates the app once, with a health check and automatic rollback. Known fixes:
   - `jellyfin-udp`: publish Jellyfin's discovery (7359) and DLNA (1900) ports as UDP;
   - `drop-rpi-paths`: remove Raspberry Pi-only mounts such as `/opt/vc/lib` on other hardware.
5. **Use Hearth for a while.** Don't change adopted apps from CasaOS's dashboard in the meantime.
6. **Turn CasaOS off.** This is a later milestone, and it never happens without your confirmation.
   CasaOS is disabled, not deleted.

## Undo

```sh
hearth migrate casaos undo <app>
```

- **The app was never recreated by Hearth:** Hearth only moves its files aside (to
  `/var/lib/hearth/removed/`). CasaOS manages the app as before.
- **Hearth recreated the app** (a fix, an update or an apply): CasaOS can only manage containers
  it created itself. So Hearth:
  1. removes its containers (data in bind mounts and volumes is kept);
  2. runs `casaos-cli app-management install -f /var/lib/casaos/apps/<app>/docker-compose.yml`;
  3. waits until CasaOS has really created the containers, and checks their health.

  If that fails, Hearth brings its own version back up: exit code 6 means the app is running under
  Hearth again, and 7 means it's down and needs attention. The app restarts once during an undo.
  Fixes are lost, because CasaOS's original file comes back.

## Rolling back everything

1. Run `hearth migrate casaos undo <app>` for each adopted app.
2. If CasaOS was turned off, turn it back on:

   ```sh
   sudo systemctl enable --now casaos casaos-gateway casaos-app-management \
       casaos-local-storage casaos-message-bus casaos-user-service
   ```
3. Optionally stop or remove Hearth: `sudo packaging/install.sh --uninstall` (add `--purge` to
   also delete its data).
4. If anything of CasaOS's own was lost, restore it from
   `/var/backups/hearth/casaos/casaos-<time>.tar.gz`. The files are stored under `files/` with
   their original paths.
5. To restore a database, feed the SQL dump into its container:

   ```sh
   zcat <dump>.sql.gz | docker exec -i <container> psql -U postgres
   ```
