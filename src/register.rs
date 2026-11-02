//! Keep the "System Update" non-Steam shortcut in shortcuts.vdf. STUB: to be implemented.

use crate::config::Config;
use anyhow::Result;

/// Steam's legacy shortcut id: crc32 of the quoted exe followed by the name, high bit set.
pub fn shortcut_appid(exe_quoted: &str, app_name: &str) -> u32 {
    crc32fast::hash(format!("{exe_quoted}{app_name}").as_bytes()) | 0x8000_0000
}

/// The 64-bit id `steam://rungameid/<id>` expects for a shortcut.
pub fn rungameid(appid: u32) -> u64 {
    ((appid as u64) << 32) | 0x0200_0000
}

pub fn run(_cfg: &Config) -> Result<()> {
    anyhow::bail!("register not implemented")
}
