//! The files the three roles share: the worker's progress, the plugin's heartbeat
//! and the install request.

use crate::util::{atomic_write, now};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The worker's runtime directory (`RuntimeDirectory=` of the system unit).
pub const RUN_DIR: &str = "/run/steam-maintenance";
pub const STATUS_PATH: &str = "/run/steam-maintenance/status.json";
const REQUEST_FILE: &str = "request.json";
/// Per-user directory under `$XDG_RUNTIME_DIR`.
const USER_DIR: &str = "steam-maintenance";
const HEARTBEAT_FILE: &str = "heartbeat";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Fetching the flake and evaluating; no percentage yet.
    Evaluating,
    /// Substituting from the binary cache.
    Downloading,
    /// At least one derivation is being built locally.
    Building,
    /// switch-to-configuration boot: bootloader entries. Not cancellable.
    Installing,
    /// nix-gc after a successful install.
    Cleaning,
    /// New generation staged; rebooting as soon as the plugin confirms it is on screen.
    Done,
    /// The flake evaluated to the generation already installed.
    UpToDate,
    /// Build failed or was cancelled; see `error` and `log_tail`.
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counter {
    pub done: u64,
    pub expected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub phase: Phase,
    /// 0..=100 once downloads or builds are known; None while evaluating.
    pub percent: Option<u8>,
    /// One line: what is being downloaded or built right now.
    pub detail: String,
    pub builds: Counter,
    pub copies: Counter,
    pub error: Option<String>,
    /// Last lines of build output, only kept when the build fails.
    pub log_tail: Vec<String>,
    /// Whether `systemctl stop` is safe right now.
    pub cancellable: bool,
    pub started_at: u64,
    pub updated_at: u64,
}

impl Status {
    pub fn new(phase: Phase) -> Status {
        let t = now();
        Status {
            phase,
            percent: None,
            detail: String::new(),
            builds: Counter::default(),
            copies: Counter::default(),
            error: None,
            log_tail: Vec::new(),
            cancellable: true,
            started_at: t,
            updated_at: t,
        }
    }

    pub fn read() -> Option<Status> {
        Self::read_from(Path::new(STATUS_PATH))
    }

    pub fn read_from(path: &Path) -> Option<Status> {
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&mut self) -> Result<()> {
        self.write_to(Path::new(STATUS_PATH))
    }

    pub fn write_to(&mut self, path: &Path) -> Result<()> {
        self.updated_at = now();
        atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }
}

// ---- per-user runtime files ----------------------------------------------

/// `$XDG_RUNTIME_DIR/steam-maintenance` for the current user.
pub fn user_dir() -> Result<PathBuf> {
    let dir =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set"))?;
    Ok(Path::new(&dir).join(USER_DIR))
}

/// Touch the heartbeat; the plugin calls this every couple of seconds while its
/// update view is on screen and no game is running.
pub fn touch_heartbeat() -> Result<()> {
    let path = user_dir()?.join(HEARTBEAT_FILE);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, now().to_string())?;
    Ok(())
}

/// True if any user's heartbeat under /run/user/*/ was touched within `max_age`.
/// The worker runs as root, so it looks at every runtime directory.
pub fn heartbeat_fresh(max_age: Duration) -> bool {
    let Ok(users) = fs::read_dir("/run/user") else {
        return false;
    };
    for entry in users.flatten() {
        let path = entry.path().join(USER_DIR).join(HEARTBEAT_FILE);
        if let Ok(meta) = fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if modified.elapsed().map(|e| e <= max_age).unwrap_or(false) {
                    return true;
                }
            }
        }
    }
    false
}

// ---- the install request (root) --------------------------------------------

/// What the plugin asked the worker to install, written as root right before
/// `systemctl start`, so the worker builds the commit the user saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub rev: String,
    pub requested_at: u64,
}

pub fn write_request(rev: &str) -> Result<()> {
    let req = Request {
        rev: rev.to_string(),
        requested_at: now(),
    };
    atomic_write(
        &Path::new(RUN_DIR).join(REQUEST_FILE),
        &serde_json::to_vec(&req)?,
    )
}

/// The request no older than `max_age`, if any.
pub fn read_request(max_age: Duration) -> Option<Request> {
    let bytes = fs::read(Path::new(RUN_DIR).join(REQUEST_FILE)).ok()?;
    let req: Request = serde_json::from_slice(&bytes).ok()?;
    let cutoff = now().saturating_sub(max_age.as_secs());
    (req.requested_at >= cutoff && !req.rev.is_empty()).then_some(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips() {
        let dir = std::env::temp_dir().join(format!("sm-status-{}", std::process::id()));
        let path = dir.join("status.json");
        let mut s = Status::new(Phase::Downloading);
        s.percent = Some(42);
        s.detail = "mesa".into();
        s.write_to(&path).unwrap();
        let back = Status::read_from(&path).unwrap();
        assert_eq!(back.phase, Phase::Downloading);
        assert_eq!(back.percent, Some(42));
        assert_eq!(back.detail, "mesa");
        fs::remove_dir_all(dir).unwrap();
    }
}
