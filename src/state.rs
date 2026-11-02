//! Per-user scheduling state: when the next weekly check is due, how often it was
//! deferred, and the shortcut's app id. Lives in `$XDG_STATE_HOME/steam-maintenance/`.

use crate::util::{atomic_write, home_dir, now};
use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, Duration, Local, NaiveTime, TimeZone, Weekday};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const DIR: &str = "steam-maintenance";
const STATE_FILE: &str = "state.json";
const LAUNCH_REASON_FILE: &str = "launch-reason";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Unix time after which the weekly check may prompt. 0 = due now.
    pub next_due: u64,
    /// Consecutive "Later" presses on scheduled prompts; reset by any upgrade.
    pub deferrals: u32,
    /// App id of the Steam shortcut written by `register`.
    pub shortcut_appid: Option<u32>,
    /// Unix time the UI was last launched by the check.
    pub last_prompt: u64,
    /// Unix time before which the check stays quiet regardless of `next_due`.
    pub snooze_until: u64,
}

pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".local/state")))?;
    Some(base.join(DIR))
}

impl State {
    pub fn load() -> State {
        state_dir()
            .and_then(|d| fs::read(d.join(STATE_FILE)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let dir = state_dir().ok_or_else(|| anyhow!("no home or XDG_STATE_HOME"))?;
        atomic_write(&dir.join(STATE_FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Mark this round handled: the next prompt waits for the next due slot.
    pub fn roll_due(&mut self, spec: &str) -> Result<()> {
        self.next_due = next_due_after(now(), spec)?;
        Ok(())
    }

    pub fn reset_deferrals(&mut self) {
        self.deferrals = 0;
    }
}

/// Written by `check` right before it launches the UI, so the UI can tell a
/// scheduled prompt from a manual launch of the same Steam shortcut.
pub fn write_launch_reason(reason: &str) -> Result<()> {
    let dir = state_dir().ok_or_else(|| anyhow!("no home or XDG_STATE_HOME"))?;
    atomic_write(&dir.join(LAUNCH_REASON_FILE), reason.as_bytes())
}

/// Read and delete the launch reason. A reason older than five minutes is stale
/// (the launch never happened) and is treated as absent.
pub fn take_launch_reason() -> Option<String> {
    let path = state_dir()?.join(LAUNCH_REASON_FILE);
    let meta = fs::metadata(&path).ok()?;
    let content = fs::read_to_string(&path).ok();
    let _ = fs::remove_file(&path);
    let fresh = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map(|age| age.as_secs() < 300)
        .unwrap_or(false);
    content.filter(|_| fresh).map(|s| s.trim().to_string())
}

/// Parse `Day HH:MM` (local time) and return the first such instant strictly after `after`.
pub fn next_due_after(after: u64, spec: &str) -> Result<u64> {
    let (day, time) = spec
        .split_once(' ')
        .with_context(|| format!("check spec {spec:?} is not `Day HH:MM`"))?;
    let weekday: Weekday = day
        .parse()
        .map_err(|_| anyhow!("bad weekday {day:?} in check spec"))?;
    let time = NaiveTime::parse_from_str(time, "%H:%M")
        .with_context(|| format!("bad time {time:?} in check spec"))?;
    let after_local = Local
        .timestamp_opt(after as i64, 0)
        .single()
        .context("timestamp out of range")?;
    let mut date = after_local.date_naive();
    for _ in 0..8 {
        if date.weekday() == weekday {
            if let Some(candidate) = Local.from_local_datetime(&date.and_time(time)).earliest() {
                if candidate > after_local {
                    return Ok(candidate.timestamp() as u64);
                }
            }
        }
        date += Duration::days(1);
    }
    Err(anyhow!("no due slot found for {spec:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_due_is_the_following_sunday_noon() {
        // 2026-11-02 is a Monday.
        let monday = Local.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
        let due = next_due_after(monday.timestamp() as u64, "Sun 12:00").unwrap();
        let due_local = Local.timestamp_opt(due as i64, 0).unwrap();
        assert_eq!(due_local.weekday(), Weekday::Sun);
        assert_eq!(
            due_local.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-08 12:00"
        );
    }

    #[test]
    fn next_due_skips_a_slot_that_already_passed_today() {
        let sunday_afternoon = Local.with_ymd_and_hms(2026, 11, 8, 13, 0, 0).unwrap();
        let due = next_due_after(sunday_afternoon.timestamp() as u64, "Sun 12:00").unwrap();
        let due_local = Local.timestamp_opt(due as i64, 0).unwrap();
        assert_eq!(
            due_local.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-15 12:00"
        );
    }

    #[test]
    fn bad_spec_is_an_error() {
        assert!(next_due_after(0, "Someday 12:00").is_err());
        assert!(next_due_after(0, "Sun noon").is_err());
    }

    #[test]
    fn state_defaults_are_due_now() {
        let s: State = serde_json::from_str("{}").unwrap();
        assert_eq!(s.next_due, 0);
        assert_eq!(s.deferrals, 0);
    }
}
