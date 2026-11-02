//! Root service: `nixos-rebuild boot` with progress, cleanup, guarded reboot.
//!
//! Runs inside the system unit. Progress is read from nix's internal-json log on
//! stderr and published to /run/steam-maintenance/status.json for the UI.

use crate::config::Config;
use crate::nixlog::Progress;
use crate::status::{self, Counter, Phase, Status};
use anyhow::{Context, Result};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";
const BOOTED_SYSTEM: &str = "/run/booted-system";
const GC_UNIT: &str = "nix-gc.service";
/// Status writes are throttled to this interval unless the phase changes.
const WRITE_INTERVAL: Duration = Duration::from_millis(250);
/// How long to wait for the UI heartbeat before leaving the generation staged.
const HEARTBEAT_WAIT: Duration = Duration::from_secs(60);
const HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(10);
/// Lowercase fragments that mark switch-to-configuration output.
const INSTALL_MARKERS: &[&str] = &[
    "switch-to-configuration",
    "systemd-boot",
    "boot loader",
    "bootloader",
    "boot entries",
    "lanzaboote",
    "activating the configuration",
    "updating grub",
];

/// What a parsed stderr line asks the worker to do with the status file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    /// Nothing changed.
    None,
    /// Counters or detail changed; write when the throttle allows.
    Progress,
    /// The phase changed; write immediately.
    Phase,
}

/// What the loop body needs from the nix log parser.
pub trait ProgressSource {
    /// Feed one stderr line; true if it was a nix log record.
    fn feed_line(&mut self, line: &str) -> bool;
    /// Phase the parser infers from what nix is doing, if it can tell.
    fn phase_hint(&self) -> Option<Phase>;
    fn percent(&self) -> Option<u8>;
    fn detail(&self) -> &str;
    fn copies(&self) -> Counter;
    fn builds(&self) -> Counter;
    /// Nix has announced work to do (downloads or builds expected).
    fn started(&self) -> bool {
        self.copies().expected > 0 || self.builds().expected > 0
    }
}

impl ProgressSource for Progress {
    fn feed_line(&mut self, line: &str) -> bool {
        Progress::feed_line(self, line)
    }
    fn phase_hint(&self) -> Option<Phase> {
        Progress::phase_hint(self)
    }
    fn percent(&self) -> Option<u8> {
        Progress::percent(self)
    }
    fn detail(&self) -> &str {
        &self.detail
    }
    fn copies(&self) -> Counter {
        self.copies
    }
    fn builds(&self) -> Counter {
        self.builds
    }
}

pub fn run(cfg: &Config) -> Result<()> {
    let mut status = Status::new(Phase::Evaluating);
    status.write()?;
    let before = fs::canonicalize(SYSTEM_PROFILE).ok();
    let booted = fs::canonicalize(BOOTED_SYSTEM).ok();
    eprintln!(
        "worker: profile {} booted {}",
        display(&before),
        display(&booted)
    );

    let mut progress = Progress::new();
    let exit = rebuild(cfg, &mut progress, &mut status)?;
    if !exit.success() {
        return fail(&mut status, &progress, exit);
    }

    let after = fs::canonicalize(SYSTEM_PROFILE).ok();
    if after.is_some() && after == booted {
        status.phase = Phase::UpToDate;
        status.percent = Some(100);
        status.detail.clear();
        status.cancellable = false;
        status.write()?;
        eprintln!("worker: already running the latest generation");
        return Ok(());
    }

    clean(&mut status)?;
    finish(&mut status)
}

fn display(path: &Option<PathBuf>) -> String {
    path.as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Run nixos-rebuild, streaming its stderr into `progress` and `status`.
fn rebuild(cfg: &Config, progress: &mut Progress, status: &mut Status) -> Result<ExitStatus> {
    let mut child = Command::new("nixos-rebuild")
        .args([
            "boot",
            "--flake",
            &cfg.flake,
            "--refresh",
            "--log-format",
            "internal-json",
            "-v",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning nixos-rebuild")?;
    let stderr = child.stderr.take().context("nixos-rebuild has no stderr")?;

    let mut last_write = Instant::now();
    for line in BufReader::new(stderr).split(b'\n') {
        let line = line.context("reading nixos-rebuild output")?;
        let line = String::from_utf8_lossy(&line);
        let update = handle_line(line.trim_end(), progress, status);
        let due = update == Update::Phase || last_write.elapsed() >= WRITE_INTERVAL;
        if update != Update::None && due {
            status.write()?;
            last_write = Instant::now();
        }
    }
    let exit = child.wait().context("waiting for nixos-rebuild")?;
    if exit.success() {
        mark_installing(status);
        status.write()?;
    }
    Ok(exit)
}

/// The loop body: apply one stderr line to the parser and the status.
pub fn handle_line<P: ProgressSource>(line: &str, progress: &mut P, status: &mut Status) -> Update {
    if progress.feed_line(line) {
        return apply_progress(progress, status);
    }
    if line.is_empty() {
        return Update::None;
    }
    eprintln!("{line}");
    if status.phase != Phase::Installing && progress.started() && looks_like_install(line, progress)
    {
        mark_installing(status);
        return Update::Phase;
    }
    Update::None
}

fn apply_progress<P: ProgressSource>(progress: &P, status: &mut Status) -> Update {
    let phase = if status.phase == Phase::Installing {
        Phase::Installing
    } else {
        progress.phase_hint().unwrap_or(status.phase)
    };
    let changed = phase != status.phase;
    status.phase = phase;
    status.detail = progress.detail().to_string();
    status.copies = progress.copies();
    status.builds = progress.builds();
    status.percent = progress.percent();
    if changed {
        Update::Phase
    } else {
        Update::Progress
    }
}

/// A non-JSON line after nix finished its work comes from switch-to-configuration.
fn looks_like_install<P: ProgressSource>(line: &str, progress: &P) -> bool {
    let lower = line.to_ascii_lowercase();
    INSTALL_MARKERS.iter().any(|m| lower.contains(m)) || progress.percent() == Some(100)
}

fn mark_installing(status: &mut Status) {
    status.phase = Phase::Installing;
    status.cancellable = false;
    status.detail = "Installing the boot entry".into();
}

fn fail(status: &mut Status, progress: &Progress, exit: ExitStatus) -> Result<()> {
    let reason = match exit.code() {
        Some(code) => format!("nixos-rebuild exited with status {code}"),
        None => format!("nixos-rebuild was killed ({exit})"),
    };
    status.phase = Phase::Failed;
    status.error = Some(
        progress
            .last_error()
            .map(str::to_string)
            .unwrap_or_else(|| reason.clone()),
    );
    status.log_tail = progress.log_tail.clone();
    status.cancellable = false;
    status.write()?;
    Err(anyhow::anyhow!(reason))
}

fn clean(status: &mut Status) -> Result<()> {
    status.phase = Phase::Cleaning;
    status.percent = Some(100);
    status.detail = "Removing old generations".into();
    status.cancellable = false;
    status.write()?;
    if let Err(err) = crate::util::run("systemctl", &["start", GC_UNIT]) {
        eprintln!("worker: cleanup failed, continuing: {err:#}");
    }
    Ok(())
}

/// Reboot once the UI confirms it is on screen; otherwise leave the generation staged.
fn finish(status: &mut Status) -> Result<()> {
    status.phase = Phase::Done;
    status.detail = "Restarting".into();
    status.write()?;
    let deadline = Instant::now() + HEARTBEAT_WAIT;
    while Instant::now() < deadline {
        if status::heartbeat_fresh(HEARTBEAT_MAX_AGE) {
            eprintln!("worker: UI heartbeat seen, rebooting");
            crate::util::run("systemctl", &["reboot"]).context("requesting reboot")?;
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    status.detail = "Restart pending".into();
    status.write()?;
    eprintln!("worker: no UI heartbeat; generation staged for the next boot");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted parser: a line `progress <copies_done>/<copies_expected> <builds_done>/<builds_expected> <detail>`
    /// is a "nix record"; anything else is not.
    #[derive(Default)]
    struct Fake {
        copies: Counter,
        builds: Counter,
        detail: String,
    }

    fn counter(spec: &str) -> Counter {
        let (done, expected) = spec.split_once('/').unwrap();
        Counter {
            done: done.parse().unwrap(),
            expected: expected.parse().unwrap(),
        }
    }

    impl ProgressSource for Fake {
        fn feed_line(&mut self, line: &str) -> bool {
            let Some(rest) = line.strip_prefix("progress ") else {
                return false;
            };
            let mut parts = rest.splitn(3, ' ');
            self.copies = counter(parts.next().unwrap());
            self.builds = counter(parts.next().unwrap());
            self.detail = parts.next().unwrap_or("").to_string();
            true
        }
        fn phase_hint(&self) -> Option<Phase> {
            if self.builds.done < self.builds.expected {
                Some(Phase::Building)
            } else if self.copies.expected > 0 {
                Some(Phase::Downloading)
            } else {
                None
            }
        }
        fn percent(&self) -> Option<u8> {
            let expected = self.copies.expected + self.builds.expected;
            if expected == 0 {
                return None;
            }
            Some(((self.copies.done + self.builds.done) * 100 / expected) as u8)
        }
        fn detail(&self) -> &str {
            &self.detail
        }
        fn copies(&self) -> Counter {
            self.copies
        }
        fn builds(&self) -> Counter {
            self.builds
        }
    }

    #[test]
    fn scripted_rebuild_walks_the_phases() {
        let mut p = Fake::default();
        let mut s = Status::new(Phase::Evaluating);

        assert_eq!(
            handle_line("building the system configuration...", &mut p, &mut s),
            Update::None
        );
        assert_eq!(s.phase, Phase::Evaluating);
        assert!(s.cancellable);

        assert_eq!(
            handle_line("progress 0/0 0/0", &mut p, &mut s),
            Update::Progress
        );
        assert_eq!(s.phase, Phase::Evaluating);
        assert_eq!(s.percent, None);

        assert_eq!(
            handle_line("progress 1/4 0/0 mesa-24.0", &mut p, &mut s),
            Update::Phase
        );
        assert_eq!(s.phase, Phase::Downloading);
        assert_eq!(s.percent, Some(25));
        assert_eq!(s.detail, "mesa-24.0");
        assert_eq!(
            s.copies,
            Counter {
                done: 1,
                expected: 4
            }
        );

        assert_eq!(
            handle_line("progress 4/4 0/1 steam-deck-config", &mut p, &mut s),
            Update::Phase
        );
        assert_eq!(s.phase, Phase::Building);
        assert_eq!(
            s.builds,
            Counter {
                done: 0,
                expected: 1
            }
        );
        assert_eq!(s.percent, Some(80));

        assert_eq!(
            handle_line("progress 4/4 1/1 ", &mut p, &mut s),
            Update::Phase
        );
        assert_eq!(s.phase, Phase::Downloading);
        assert_eq!(s.percent, Some(100));
        assert!(s.cancellable);

        assert_eq!(
            handle_line(
                "updating systemd-boot from (26.05) to (26.05)",
                &mut p,
                &mut s
            ),
            Update::Phase
        );
        assert_eq!(s.phase, Phase::Installing);
        assert!(!s.cancellable);

        // Late nix records never pull the phase back.
        assert_eq!(
            handle_line("progress 4/4 1/1 late", &mut p, &mut s),
            Update::Progress
        );
        assert_eq!(s.phase, Phase::Installing);
        assert_eq!(s.detail, "late");
    }

    #[test]
    fn install_marker_before_progress_is_ignored() {
        let mut p = Fake::default();
        let mut s = Status::new(Phase::Evaluating);
        assert_eq!(
            handle_line("warning: boot loader looks fine", &mut p, &mut s),
            Update::None
        );
        assert_eq!(s.phase, Phase::Evaluating);
        assert!(s.cancellable);
    }

    #[test]
    fn plain_line_mid_download_is_not_install() {
        let mut p = Fake::default();
        let mut s = Status::new(Phase::Evaluating);
        handle_line("progress 1/4 0/0 mesa", &mut p, &mut s);
        assert_eq!(
            handle_line("some nixos-rebuild chatter", &mut p, &mut s),
            Update::None
        );
        assert_eq!(s.phase, Phase::Downloading);
        assert!(s.cancellable);
    }

    #[test]
    fn any_plain_line_at_full_progress_is_install() {
        let mut p = Fake::default();
        let mut s = Status::new(Phase::Evaluating);
        handle_line("progress 2/2 0/0 done", &mut p, &mut s);
        assert_eq!(handle_line("Copying files", &mut p, &mut s), Update::Phase);
        assert_eq!(s.phase, Phase::Installing);
        assert_eq!(s.percent, Some(100));
    }

    #[test]
    fn empty_lines_are_ignored() {
        let mut p = Fake::default();
        let mut s = Status::new(Phase::Evaluating);
        handle_line("progress 2/2 0/0 done", &mut p, &mut s);
        assert_eq!(handle_line("", &mut p, &mut s), Update::None);
        assert_eq!(s.phase, Phase::Downloading);
    }
}
