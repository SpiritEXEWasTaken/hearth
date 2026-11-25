//! `hearth`: one binary for the CLI, the daemon (`hearth daemon`) and the root helper (`hearth helper`).

mod cli;

use clap::Parser;
use cli::{Cli, Command};
use hearth_core::output::Envelope;
use hearth_core::{Error, ExitCode};
use serde::Serialize;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let json = cli.global.json;

    let result = run(&cli);
    match result {
        Ok(()) => ExitCode::Ok.into(),
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

fn run(cli: &Cli) -> Result<(), Error> {
    match &cli.command {
        Command::Version => {
            let info = VersionInfo::current();
            if cli.global.json {
                print_json(&Envelope::success(info));
            } else {
                println!("hearth {} ({})", info.version, info.target);
            }
            Ok(())
        }
        Command::Status => Err(Error::NotImplemented("status (milestone M1)")),
        Command::Daemon => Err(Error::NotImplemented("daemon (milestone M1)")),
        Command::Helper => Err(Error::NotImplemented("helper (milestone M1)")),
    }
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
