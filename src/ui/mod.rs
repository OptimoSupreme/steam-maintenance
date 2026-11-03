//! Full-screen SDL2 "System Update" UI, launched by Steam as a non-Steam shortcut.
//!
//! `run` owns the SDL objects and the frame loop; `App` is the state machine that
//! the loop feeds with time and input and asks for a `View` to draw.

mod draw;
mod input;
mod screens;
mod text;

use crate::battery::{self, Battery};
use crate::check::{self, UpdateInfo};
use crate::config::Config;
use crate::state::{self, State};
use crate::status::{self, Counter, Phase, Status};
use crate::util::{self, now, run_opt};
use anyhow::{bail, Context, Result};
use draw::{sdl_err, Scale};
use input::{Action, Dir, Stick};
use screens::{
    battery_blocks, later_policy, screen_buttons, short_rev, Available, Button, ButtonId, Ctx,
    Modal, ModalKind, Screen, View, SUBJECT_LINES,
};
use sdl2::controller::GameController;
use sdl2::event::{Event, WindowEvent};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{BlendMode, WindowCanvas};
use sdl2::surface::Surface;
use sdl2::GameControllerSubsystem;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use text::{Fonts, TextCache};

const FRAME: Duration = Duration::from_millis(33);
const IDLE_WAIT: Duration = Duration::from_millis(100);
const DEBOUNCE: Duration = Duration::from_millis(300);
const STATUS_POLL: Duration = Duration::from_millis(250);
const UNIT_POLL: Duration = Duration::from_secs(1);
const HEARTBEAT: Duration = Duration::from_secs(2);
const BATTERY_POLL: Duration = Duration::from_secs(10);
/// How long a freshly started unit may show nothing before it counts as stopped.
const START_GRACE: Duration = Duration::from_secs(20);
/// How long Done may wait for the worker's reboot before offering one.
const REBOOT_WAIT: Duration = Duration::from_secs(90);
const SNOOZE: u64 = 86_400;

const MOCKS: &[&str] = &[
    "checking",
    "up-to-date",
    "restart-pending",
    "update",
    "update-forced",
    "update-battery",
    "warning",
    "progress-evaluating",
    "progress",
    "progress-building",
    "cancel",
    "restarting",
    "failed",
];

/// Run the UI until the user leaves it. `mock` renders canned data with no side
/// effects; `size` opens a window instead of going full screen.
pub fn run(cfg: &Config, mock: Option<String>, size: Option<(u32, u32)>) -> Result<()> {
    let mut app = match mock {
        Some(name) => App::mock(cfg.clone(), &name)?,
        None => App::live(cfg.clone()),
    };

    let sdl = sdl2::init().map_err(sdl_err)?;
    let video = sdl.video().map_err(sdl_err)?;
    let controllers = sdl.game_controller().map_err(sdl_err)?;
    let (w, h) = size.unwrap_or((1280, 800));
    let mut builder = video.window("System Update", w, h);
    if size.is_some() {
        builder.position_centered();
    } else {
        builder.fullscreen_desktop();
    }
    let window = builder.build().context("creating the window")?;
    let mut canvas = window
        .into_canvas()
        .build()
        .context("creating the renderer")?;
    canvas.set_blend_mode(BlendMode::Blend);
    let creator = canvas.texture_creator();
    let fonts = Fonts::load()?;
    let mut text = TextCache::new(&creator, &fonts);
    let mut pump = sdl.event_pump().map_err(sdl_err)?;
    let mut pads = open_controllers(&controllers);
    let screenshot = std::env::var_os("STEAM_MAINTENANCE_SCREENSHOT");
    let mut last_frame = Instant::now() - FRAME;

    loop {
        let now = Instant::now();
        app.tick(now);
        if app.exit {
            return Ok(());
        }
        if app.dirty || (app.animating() && now.duration_since(last_frame) >= FRAME) {
            let (w, h) = canvas.output_size().map_err(sdl_err)?;
            let rects = {
                let mut ctx = Ctx {
                    canvas: &mut canvas,
                    text: &mut text,
                    s: Scale::new(w, h),
                };
                screens::draw_frame(&mut ctx, &app.view())?
            };
            app.hit_rects = rects;
            if let Some(path) = &screenshot {
                return save_screenshot(&canvas, path);
            }
            canvas.present();
            text.trim();
            app.dirty = false;
            last_frame = Instant::now();
        }
        let wait = if app.animating() {
            FRAME.saturating_sub(last_frame.elapsed())
        } else {
            IDLE_WAIT
        };
        let mut events = Vec::new();
        if let Some(event) = pump.wait_event_timeout(wait.as_millis().max(1) as u32) {
            events.push(event);
            events.extend(pump.poll_iter());
        }
        for event in events {
            match event {
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Ok(pad) = controllers.open(which) {
                        pads.push(pad);
                    }
                }
                Event::Window {
                    win_event: WindowEvent::SizeChanged(..) | WindowEvent::Exposed,
                    ..
                } => app.dirty = true,
                Event::Window { win_event, .. } => app.window_event(&win_event),
                other => {
                    if let Some(action) = input::map_event(&other, &mut app.stick) {
                        app.handle(action);
                    }
                }
            }
        }
        if let Some(action) = app.stick.poll(Instant::now()) {
            app.handle(action);
        }
    }
}

fn open_controllers(subsystem: &GameControllerSubsystem) -> Vec<GameController> {
    let count = subsystem.num_joysticks().unwrap_or(0);
    (0..count)
        .filter(|&i| subsystem.is_game_controller(i))
        .filter_map(|i| subsystem.open(i).ok())
        .collect()
}

fn save_screenshot(canvas: &WindowCanvas, path: &OsStr) -> Result<()> {
    let (w, h) = canvas.output_size().map_err(sdl_err)?;
    let mut pixels = canvas
        .read_pixels(None, PixelFormatEnum::RGB24)
        .map_err(sdl_err)?;
    let surface =
        Surface::from_data(&mut pixels, w, h, w * 3, PixelFormatEnum::RGB24).map_err(sdl_err)?;
    surface.save_bmp(path).map_err(sdl_err)
}

fn unit_is_active(unit: &str) -> bool {
    run_opt("systemctl", &["is-active", unit]).as_deref() == Some("active")
}

/// Whether the upgrade unit is active, polled off the render thread.
#[derive(Clone)]
struct UnitWatch {
    active: Arc<AtomicBool>,
    /// False until the first poll has answered.
    known: Arc<AtomicBool>,
}

impl UnitWatch {
    /// A fixed answer with no poller, for mocks and tests.
    fn fixed(active: bool) -> UnitWatch {
        UnitWatch {
            active: Arc::new(AtomicBool::new(active)),
            known: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Poll `systemctl is-active` every `UNIT_POLL` for as long as the app runs.
    fn spawn(unit: &str) -> UnitWatch {
        let watch = UnitWatch {
            active: Arc::new(AtomicBool::new(false)),
            known: Arc::new(AtomicBool::new(false)),
        };
        let poller = watch.clone();
        let unit = unit.to_string();
        std::thread::spawn(move || loop {
            poller.set(unit_is_active(&unit));
            std::thread::sleep(UNIT_POLL);
        });
        watch
    }

    fn set(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
        self.known.store(true, Ordering::Relaxed);
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// The unit is known to be inactive; an unanswered poll is not a stop.
    fn stopped(&self) -> bool {
        self.known.load(Ordering::Relaxed) && !self.active()
    }
}

/// `systemctl stop` is still safe: nothing written yet, or a cancellable phase
/// before the install.
fn cancellable_now(status: Option<&Status>) -> bool {
    match status {
        None => true,
        Some(s) => {
            s.cancellable
                && !matches!(
                    s.phase,
                    Phase::Installing
                        | Phase::Cleaning
                        | Phase::Done
                        | Phase::UpToDate
                        | Phase::Failed
                )
        }
    }
}

/// The round is over: the deferral count starts again.
fn round_settled(phase: Option<Phase>) -> bool {
    matches!(phase, Some(Phase::Done | Phase::UpToDate))
}

/// The fields of a status that change what is on screen.
fn status_key(
    s: &Status,
) -> (
    Phase,
    Option<u8>,
    &str,
    Counter,
    Counter,
    &Option<String>,
    bool,
    usize,
) {
    (
        s.phase,
        s.percent,
        s.detail.as_str(),
        s.builds,
        s.copies,
        &s.error,
        s.cancellable,
        s.log_tail.len(),
    )
}

fn same_status(a: &Option<Status>, b: &Option<Status>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => status_key(a) == status_key(b),
        (None, None) => true,
        _ => false,
    }
}

struct Polls {
    status: Instant,
    heartbeat: Instant,
    battery: Instant,
}

struct App {
    cfg: Config,
    mock: bool,
    state: State,
    scheduled: bool,
    screen: Screen,
    modal: Option<Modal>,
    focus: Option<ButtonId>,
    status: Option<Status>,
    battery: Option<Battery>,
    unit: UnitWatch,
    probe: Option<Receiver<(UpdateInfo, Vec<String>)>>,
    /// Head of the followed branch as the probe saw it; what Update asks for.
    remote_rev: Option<String>,
    progress_since: Option<Instant>,
    /// Statuses started before this (unix time) belong to an older run and are ignored.
    start_requested_at: u64,
    cancel_requested: bool,
    done_since: Option<Instant>,
    /// Deferrals were reset for this run's Done or UpToDate.
    settled: bool,
    shown_percent: Option<f32>,
    /// The window has keyboard focus; the heartbeat pauses without it.
    focused: bool,
    shown: bool,
    polls: Polls,
    changed_at: Instant,
    started: Instant,
    stick: Stick,
    hit_rects: Vec<(ButtonId, Rect)>,
    dirty: bool,
    exit: bool,
}

impl App {
    fn base(cfg: Config, mock: bool, state: State, scheduled: bool) -> App {
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(60);
        App {
            cfg,
            mock,
            state,
            scheduled,
            screen: Screen::Checking,
            modal: None,
            focus: None,
            status: None,
            battery: None,
            unit: UnitWatch::fixed(false),
            probe: None,
            remote_rev: None,
            progress_since: None,
            start_requested_at: 0,
            cancel_requested: false,
            done_since: None,
            settled: false,
            shown_percent: None,
            focused: true,
            shown: true,
            polls: Polls {
                status: long_ago,
                heartbeat: long_ago,
                battery: long_ago,
            },
            changed_at: now,
            started: now,
            stick: Stick::default(),
            hit_rects: Vec::new(),
            dirty: true,
            exit: false,
        }
    }

    fn live(cfg: Config) -> App {
        let state = State::load();
        let scheduled = state::take_launch_reason().as_deref() == Some("scheduled");
        let mut app = App::base(cfg, false, state, scheduled);
        app.unit = UnitWatch::spawn(&app.cfg.unit);
        // A stale status file outlives a dead worker until reboot; only a running
        // unit is worth attaching to. The probe spots a staged generation itself.
        if unit_is_active(&app.cfg.unit) {
            app.unit.set(true);
            app.status = Status::read();
            app.enter_progress();
        } else {
            app.spawn_probe();
        }
        app
    }

    fn mock(cfg: Config, name: &str) -> Result<App> {
        let mut app = App::base(cfg, true, State::default(), false);
        let available = |show_later: bool| {
            Screen::UpdateAvailable(Available {
                installed: "bbbc145".into(),
                available: "3f9c2e1".into(),
                subjects: vec![
                    "feat(smeagol): add the steam deck host".into(),
                    "chore(flake): weekly lock update".into(),
                    "chore(installer): ship the install guide instead of cloning the repo".into(),
                    "fix(work_patch): take freerdp from unstable until 26.05 catches up".into(),
                    "docs: describe the install, update and secure boot flow".into(),
                ],
                show_later,
            })
        };
        let progress = |phase, percent, detail: &str, copies, builds| {
            let mut s = Status::new(phase);
            s.percent = percent;
            s.detail = detail.to_string();
            s.copies = copies;
            s.builds = builds;
            s.cancellable = !matches!(phase, Phase::Installing | Phase::Cleaning | Phase::Done);
            s
        };
        let counter = |done, expected| Counter { done, expected };
        let downloading = progress(
            Phase::Downloading,
            Some(42),
            "mesa-25.2.3",
            counter(57, 136),
            counter(0, 2),
        );
        match name {
            "checking" => app.set_screen(Screen::Checking),
            "up-to-date" => app.set_screen(Screen::up_to_date(Some("bbbc145"))),
            "restart-pending" => app.set_screen(Screen::restart_pending(true)),
            "update" => {
                app.battery = Some(Battery {
                    percent: 63,
                    charging: false,
                });
                app.set_screen(available(true));
            }
            "update-forced" => app.set_screen(available(false)),
            "update-battery" => {
                app.battery = Some(Battery {
                    percent: 22,
                    charging: false,
                });
                app.set_screen(available(true));
            }
            "warning" => {
                app.set_screen(available(true));
                app.set_modal(Some(Modal::warning()));
            }
            "progress-evaluating" => {
                app.status = Some(progress(
                    Phase::Evaluating,
                    None,
                    "Evaluating github:OptimoSupreme/nixos-configs",
                    counter(0, 0),
                    counter(0, 0),
                ));
                app.enter_progress();
            }
            "progress" | "cancel" => {
                app.battery = Some(Battery {
                    percent: 78,
                    charging: true,
                });
                app.status = Some(downloading);
                app.enter_progress();
                if name == "cancel" {
                    app.set_modal(Some(Modal::confirm_cancel()));
                }
            }
            "progress-building" => {
                app.status = Some(progress(
                    Phase::Building,
                    Some(71),
                    "linux-6.17.4 (buildPhase)",
                    counter(136, 136),
                    counter(1, 2),
                ));
                app.enter_progress();
            }
            "restarting" => {
                app.status = Some(progress(
                    Phase::Done,
                    Some(100),
                    "",
                    counter(136, 136),
                    counter(2, 2),
                ));
                app.enter_progress();
            }
            "failed" => {
                let log = [
                    "building '/nix/store/9k2m...-linux-6.17.4.drv'...",
                    "  CC      drivers/gpu/drm/amd/display/dc/dml/dml_display_rq_dlg_calc.o",
                    "make[4]: *** [scripts/Makefile.build:287: drivers/gpu/drm/amd/amdgpu/amdgpu_device.o] Error 1",
                    "make[3]: *** [scripts/Makefile.build:555: drivers/gpu/drm/amd/amdgpu] Error 2",
                    "make[2]: *** [scripts/Makefile.build:555: drivers/gpu/drm] Error 2",
                    "make[1]: *** [Makefile:2005: drivers] Error 2",
                    "error: builder for '/nix/store/9k2m...-linux-6.17.4.drv' failed with exit code 2",
                    "error: 1 dependencies of derivation '/nix/store/...-nixos-system-smeagol.drv' failed to build",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
                app.set_screen(Screen::failed("nixos-rebuild exited with status 1", &log));
            }
            other => bail!("unknown mock screen {other:?}; one of {}", MOCKS.join(", ")),
        }
        Ok(app)
    }

    fn spawn_probe(&mut self) {
        let cfg = self.cfg.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let info = check::probe_update(&cfg);
            let subjects = match (&info.local_rev, &info.remote_rev) {
                (Some(local), Some(remote)) if local != remote => {
                    check::commit_subjects(&cfg, local, remote)
                }
                _ => Vec::new(),
            };
            let _ = tx.send((info, subjects));
        });
        self.probe = Some(rx);
        self.set_screen(Screen::Checking);
    }

    fn view(&self) -> View<'_> {
        View {
            screen: &self.screen,
            modal: self.modal.as_ref(),
            status: self.status.as_ref(),
            stalled: self.stalled(),
            battery: self.battery,
            battery_blocked: self.battery_blocked(),
            focus: self.focused(),
            percent: self.shown_percent,
            t: self.started.elapsed().as_secs_f32(),
        }
    }

    fn battery_blocked(&self) -> bool {
        battery_blocks(self.battery, self.cfg.battery_gate)
    }

    /// Done, but the worker's reboot never came.
    fn stalled(&self) -> bool {
        self.stalled_at(Instant::now())
    }

    /// Only the clock decides: the worker exits as it reboots, so an inactive
    /// unit after Done is the reboot itself, not a stall.
    fn stalled_at(&self, now: Instant) -> bool {
        match (self.screen == Screen::Progress, self.done_since) {
            (true, Some(since)) => !self.mock && now.duration_since(since) >= REBOOT_WAIT,
            _ => false,
        }
    }

    fn on_screen(&self) -> bool {
        self.focused && self.shown
    }

    fn heartbeat_due(&self, now: Instant) -> bool {
        !self.mock && self.on_screen() && now.duration_since(self.polls.heartbeat) >= HEARTBEAT
    }

    /// Focus and visibility gate the heartbeat, so the worker reboots only while
    /// the UI is the thing on screen.
    fn window_event(&mut self, event: &WindowEvent) {
        let was_on_screen = self.on_screen();
        match event {
            WindowEvent::FocusGained => self.focused = true,
            WindowEvent::FocusLost => self.focused = false,
            WindowEvent::Shown | WindowEvent::Restored => self.shown = true,
            WindowEvent::Hidden | WindowEvent::Minimized => self.shown = false,
            _ => return,
        }
        if self.on_screen() && !was_on_screen {
            self.polls.heartbeat = Instant::now() - HEARTBEAT;
            self.dirty = true;
        }
    }

    fn buttons(&self) -> Vec<Button> {
        match &self.modal {
            Some(m) => m.buttons.clone(),
            None => screen_buttons(
                &self.screen,
                self.status.as_ref(),
                self.stalled(),
                self.battery_blocked(),
            ),
        }
    }

    /// The button that has focus: the remembered one if it is still there and enabled,
    /// otherwise the first enabled button.
    fn focused(&self) -> Option<ButtonId> {
        let buttons = self.buttons();
        buttons
            .iter()
            .find(|b| Some(b.id) == self.focus && b.enabled)
            .or_else(|| buttons.iter().find(|b| b.enabled))
            .map(|b| b.id)
    }

    fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.modal = None;
        self.focus = None;
        self.changed_at = Instant::now();
        self.dirty = true;
    }

    fn set_modal(&mut self, modal: Option<Modal>) {
        self.modal = modal;
        self.focus = None;
        self.changed_at = Instant::now();
        self.dirty = true;
    }

    fn enter_progress(&mut self) {
        self.progress_since = Some(Instant::now());
        self.done_since = None;
        self.shown_percent = self.target_percent();
        self.set_screen(Screen::Progress);
    }

    fn animating(&self) -> bool {
        match self.screen {
            Screen::Checking => true,
            Screen::Progress => {
                self.shown_percent.is_none() || self.shown_percent != self.target_percent()
            }
            _ => false,
        }
    }

    /// Bar position the status asks for; None means the indeterminate sweep.
    fn target_percent(&self) -> Option<f32> {
        match &self.status {
            Some(s) if !matches!(s.phase, Phase::Done | Phase::Installing | Phase::Cleaning) => {
                s.percent.map(f32::from)
            }
            _ => None,
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.heartbeat_due(now) {
            self.polls.heartbeat = now;
            if let Err(err) = status::touch_heartbeat() {
                eprintln!("steam-maintenance: heartbeat: {err:#}");
            }
        }
        if !self.mock && now.duration_since(self.polls.battery) >= BATTERY_POLL {
            self.polls.battery = now;
            let battery = battery::read();
            if battery != self.battery {
                self.battery = battery;
                self.dirty = true;
            }
        }
        if let Some(rx) = &self.probe {
            if let Ok((info, subjects)) = rx.try_recv() {
                self.probe = None;
                self.decide(info, subjects);
            }
        }
        if self.screen == Screen::Progress {
            if !self.mock {
                self.poll_progress(now);
            }
            self.ease_percent();
            if self.status.as_ref().map(|s| s.phase) == Some(Phase::Done)
                && self.done_since.is_none()
            {
                self.done_since = Some(now);
            }
        }
    }

    /// Move the drawn bar toward the status a step per frame, so jumps read as motion.
    fn ease_percent(&mut self) {
        let target = self.target_percent();
        self.shown_percent = match (self.shown_percent, target) {
            (Some(shown), Some(target)) if (target - shown).abs() > 0.05 => {
                Some(shown + (target - shown) * 0.2)
            }
            _ => target,
        };
    }

    fn poll_progress(&mut self, now: Instant) {
        if now.duration_since(self.polls.status) >= STATUS_POLL {
            self.polls.status = now;
            let fresh = Status::read().filter(|s| s.started_at >= self.start_requested_at);
            if !same_status(&fresh, &self.status) {
                self.status = fresh;
                self.dirty = true;
            }
        }
        self.guard_cancel_modal();
        self.settle_round();
        match self.status.as_ref().map(|s| s.phase) {
            Some(Phase::Done) => {}
            Some(Phase::UpToDate) => self.set_screen(Screen::up_to_date(None)),
            Some(Phase::Failed) => {
                let s = self
                    .status
                    .clone()
                    .unwrap_or_else(|| Status::new(Phase::Failed));
                self.set_screen(Screen::failed(
                    s.error.as_deref().unwrap_or(""),
                    &s.log_tail,
                ));
            }
            _ => {
                let stopped = self.status.is_none() || self.unit.stopped();
                let grace = self
                    .progress_since
                    .map(|t| now.duration_since(t) < START_GRACE)
                    .unwrap_or(false);
                if stopped && !grace {
                    let log = self
                        .status
                        .as_ref()
                        .map(|s| s.log_tail.clone())
                        .unwrap_or_default();
                    self.set_screen(if self.cancel_requested {
                        Screen::cancelled()
                    } else {
                        Screen::stopped(&log)
                    });
                }
            }
        }
    }

    /// Close the cancel prompt once the worker has moved past the point of no return.
    fn guard_cancel_modal(&mut self) {
        let asking = matches!(&self.modal, Some(m) if m.kind == ModalKind::ConfirmCancel);
        if asking && !cancellable_now(self.status.as_ref()) {
            self.set_modal(None);
        }
    }

    /// Done or UpToDate from the worker ends the round: the deferral count starts
    /// again. `next_due` is left alone; the check rolls it once local == remote.
    fn settle_round(&mut self) {
        if self.mock || self.settled || !round_settled(self.status.as_ref().map(|s| s.phase)) {
            return;
        }
        self.settled = true;
        self.state.reset_deferrals();
        self.save_state();
    }

    /// Turn the probe result into the first real screen.
    fn decide(&mut self, info: UpdateInfo, subjects: Vec<String>) {
        self.remote_rev = info.remote_rev.clone();
        let installed = short_rev(info.local_rev.as_deref());
        let policy = later_policy(self.state.deferrals, self.cfg.max_deferrals, self.scheduled);
        if info.update_available() {
            let mut subjects = subjects;
            subjects.truncate(SUBJECT_LINES);
            self.set_screen(Screen::UpdateAvailable(Available {
                installed,
                available: short_rev(info.remote_rev.as_deref()),
                subjects,
                show_later: policy.show,
            }));
        } else if info.restart_pending {
            self.set_screen(Screen::restart_pending(policy.show));
        } else if info.remote_rev.is_none() {
            self.set_screen(Screen::offline(&installed));
        } else if info.remote_rev != info.local_rev {
            self.set_screen(Screen::preparing());
        } else {
            self.state.reset_deferrals();
            self.roll_due_and_save();
            self.set_screen(Screen::up_to_date(Some(&installed)));
        }
    }

    fn roll_due_and_save(&mut self) {
        if let Err(err) = self.state.roll_due(&self.cfg.check_due) {
            eprintln!("steam-maintenance: {err:#}");
        }
        self.save_state();
    }

    fn save_state(&self) {
        if let Err(err) = self.state.save() {
            eprintln!("steam-maintenance: saving state: {err:#}");
        }
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Quit => self.exit = true,
            Action::Hover(x, y) => {
                if let Some(id) = self.hit(x, y) {
                    if self.focus != Some(id) {
                        self.focus = Some(id);
                        self.dirty = true;
                    }
                }
            }
            _ if self.changed_at.elapsed() < DEBOUNCE => {}
            Action::Click(x, y) => {
                if let Some(id) = self.hit(x, y) {
                    self.focus = Some(id);
                    self.activate(id);
                }
            }
            Action::Confirm => {
                if let Some(id) = self.focused() {
                    self.activate(id);
                }
            }
            Action::Back => self.back(),
            Action::Move(dir) => self.move_focus(dir),
        }
    }

    /// The enabled button under (x, y) in the last drawn frame.
    fn hit(&self, x: i32, y: i32) -> Option<ButtonId> {
        let id = self
            .hit_rects
            .iter()
            .find(|(_, r)| r.contains_point((x, y)))
            .map(|(id, _)| *id)?;
        self.buttons()
            .iter()
            .find(|b| b.id == id && b.enabled)
            .map(|b| b.id)
    }

    fn move_focus(&mut self, dir: Dir) {
        let buttons: Vec<Button> = self.buttons().into_iter().filter(|b| b.enabled).collect();
        if buttons.is_empty() {
            return;
        }
        let current = self
            .focused()
            .and_then(|id| buttons.iter().position(|b| b.id == id))
            .unwrap_or(0);
        let next = match dir {
            Dir::Left | Dir::Up => (current + buttons.len() - 1) % buttons.len(),
            Dir::Right | Dir::Down => (current + 1) % buttons.len(),
        };
        self.focus = Some(buttons[next].id);
        self.dirty = true;
    }

    /// Esc or B: close a modal, or press the screen's Close, Later or Cancel.
    fn back(&mut self) {
        if let Some(modal) = &self.modal {
            match modal.kind {
                ModalKind::ConfirmCancel => self.set_modal(None),
                ModalKind::Warning => self.exit = true,
            }
            return;
        }
        if self.screen == Screen::Checking {
            self.exit = true;
            return;
        }
        let buttons = self.buttons();
        for id in [ButtonId::Close, ButtonId::Later, ButtonId::Cancel] {
            if buttons.iter().any(|b| b.id == id && b.enabled) {
                self.activate(id);
                return;
            }
        }
    }

    fn activate(&mut self, id: ButtonId) {
        match id {
            ButtonId::Close | ButtonId::Ok => self.exit = true,
            ButtonId::Later => self.later(),
            ButtonId::Update => self.start_update(),
            ButtonId::Restart => self.reboot(),
            ButtonId::Cancel => self.set_modal(Some(Modal::confirm_cancel())),
            ButtonId::KeepUpdating => self.set_modal(None),
            ButtonId::CancelUpdate => self.cancel_update(),
        }
    }

    fn later(&mut self) {
        if self.mock || !self.scheduled {
            self.exit = true;
            return;
        }
        let policy = later_policy(self.state.deferrals, self.cfg.max_deferrals, true);
        if !policy.show {
            return;
        }
        self.state.deferrals += 1;
        self.roll_due_and_save();
        if policy.warn {
            self.set_modal(Some(Modal::warning()));
        } else {
            self.exit = true;
        }
    }

    fn start_update(&mut self) {
        if self.battery_blocked() {
            return;
        }
        self.cancel_requested = false;
        if self.mock {
            let mut s = Status::new(Phase::Downloading);
            s.percent = Some(42);
            s.detail = "mesa-25.2.3".into();
            s.copies = Counter {
                done: 57,
                expected: 136,
            };
            s.builds = Counter {
                done: 0,
                expected: 2,
            };
            self.status = Some(s);
            self.enter_progress();
            return;
        }
        // Schedule state changes only once the worker reports a result; a start
        // that fails must not silence the next prompt.
        let started = self
            .remote_rev
            .as_deref()
            .map_or(Ok(()), status::write_request)
            .and_then(|_| util::run("systemctl", &["start", &self.cfg.unit]).map(|_| ()));
        if let Err(err) = started {
            self.set_screen(Screen::failed(
                &format!("Couldn't start the update: {err:#}"),
                &[],
            ));
            return;
        }
        self.start_requested_at = now();
        self.status = None;
        self.settled = false;
        self.unit.set(true);
        self.enter_progress();
    }

    fn reboot(&mut self) {
        if self.mock {
            self.exit = true;
            return;
        }
        match util::run("systemctl", &["reboot"]) {
            Ok(_) => self.set_screen(Screen::restarting()),
            Err(err) => self.set_screen(Screen::error("Couldn't restart", &format!("{err:#}"))),
        }
    }

    fn cancel_update(&mut self) {
        if !self.mock {
            // The prompt may have sat open while the worker moved on to the install.
            let fresh = Status::read().filter(|s| s.started_at >= self.start_requested_at);
            if !same_status(&fresh, &self.status) {
                self.status = fresh;
                self.dirty = true;
            }
        }
        if !cancellable_now(self.status.as_ref()) {
            self.set_modal(None);
            return;
        }
        self.cancel_requested = true;
        if !self.mock {
            let unit = self.cfg.unit.clone();
            std::thread::spawn(move || {
                if let Err(err) = util::run("systemctl", &["stop", &unit]) {
                    eprintln!("steam-maintenance: {err:#}");
                }
            });
            self.state.snooze_until = now() + SNOOZE;
            self.save_state();
        }
        self.set_screen(Screen::cancelled());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::base(Config::from_env(), false, State::default(), false)
    }

    fn status(phase: Phase, cancellable: bool) -> Status {
        let mut s = Status::new(phase);
        s.cancellable = cancellable;
        s
    }

    #[test]
    fn cancel_is_refused_past_the_point_of_no_return() {
        assert!(cancellable_now(None));
        assert!(cancellable_now(Some(&status(Phase::Downloading, true))));
        assert!(!cancellable_now(Some(&status(Phase::Downloading, false))));
        for phase in [
            Phase::Installing,
            Phase::Cleaning,
            Phase::Done,
            Phase::UpToDate,
            Phase::Failed,
        ] {
            assert!(!cancellable_now(Some(&status(phase, true))), "{phase:?}");
        }
    }

    #[test]
    fn open_cancel_prompt_closes_when_the_worker_starts_installing() {
        let mut app = app();
        app.status = Some(status(Phase::Downloading, true));
        app.enter_progress();
        app.set_modal(Some(Modal::confirm_cancel()));
        app.guard_cancel_modal();
        assert!(app.modal.is_some());
        app.status = Some(status(Phase::Installing, false));
        app.guard_cancel_modal();
        assert!(app.modal.is_none());
        assert_eq!(app.screen, Screen::Progress);
    }

    #[test]
    fn stalled_waits_for_the_timer_and_ignores_the_unit_going_away() {
        let mut app = app();
        app.unit = UnitWatch::fixed(false);
        app.status = Some(status(Phase::Done, false));
        app.enter_progress();
        let t0 = Instant::now();
        assert!(!app.stalled_at(t0));
        app.done_since = Some(t0);
        assert!(!app.stalled_at(t0 + Duration::from_secs(10)));
        assert!(!app.stalled_at(t0 + REBOOT_WAIT - Duration::from_secs(1)));
        assert!(app.stalled_at(t0 + REBOOT_WAIT));
    }

    #[test]
    fn a_round_settles_on_done_or_up_to_date_only() {
        assert!(round_settled(Some(Phase::Done)));
        assert!(round_settled(Some(Phase::UpToDate)));
        assert!(!round_settled(None));
        for phase in [
            Phase::Evaluating,
            Phase::Downloading,
            Phase::Building,
            Phase::Installing,
            Phase::Cleaning,
            Phase::Failed,
        ] {
            assert!(!round_settled(Some(phase)), "{phase:?}");
        }
    }

    #[test]
    fn heartbeat_only_while_focused_and_shown() {
        let mut app = app();
        // A regain re-arms the heartbeat relative to the real clock, so each
        // check after one reads the clock afresh.
        assert!(app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::FocusLost);
        assert!(!app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::FocusGained);
        assert!(app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::Minimized);
        assert!(!app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::Restored);
        assert!(app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::Hidden);
        app.window_event(&WindowEvent::FocusLost);
        app.window_event(&WindowEvent::FocusGained);
        assert!(!app.heartbeat_due(Instant::now()));
        app.window_event(&WindowEvent::Shown);
        assert!(app.heartbeat_due(Instant::now()));
        let now = Instant::now();
        app.polls.heartbeat = now;
        assert!(!app.heartbeat_due(now + Duration::from_secs(1)));
        assert!(app.heartbeat_due(now + HEARTBEAT));
    }

    #[test]
    fn focus_gain_makes_the_heartbeat_due_at_once() {
        let mut app = app();
        app.window_event(&WindowEvent::FocusLost);
        app.polls.heartbeat = Instant::now();
        app.window_event(&WindowEvent::FocusGained);
        assert!(app.heartbeat_due(Instant::now()));
    }

    #[test]
    fn unit_watch_is_unknown_until_set() {
        let watch = UnitWatch {
            active: Arc::new(AtomicBool::new(false)),
            known: Arc::new(AtomicBool::new(false)),
        };
        assert!(!watch.stopped());
        watch.set(false);
        assert!(watch.stopped());
        watch.set(true);
        assert!(watch.active() && !watch.stopped());
    }
}
