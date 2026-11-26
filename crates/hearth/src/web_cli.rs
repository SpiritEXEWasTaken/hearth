//! `hearth web …`: setup code, HTTPS status and web UI accounts.

use crate::cli::{GlobalArgs as Global, WebCmd};
use crate::client::Client;
use hearth_core::output::Envelope;
use hearth_core::{Error, ExitCode};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};

fn out(json: bool, v: &Value, human: impl FnOnce(&Value)) {
    if json {
        crate::print_json(&Envelope::success(v));
    } else {
        human(v);
    }
}

/// Reads a password from stdin; on a terminal it asks twice without echoing.
fn read_password() -> Result<String, Error> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| Error::Other(e.to_string()))?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_string());
    }
    let ask = |prompt: &str| -> Result<String, Error> {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let fd = std::io::stdin();
        let old = rustix::termios::tcgetattr(&fd).map_err(|e| Error::Other(e.to_string()))?;
        let mut quiet = old.clone();
        quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
        let _ = rustix::termios::tcsetattr(&fd, rustix::termios::OptionalActions::Now, &quiet);
        let mut line = String::new();
        let r = fd.lock().read_line(&mut line);
        let _ = rustix::termios::tcsetattr(&fd, rustix::termios::OptionalActions::Now, &old);
        eprintln!();
        r.map_err(|e| Error::Other(e.to_string()))?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    };
    let a = ask("Password: ")?;
    let b = ask("Same password again: ")?;
    if a != b {
        return Err(Error::Usage("the passwords don't match".into()));
    }
    Ok(a)
}

async fn user_id(c: &Client, username: &str) -> Result<String, Error> {
    let users: Vec<Value> = c.get("/v1/users").await?;
    users
        .iter()
        .find(|u| {
            u["username"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(username))
        })
        .and_then(|u| u["id"].as_str().map(str::to_string))
        .ok_or_else(|| Error::NotFound {
            what: "user",
            name: username.to_string(),
        })
}

pub async fn run(cmd: &WebCmd, g: &Global) -> Result<ExitCode, Error> {
    let c = Client::new(&g.socket);
    let json = g.json;
    match cmd {
        WebCmd::SetupCode => {
            let v: Value = c.get("/v1/setup/code").await?;
            out(json, &v, |v| match v["code"].as_str() {
                Some(code) => {
                    println!("Setup code: {code}");
                    println!(
                        "Open the web UI (https://<this server>) and enter it to create the admin account."
                    );
                }
                None => println!(
                    "Hearth is already set up. Forgot the password? `sudo hearth web user-password NAME`."
                ),
            });
        }
        WebCmd::Status => {
            let v: Value = c.get("/v1/tls").await?;
            out(json, &v, |v| {
                let list = |k: &str| -> Vec<String> {
                    v[k].as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default()
                };
                let ts = list("tailscale_names");
                if ts.is_empty() {
                    println!(
                        "Tailscale: no certificate (Tailscale off, or HTTPS not enabled for the tailnet)"
                    );
                } else {
                    for n in ts {
                        println!("https://{n}  (Tailscale certificate, trusted everywhere)");
                    }
                }
                for n in list("local_names") {
                    let host = if n.contains(':') { format!("[{n}]") } else { n };
                    println!("https://{host}  (Hearth's own certificate)");
                }
                if let Some(f) = v["ca_fingerprint_sha256"].as_str() {
                    println!("Hearth CA fingerprint (SHA-256): {f}");
                    println!(
                        "Install the CA from https://<server>/ca.crt to remove browser warnings on the LAN."
                    );
                }
            });
        }
        WebCmd::Users => {
            let users: Vec<Value> = c.get("/v1/users").await?;
            let v = Value::Array(users);
            out(json, &v, |v| {
                let a = v.as_array().cloned().unwrap_or_default();
                if a.is_empty() {
                    println!("No accounts yet. Create the admin in the web UI with `hearth web setup-code`.");
                }
                for u in a {
                    println!(
                        "{:<20} {:<7} {}",
                        u["username"].as_str().unwrap_or(""),
                        u["role"].as_str().unwrap_or(""),
                        if u["last_login_at"].is_null() {
                            "never signed in"
                        } else {
                            ""
                        }
                    );
                }
            });
        }
        WebCmd::UserAdd { username, role } => {
            if g.dry_run {
                out(
                    json,
                    &serde_json::json!({"would_add": username, "role": role}),
                    |_| println!("Would add {role} account '{username}'."),
                );
                return Ok(ExitCode::Ok);
            }
            let password = read_password()?;
            let v: Value = c
                .post(
                    "/v1/users",
                    Some(serde_json::json!({"username": username, "password": password, "role": role})),
                )
                .await?;
            out(json, &v, |_| println!("Added {role} account '{username}'."));
        }
        WebCmd::UserPassword { username } => {
            let id = user_id(&c, username).await?;
            if g.dry_run {
                out(json, &serde_json::json!({"would_set_password": username}), |_| {
                    println!("Would set a new password for '{username}'.")
                });
                return Ok(ExitCode::Ok);
            }
            let password = read_password()?;
            let v: Value = c
                .call_put(
                    &format!("/v1/users/{id}"),
                    serde_json::json!({"password": password}),
                )
                .await?;
            out(json, &v, |_| {
                println!("New password set for '{username}'; their other sessions were signed out.")
            });
        }
        WebCmd::UserRole { username, role } => {
            let id = user_id(&c, username).await?;
            if g.dry_run {
                out(
                    json,
                    &serde_json::json!({"would_set_role": username, "role": role}),
                    |_| println!("Would make '{username}' {role}."),
                );
                return Ok(ExitCode::Ok);
            }
            let v: Value = c
                .call_put(&format!("/v1/users/{id}"), serde_json::json!({"role": role}))
                .await?;
            out(json, &v, |_| println!("'{username}' is now {role}."));
        }
        WebCmd::UserRemove { username } => {
            let id = user_id(&c, username).await?;
            if g.dry_run {
                out(json, &serde_json::json!({"would_remove": username}), |_| {
                    println!("Would remove account '{username}'.")
                });
                return Ok(ExitCode::Ok);
            }
            let v: Value = c.delete(&format!("/v1/users/{id}")).await?;
            out(json, &v, |_| println!("Removed account '{username}'."));
        }
    }
    Ok(ExitCode::Ok)
}
