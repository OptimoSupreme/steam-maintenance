//! Small shared helpers: subprocesses, atomic file writes, time.

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run a command, returning trimmed stdout; stderr is passed through.
pub fn run<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Result<String> {
    let program = program.as_ref().to_owned();
    let output = Command::new(&program)
        .args(args)
        .output()
        .with_context(|| format!("running {}", program.to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            program.to_string_lossy(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like `run`, but a failure yields `None` instead of an error.
pub fn run_opt<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Option<String> {
    run(program, args).ok()
}

/// Write a file through a temporary sibling and rename, so readers never see a partial file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// `$HOME`, or the passwd entry's home when launched without one.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}
