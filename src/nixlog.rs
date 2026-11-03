//! Parser for `nix --log-format internal-json` output.
//!
//! nix writes one `@nix {...}` object per stderr line. Activities (downloads, builds
//! and their aggregates) start, report results and stop; free-standing messages carry
//! errors and warnings. `Progress` folds that stream into counters, a percentage, a
//! one-line detail and a tail of build output for the worker to publish as `Status`.
//!
//! Two things the stream does that are easy to get wrong:
//! - `setExpected` for copyPath/fileTransfer is a byte total, not a path count. The
//!   path counts come from the copyPaths/builds aggregates' `progress` results.
//! - The aggregates' `expected` shrinks as well as grows: a derivation goal that turns
//!   out to be substitutable leaves the build count, and the last report before the
//!   aggregate stops can still count a finished goal as pending. `done` is kept
//!   monotonic, `expected` follows nix and closes onto `done` when the aggregate stops
//!   without failures, and the percentage is held monotonic with a high-water mark.
//! - One nixos-rebuild run can hold several nix builds (fetching the flake, then the
//!   system). A fresh top-level aggregate after the previous ones stopped starts the
//!   counters and the high-water mark over, so the second build is not pinned at 100%.

use crate::status::{Counter, Phase};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

const PREFIX: &str = "@nix ";
const LOG_TAIL_LINES: usize = 40;
/// One local build counts as this many downloads in the percentage.
const BUILD_WEIGHT: u64 = 3;

// Activity types.
const ACT_COPY_PATH: u64 = 100;
const ACT_FILE_TRANSFER: u64 = 101;
const ACT_REALISE: u64 = 102;
const ACT_COPY_PATHS: u64 = 103;
const ACT_BUILDS: u64 = 104;
const ACT_BUILD: u64 = 105;
const ACT_SUBSTITUTE: u64 = 108;

// Result types.
const RES_BUILD_LOG_LINE: u64 = 101;
const RES_SET_PHASE: u64 = 104;
const RES_PROGRESS: u64 = 105;
const RES_SET_EXPECTED: u64 = 106;

const LEVEL_ERROR: u64 = 0;

/// One `@nix` line; every field is optional in the stream, so all default.
#[derive(Deserialize)]
struct Line {
    action: String,
    #[serde(default)]
    id: u64,
    #[serde(default, rename = "type")]
    kind: u64,
    #[serde(default)]
    level: u64,
    #[serde(default)]
    parent: u64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    fields: Vec<Value>,
}

/// An activity that has started and not yet stopped.
#[derive(Debug)]
struct Activity {
    kind: u64,
    parent: u64,
    label: String,
    phase: Option<String>,
    /// Start order, so "most recent" is well defined.
    seq: u64,
    /// Bytes for copies and transfers.
    bytes: Counter,
    /// Failures reported so far, for aggregates.
    failed: u64,
}

#[derive(Debug, Default)]
pub struct Progress {
    pub copies: Counter,
    pub builds: Counter,
    pub detail: String,
    pub log_tail: Vec<String>,
    open: HashMap<u64, Activity>,
    next_seq: u64,
    high_water: Option<u8>,
    last_error: Option<String>,
}

impl Progress {
    pub fn new() -> Progress {
        Progress::default()
    }

    /// Feed one stderr line. Lines not starting with `@nix ` are ignored and return false.
    /// Malformed JSON after the prefix is dropped but still returns true.
    pub fn feed_line(&mut self, line: &str) -> bool {
        let Some(json) = line.strip_prefix(PREFIX) else {
            return false;
        };
        let Ok(entry) = serde_json::from_str::<Line>(json) else {
            return true;
        };
        match entry.action.as_str() {
            "start" => self.start(entry),
            "result" => self.result(entry),
            "stop" => self.stop(entry.id),
            "msg" => self.message(entry),
            _ => {}
        }
        self.high_water = self.high_water.max(self.current_percent());
        true
    }

    /// Weighted, monotonic percentage; None until anything is expected.
    pub fn percent(&self) -> Option<u8> {
        self.high_water
    }

    /// Building while a build is open; Downloading while a copy, substitution or
    /// transfer is open or downloads are expected; None while still evaluating.
    pub fn phase_hint(&self) -> Option<Phase> {
        if self.open.values().any(|a| a.kind == ACT_BUILD) {
            Some(Phase::Building)
        } else if self.copies.expected > 0 || self.open.values().any(|a| is_download(a.kind)) {
            Some(Phase::Downloading)
        } else {
            None
        }
    }

    /// The last error message nix printed, with terminal colours stripped.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Builds and downloads in flight. A substitution and the copy it performs count once.
    #[cfg(test)]
    pub fn active_count(&self) -> usize {
        self.open
            .values()
            .filter(|a| match a.kind {
                ACT_BUILD | ACT_SUBSTITUTE => true,
                ACT_COPY_PATH => !self.parent_is(a, ACT_SUBSTITUTE),
                _ => false,
            })
            .count()
    }

    #[cfg(test)]
    fn parent_is(&self, act: &Activity, kind: u64) -> bool {
        self.open.get(&act.parent).is_some_and(|p| p.kind == kind)
    }

    fn start(&mut self, e: Line) {
        let subject = field_str(&e.fields, 0).unwrap_or_else(|| quoted(&e.text));
        let label = match e.kind {
            ACT_BUILD => derivation_name(subject),
            ACT_COPY_PATH | ACT_SUBSTITUTE => store_path_name(subject),
            ACT_FILE_TRANSFER => file_name(subject),
            ACT_REALISE | ACT_COPY_PATHS | ACT_BUILDS => String::new(),
            _ => return,
        };
        if e.parent == 0
            && is_aggregate(e.kind)
            && !self.open.values().any(|a| is_aggregate(a.kind))
        {
            self.reset_counters();
        }
        self.next_seq += 1;
        self.open.insert(
            e.id,
            Activity {
                kind: e.kind,
                parent: e.parent,
                label,
                phase: None,
                seq: self.next_seq,
                bytes: Counter::default(),
                failed: 0,
            },
        );
        self.refresh_detail();
    }

    /// A new nix build is starting: what the previous one counted no longer applies.
    fn reset_counters(&mut self) {
        self.copies = Counter::default();
        self.builds = Counter::default();
        self.high_water = None;
    }

    /// An aggregate that stops without failures has done everything it expected, even
    /// when its last report still counted the final goal as pending.
    fn stop(&mut self, id: u64) {
        let Some(act) = self.open.remove(&id) else {
            return;
        };
        if act.failed == 0 {
            match act.kind {
                ACT_COPY_PATHS => self.copies.expected = self.copies.done,
                ACT_BUILDS => self.builds.expected = self.builds.done,
                _ => {}
            }
        }
        self.refresh_detail();
    }

    fn result(&mut self, e: Line) {
        match e.kind {
            RES_BUILD_LOG_LINE => {
                if let Some(line) = field_str(&e.fields, 0) {
                    self.push_log(line);
                }
            }
            RES_SET_EXPECTED => self.set_expected(&e.fields),
            RES_PROGRESS => self.progress(e.id, &e.fields),
            RES_SET_PHASE => {
                if let Some(act) = self.open.get_mut(&e.id) {
                    act.phase = field_str(&e.fields, 0).map(str::to_string);
                    self.refresh_detail();
                }
            }
            _ => {}
        }
    }

    fn message(&mut self, e: Line) {
        let text = strip_ansi(&e.msg);
        if e.level == LEVEL_ERROR || text.starts_with("error:") {
            self.last_error = Some(text);
        }
    }

    fn push_log(&mut self, line: &str) {
        self.log_tail.push(line.to_string());
        if self.log_tail.len() > LOG_TAIL_LINES {
            let excess = self.log_tail.len() - LOG_TAIL_LINES;
            self.log_tail.drain(..excess);
        }
    }

    /// `[activityType, n]`: a count for builds and substitutions, a byte total for
    /// copies and transfers (which the counters must not see).
    fn set_expected(&mut self, fields: &[Value]) {
        let (Some(kind), Some(n)) = (field_u64(fields, 0), field_u64(fields, 1)) else {
            return;
        };
        match kind {
            ACT_BUILD => self.builds.expected = self.builds.expected.max(n),
            ACT_SUBSTITUTE => self.copies.expected = self.copies.expected.max(n),
            _ => {}
        }
    }

    /// `[done, expected, running, failed]` on an aggregate, `[done, expected]` bytes on
    /// a copy or transfer.
    fn progress(&mut self, id: u64, fields: &[Value]) {
        let (Some(done), Some(expected)) = (field_u64(fields, 0), field_u64(fields, 1)) else {
            return;
        };
        let Some(act) = self.open.get_mut(&id) else {
            return;
        };
        act.failed = field_u64(fields, 3).unwrap_or(0);
        match act.kind {
            ACT_COPY_PATHS => update_counter(&mut self.copies, done, expected),
            ACT_BUILDS => update_counter(&mut self.builds, done, expected),
            ACT_COPY_PATH | ACT_FILE_TRANSFER => act.bytes = Counter { done, expected },
            _ => {}
        }
    }

    /// The most recently started build, else the most recent copy, with its phase.
    fn refresh_detail(&mut self) {
        let newest = |pred: &dyn Fn(u64) -> bool| {
            self.open
                .values()
                .filter(|a| pred(a.kind))
                .max_by_key(|a| a.seq)
        };
        let act = newest(&|k| k == ACT_BUILD)
            .or_else(|| newest(&|k| k == ACT_COPY_PATH || k == ACT_SUBSTITUTE));
        self.detail = match act {
            Some(Activity {
                label,
                phase: Some(phase),
                ..
            }) => format!("{label} ({phase})"),
            Some(act) => act.label.clone(),
            None => String::new(),
        };
    }

    fn current_percent(&self) -> Option<u8> {
        let expected = self.copies.expected + BUILD_WEIGHT * self.builds.expected;
        if expected == 0 {
            return None;
        }
        let done = (self.copies.done + BUILD_WEIGHT * self.builds.done) as f64
            + self.inflight_copy_fraction();
        let percent = (100.0 * done / expected as f64).round().min(100.0);
        Some(percent as u8)
    }

    /// How far the copies in flight have got, averaged, so it adds at most one
    /// download's worth to the count-based percentage.
    fn inflight_copy_fraction(&self) -> f64 {
        let copies: Vec<(&u64, &Activity)> = self
            .open
            .iter()
            .filter(|(_, a)| a.kind == ACT_COPY_PATH)
            .collect();
        if copies.is_empty() {
            return 0.0;
        }
        let total: f64 = copies
            .iter()
            .map(|(id, copy)| self.copy_fraction(**id, copy))
            .sum();
        (total / copies.len() as f64).clamp(0.0, 1.0)
    }

    /// A copy's own byte fraction, or while that is still unknown the fraction of the
    /// transfers feeding it.
    fn copy_fraction(&self, id: u64, copy: &Activity) -> f64 {
        if copy.bytes.expected > 0 {
            return fraction(copy.bytes);
        }
        let mut sum = Counter::default();
        for t in self.open.values() {
            if t.kind == ACT_FILE_TRANSFER && t.parent == id {
                sum.done += t.bytes.done;
                sum.expected += t.bytes.expected;
            }
        }
        fraction(sum)
    }
}

/// `done` never decreases; `expected` follows nix but never drops below `done`.
fn update_counter(counter: &mut Counter, done: u64, expected: u64) {
    counter.done = counter.done.max(done);
    counter.expected = expected.max(counter.done);
}

fn fraction(c: Counter) -> f64 {
    if c.expected == 0 {
        0.0
    } else {
        (c.done as f64 / c.expected as f64).clamp(0.0, 1.0)
    }
}

fn is_download(kind: u64) -> bool {
    matches!(kind, ACT_COPY_PATH | ACT_SUBSTITUTE | ACT_FILE_TRANSFER)
}

/// The activities nix opens once per build to report totals under.
fn is_aggregate(kind: u64) -> bool {
    matches!(kind, ACT_REALISE | ACT_COPY_PATHS | ACT_BUILDS)
}

fn field_str(fields: &[Value], i: usize) -> Option<&str> {
    fields.get(i).and_then(Value::as_str)
}

fn field_u64(fields: &[Value], i: usize) -> Option<u64> {
    fields.get(i).and_then(Value::as_u64)
}

/// The text between the first pair of single quotes, or the whole text.
fn quoted(text: &str) -> &str {
    let mut parts = text.splitn(3, '\'');
    match (parts.next(), parts.next(), parts.next()) {
        (_, Some(inner), Some(_)) => inner,
        _ => text,
    }
}

/// `/nix/store/<hash>-name` -> `name`.
fn store_path_name(path: &str) -> String {
    let base = file_name(path);
    match base.split_once('-') {
        Some((hash, name)) if hash.len() == 32 && !name.is_empty() => name.to_string(),
        _ => base,
    }
}

/// `/nix/store/<hash>-name.drv` -> `name`.
fn derivation_name(path: &str) -> String {
    let name = store_path_name(path);
    name.strip_suffix(".drv").unwrap_or(&name).to_string()
}

/// The last path or URL segment.
fn file_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Drop ANSI escape sequences (`ESC [ ... <final byte>`).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWNLOAD: &str = include_str!("../tests/fixtures/nix-log-download.jsonl");
    const BUILD: &str = include_str!("../tests/fixtures/nix-log-build.jsonl");
    const FAIL: &str = include_str!("../tests/fixtures/nix-log-fail.jsonl");

    /// What was observed while feeding a log line by line.
    struct Run {
        progress: Progress,
        phases: Vec<Option<Phase>>,
        details: Vec<String>,
        percents: Vec<Option<u8>>,
    }

    fn feed(log: &str) -> Run {
        let mut run = Run {
            progress: Progress::new(),
            phases: Vec::new(),
            details: Vec::new(),
            percents: Vec::new(),
        };
        for line in log.lines() {
            assert!(run.progress.feed_line(line), "unparsed line: {line}");
            run.phases.push(run.progress.phase_hint());
            run.details.push(run.progress.detail.clone());
            run.percents.push(run.progress.percent());
        }
        run
    }

    fn assert_monotonic(percents: &[Option<u8>]) {
        let mut last = None;
        for p in percents {
            assert!(*p >= last, "percent went from {last:?} to {p:?}");
            last = *p;
        }
    }

    fn start(id: u64, kind: u64, parent: u64, fields: &str) -> String {
        format!(
            r#"@nix {{"action":"start","id":{id},"level":3,"parent":{parent},"text":"","type":{kind},"fields":{fields}}}"#
        )
    }

    fn result(id: u64, kind: u64, fields: &str) -> String {
        format!(r#"@nix {{"action":"result","id":{id},"type":{kind},"fields":{fields}}}"#)
    }

    fn stop(id: u64) -> String {
        format!(r#"@nix {{"action":"stop","id":{id}}}"#)
    }

    fn feed_all(p: &mut Progress, lines: &[String]) {
        for line in lines {
            assert!(p.feed_line(line));
        }
    }

    const LINUX: &str =
        r#"["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-linux-6.12", "https://cache.nixos.org"]"#;
    const MESA: &str = r#"["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-mesa-24.0.drv", "", 1, 1]"#;
    const GLIB: &str = r#"["/nix/store/cccccccccccccccccccccccccccccccc-glib-2.80.drv", "", 1, 1]"#;

    #[test]
    fn download_fixture_completes() {
        let run = feed(DOWNLOAD);
        let p = &run.progress;
        assert!(p.copies.expected > 0);
        assert_eq!(p.copies.done, p.copies.expected);
        assert_eq!(p.percent(), Some(100));
        assert!(run.phases.contains(&Some(Phase::Downloading)));
        assert!(!run.phases.contains(&Some(Phase::Building)));
        assert!(run.details.iter().any(|d| d == "cowsay-3.8.4"));
        assert!(p.detail.is_empty(), "nothing open at the end");
        assert_eq!(p.active_count(), 0);
        assert!(p.last_error().is_none());
        assert_monotonic(&run.percents);
    }

    #[test]
    fn build_fixture_builds_one_derivation() {
        let run = feed(BUILD);
        let p = &run.progress;
        assert_eq!(p.builds.done, 1);
        assert_eq!(p.builds.expected, 1);
        assert_eq!(p.percent(), Some(100));
        assert!(run.phases.contains(&Some(Phase::Building)));
        assert!(run.details.iter().any(|d| d == "sm-fixture-build"));
        assert_eq!(p.log_tail, vec!["hello", "world"]);
        assert!(p.last_error().is_none());
        assert_monotonic(&run.percents);
    }

    #[test]
    fn fail_fixture_reports_the_error() {
        let run = feed(FAIL);
        let p = &run.progress;
        let err = p.last_error().expect("an error message");
        assert!(err.starts_with("error: Cannot build"), "{err}");
        assert!(err.contains("sm-fixture-fail.drv"));
        assert!(!err.contains('\u{1b}'), "ANSI stripped: {err}");
        assert_eq!(
            p.log_tail,
            vec!["starting", "step two", "something went wrong"]
        );
        assert!(run.details.iter().any(|d| d == "sm-fixture-fail"));
        assert_eq!(p.builds.done, 0);
        assert!(
            p.percent() < Some(100),
            "a failed build does not reach 100%"
        );
        assert_monotonic(&run.percents);
    }

    #[test]
    fn foreign_lines_are_ignored() {
        let mut p = Progress::new();
        assert!(!p.feed_line("building the system configuration..."));
        assert!(!p.feed_line(""));
        assert!(!p.feed_line("@nixos is not the prefix"));
        assert!(p.feed_line("@nix not json at all"));
        assert!(p.feed_line(r#"@nix {"action":"dance"}"#));
        assert_eq!(p.percent(), None);
        assert_eq!(p.phase_hint(), None);
        assert!(p.detail.is_empty());
    }

    #[test]
    fn set_expected_counts_builds_and_substitutions_only() {
        let mut p = Progress::new();
        feed_all(
            &mut p,
            &[
                start(1, ACT_REALISE, 0, "[]"),
                result(1, RES_SET_EXPECTED, "[105, 3]"),
                result(1, RES_SET_EXPECTED, "[108, 5]"),
                result(1, RES_SET_EXPECTED, "[100, 52448]"),
                result(1, RES_SET_EXPECTED, "[101, 16439]"),
                result(1, RES_SET_EXPECTED, "[105, 2]"),
            ],
        );
        assert_eq!(p.builds.expected, 3, "max of what is known");
        assert_eq!(p.copies.expected, 5, "byte totals do not count paths");
        assert_eq!(p.builds.done, 0);
        assert_eq!(p.percent(), Some(0));
        assert_eq!(p.phase_hint(), Some(Phase::Downloading));
    }

    #[test]
    fn aggregate_progress_drives_the_counters() {
        let mut p = Progress::new();
        feed_all(
            &mut p,
            &[
                start(3, ACT_COPY_PATHS, 0, "[]"),
                start(4, ACT_BUILDS, 0, "[]"),
                result(3, RES_PROGRESS, "[1, 4, 1, 0]"),
                result(4, RES_PROGRESS, "[1, 2, 0, 0]"),
            ],
        );
        assert_eq!(
            p.copies,
            Counter {
                done: 1,
                expected: 4
            }
        );
        assert_eq!(
            p.builds,
            Counter {
                done: 1,
                expected: 2
            }
        );
        // (1 + 3*1) / (4 + 3*2) = 40%
        assert_eq!(p.percent(), Some(40));

        // done never decreases; expected follows nix but stays >= done.
        feed_all(
            &mut p,
            &[
                result(3, RES_PROGRESS, "[0, 4, 0, 0]"),
                result(4, RES_PROGRESS, "[1, 1, 0, 0]"),
            ],
        );
        assert_eq!(
            p.copies,
            Counter {
                done: 1,
                expected: 4
            }
        );
        assert_eq!(
            p.builds,
            Counter {
                done: 1,
                expected: 1
            }
        );
        // (1 + 3) / (4 + 3) = 57%
        assert_eq!(p.percent(), Some(57));

        // A later rise in expected cannot pull the percentage back down.
        feed_all(&mut p, &[result(4, RES_PROGRESS, "[1, 10, 0, 0]")]);
        assert_eq!(p.percent(), Some(57));

        // An aggregate stopping without failures closes expected onto done.
        feed_all(&mut p, &[stop(4)]);
        assert_eq!(
            p.builds,
            Counter {
                done: 1,
                expected: 1
            }
        );
        // (1 + 3) / (4 + 3) = 57%, still, until the copies finish.
        assert_eq!(p.percent(), Some(57));
        feed_all(&mut p, &[result(3, RES_PROGRESS, "[3, 4, 0, 1]"), stop(3)]);
        assert_eq!(
            p.copies,
            Counter {
                done: 3,
                expected: 4
            },
            "a failure keeps it open"
        );
        assert_eq!(p.percent(), Some(86));
    }

    #[test]
    fn a_second_build_starts_the_counters_over() {
        let mut p = Progress::new();
        for line in DOWNLOAD.lines() {
            assert!(p.feed_line(line));
        }
        assert_eq!(p.percent(), Some(100));
        assert!(p.copies.expected > 0);

        // The real system build opens its own realise/builds/copyPaths aggregates.
        feed_all(
            &mut p,
            &[
                start(50, ACT_REALISE, 0, "[]"),
                start(51, ACT_BUILDS, 0, "[]"),
                start(52, ACT_COPY_PATHS, 0, "[]"),
            ],
        );
        assert_eq!(p.copies, Counter::default());
        assert_eq!(p.builds, Counter::default());
        assert_eq!(p.percent(), None, "nothing expected yet");

        feed_all(&mut p, &[result(52, RES_PROGRESS, "[10, 300, 4, 0]")]);
        assert_eq!(p.percent(), Some(3));
        assert_eq!(p.phase_hint(), Some(Phase::Downloading));

        // Aggregates opening while another is still open belong to the same build.
        feed_all(&mut p, &[stop(51), start(53, ACT_BUILDS, 0, "[]")]);
        assert_eq!(
            p.copies,
            Counter {
                done: 10,
                expected: 300
            }
        );
        assert_eq!(p.percent(), Some(3));
    }

    #[test]
    fn detail_prefers_the_newest_build_and_shows_its_phase() {
        let mut p = Progress::new();
        feed_all(
            &mut p,
            &[
                start(10, ACT_SUBSTITUTE, 0, LINUX),
                start(11, ACT_COPY_PATH, 10, LINUX),
            ],
        );
        assert_eq!(p.detail, "linux-6.12");
        assert_eq!(p.active_count(), 1, "substitute and its copy count once");
        assert_eq!(p.phase_hint(), Some(Phase::Downloading));

        feed_all(
            &mut p,
            &[start(20, ACT_BUILD, 0, MESA), start(21, ACT_BUILD, 0, GLIB)],
        );
        assert_eq!(p.detail, "glib-2.80");
        assert_eq!(p.active_count(), 3);
        assert_eq!(p.phase_hint(), Some(Phase::Building));

        feed_all(&mut p, &[result(20, RES_SET_PHASE, r#"["buildPhase"]"#)]);
        assert_eq!(p.detail, "glib-2.80", "another build's phase is irrelevant");
        feed_all(
            &mut p,
            &[stop(21), result(20, RES_SET_PHASE, r#"["installPhase"]"#)],
        );
        assert_eq!(p.detail, "mesa-24.0 (installPhase)");

        feed_all(&mut p, &[stop(20)]);
        assert_eq!(p.detail, "linux-6.12");
        assert_eq!(p.phase_hint(), Some(Phase::Downloading));
        feed_all(&mut p, &[stop(11), stop(10)]);
        assert!(p.detail.is_empty());
        assert_eq!(p.active_count(), 0);
    }

    #[test]
    fn build_start_without_fields_uses_the_text() {
        let mut p = Progress::new();
        assert!(p.feed_line(
            r#"@nix {"action":"start","id":5,"level":3,"parent":0,"text":"building '/nix/store/dddddddddddddddddddddddddddddddd-foo-1.0.drv'","type":105}"#
        ));
        assert_eq!(p.detail, "foo-1.0");
    }

    #[test]
    fn byte_progress_adds_at_most_one_download() {
        let a = r#"["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a", "x"]"#;
        let b = r#"["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b", "x"]"#;
        let mut p = Progress::new();
        feed_all(
            &mut p,
            &[
                start(3, ACT_COPY_PATHS, 0, "[]"),
                result(3, RES_PROGRESS, "[0, 2, 2, 0]"),
                start(10, ACT_SUBSTITUTE, 0, a),
                start(11, ACT_COPY_PATH, 10, a),
                start(12, ACT_FILE_TRANSFER, 11, r#"["https://x/nar/a.nar.zst"]"#),
                start(20, ACT_SUBSTITUTE, 0, b),
                start(21, ACT_COPY_PATH, 20, b),
            ],
        );
        assert_eq!(p.percent(), Some(0));
        // The transfer feeding copy 11 is half done: the two copies average a quarter.
        feed_all(&mut p, &[result(12, RES_PROGRESS, "[500, 1000, 0, 0]")]);
        assert_eq!(p.percent(), Some(13));
        // Both copies fully unpacked but not yet counted: exactly one download's worth.
        feed_all(
            &mut p,
            &[
                result(11, RES_PROGRESS, "[4000, 4000, 0, 0]"),
                result(21, RES_PROGRESS, "[9000, 9000, 0, 0]"),
            ],
        );
        assert_eq!(p.percent(), Some(50));
        // Bytes past the expected total do not push further.
        feed_all(&mut p, &[result(21, RES_PROGRESS, "[9999, 9000, 0, 0]")]);
        assert_eq!(p.percent(), Some(50));
    }

    #[test]
    fn log_tail_keeps_the_last_forty_lines() {
        let mut p = Progress::new();
        feed_all(&mut p, &[start(7, ACT_BUILD, 0, MESA)]);
        for i in 0..50 {
            let line = result(7, RES_BUILD_LOG_LINE, &format!("[\"line {i}\"]"));
            assert!(p.feed_line(&line));
        }
        assert_eq!(p.log_tail.len(), 40);
        assert_eq!(p.log_tail[0], "line 10");
        assert_eq!(p.log_tail[39], "line 49");
    }

    #[test]
    fn error_messages_are_remembered_without_colour() {
        let mut p = Progress::new();
        assert!(
            p.feed_line(r#"@nix {"action":"msg","level":3,"msg":"these 2 paths will be fetched"}"#)
        );
        assert_eq!(p.last_error(), None);
        assert!(p.feed_line(
            r#"@nix {"action":"msg","level":0,"msg":"\u001b[31;1merror:\u001b[0m boom"}"#
        ));
        assert_eq!(p.last_error(), Some("error: boom"));
        assert!(p.feed_line(r#"@nix {"action":"msg","level":1,"msg":"error: later and louder"}"#));
        assert_eq!(p.last_error(), Some("error: later and louder"));
        assert!(p.feed_line(r#"@nix {"action":"msg","level":1,"msg":"warning: dirty tree"}"#));
        assert_eq!(p.last_error(), Some("error: later and louder"));
    }

    #[test]
    fn labels() {
        assert_eq!(
            store_path_name("/nix/store/ycj6knm38piypkxn00zjwjcdvnfd5xaj-cowsay-3.8.4"),
            "cowsay-3.8.4"
        );
        assert_eq!(
            derivation_name("/nix/store/ndwgi0fpjidzc50gj47ah1j4g695q2y2-sm-fixture-fail.drv"),
            "sm-fixture-fail"
        );
        assert_eq!(store_path_name("not-a-store-path"), "not-a-store-path");
        assert_eq!(
            file_name("https://cache.nixos.org/nar/abc.nar.zst"),
            "abc.nar.zst"
        );
        assert_eq!(
            quoted("copying path '/nix/store/x' to 'local'"),
            "/nix/store/x"
        );
        assert_eq!(quoted("no quotes"), "no quotes");
        assert_eq!(strip_ansi("\u{1b}[1mbold\u{1b}[0m plain"), "bold plain");
    }
}
