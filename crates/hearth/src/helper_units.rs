//! systemd units for service, native and package apps (M4c), through `systemctl` and
//! `journalctl`. PID1 does the actual work (starting, enabling, linking), so the helper's
//! read-only view of /etc doesn't get in the way. User units are reached with
//! `systemctl --user -M <user>@`.

use crate::helper::HelperConfig;
use crate::helper_apps;
use crate::users;
use hearth_core::model::UnitStatus;
use hearth_core::proto::{self, LogRecord, Logs, UnitAction};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LOG_LINES: u32 = 5000;

fn check(unit: &str, user: Option<&str>) -> Result<(), String> {
    if !proto::is_valid_unit_name(unit) {
        return Err(format!("'{unit}' isn't a valid unit name"));
    }
    if let Some(u) = user {
        if !proto::is_valid_user_name(u) {
            return Err(format!("'{u}' isn't a valid user name"));
        }
        if users::uid_of(u).is_none() {
            return Err(format!("there's no user called '{u}'"));
        }
    }
    Ok(())
}

async fn run(prog: &str, args: &[String]) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env("LC_ALL", "C")
        .env("SYSTEMD_PAGER", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{prog} didn't finish within {} s", TIMEOUT.as_secs()))?
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// `systemctl` arguments, with `--user -M <user>@` for user units.
fn systemctl_args(user: Option<&str>, rest: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if let Some(u) = user {
        a.push("--user".into());
        a.push(format!("--machine={u}@"));
    }
    a.push("--no-ask-password".into());
    a.extend(rest.iter().map(|s| s.to_string()));
    a
}

pub fn parse_show(unit: &str, user: Option<&str>, out: &str) -> UnitStatus {
    let mut s = UnitStatus {
        name: unit.to_string(),
        user: user.map(str::to_string),
        ..Default::default()
    };
    for line in out.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "LoadState" => s.load_state = v.to_string(),
            "ActiveState" => s.active_state = v.to_string(),
            "SubState" => s.sub_state = v.to_string(),
            "UnitFileState" => s.unit_file_state = v.to_string(),
            "Description" => s.description = v.to_string(),
            "MemoryCurrent" => s.memory_bytes = v.parse().ok().filter(|m| *m < u64::MAX / 2),
            "MainPID" => s.main_pid = v.parse().ok().filter(|p| *p > 0),
            _ => {}
        }
    }
    s
}

pub async fn status(unit: &str, user: Option<&str>) -> Result<Value, String> {
    check(unit, user)?;
    let props = "--property=LoadState,ActiveState,SubState,UnitFileState,Description,MemoryCurrent,MainPID";
    let (code, out, err) = run("systemctl", &systemctl_args(user, &["show", props, "--", unit])).await?;
    if code != 0 {
        return Err(format!("systemctl show {unit}: {}", err.trim()));
    }
    Ok(json!(parse_show(unit, user, &out)))
}

pub async fn action(unit: &str, user: Option<&str>, action: UnitAction) -> Result<Value, String> {
    check(unit, user)?;
    let rest: &[&str] = match action {
        UnitAction::Start => &["enable", "--now", "--"],
        UnitAction::Stop => &["disable", "--now", "--"],
        UnitAction::Restart => &["restart", "--"],
    };
    let mut args = systemctl_args(user, rest);
    args.push(unit.to_string());
    let (code, _, err) = run("systemctl", &args).await?;
    // Static units (no [Install] section) can't be enabled: just start/stop them.
    if code != 0 && action != UnitAction::Restart && err.contains("no installation config") {
        let verb = if action == UnitAction::Start {
            "start"
        } else {
            "stop"
        };
        let mut args = systemctl_args(user, &[verb, "--"]);
        args.push(unit.to_string());
        let (code, _, err) = run("systemctl", &args).await?;
        if code != 0 {
            return Err(format!("systemctl {verb} {unit}: {}", err.trim()));
        }
    } else if code != 0 {
        return Err(format!("systemctl {:?} {unit}: {}", action, err.trim()).to_lowercase());
    }
    status(unit, user).await
}

pub async fn logs(unit: &str, user: Option<&str>, tail: u32, since: Option<i64>) -> Result<Value, String> {
    check(unit, user)?;
    let mut args: Vec<String> = vec![
        "--no-pager".into(),
        "--output=short-iso".into(),
        format!("--lines={}", tail.min(MAX_LOG_LINES)),
    ];
    match user {
        Some(u) => {
            let uid = users::uid_of(u).ok_or_else(|| format!("there's no user called '{u}'"))?;
            args.push(format!("_SYSTEMD_USER_UNIT={unit}"));
            args.push(format!("_UID={uid}"));
        }
        None => {
            args.push("--unit".into());
            args.push(unit.to_string());
        }
    }
    if let Some(s) = since {
        args.push(format!("--since=@{s}"));
    }
    let (code, out, err) = run("journalctl", &args).await?;
    if code != 0 {
        return Err(format!("journalctl: {}", err.trim()));
    }
    let lines: Vec<LogRecord> = out
        .lines()
        .filter(|l| !l.starts_with("-- "))
        .map(|l| LogRecord {
            stream: "journal".into(),
            line: l.to_string(),
        })
        .collect();
    Ok(json!(Logs {
        lines,
        truncated: false
    }))
}

pub async fn install_native(cfg: &HelperConfig, app: &str, text: &str) -> Result<Value, String> {
    let path = helper_apps::write_native_unit(cfg, app, text)?;
    let unit = format!("hearth-native-{app}.service");
    // Re-linking an existing unit: unlink first so systemd re-reads the file.
    let _ = run("systemctl", &systemctl_args(None, &["daemon-reload"])).await;
    let (code, _, err) = run(
        "systemctl",
        &systemctl_args(None, &["link", "--force", "--", &path.to_string_lossy()]),
    )
    .await?;
    if code != 0 {
        return Err(format!("systemctl link {unit}: {}", err.trim()));
    }
    let _ = run("systemctl", &systemctl_args(None, &["daemon-reload"])).await;
    status(&unit, None).await
}

pub async fn remove_native(app: &str) -> Result<Value, String> {
    if !proto::is_valid_hearth_app_name(app) {
        return Err(format!("invalid app name '{app}'"));
    }
    let unit = format!("hearth-native-{app}.service");
    // disable also removes the link into /etc/systemd/system.
    let (code, _, err) = run(
        "systemctl",
        &systemctl_args(None, &["disable", "--now", "--", &unit]),
    )
    .await?;
    if code != 0 && !err.contains("not loaded") && !err.contains("does not exist") {
        return Err(format!("systemctl disable {unit}: {}", err.trim()));
    }
    let _ = run("systemctl", &systemctl_args(None, &["daemon-reload"])).await;
    Ok(json!({ "removed": unit }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_output_parses() {
        let out = "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n\
                   Description=Samba SMB Daemon\nMemoryCurrent=12345678\nMainPID=812\n";
        let s = parse_show("smbd.service", None, out);
        assert_eq!(s.active_state, "active");
        assert_eq!(s.memory_bytes, Some(12345678));
        assert_eq!(s.main_pid, Some(812));
        // systemd prints [not set] or u64::MAX when there's no value.
        let s = parse_show(
            "x.service",
            Some("jarvis"),
            "MemoryCurrent=[not set]\nMainPID=0\n",
        );
        assert_eq!(s.memory_bytes, None);
        assert_eq!(s.main_pid, None);
        assert_eq!(s.user.as_deref(), Some("jarvis"));
        let s = parse_show("x.service", None, "MemoryCurrent=18446744073709551615\n");
        assert_eq!(s.memory_bytes, None);
    }

    #[test]
    fn user_units_use_the_machine_option() {
        assert_eq!(
            systemctl_args(Some("jarvis"), &["show", "--", "a.service"]),
            vec![
                "--user",
                "--machine=jarvis@",
                "--no-ask-password",
                "show",
                "--",
                "a.service"
            ]
        );
        assert_eq!(
            systemctl_args(None, &["restart"]),
            vec!["--no-ask-password", "restart"]
        );
    }
}
