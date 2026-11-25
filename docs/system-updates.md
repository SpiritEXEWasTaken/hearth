# System updates and packages

Hearth lists and installs updates for the distro's packages and for Flatpak apps. **It never
reboots by itself.** When an update needs a reboot (a new kernel, or the package manager asks for
one), Hearth says so and why, and leaves the timing to you.

## Commands

```sh
hearth system info                        # distro, package backends, reboot needed?
hearth system updates [--security]        # the last list (exit code 10 when there are updates)
hearth system updates --check             # refresh the package lists first (apt update etc.)
hearth system upgrade <id|name>... [--security] [--dry-run]
hearth system upgrade --security          # every security update
hearth system upgrade --all               # everything
```

- Listing and `--check` need only the `read` scope (a check refreshes package lists but installs
  nothing).
- Installing needs a token with **`system:write`**:
  `sudo hearth token create --name NAME --scope read --scope system:write`.
- `upgrade` always shows the plan first and asks before it starts (or needs `--yes`).
- The list is refreshed by itself once a day (first 5 minutes after the daemon starts).

## Backends

| Distro | Backend | Security updates | Notes |
|---|---|---|---|
| Debian / Ubuntu | PackageKit (apt) | from the `-security` repositories | needs `apt-get -y install packagekit` |
| Fedora / RHEL | PackageKit (dnf5) | from security advisories | needs `PackageKit` |
| openSUSE | PackageKit (zypp) | from security patches | needs `PackageKit` |
| Arch / CachyOS | `checkupdates` + `pacman -Syu` | none (Arch has no security channel) | needs `pacman-contrib`; always a full upgrade |
| any | Flatpak (system installation) | none | used when `flatpak` is installed |

- **PackageKit** does the installing in its own daemon. Only signed packages are installed.
- **Arch** doesn't support upgrading only some packages. Choosing one pacman update installs all of
  them, and the plan says so. Updates are listed with `checkupdates`, which syncs a separate copy of
  the package databases (in `/var/lib/hearth/checkupdates-db`), so listing never leaves the system
  half-synced.
- **pacman and Flatpak** run as transient systemd units (`hearth-pacman-…`, `hearth-flatpak-…`),
  because the helper itself can't write `/usr`. If one runs longer than its time limit, it keeps
  going; follow it with `journalctl -u <unit>`.

## Reboot notice

`hearth system info` (and the update list) shows "reboot needed" when:

- a newer kernel of the same flavour is installed than the one running, or the running kernel was
  removed (Arch);
- `/run/reboot-required` exists (Debian/Ubuntu packages that ask for a reboot);
- PackageKit reported that an installed update needs a system restart.

The notice goes away after the next boot.

## Scheduled security updates (off by default)

```sh
sudo hearth settings security_updates 04:17   # every day at 04:17, local time
sudo hearth settings security_updates off     # the default
```

When set, Hearth refreshes the lists at that time and installs only the security updates (as job
`system-upgrade`, actor `schedule`). Other updates wait for you. It still never reboots.

## Packages

```sh
hearth package search <words>... [--flatpak|--native]
hearth package info <name> [--flatpak]
hearth package install <name>... [--flatpak] [--dry-run]
hearth package remove <name>... [--flatpak] [--allow-critical] [--dry-run]
```

- Search and info need only `read`; install and remove need `system:write`.
- Install and remove always show the full plan first: every package that would be installed
  (dependencies too), upgraded, or **removed** (packages that depend on a removed one go with it).
  PackageKit computes the plan with a simulated transaction; pacman with `--print`. Flatpak has
  no dry run: the plan lists the apps, and the runtimes they need come along.
- **Critical packages are guarded.** A removal that would take SSH, sudo, systemd, D-Bus, Docker,
  Tailscale, networking, the package manager itself, a kernel or a boot loader with it is refused
  unless you add `--allow-critical`.
- On Arch, installs use the current package databases (no `-y`), so they never cause a partial
  upgrade. If a package is gone from the mirrors, run `hearth system upgrade --all` first.
