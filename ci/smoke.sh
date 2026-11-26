#!/usr/bin/env bash
# End-to-end smoke test: real helper (root) + daemon (this user) + CLI against the host's Docker.
# Fails if any command misbehaves or if idle memory (helper + daemon RSS) exceeds the budget.
#
#   ci/smoke.sh path/to/hearth            (needs sudo and a running Docker daemon)
set -euo pipefail

BIN=$(realpath "$1")
BUDGET_KB=${HEARTH_RSS_BUDGET_KB:-30720}   # 30 MiB
T=$(mktemp -d)
# Don't send the user's own token to this throwaway daemon.
unset HEARTH_TOKEN
export XDG_CONFIG_HOME="$T/config"
SOCK=$T/hearth.sock
HSOCK=$T/helper/helper.sock
pids=()
cleanup() {
    for p in "${pids[@]}"; do sudo kill "$p" 2>/dev/null || true; done
    docker rm -f hearth-smoke >/dev/null 2>&1 || true
    docker rm -f smokeweb-web-1 >/dev/null 2>&1 || true
    sudo rm -rf "$T"
}
trap cleanup EXIT

docker run -d --name hearth-smoke --label com.docker.compose.project=smoke \
    --label com.docker.compose.service=web busybox:1.37 sleep 600 >/dev/null

sudo "$BIN" helper --helper-socket "$HSOCK" --daemon-user "$(id -un)" --docker-socket /var/run/docker.sock \
    >"$T/helper.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [ -S "$HSOCK" ] && break; sleep 0.1; done

"$BIN" daemon --socket "$SOCK" --helper-socket "$HSOCK" --state-dir "$T/state" --web-listen 127.0.0.1:9443 >"$T/daemon.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [ -S "$SOCK" ] && break; sleep 0.1; done

h() { "$BIN" --socket "$SOCK" "$@"; }

echo "== status"
h status
h status --json | jq -e '.ok and (.data.apps | map(.id) | index("smoke") != null)' >/dev/null

echo "== app info / logs"
h app info smoke --json | jq -e '.data.managed_by == "compose" and .data.state == "running"' >/dev/null
h app logs smoke --tail 5 --json | jq -e '.ok' >/dev/null

echo "== error contract"
set +e
h app info does-not-exist --json >"$T/nf.json"; code=$?
set -e
[ "$code" = 3 ] || { echo "expected exit 3 for missing app, got $code"; exit 1; }
jq -e '.ok == false and .error.kind == "not_found"' "$T/nf.json" >/dev/null

echo "== tokens need admin"
set +e
h token create --name x --scope read >/dev/null 2>&1; code=$?
set -e
[ "$code" = 5 ] || { echo "expected exit 5 for token create as non-root, got $code"; exit 1; }

echo "== M2: install, failed change rolls back, stop/start, uninstall"
cat >"$T/web.yml" <<'YML'
services:
  web:
    image: nginx:1.27-alpine
    ports:
      - "18081:80"
    environment:
      TZ: $TZ
      ADMIN_PASSWORD: hunter2
x-hearth:
  web:
    port: 18081
YML
sudo "$BIN" --socket "$SOCK" install --name smokeweb -f "$T/web.yml" --yes
h app info smokeweb --json | jq -e '.data.managed_by == "hearth" and .data.read_only == false' >/dev/null
sudo grep -q 'ADMIN_PASSWORD: ${ADMIN_PASSWORD}' /var/lib/hearth/apps/smokeweb/compose.yaml
[ "$(sudo stat -c %a /var/lib/hearth/apps/smokeweb/.env)" = 600 ]
sed 's|    image: nginx:1.27-alpine|    image: nginx:1.27-alpine\n    command: ["sh", "-c", "exit 3"]|' "$T/web.yml" >"$T/bad.yml"
set +e
sudo "$BIN" --socket "$SOCK" app apply smokeweb -f "$T/bad.yml" --yes; code=$?
set -e
[ "$code" = 6 ] || { echo "expected exit 6 (rolled back), got $code"; exit 1; }
curl -fsS -o /dev/null http://127.0.0.1:18081/ || { echo "app not serving after rollback"; exit 1; }
sudo "$BIN" --socket "$SOCK" app stop smokeweb --yes
h app info smokeweb --json | jq -e '.data.state == "stopped"' >/dev/null
sudo "$BIN" --socket "$SOCK" app start smokeweb
h app info smokeweb --json | jq -e '.data.state == "running"' >/dev/null
set +e
h app stop smokeweb --json >/dev/null; code=$?   # no --yes, no terminal
set -e
[ "$code" = 4 ] || { echo "expected exit 4 without --yes, got $code"; exit 1; }
sudo "$BIN" --socket "$SOCK" uninstall smokeweb --delete-data --yes
set +e
h app info smokeweb --json >/dev/null; code=$?
set -e
[ "$code" = 3 ] || { echo "expected the app to be gone (exit 3), got $code"; exit 1; }

echo "== web UI over HTTPS"
for _ in $(seq 50); do curl -sk -o /dev/null https://127.0.0.1:9443/ && break; sleep 0.2; done
curl -skf https://127.0.0.1:9443/v1/setup | grep -q '"needed":true'
curl -skf https://127.0.0.1:9443/ | grep -qi '<html'
# Network clients get nothing without signing in.
[ "$(curl -sk -o /dev/null -w '%{http_code}' https://127.0.0.1:9443/v1/status)" = 403 ]

echo "== idle memory"
sleep 3
rss=0
for p in "${pids[@]}"; do
    child=$(pgrep -P "$p" hearth || echo "$p")   # sudo wraps the helper
    kb=$(awk '/VmRSS/{print $2}' "/proc/$child/status")
    echo "pid $child: ${kb} KiB"
    rss=$((rss + kb))
done
echo "helper + daemon: ${rss} KiB (budget ${BUDGET_KB} KiB)"
[ "$rss" -le "$BUDGET_KB" ] || { echo "over the memory budget"; cat "$T"/*.log; exit 1; }
echo "smoke test passed"
