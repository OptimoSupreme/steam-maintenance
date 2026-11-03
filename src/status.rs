//! Progress of a running upgrade, written by the root worker and polled by the UI,
//! plus the UI heartbeat the worker checks before rebooting.

use crate::util::{atomic_write, now};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STATUS_PATH: &str = "/run/steam-maintenance/status.json";
const HEARTBEAT_DIR: &str = "steam-maintenance";
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
    /// New generation staged; rebooting as soon as the UI confirms it is on screen.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// `$XDG_RUNTIME_DIR/steam-maintenance/heartbeat` for the current user.
pub fn heartbeat_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(Path::new(&dir).join(HEARTBEAT_DIR).join(HEARTBEAT_FILE))
}

/// Touch the heartbeat; the UI calls this every couple of seconds while on screen.
pub fn touch_heartbeat() -> Result<()> {
    if let Some(path) = heartbeat_path() {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, now().to_string())?;
    }
    Ok(())
}

/// True if any user's heartbeat under /run/user/*/ was touched within `max_age`.
/// The worker runs as root, so it looks at every runtime directory.
pub fn heartbeat_fresh(max_age: Duration) -> bool {
    let Ok(users) = fs::read_dir("/run/user") else {
        return false;
    };
    for entry in users.flatten() {
        let path = entry.path().join(HEARTBEAT_DIR).join(HEARTBEAT_FILE);
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

const REQUEST_FILE: &str = "request.json";

/// What the UI asked the worker to install, written next to the heartbeat right
/// before `systemctl start`, so the worker builds the commit the user saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub rev: String,
    pub requested_at: u64,
}

/// Record the commit the UI is about to install.
pub fn write_request(rev: &str) -> Result<()> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Ok(());
    };
    let req = Request {
        rev: rev.to_string(),
        requested_at: now(),
    };
    atomic_write(
        &Path::new(&dir).join(HEARTBEAT_DIR).join(REQUEST_FILE),
        &serde_json::to_vec(&req)?,
    )
}

/// The freshest request under /run/user/*/ no older than `max_age`. Root reads this.
pub fn read_request(max_age: Duration) -> Option<Request> {
    let users = fs::read_dir("/run/user").ok()?;
    let cutoff = now().saturating_sub(max_age.as_secs());
    users
        .flatten()
        .filter_map(|e| fs::read(e.path().join(HEARTBEAT_DIR).join(REQUEST_FILE)).ok())
        .filter_map(|b| serde_json::from_slice::<Request>(&b).ok())
        .filter(|r| r.requested_at >= cutoff && !r.rev.is_empty())
        .max_by_key(|r| r.requested_at)
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
