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
fn not_implemented_fails_with_json_error_and_exit_1() {
    let out = hearth(&["status", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => panic!("stdout is not JSON: {e}"),
    };
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "not_implemented");
}

#[test]
fn bad_usage_exits_2() {
    let out = hearth(&["no-such-command"]);
    assert_eq!(out.status.code(), Some(2));
}
