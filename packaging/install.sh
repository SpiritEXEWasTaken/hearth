#!/bin/sh
# Install (or remove) Hearth: the binary, the `hearth` system user/group and the two systemd units.
#
#   sudo ./install.sh [--binary PATH] [--add-user NAME]...   install or upgrade, start services
#   sudo ./install.sh --uninstall [--purge]                   stop and remove (--purge: also data and user)
#
# Safe to run again: existing users, groups and data are kept.
set -eu

BIN_SRC=./hearth
ADD_USERS=""
MODE=install
PURGE=0
UNIT_DIR=/etc/systemd/system
HERE=$(cd "$(dirname "$0")" && pwd)

while [ $# -gt 0 ]; do
    case "$1" in
        --binary) BIN_SRC=$2; shift 2 ;;
        --add-user) ADD_USERS="$ADD_USERS $2"; shift 2 ;;
        --uninstall) MODE=uninstall; shift ;;
        --purge) PURGE=1; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

[ "$(id -u)" = 0 ] || { echo "run as root (sudo $0 ...)" >&2; exit 1; }

if [ "$MODE" = uninstall ]; then
    systemctl disable --now hearth.service hearth-helper.service 2>/dev/null || true
    rm -f "$UNIT_DIR/hearth.service" "$UNIT_DIR/hearth-helper.service" /usr/local/bin/hearth
    systemctl daemon-reload
    if [ "$PURGE" = 1 ]; then
        rm -rf /var/lib/hearth
        userdel hearth 2>/dev/null || true
        groupdel hearth 2>/dev/null || true
        echo "Hearth removed, including its data and the hearth user."
    else
        echo "Hearth removed. Data kept in /var/lib/hearth (use --purge to delete it)."
    fi
    exit 0
fi

[ -x "$BIN_SRC" ] || { echo "binary not found or not executable: $BIN_SRC" >&2; exit 1; }
"$BIN_SRC" version >/dev/null || { echo "$BIN_SRC doesn't run on this machine" >&2; exit 1; }

getent group hearth >/dev/null || groupadd --system hearth
if ! id hearth >/dev/null 2>&1; then
    useradd --system --gid hearth --home-dir /var/lib/hearth --no-create-home \
        --shell "$(command -v nologin || echo /usr/sbin/nologin)" --comment "Hearth daemon" hearth
fi
for u in $ADD_USERS; do
    usermod -aG hearth "$u"
    echo "Added $u to the hearth group (takes effect at their next login)."
done

install -m 0755 "$BIN_SRC" /usr/local/bin/hearth.new
mv -f /usr/local/bin/hearth.new /usr/local/bin/hearth
install -m 0644 "$HERE/systemd/hearth-helper.service" "$UNIT_DIR/hearth-helper.service"
install -m 0644 "$HERE/systemd/hearth.service" "$UNIT_DIR/hearth.service"
systemctl daemon-reload
systemctl enable hearth-helper.service hearth.service >/dev/null 2>&1
systemctl restart hearth-helper.service
systemctl restart hearth.service

# Wait for the daemon socket (up to 15 s) and report.
i=0
while [ ! -S /run/hearth/hearth.sock ] && [ $i -lt 30 ]; do sleep 0.5; i=$((i + 1)); done
if [ -S /run/hearth/hearth.sock ] && /usr/local/bin/hearth version >/dev/null; then
    echo "$(/usr/local/bin/hearth version) is running."
    systemctl --no-pager --lines=0 status hearth-helper.service hearth.service | grep -E '●|Active:' || true
else
    echo "Hearth did not start. See: journalctl -u hearth -u hearth-helper" >&2
    exit 1
fi

# First-time web UI setup: show where to go and the one-time code.
sleep 1
if code=$(/usr/local/bin/hearth web setup-code --json 2>/dev/null | sed -n 's/.*"code":"\([A-Z0-9-]*\)".*/\1/p') && [ -n "$code" ]; then
    echo
    echo "Web UI: https://$(hostname)/ (or this server's IP address, or its Tailscale name)"
    echo "Create the admin account there with this setup code: $code"
fi
