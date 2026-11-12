//! Shows which Mac each Magic keyboard, mouse and trackpad is on, and moves them between Macs.
//!
//! Run with no arguments for the interactive view, or `magic help` for the commands.

mod bluetooth;
mod cli;
mod device;
mod host;
mod tui;

use std::{env, process};

fn main() {
    let hosts = host::load();
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() {
        process::exit(cli::run(&args, &hosts));
    }
    if let Err(err) = tui::run(hosts) {
        eprintln!("magic: {err}");
        process::exit(1);
    }
}
