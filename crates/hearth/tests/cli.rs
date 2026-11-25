//! End-to-end tests of the compiled `hearth` binary's CLI contract.

use std::process::Command;

fn hearth(args: &[&str]) -> std::process::Output {
    match Command::new(env!("CARGO_BIN_EXE_hearth")).args(args).output() {
        Ok(o) => o,
        Err(e) => panic!("failed to run hearth: {e}"),
    }
}

#[test]
fn version_json_is_one_document_with_ok_true() {
    let out = hearth(&["version", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => panic!("stdout is not JSON: {e}"),
    };
    assert_eq!(v["ok"], true);
    assert_eq!(v["schema"], 1);
    assert_eq!(v["data"]["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn daemon_not_running_is_a_clear_json_error() {
    let out = hearth(&["status", "--json", "--socket", "/nonexistent/hearth.sock"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => panic!("stdout is not JSON: {e}"),
    };
    assert_eq!(v["ok"], false);
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("isn't running")
    );
}

#[test]
fn changes_without_a_terminal_need_yes() {
    // stdin is not a terminal here, so `stop` must refuse with exit code 4 instead of prompting,
    // before it even contacts the daemon.
    let out = hearth(&["app", "stop", "jellyfin", "--json", "--socket", "/nonexistent"]);
    assert_eq!(out.status.code(), Some(4));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    assert_eq!(v["error"]["kind"], "needs_confirmation");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--yes")
    );
}

#[test]
fn update_all_without_check_is_refused() {
    let out = hearth(&["app", "update", "--all", "--json", "--socket", "/nonexistent"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn bad_usage_exits_2() {
    let out = hearth(&["no-such-command"]);
    assert_eq!(out.status.code(), Some(2));
}
