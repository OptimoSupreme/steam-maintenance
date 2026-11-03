//! The JSON commands behind the Decky plugin. The backend runs the user verbs as
//! the Steam user (they read and write that user's state and runtime files) and
//! the root verbs as root (they drive the system unit).
//!
//! Every verb prints one JSON document on stdout; `watch` prints one per event
//! for as long as it runs.

use crate::battery::{self, Battery};
use crate::check::{self, Offer, OfferKind, Prompt};
use crate::config::Config;
use crate::state::{later_policy, State};
use crate::status::{self, Phase, Status};
use crate::util::{self, now, run_opt};
use anyhow::{bail, Result};
use serde::Serialize;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

/// Cancelling snoozes the weekly prompt for a day.
const CANCEL_SNOOZE_SECS: u64 = 86_400;
/// A prompt the plugin did not pick up within this time is dropped.
const PROMPT_MAX_AGE: Duration = Duration::from_secs(600);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const UNIT_POLL: Duration = Duration::from_secs(2);

/// What the panel needs on open, without touching the network.
#[derive(Debug, Serialize)]
struct Snapshot {
    installed: Option<String>,
    unit_active: bool,
    status: Option<Status>,
    battery: Option<Battery>,
    battery_blocked: bool,
    battery_gate: u8,
    restart_pending: bool,
    game_running: bool,
    deferrals: u32,
    max_deferrals: u32,
    next_due: u64,
}

#[derive(Debug, Serialize)]
struct LaterReply {
    /// False when Later is no longer on offer this round.
    accepted: bool,
    /// The next round installs on its own.
    warn: bool,
}

#[derive(Debug, Serialize)]
struct HeartbeatReply {
    accepted: bool,
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum Event<'a> {
    Prompt {
        prompt: &'a Prompt,
    },
    Status {
        status: Option<&'a Status>,
        unit_active: bool,
    },
}

#[derive(Debug, Serialize)]
struct Empty {}

pub fn run(cfg: &Config, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("state") => emit(&snapshot(cfg)),
        Some("probe") => emit(&probe(cfg)?),
        Some("later") => emit(&later(cfg)?),
        Some("snooze") => {
            snooze()?;
            emit(&Empty {})
        }
        Some("heartbeat") => emit(&heartbeat()?),
        Some("watch") => watch(cfg),
        Some("start") => {
            start(cfg, args.get(1).map(String::as_str))?;
            emit(&Empty {})
        }
        Some("stop") => {
            util::run("systemctl", &["stop", &cfg.unit])?;
            emit(&Empty {})
        }
        Some("reboot") => {
            util::run("systemctl", &["reboot"])?;
            emit(&Empty {})
        }
        other => bail!("unknown plugin verb {other:?}"),
    }
}

fn emit<T: Serialize>(value: &T) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn unit_active(unit: &str) -> bool {
    run_opt("systemctl", &["is-active", unit]).as_deref() == Some("active")
}

fn snapshot(cfg: &Config) -> Snapshot {
    let state = State::load();
    let battery = battery::read();
    Snapshot {
        installed: check::local_rev(),
        unit_active: unit_active(&cfg.unit),
        status: Status::read(),
        battery,
        battery_blocked: battery::blocks(battery, cfg.battery_gate),
        battery_gate: cfg.battery_gate,
        restart_pending: check::restart_pending(),
        game_running: check::game_running(),
        deferrals: state.deferrals,
        max_deferrals: cfg.max_deferrals,
        next_due: state.next_due,
    }
}

/// A manual check from the panel. Finding the machine current ends the round,
/// as the weekly check would.
fn probe(cfg: &Config) -> Result<Offer> {
    let (_, offer) = check::offer(cfg);
    if offer.kind == OfferKind::Current {
        let mut state = State::load();
        state.reset_deferrals();
        state.roll_due(&cfg.check_due)?;
        state.save()?;
    }
    Ok(offer)
}

/// Later on a scheduled prompt: one more deferral, next prompt next week.
fn later(cfg: &Config) -> Result<LaterReply> {
    let mut state = State::load();
    let policy = later_policy(state.deferrals, cfg.max_deferrals);
    if policy.show {
        state.deferrals += 1;
        state.roll_due(&cfg.check_due)?;
        state.save()?;
    }
    Ok(LaterReply {
        accepted: policy.show,
        warn: policy.warn,
    })
}

fn snooze() -> Result<()> {
    let mut state = State::load();
    state.snooze_until = now() + CANCEL_SNOOZE_SECS;
    state.save()
}

/// Touch the heartbeat the worker needs before it reboots, unless a game is
/// running: the panel can be open over a game, and a reboot must not be.
fn heartbeat() -> Result<HeartbeatReply> {
    if check::game_running() {
        return Ok(HeartbeatReply {
            accepted: false,
            reason: Some("a game is running"),
        });
    }
    status::touch_heartbeat()?;
    Ok(HeartbeatReply {
        accepted: true,
        reason: None,
    })
}

/// Stream prompts from the check and progress from the worker, one JSON line
/// each, until killed. A finished round resets the deferral count.
fn watch(cfg: &Config) -> Result<()> {
    let mut last_status: Option<Status> = None;
    let mut active = unit_active(&cfg.unit);
    let mut unit_polled = Instant::now();
    loop {
        if let Some(prompt) = check::take_prompt(PROMPT_MAX_AGE) {
            emit(&Event::Prompt { prompt: &prompt })?;
        }
        if unit_polled.elapsed() >= UNIT_POLL {
            unit_polled = Instant::now();
            let now_active = unit_active(&cfg.unit);
            if now_active != active {
                active = now_active;
                emit(&Event::Status {
                    status: last_status.as_ref(),
                    unit_active: active,
                })?;
            }
        }
        let status = Status::read();
        if status != last_status {
            if round_settled(last_status.as_ref(), status.as_ref()) {
                let mut state = State::load();
                state.reset_deferrals();
                state.save()?;
            }
            emit(&Event::Status {
                status: status.as_ref(),
                unit_active: active,
            })?;
            last_status = status;
        }
        thread::sleep(WATCH_INTERVAL);
    }
}

/// The worker just reached Done or UpToDate: this round is over.
fn round_settled(before: Option<&Status>, after: Option<&Status>) -> bool {
    let settled =
        |s: Option<&Status>| matches!(s.map(|s| s.phase), Some(Phase::Done | Phase::UpToDate));
    settled(after)
        && (!settled(before) || before.map(|s| s.started_at) != after.map(|s| s.started_at))
}

/// Record the commit the user saw, then start the worker.
fn start(cfg: &Config, rev: Option<&str>) -> Result<()> {
    if let Some(rev) = rev.filter(|r| !r.is_empty()) {
        status::write_request(rev)?;
    }
    util::run("systemctl", &["start", &cfg.unit])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(phase: Phase, started_at: u64) -> Status {
        let mut s = Status::new(phase);
        s.started_at = started_at;
        s
    }

    #[test]
    fn a_round_settles_once_per_run() {
        let downloading = status(Phase::Downloading, 10);
        let done = status(Phase::Done, 10);
        let done_later = status(Phase::Done, 20);
        assert!(round_settled(Some(&downloading), Some(&done)));
        assert!(round_settled(None, Some(&done)));
        assert!(!round_settled(Some(&done), Some(&done)));
        assert!(round_settled(Some(&done), Some(&done_later)));
        assert!(!round_settled(Some(&done), None));
        assert!(!round_settled(None, Some(&status(Phase::Failed, 10))));
        assert!(round_settled(None, Some(&status(Phase::UpToDate, 10))));
    }

    #[test]
    fn events_serialize_with_a_tag() {
        let s = status(Phase::Done, 1);
        let json = serde_json::to_string(&Event::Status {
            status: Some(&s),
            unit_active: true,
        })
        .unwrap();
        assert!(json.starts_with(r#"{"event":"status","#));
        assert!(json.contains(r#""phase":"done""#));
    }
}
