//! `hearth`: one binary for the CLI, the daemon (`hearth daemon`) and the root helper (`hearth helper`).

mod changes;
mod cli;
mod client;
mod compose_norm;
mod daemon;
mod helper;
mod helper_apps;
mod render;
mod unixhttp;
mod users;

use clap::Parser;
use cli::{AppCmd, AppsCmd, Cli, Command, TokenCmd};
use client::Client;
use hearth_core::model::{AppDetail, AppSummary, HostStats, LogLine, NewToken, Status, TokenInfo};
use hearth_core::output::Envelope;
use hearth_core::{Error, ExitCode};
use serde::Serialize;
use std::time::Duration;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let json = cli.global.json;

    let runtime = match &cli.command {
        // Services: two worker threads are plenty and keep memory low.
        Command::Daemon(_) | Command::Helper(_) => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build(),
        _ => tokio::runtime::Builder::new_current_thread().enable_all().build(),
    };
    let runtime = match runtime {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hearth: cannot start async runtime: {e}");
            return ExitCode::Error.into();
        }
    };

    match runtime.block_on(run(&cli)) {
        Ok(code) => code.into(),
        Err(e) => {
            if json {
                print_json(&Envelope::failure(&e));
            } else {
                eprintln!("hearth: {e}");
            }
            e.exit_code().into()
        }
    }
}

/// Prints `data` as JSON or with the human renderer.
fn out<T: Serialize>(json: bool, data: &T, human: impl FnOnce(&T)) {
    if json {
        print_json(&Envelope::success(data));
    } else {
        human(data);
    }
}

async fn run(cli: &Cli) -> Result<ExitCode, Error> {
    let g = &cli.global;
    let json = g.json;
    if let Some(result) = changes::run(&cli.command, g).await {
        return result;
    }
    match &cli.command {
        Command::Version => {
            let info = VersionInfo::current();
            out(json, &info, |i| println!("hearth {} ({})", i.version, i.target));
        }
        Command::Status => {
            let s: Status = Client::new(&g.socket).get("/v1/status").await?;
            out(json, &s, render::status);
        }
        Command::Stats => {
            let s: HostStats = Client::new(&g.socket).get("/v1/stats").await?;
            out(json, &s, render::host);
        }
        Command::Apps(AppsCmd::List { check }) => {
            let c = Client::new(&g.socket);
            if *check {
                if !json {
                    eprintln!("Checking registries for the latest versions…");
                }
                let _: Vec<AppSummary> = c.post("/v1/versions/check", None).await?;
            }
            let apps: Vec<AppSummary> = c.get("/v1/apps").await?;
            out(json, &apps, |a| render::apps(a));
        }
        Command::App(cmd) => return app(cli, cmd).await,
        Command::Token(cmd) => {
            let c = Client::new(&g.socket);
            match cmd {
                TokenCmd::Create { name, scopes } => {
                    if g.dry_run {
                        out(
                            json,
                            &serde_json::json!({"would_create": {"name": name, "scopes": scopes}}),
                            |_| println!("Would create token '{name}' with scopes: {}", scopes.join(" ")),
                        );
                        return Ok(ExitCode::Ok);
                    }
                    let t: NewToken = c
                        .post(
                            "/v1/tokens",
                            Some(serde_json::json!({ "name": name, "scopes": scopes })),
                        )
                        .await?;
                    out(json, &t, |t| {
                        println!(
                            "Created token {} ('{}', scopes: {}).",
                            t.id,
                            t.name,
                            t.scopes.join(" ")
                        );
                        println!("Secret (shown only now):\n{}", t.token);
                        println!("Store it in ~/.config/hearth/token (mode 600) or pass it as HEARTH_TOKEN.");
                    });
                }
                TokenCmd::List => {
                    let l: Vec<TokenInfo> = c.get("/v1/tokens").await?;
                    out(json, &l, |l| render::tokens(l));
                }
                TokenCmd::Revoke { id } => {
                    if g.dry_run {
                        out(json, &serde_json::json!({"would_revoke": id}), |_| {
                            println!("Would revoke token {id}")
                        });
                        return Ok(ExitCode::Ok);
                    }
                    let v: serde_json::Value = c.delete(&format!("/v1/tokens/{}", urlencode(id))).await?;
                    out(json, &v, |_| println!("Revoked token {id}."));
                }
            }
        }
        Command::Daemon(a) => {
            init_logging();
            let cfg = daemon::DaemonConfig {
                socket: g.socket.clone(),
                helper_socket: a.helper_socket.clone(),
                state_dir: a.state_dir.clone(),
                check_interval: Duration::from_secs(a.check_interval.max(600)),
                puid: a.puid,
                pgid: a.pgid,
                ..Default::default()
            };
            daemon::run(cfg).await.map_err(Error::Other)?;
        }
        Command::Helper(a) => {
            init_logging();
            let daemon_uid = users::uid_of(&a.daemon_user);
            if daemon_uid.is_none() {
                tracing::warn!(user = %a.daemon_user, "daemon user not found; only root may use the helper");
            }
            let cfg = helper::HelperConfig {
                socket: a.helper_socket.clone(),
                daemon_uid,
                docker_socket: a.docker_socket.clone(),
                casaos_apps_dir: a.casaos_apps_dir.clone(),
                daemon_gid: users::primary_gid_of(&a.daemon_user),
                docker_bin: a.docker_bin.clone(),
                ..Default::default()
            };
            helper::run(cfg).await?;
        }
        // Change commands are handled by `changes::run` above.
        _ => return Err(Error::Usage("unsupported command".into())),
    }
    Ok(ExitCode::Ok)
}

async fn app(cli: &Cli, cmd: &AppCmd) -> Result<ExitCode, Error> {
    let g = &cli.global;
    let json = g.json;
    let c = Client::new(&g.socket);
    match cmd {
        AppCmd::Info { app, compose } => {
            let d: AppDetail = c.get(&format!("/v1/apps/{}", urlencode(app))).await?;
            if *compose {
                let text: String = c.get(&format!("/v1/apps/{}/compose", urlencode(app))).await?;
                out(json, &serde_json::json!({ "app": d, "compose": text }), |_| {
                    render::app_detail(&d);
                    println!("\n--- compose file (secrets hidden) ---\n{text}");
                });
            } else {
                out(json, &d, render::app_detail);
            }
        }
        AppCmd::Logs {
            app,
            service,
            tail,
            since,
        } => {
            let mut path = format!("/v1/apps/{}/logs?tail={tail}", urlencode(app));
            if let Some(s) = service {
                path.push_str(&format!("&service={}", urlencode(s)));
            }
            if let Some(s) = since {
                let now = daemon::db::now();
                path.push_str(&format!("&since={}", now - *s as i64));
            }
            let lines: Vec<LogLine> = c.get(&path).await?;
            out(json, &lines, |l| render::logs(l));
        }
        AppCmd::Update { app, .. } => {
            let path = match app {
                Some(a) => format!("/v1/versions/check?app={}", urlencode(a)),
                None => "/v1/versions/check".to_string(),
            };
            if !json {
                eprintln!("Checking registries…");
            }
            let apps: Vec<AppSummary> = c.post(&path, None).await?;
            let updates = apps
                .iter()
                .flat_map(|a| &a.images)
                .any(|i| i.latest.as_ref().is_some_and(|l| l.update_available));
            out(json, &apps, |a| {
                render::apps(a);
                println!();
                println!(
                    "{}",
                    if updates {
                        "Updates are available."
                    } else {
                        "Everything is up to date."
                    }
                );
            });
            return Ok(if updates {
                ExitCode::UpdatesAvailable
            } else {
                ExitCode::Ok
            });
        }
        // Changes are handled by `changes::run`.
        _ => return Err(Error::Usage("unsupported command".into())),
    }
    Ok(ExitCode::Ok)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("HEARTH_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // journald adds timestamps; keep lines short.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .with_target(false)
        // No colour codes in journald.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
}

#[derive(Debug, Serialize)]
struct VersionInfo {
    version: &'static str,
    target: &'static str,
}

impl VersionInfo {
    fn current() -> Self {
        VersionInfo {
            version: env!("CARGO_PKG_VERSION"),
            target: std::env::consts::ARCH,
        }
    }
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("hearth: failed to encode JSON: {e}"),
    }
}
