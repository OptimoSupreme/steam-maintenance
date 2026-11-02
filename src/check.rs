//! Periodic decision: is an update ready, is Steam idle on its home screen, open the UI.
//! Also the update probe the UI shares. STUB: to be implemented.

use crate::config::Config;
use anyhow::Result;

/// What the fleet repo offers versus what this machine runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateInfo {
    /// `configurationRevision` of the running system, None if unknown.
    pub local_rev: Option<String>,
    /// Head of the followed branch on GitHub, None if offline.
    pub remote_rev: Option<String>,
    /// The cache workflow succeeded for `remote_rev`, so the build is a download.
    pub cache_ready: bool,
    /// A newer generation is already staged and waits for a reboot.
    pub restart_pending: bool,
}

impl UpdateInfo {
    /// An update exists that the device can install right now.
    pub fn update_available(&self) -> bool {
        match (&self.local_rev, &self.remote_rev) {
            (Some(local), Some(remote)) => local != remote && self.cache_ready,
            (None, Some(_)) => self.cache_ready,
            _ => false,
        }
    }
}

/// Compare the running system with the repo and the cache. Network calls, may take seconds.
pub fn probe_update(_cfg: &Config) -> UpdateInfo {
    UpdateInfo::default()
}

/// Commit subjects between two revisions from the public GitHub compare API,
/// newest first; empty when offline or on any error.
pub fn commit_subjects(_cfg: &Config, _from: &str, _to: &str) -> Vec<String> {
    Vec::new()
}

pub fn run(_cfg: &Config, _force: bool) -> Result<()> {
    anyhow::bail!("check not implemented")
}
