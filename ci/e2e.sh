#!/usr/bin/env bash
# Runs the web UI end-to-end tests against a throwaway Hearth (helper + daemon, unprivileged,
# with the runner's Docker). Usage: ci/e2e.sh path/to/hearth
set -euo pipefail
BIN=$(realpath "$1")
T=$(mktemp -d)
mkdir -p "$T/lib" "$T/backups" "$T/state"
cleanup() {
    kill "${DP:-}" "${HP:-}" 2>/dev/null || true
    docker rm -f e2e-pebble e2e-challtestsrv >/dev/null 2>&1 || true
    for p in whoami ticker; do docker compose -p "$p" down -v >/dev/null 2>&1 || true; done
    echo "--- daemon log"; tail -50 "$T/daemon.log" || true
    echo "--- helper log"; tail -30 "$T/helper.log" || true
}
trap cleanup EXIT

# Pebble (Let's Encrypt's test server) and its fake DNS, for the own-domain test.
if docker run -d --rm --name e2e-challtestsrv --network host \
        ghcr.io/letsencrypt/pebble-challtestsrv:latest -defaultIPv6 "" -defaultIPv4 127.0.0.1 >/dev/null 2>&1 &&
    docker run -d --rm --name e2e-pebble --network host -e PEBBLE_VA_NOSLEEP=1 -e PEBBLE_WFE_NONCEREJECT=0 \
        ghcr.io/letsencrypt/pebble:latest -dnsserver 127.0.0.1:8053 >/dev/null 2>&1; then
    for _ in $(seq 30); do docker cp e2e-pebble:/test/certs/pebble.minica.pem "$T/pebble.pem" 2>/dev/null && break; sleep 1; done
    for _ in $(seq 30); do curl -sf --cacert "$T/pebble.pem" https://localhost:14000/dir >/dev/null && break; sleep 1; done
    export HEARTH_ACME_DIRECTORY=https://localhost:14000/dir HEARTH_ACME_ROOT="$T/pebble.pem"
    export HEARTH_DNS_TESTSRV=http://localhost:8055 HEARTH_PEBBLE=1
fi
"$BIN" helper --helper-socket "$T/helper.sock" --daemon-user "$(id -un)" --lib-dir "$T/lib" --backups-dir "$T/backups" > "$T/helper.log" 2>&1 &
HP=$!
sleep 1
"$BIN" --socket "$T/h.sock" daemon --state-dir "$T/state" --helper-socket "$T/helper.sock" --web-listen 127.0.0.1:9443 > "$T/daemon.log" 2>&1 &
DP=$!
for _ in $(seq 30); do [ -S "$T/h.sock" ] && break; sleep 0.5; done
export HEARTH_SETUP_CODE
HEARTH_SETUP_CODE=$("$BIN" --socket "$T/h.sock" web setup-code --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"]["code"])')
export HEARTH_E2E_URL=https://127.0.0.1:9443
cd ui && npx playwright test
