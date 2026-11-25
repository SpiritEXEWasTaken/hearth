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
fn applying_updates_is_not_implemented_yet() {
    let out = hearth(&["app", "update", "jellyfin", "--json", "--socket", "/nonexistent"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    assert_eq!(v["error"]["kind"], "not_implemented");
}

#[test]
fn bad_usage_exits_2() {
    let out = hearth(&["no-such-command"]);
    assert_eq!(out.status.code(), Some(2));
}
