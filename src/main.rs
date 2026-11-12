//! Shows which Mac each Magic keyboard, mouse and trackpad is on, and moves them between Macs.
//!
//! Run with no arguments for the interactive view, or `magic help` for the commands.

mod bluetooth;
mod cli;
mod device;
mod host;
mod tui;

use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let hosts = host::load();
    let args: Vec<String> = env::args().skip(1).collect();

    if let Some(command) = args.first() {
        return cli::run(&hosts, command, args.get(1).map(String::as_str));
    }
    match tui::run(hosts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("magic: {err}");
            ExitCode::FAILURE
        }
    }
}
