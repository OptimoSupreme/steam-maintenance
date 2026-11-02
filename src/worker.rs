//! Root service: nixos-rebuild boot with progress, cleanup, guarded reboot. STUB.

use crate::config::Config;
use anyhow::Result;

pub fn run(_cfg: &Config) -> Result<()> {
    anyhow::bail!("worker not implemented")
}
