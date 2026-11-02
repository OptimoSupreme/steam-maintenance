//! steam-maintenance: user-driven NixOS upgrades for Steam machines.
//!
//! One binary, four roles:
//! - `ui`       full-screen menu and progress page, launched by Steam as a non-Steam shortcut
//! - `check`    periodic user-session poll that decides whether to open the UI
//! - `register` keeps the "System Update" shortcut in Steam's shortcuts.vdf
//! - `worker`   root service that runs `nixos-rebuild boot` and reports progress

mod battery;
mod check;
mod config;
mod nixlog;
mod register;
mod state;
mod status;
mod ui;
mod util;
mod vdf;
mod worker;

use anyhow::{bail, Result};

const USAGE: &str = "\
usage: steam-maintenance <command> [options]

  ui [--mock <screen>] [--size WxH]   full-screen update UI (launched by Steam)
  check [--force]                     decide whether to open the UI now
  register                            upsert the Steam library shortcut
  worker                              run the upgrade (root service)
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
        Some("ui") => {
            let mut mock = None;
            let mut size = None;
            let mut it = args[1..].iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--mock" => mock = it.next().cloned(),
                    "--size" => size = it.next().and_then(|s| parse_size(s)),
                    other => bail!("unknown ui option {other}\n{USAGE}"),
                }
            }
            ui::run(&cfg, mock, size)
        }
        Some("check") => check::run(&cfg, args.iter().any(|a| a == "--force")),
        Some("register") => register::run(&cfg),
        Some("worker") => worker::run(&cfg),
        _ => bail!("{USAGE}"),
    }
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}
