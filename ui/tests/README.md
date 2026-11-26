# Web UI end-to-end tests

They run against a live Hearth (helper + daemon + Docker). Without root, on a machine where your user
is in the `docker` group:

```sh
T=/tmp/hearth-e2e; rm -rf $T; mkdir -p $T/lib $T/backups $T/state
hearth helper --helper-socket $T/helper.sock --daemon-user $USER --lib-dir $T/lib --backups-dir $T/backups &
hearth --socket $T/h.sock daemon --state-dir $T/state --helper-socket $T/helper.sock --web-listen 127.0.0.1:9443 &
export HEARTH_SETUP_CODE=$(hearth --socket $T/h.sock web setup-code --json | jq -r .data.code)
PW_CHANNEL=chrome npx playwright test     # or install Playwright's Chromium and leave PW_CHANNEL unset
```

The tests create the admin through the setup page, install two tiny test apps (`traefik/whoami`,
`busybox`), and exercise the dashboard, app tabs, users and the phone layout (360 px wide).
Remove the test apps afterwards with `docker compose -p whoami down` / `-p ticker down`.
