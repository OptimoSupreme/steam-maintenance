//! Parser for `nix --log-format internal-json` output. STUB: to be implemented.

use crate::status::Counter;

#[derive(Debug, Default)]
pub struct Progress {
    pub copies: Counter,
    pub builds: Counter,
    pub detail: String,
    pub log_tail: Vec<String>,
}

impl Progress {
    pub fn new() -> Progress {
        Progress::default()
    }

    /// Feed one stderr line. Lines not starting with `@nix ` are ignored and return false.
    pub fn feed_line(&mut self, _line: &str) -> bool {
        false
    }

    /// Weighted, monotonic percentage; None until anything is expected.
    pub fn percent(&self) -> Option<u8> {
        None
    }
}
