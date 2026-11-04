//! Periodic decision: is an update ready, is Steam idle on its home screen, hand a
//! prompt to the plugin. Also the update probe the plugin shares.
//!
//! Every external fact comes from a subprocess on PATH (git, curl, systemctl,
//! nixos-version); a missing tool degrades to "unknown", never a panic.

use crate::battery;
use crate::config::Config;
use crate::state::{later_policy, State};
use crate::status;
use crate::util::{atomic_write, now, run_opt};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::Duration;

const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";
const BOOTED_SYSTEM: &str = "/run/booted-system";
const STEAM_UNIT: &str = "steam-launcher.service";
/// gamescope reports this app id while Steam's own home screen has focus.
const STEAM_HOME_APPID: u32 = 769;
const FOCUSED_APP_PROPERTY: &[u8] = b"GAMESCOPE_FOCUSED_APP_GFX";
/// Steam must have been up this long before a prompt is considered.
const MIN_STEAM_UPTIME_SECS: u64 = 120;
/// After a prompt the check stays quiet for an hour.
const SNOOZE_SECS: u64 = 3600;
/// Upper bound on any network call, so a black-holed link cannot stall the check.
const NETWORK_TIMEOUT_SECS: &str = "30";
const CONNECT_TIMEOUT_SECS: &str = "10";
const PROMPT_FILE: &str = "prompt.json";

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

    /// The repo head differs from the running system (or the latter is unknown),
    /// whether or not the cache has caught up.
    fn remote_differs(&self) -> bool {
        match (&self.local_rev, &self.remote_rev) {
            (Some(local), Some(remote)) => local != remote,
            (None, Some(_)) => true,
            _ => false,
        }
    }

    /// Both revisions are known and the same: nothing to do this round.
    fn confirmed_current(&self) -> bool {
        matches!((&self.local_rev, &self.remote_rev), (Some(l), Some(r)) if l == r)
    }

    /// The one word the plugin renders a screen from.
    fn kind(&self) -> OfferKind {
        if self.update_available() {
            OfferKind::Update
        } else if self.restart_pending {
            OfferKind::Restart
        } else if self.remote_rev.is_none() {
            OfferKind::Offline
        } else if self.remote_differs() {
            OfferKind::Preparing
        } else {
            OfferKind::Current
        }
    }
}

/// What the plugin shows after a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OfferKind {
    /// A cached build of a newer commit: Update is on.
    Update,
    /// A newer generation is staged: Restart is on.
    Restart,
    /// The repo moved but the cache has not caught up yet.
    Preparing,
    /// GitHub could not be reached.
    Offline,
    /// Running the branch head.
    Current,
}

/// The probe result as the plugin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub kind: OfferKind,
    pub installed: Option<String>,
    pub available: Option<String>,
}

/// Probe the repo and the cache and describe the result. Network calls, may take seconds.
pub fn offer(cfg: &Config) -> (UpdateInfo, Offer) {
    let info = probe_update(cfg);
    let offer = Offer {
        kind: info.kind(),
        installed: info.local_rev.clone(),
        available: info.remote_rev.clone(),
    };
    (info, offer)
}

/// Compare the running system with the repo and the cache. Network calls, may take seconds.
pub fn probe_update(cfg: &Config) -> UpdateInfo {
    let local_rev = local_rev();
    let remote_rev = remote_rev(cfg);
    let cache_ready = match (&local_rev, &remote_rev) {
        (Some(local), Some(remote)) if local == remote => false,
        (_, Some(remote)) => cache_ready(cfg, remote),
        _ => false,
    };
    UpdateInfo {
        local_rev,
        remote_rev,
        cache_ready,
        restart_pending: restart_pending(),
    }
}

/// What the check decided the plugin should show. Written by `check` in the
/// gaming session, picked up by `plugin watch`, acted on by the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub offer: Offer,
    /// Deferrals are used up: install without asking.
    pub forced: bool,
    /// Whether Later is offered and whether pressing it warns.
    pub show_later: bool,
    pub warn_on_later: bool,
    pub requested_at: u64,
}

pub fn write_prompt(prompt: &Prompt) -> Result<()> {
    atomic_write(
        &status::user_dir()?.join(PROMPT_FILE),
        &serde_json::to_vec(prompt)?,
    )
}

/// Read and delete the pending prompt, if any. A prompt older than `max_age`
/// was for a moment that has passed (the plugin was not listening) and is dropped.
pub fn take_prompt(max_age: Duration) -> Option<Prompt> {
    let path = status::user_dir().ok()?.join(PROMPT_FILE);
    let bytes = fs::read(&path).ok()?;
    let _ = fs::remove_file(&path);
    let prompt: Prompt = serde_json::from_slice(&bytes).ok()?;
    let cutoff = now().saturating_sub(max_age.as_secs());
    (prompt.requested_at >= cutoff).then_some(prompt)
}

/// The scheduled check: decide whether to prompt now.
pub fn run(cfg: &Config, force: bool) -> Result<()> {
    let mut state = State::load();
    let t = now();
    if !force && (t < state.next_due || t < state.snooze_until) {
        return Ok(());
    }
    if let Some(why) = gate_blocked() {
        println!("not prompting: {why}");
        return Ok(());
    }

    let (info, offer) = offer(cfg);
    if !info.update_available() && !info.restart_pending {
        if info.confirmed_current() {
            state.reset_deferrals();
            state.roll_due(&cfg.check_due)?;
            state.save()?;
            println!("up to date; next check after {}", state.next_due);
        } else if info.remote_differs() {
            println!(
                "update {} is not in the cache yet; waiting for the cache",
                short(&info.remote_rev)
            );
        } else {
            println!("could not reach the repo; will try again");
        }
        return Ok(());
    }

    let prompt = build_prompt(cfg, &state, offer, battery::read());
    write_prompt(&prompt)?;
    state.last_prompt = t;
    state.snooze_until = now() + SNOOZE_SECS;
    state.save()?;
    println!(
        "handed a prompt to the plugin ({:?}{})",
        prompt.offer.kind,
        if prompt.forced { ", forced" } else { "" }
    );
    Ok(())
}

/// Apply the deferral policy. A round past its deferrals installs on its own,
/// unless the battery is too low for that, in which case it asks once more
/// without offering Later. A pending restart is never forced.
fn build_prompt(
    cfg: &Config,
    state: &State,
    offer: Offer,
    battery: Option<battery::Battery>,
) -> Prompt {
    let policy = later_policy(state.deferrals, cfg.max_deferrals);
    let forced = !policy.show
        && offer.kind == OfferKind::Update
        && !battery::blocks(battery, cfg.battery_gate);
    Prompt {
        offer,
        forced,
        show_later: policy.show,
        warn_on_later: policy.warn,
        requested_at: now(),
    }
}

fn short(rev: &Option<String>) -> String {
    rev.as_deref()
        .map(|r| r.chars().take(7).collect())
        .unwrap_or_else(|| "?".into())
}

// ---- gates ---------------------------------------------------------------

/// Why a prompt would be unwelcome right now, or None when Steam is idle on its home screen.
fn gate_blocked() -> Option<String> {
    match steam_uptime_secs() {
        None => return Some("steam is not running".into()),
        Some(up) if up < MIN_STEAM_UPTIME_SECS => {
            return Some(format!("steam has only been up {up}s"))
        }
        Some(_) => {}
    }
    if !steam_home_shown() {
        return Some("steam home screen is not shown".into());
    }
    if game_running() {
        return Some("a game is running".into());
    }
    None
}

/// gamescope's focused app is Steam's own home screen. False on any X error.
pub fn steam_home_shown() -> bool {
    focused_app_gfx().ok().flatten() == Some(STEAM_HOME_APPID)
}

fn focused_app_gfx() -> Result<Option<u32>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atom = conn.intern_atom(true, FOCUSED_APP_PROPERTY)?.reply()?.atom;
    if atom == u32::from(AtomEnum::NONE) {
        return Ok(None);
    }
    let reply = conn
        .get_property(false, root, atom, AtomEnum::CARDINAL, 0, 1)?
        .reply()?;
    Ok(reply.value32().and_then(|mut values| values.next()))
}

/// A Steam-launched game is alive: a `reaper` process whose cmdline carries `SteamLaunch`.
pub fn game_running() -> bool {
    game_running_in(Path::new("/proc"))
}

fn game_running_in(proc_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(proc_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let is_pid = entry
            .file_name()
            .to_str()
            .map(|n| n.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(false);
        is_pid && is_steam_launch(&path)
    })
}

fn is_steam_launch(proc_entry: &Path) -> bool {
    let comm = fs::read_to_string(proc_entry.join("comm")).unwrap_or_default();
    if comm.trim() != "reaper" {
        return false;
    }
    let cmdline = fs::read(proc_entry.join("cmdline")).unwrap_or_default();
    String::from_utf8_lossy(&cmdline).contains("SteamLaunch")
}

/// Seconds since the user's Steam unit became active; None unless it is active.
/// Wall-clock, so time spent suspended does not count as Steam being up.
pub fn steam_uptime_secs() -> Option<u64> {
    let show = run_opt(
        "systemctl",
        &[
            "--user",
            "show",
            STEAM_UNIT,
            "-p",
            "ActiveState",
            "-p",
            "ActiveEnterTimestamp",
            "--timestamp=unix",
        ],
    )?;
    uptime_from(&show, now())
}

/// Seconds between `systemctl show --timestamp=unix` output (`ActiveEnterTimestamp=@1762102800`)
/// and `now`, None unless the unit is active.
fn uptime_from(show: &str, now: u64) -> Option<u64> {
    let mut active = false;
    let mut entered = None;
    for line in show.lines() {
        match line.split_once('=') {
            Some(("ActiveState", v)) => active = v.trim() == "active",
            Some(("ActiveEnterTimestamp", v)) => {
                entered = v.trim().trim_start_matches('@').parse::<u64>().ok()
            }
            _ => {}
        }
    }
    if !active {
        return None;
    }
    Some(now.saturating_sub(entered?))
}

// ---- probes --------------------------------------------------------------

pub fn local_rev() -> Option<String> {
    run_opt("nixos-version", &["--json"]).and_then(|json| configuration_revision(&json))
}

/// Head of the followed branch on GitHub; None when offline or after the timeout.
pub fn remote_rev(cfg: &Config) -> Option<String> {
    let url = format!("https://github.com/{}", cfg.repo);
    let refspec = format!("refs/heads/{}", cfg.branch);
    run_opt(
        "timeout",
        &[NETWORK_TIMEOUT_SECS, "git", "ls-remote", &url, &refspec],
    )
    .and_then(|out| first_field(&out))
}

fn cache_ready(cfg: &Config, rev: &str) -> bool {
    let path = format!(
        "repos/{}/actions/workflows/{}/runs?head_sha={rev}&status=success&per_page=1",
        cfg.repo, cfg.cache_workflow
    );
    github_get(&path)
        .map(|body| runs_succeeded(&body))
        .unwrap_or(false)
}

/// The system profile points past the booted generation.
pub fn restart_pending() -> bool {
    match (
        fs::canonicalize(SYSTEM_PROFILE),
        fs::canonicalize(BOOTED_SYSTEM),
    ) {
        (Ok(profile), Ok(booted)) => profile != booted,
        _ => false,
    }
}

/// GET a GitHub REST path with curl; None on any failure (offline, 4xx, no curl).
fn github_get(path: &str) -> Option<String> {
    let url = format!("https://api.github.com/{path}");
    run_opt(
        "curl",
        &[
            "-sf",
            "--connect-timeout",
            CONNECT_TIMEOUT_SECS,
            "--max-time",
            NETWORK_TIMEOUT_SECS,
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: steam-maintenance",
            &url,
        ],
    )
}

// ---- pure extraction helpers -------------------------------------------

/// `configurationRevision` from `nixos-version --json`.
fn configuration_revision(json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(json).ok()?;
    value
        .get("configurationRevision")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// First whitespace-separated field of `git ls-remote` output.
fn first_field(out: &str) -> Option<String> {
    out.split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `total_count > 0` from an Actions workflow-runs response.
fn runs_succeeded(json: &str) -> bool {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v.get("total_count")?.as_u64())
        .map(|n| n > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::Battery;

    #[test]
    fn configuration_revision_is_read_and_optional() {
        let with = r#"{"nixosVersion":"26.05.1234.abc","nixpkgsRevision":"abc","configurationRevision":"deadbeef"}"#;
        assert_eq!(configuration_revision(with).as_deref(), Some("deadbeef"));
        let without = r#"{"nixosVersion":"26.05.1234.abc","nixpkgsRevision":"abc"}"#;
        assert_eq!(configuration_revision(without), None);
        assert_eq!(configuration_revision("not json"), None);
    }

    #[test]
    fn ls_remote_first_field() {
        let out = "0123456789abcdef0123456789abcdef01234567\trefs/heads/main";
        assert_eq!(
            first_field(out).as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(first_field(""), None);
        assert_eq!(first_field("   \n"), None);
    }

    #[test]
    fn workflow_runs_total_count() {
        assert!(runs_succeeded(
            r#"{"total_count":1,"workflow_runs":[{"id":1}]}"#
        ));
        assert!(!runs_succeeded(r#"{"total_count":0,"workflow_runs":[]}"#));
        assert!(!runs_succeeded(r#"{"message":"Not Found"}"#));
        assert!(!runs_succeeded("<html>"));
    }

    #[test]
    fn steam_uptime_from_unix_timestamps() {
        let show = "ActiveState=active\nActiveEnterTimestamp=@1762102800\n";
        assert_eq!(uptime_from(show, 1_762_103_000), Some(200));
        assert_eq!(
            uptime_from(show, 1_762_102_000),
            Some(0),
            "clock stepped back"
        );
        let inactive = "ActiveState=inactive\nActiveEnterTimestamp=\n";
        assert_eq!(uptime_from(inactive, 1_762_103_000), None);
        assert_eq!(uptime_from("ActiveState=active\n", 1_762_103_000), None);
        assert_eq!(
            uptime_from("ActiveState=active\nActiveEnterTimestamp=garbage\n", 1),
            None
        );
    }

    fn info(
        local: Option<&str>,
        remote: Option<&str>,
        cache_ready: bool,
        restart_pending: bool,
    ) -> UpdateInfo {
        UpdateInfo {
            local_rev: local.map(str::to_string),
            remote_rev: remote.map(str::to_string),
            cache_ready,
            restart_pending,
        }
    }

    #[test]
    fn update_info_states() {
        let same = info(Some("a"), Some("a"), false, false);
        assert!(same.confirmed_current());
        assert!(!same.remote_differs());
        assert!(!same.update_available());
        assert_eq!(same.kind(), OfferKind::Current);

        let uncached = info(Some("a"), Some("b"), false, false);
        assert!(uncached.remote_differs());
        assert!(!uncached.update_available());
        assert_eq!(uncached.kind(), OfferKind::Preparing);

        let ready = info(Some("a"), Some("b"), true, false);
        assert!(ready.update_available());
        assert_eq!(ready.kind(), OfferKind::Update);

        let offline = info(Some("a"), None, false, false);
        assert!(!offline.remote_differs());
        assert!(!offline.confirmed_current());
        assert_eq!(offline.kind(), OfferKind::Offline);

        let staged = info(Some("a"), Some("a"), false, true);
        assert_eq!(staged.kind(), OfferKind::Restart);
        let staged_and_newer = info(Some("a"), Some("b"), true, true);
        assert_eq!(staged_and_newer.kind(), OfferKind::Update);
    }

    fn offer_of(kind: OfferKind) -> Offer {
        Offer {
            kind,
            installed: Some("a".into()),
            available: Some("b".into()),
        }
    }

    fn cfg() -> Config {
        Config::from_env()
    }

    #[test]
    fn prompt_offers_later_until_the_limit() {
        let state = State::default();
        let p = build_prompt(&cfg(), &state, offer_of(OfferKind::Update), None);
        assert!(p.show_later && !p.warn_on_later && !p.forced);
        let state = State {
            deferrals: 1,
            ..State::default()
        };
        let p = build_prompt(&cfg(), &state, offer_of(OfferKind::Update), None);
        assert!(p.show_later && p.warn_on_later && !p.forced);
    }

    #[test]
    fn exhausted_deferrals_force_an_install_unless_the_battery_is_low() {
        let state = State {
            deferrals: 2,
            ..State::default()
        };
        let p = build_prompt(&cfg(), &state, offer_of(OfferKind::Update), None);
        assert!(p.forced && !p.show_later);
        let low = Some(Battery {
            percent: 10,
            charging: false,
        });
        let p = build_prompt(&cfg(), &state, offer_of(OfferKind::Update), low);
        assert!(!p.forced && !p.show_later);
    }

    #[test]
    fn a_pending_restart_is_never_forced() {
        let state = State {
            deferrals: 5,
            ..State::default()
        };
        let p = build_prompt(&cfg(), &state, offer_of(OfferKind::Restart), None);
        assert!(!p.forced && !p.show_later);
    }

    #[test]
    fn prompt_round_trips_and_is_taken_once() {
        let dir = std::env::temp_dir().join(format!("sm-prompt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let prompt = Prompt {
            offer: offer_of(OfferKind::Update),
            forced: false,
            show_later: true,
            warn_on_later: false,
            requested_at: now(),
        };
        write_prompt(&prompt).unwrap();
        assert_eq!(take_prompt(Duration::from_secs(60)), Some(prompt.clone()));
        assert_eq!(take_prompt(Duration::from_secs(60)), None);
        let stale = Prompt {
            requested_at: now() - 120,
            ..prompt
        };
        write_prompt(&stale).unwrap();
        assert_eq!(take_prompt(Duration::from_secs(60)), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn game_running_scans_reaper_processes() {
        let dir = std::env::temp_dir().join(format!("sm-proc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mk = |pid: &str, comm: &str, cmdline: &[&str]| {
            let p = dir.join(pid);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(p.join("cmdline"), cmdline.join("\0")).unwrap();
        };
        mk("100", "steam", &["steam"]);
        mk(
            "101",
            "reaper",
            &["reaper", "SteamLaunch", "AppId=1234", "--", "game"],
        );
        fs::create_dir_all(dir.join("self")).unwrap();
        assert!(game_running_in(&dir));
        fs::remove_dir_all(dir.join("101")).unwrap();
        assert!(!game_running_in(&dir));
        mk("102", "reaper", &["reaper", "something-else"]);
        assert!(!game_running_in(&dir));
        fs::remove_dir_all(&dir).unwrap();
        assert!(!game_running_in(&dir));
    }
}
