//! Running package tools: plainly (for read-only queries) or as a transient systemd unit (for
//! installs, which must write /usr and /etc outside the helper's sandbox).

use super::Tx;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct Captured {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `prog args` and collects its output. Not found → Err.
pub async fn capture(prog: &str, args: &[&str], timeout: Duration) -> Result<Captured, String> {
    capture_with(prog, args, &[], timeout).await
}

/// Like [`capture`], with extra environment variables.
pub async fn capture_with(
    prog: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<Captured, String> {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .envs(envs.iter().copied())
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("{prog} didn't finish within {} s", timeout.as_secs()))?
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    Ok(Captured {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Runs `prog args` as a transient systemd service (outside the helper's sandbox), streaming its
/// output lines to `tx`. Returns the exit code.
pub async fn unconfined(
    name: &str,
    prog: &str,
    args: &[&str],
    tx: &Tx,
    timeout: Duration,
) -> Result<i32, String> {
    let unit = format!("hearth-{name}-{}", super::now());
    let mut cmd = Command::new("systemd-run");
    cmd.args([
        "--wait",
        "--pipe",
        "--collect",
        "--quiet",
        "--service-type=exec",
        &format!("--unit={unit}"),
        "--description=Hearth system upgrade",
        "--setenv=LC_ALL=C",
        "--setenv=DEBIAN_FRONTEND=noninteractive",
        "--",
        prog,
    ])
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(false);
    let _ = tx.send(format!("running {prog} {} (unit {unit})", args.join(" ")));
    let mut child = cmd.spawn().map_err(|e| format!("cannot run systemd-run: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (t1, t2) = (tx.clone(), tx.clone());
    let a = tokio::spawn(async move {
        if let Some(s) = stdout {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = clean_line(&l);
                if !l.is_empty() {
                    let _ = t1.send(format!("  {l}"));
                }
            }
        }
    });
    let b = tokio::spawn(async move {
        if let Some(s) = stderr {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = clean_line(&l);
                if !l.is_empty() {
                    let _ = t2.send(format!("  {l}"));
                }
            }
        }
    });
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(s) => s.map_err(|e| format!("systemd-run: {e}"))?,
        Err(_) => {
            return Err(format!(
                "{prog} didn't finish within {} min; it keeps running as {unit} (see `journalctl -u {unit}`)",
                timeout.as_secs() / 60
            ));
        }
    };
    let _ = a.await;
    let _ = b.await;
    Ok(status.code().unwrap_or(-1))
}

/// Keeps the last part of a line that redraws itself with `\r`, without colour codes.
fn clean_line(l: &str) -> String {
    let last = l.rsplit('\r').find(|p| !p.trim().is_empty()).unwrap_or("");
    let mut out = String::with_capacity(last.len());
    let mut chars = last.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ ... letter
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_line;

    #[test]
    fn redraws_and_colours_are_cleaned() {
        assert_eq!(clean_line("a 10%\rb 50%\rc 100%"), "c 100%");
        assert_eq!(
            clean_line("\u{1b}[1;32m::\u{1b}[0m Synchronizing"),
            ":: Synchronizing"
        );
        assert_eq!(clean_line("done\r  "), "done");
    }
}
