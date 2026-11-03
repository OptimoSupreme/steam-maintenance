//! steam-maintenance: user-driven NixOS upgrades for Steam machines.
//!
//! One binary, three roles:
//! - `check`    periodic user-session poll that decides whether to prompt
//! - `plugin`   JSON commands behind the Decky plugin's panel and prompt
//! - `worker`   root service that runs `nixos-rebuild boot` and reports progress

mod battery;
mod check;
mod config;
mod nixlog;
mod plugin;
mod state;
mod status;
mod util;
mod worker;

use anyhow::{bail, Result};

const USAGE: &str = "\
usage: steam-maintenance <command> [options]

  check [--force]      decide whether to prompt now; hands the prompt to the plugin
  plugin <verb> ...    JSON commands for the Decky plugin backend
  worker               run the upgrade (root service)

plugin verbs (as the Steam user): state, probe, later, snooze, heartbeat, watch
plugin verbs (as root):           start [<rev>], stop, reboot
";

fn main() {
    if let Err(err) = run() {
        eprintln!("steam-maintenance: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::Config::from_env();
    match args.first().map(String::as_str) {
        Some("check") => check::run(&cfg, args.iter().any(|a| a == "--force")),
        Some("plugin") => plugin::run(&cfg, &args[1..]),
        Some("worker") => worker::run(&cfg),
        _ => bail!("{USAGE}"),
    }
}
