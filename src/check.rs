//! Periodic decision: is an update ready, is Steam idle on its home screen, open the UI. STUB.

use crate::config::Config;
use anyhow::Result;

pub fn run(_cfg: &Config, _force: bool) -> Result<()> {
    anyhow::bail!("check not implemented")
}
