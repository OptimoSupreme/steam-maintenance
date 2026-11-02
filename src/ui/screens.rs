//! The screens and modals, the pure decisions behind them, and the one function
//! that draws a frame.

use super::draw::{self, Scale, ACCENT, BLACK, DANGER, DIM, HAIRLINE, PRIMARY, SECONDARY};
use super::text::{Style, TextCache};
use crate::battery::Battery;
use crate::status::{Phase, Status};
use anyhow::Result;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::WindowCanvas;

/// Log lines shown on the failure screen.
pub const LOG_LINES: usize = 8;
/// Commit subjects shown on the update screen.
pub const SUBJECT_LINES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonId {
    Close,
    Later,
    Update,
    Restart,
    Cancel,
    KeepUpdating,
    CancelUpdate,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub id: ButtonId,
    pub label: &'static str,
    pub enabled: bool,
    pub danger: bool,
}

impl Button {
    pub fn new(id: ButtonId, label: &'static str) -> Button {
        Button {
            id,
            label,
            enabled: true,
            danger: false,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Button {
        self.enabled = enabled;
        self
    }

    pub fn danger(mut self) -> Button {
        self.danger = true;
        self
    }
}

/// A title, a few lines, optionally a log excerpt, and buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
    pub log: Vec<String>,
    pub buttons: Vec<Button>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub installed: String,
    pub available: String,
    pub subjects: Vec<String>,
    pub show_later: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Checking,
    Notice(Notice),
    UpdateAvailable(Available),
    /// Drawn from the latest `Status`; title and buttons follow its phase.
    Progress,
}

impl Screen {
    fn notice(title: &str, lines: &[String], buttons: Vec<Button>) -> Screen {
        Screen::Notice(Notice {
            title: title.to_string(),
            lines: lines.to_vec(),
            log: Vec::new(),
            buttons,
        })
    }

    pub fn up_to_date(installed: Option<&str>) -> Screen {
        let line = match installed {
            Some(rev) => format!("Installed {rev}"),
            None => "Nothing new to install".to_string(),
        };
        Screen::notice(
            "You're up to date",
            &[line],
            vec![Button::new(ButtonId::Close, "Close")],
        )
    }

    pub fn cancelled() -> Screen {
        Screen::notice(
            "Update cancelled",
            &["Nothing has been changed".to_string()],
            vec![Button::new(ButtonId::Close, "Close")],
        )
    }

    /// The unit went away without reporting a result.
    pub fn stopped(log: &[String]) -> Screen {
        let mut screen = Screen::failed("The update stopped unexpectedly", log);
        if let Screen::Notice(n) = &mut screen {
            n.title = "Update stopped".to_string();
        }
        screen
    }

    /// Shown after the UI itself asked for a reboot.
    pub fn restarting() -> Screen {
        Screen::notice("Restarting…", &[], Vec::new())
    }

    pub fn preparing() -> Screen {
        Screen::notice(
            "An update is on its way",
            &["The build is still being prepared".to_string()],
            vec![Button::new(ButtonId::Close, "Close")],
        )
    }

    pub fn offline(installed: &str) -> Screen {
        Screen::notice(
            "Couldn't check for updates",
            &[
                "GitHub could not be reached".to_string(),
                format!("Installed {installed}"),
            ],
            vec![Button::new(ButtonId::Close, "Close")],
        )
    }

    pub fn restart_pending(show_later: bool) -> Screen {
        let mut buttons = vec![Button::new(ButtonId::Restart, "Restart now")];
        if show_later {
            buttons.push(Button::new(ButtonId::Later, "Later"));
        }
        Screen::notice(
            "An update is ready",
            &["Restart to finish installing it".to_string()],
            buttons,
        )
    }

    pub fn failed(error: &str, log: &[String]) -> Screen {
        let skip = log.len().saturating_sub(LOG_LINES);
        Screen::Notice(Notice {
            title: "Update failed".to_string(),
            lines: vec![if error.is_empty() {
                "The update did not complete".to_string()
            } else {
                error.to_string()
            }],
            log: log[skip..].to_vec(),
            buttons: vec![Button::new(ButtonId::Close, "Close")],
        })
    }

    pub fn error(title: &str, detail: &str) -> Screen {
        Screen::notice(
            title,
            &[detail.to_string()],
            vec![Button::new(ButtonId::Close, "Close")],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    ConfirmCancel,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modal {
    pub kind: ModalKind,
    pub title: &'static str,
    pub line: &'static str,
    pub buttons: Vec<Button>,
    pub accent: Color,
}

impl Modal {
    pub fn confirm_cancel() -> Modal {
        Modal {
            kind: ModalKind::ConfirmCancel,
            title: "Are you sure?",
            line: "The update will stop. Nothing has been changed yet.",
            buttons: vec![
                Button::new(ButtonId::KeepUpdating, "Keep updating"),
                Button::new(ButtonId::CancelUpdate, "Cancel update").danger(),
            ],
            accent: DANGER,
        }
    }

    pub fn warning() -> Modal {
        Modal {
            kind: ModalKind::Warning,
            title: "Updates are important",
            line: "The next check cannot be deferred.",
            buttons: vec![Button::new(ButtonId::Ok, "OK")],
            accent: ACCENT,
        }
    }
}

/// Title of the progress screen for a phase.
pub fn phase_title(phase: Phase) -> &'static str {
    match phase {
        Phase::Evaluating => "Preparing…",
        Phase::Downloading => "Downloading",
        Phase::Building => "Building",
        Phase::Installing => "Installing",
        Phase::Cleaning => "Cleaning up",
        Phase::Done => "Restarting…",
        Phase::UpToDate => "Up to date",
        Phase::Failed => "Update failed",
    }
}

/// Buttons on the progress screen. `stalled`: Done but the reboot never came.
pub fn progress_buttons(status: &Status, stalled: bool) -> Vec<Button> {
    match status.phase {
        Phase::Done if stalled => vec![
            Button::new(ButtonId::Restart, "Restart now"),
            Button::new(ButtonId::Close, "Close"),
        ],
        Phase::Done | Phase::Cleaning | Phase::Installing => Vec::new(),
        _ if status.cancellable => vec![Button::new(ButtonId::Cancel, "Cancel")],
        _ => Vec::new(),
    }
}

/// Whether a scheduled prompt still offers Later, and whether pressing it warns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaterPolicy {
    pub show: bool,
    pub warn: bool,
}

/// Later is always free on a manual launch. On a scheduled one it is hidden once
/// the limit is reached, and the press that reaches the limit shows the warning.
pub fn later_policy(deferrals: u32, max: u32, scheduled: bool) -> LaterPolicy {
    if !scheduled {
        return LaterPolicy {
            show: true,
            warn: false,
        };
    }
    let show = deferrals < max;
    LaterPolicy {
        show,
        warn: show && deferrals + 1 >= max,
    }
}

/// True when the battery is too low to start an upgrade without a charger.
pub fn battery_blocks(battery: Option<Battery>, gate: u8) -> bool {
    match battery {
        Some(b) if gate > 0 => !b.charging && b.percent < gate,
        _ => false,
    }
}

/// First seven characters of a revision, or "unknown".
pub fn short_rev(rev: Option<&str>) -> String {
    match rev {
        Some(r) if !r.is_empty() => r.chars().take(7).collect(),
        _ => "unknown".to_string(),
    }
}

/// Focusable buttons of a screen (not of a modal over it).
pub fn screen_buttons(
    screen: &Screen,
    status: Option<&Status>,
    stalled: bool,
    battery_blocked: bool,
) -> Vec<Button> {
    match screen {
        Screen::Checking => Vec::new(),
        Screen::Notice(n) => n.buttons.clone(),
        Screen::UpdateAvailable(a) => {
            let mut buttons =
                vec![Button::new(ButtonId::Update, "Update").enabled(!battery_blocked)];
            if a.show_later {
                buttons.push(Button::new(ButtonId::Later, "Later"));
            }
            buttons
        }
        Screen::Progress => status
            .map(|s| progress_buttons(s, stalled))
            .unwrap_or_default(),
    }
}

/// Everything a frame needs.
pub struct View<'a> {
    pub screen: &'a Screen,
    pub modal: Option<&'a Modal>,
    pub status: Option<&'a Status>,
    pub stalled: bool,
    pub battery: Option<Battery>,
    pub battery_blocked: bool,
    pub focus: Option<ButtonId>,
    /// Smoothed bar position; None for the indeterminate sweep.
    pub percent: Option<f32>,
    /// Seconds since launch, for animations.
    pub t: f32,
}

// Layout in units (pixels on a Steam Deck).
const MARGIN: f32 = 112.0;
const TOP: f32 = 168.0;
const TITLE_PX: f32 = 54.0;
const SUB_PX: f32 = 26.0;
const LINE_PX: f32 = 24.0;
const SMALL_PX: f32 = 20.0;
const BUTTON_H: f32 = 60.0;
const BUTTON_MIN_W: f32 = 200.0;
const BUTTON_PAD: f32 = 36.0;
const BUTTON_GAP: f32 = 20.0;
const BUTTON_RADIUS: f32 = 12.0;
const BUTTON_PX: f32 = 22.0;
const BOTTOM: f32 = 112.0;
const BAR_Y: f32 = 436.0;
const BAR_H: f32 = 6.0;
const MODAL_W: f32 = 800.0;
const MODAL_PAD: f32 = 40.0;

pub struct Ctx<'c, 'a> {
    pub canvas: &'c mut WindowCanvas,
    pub text: &'c mut TextCache<'a>,
    pub s: Scale,
}

impl Ctx<'_, '_> {
    fn style(&self, px: f32, bold: bool, color: Color) -> Style {
        Style {
            px: self.s.font(px),
            bold,
            color,
        }
    }

    fn label(
        &mut self,
        text: &str,
        px: f32,
        bold: bool,
        color: Color,
        x: i32,
        y: i32,
    ) -> Result<(u32, u32)> {
        let style = self.style(px, bold, color);
        self.text.draw(self.canvas, text, style, x, y)
    }

    fn label_right(
        &mut self,
        text: &str,
        px: f32,
        bold: bool,
        color: Color,
        right: i32,
        y: i32,
    ) -> Result<(u32, u32)> {
        let style = self.style(px, bold, color);
        self.text.draw_right(self.canvas, text, style, right, y)
    }

    fn fit(&self, text: &str, px: f32, bold: bool, max_width: i32) -> String {
        self.text
            .fonts()
            .fit(text, self.s.font(px) as f32, bold, max_width.max(0) as u32)
    }

    fn measure(&self, text: &str, px: f32, bold: bool) -> (u32, u32) {
        self.text.measure(text, self.s.font(px), bold)
    }

    fn content_width(&self) -> i32 {
        self.s.w - 2 * self.s.px(MARGIN)
    }
}

/// Draw one frame; returns the rectangles of the buttons that can take focus.
pub fn draw_frame(ctx: &mut Ctx, view: &View) -> Result<Vec<(ButtonId, Rect)>> {
    ctx.canvas.set_draw_color(BLACK);
    ctx.canvas.clear();
    if let Some(b) = view.battery {
        draw_battery(ctx, b)?;
    }
    let margin = ctx.s.px(MARGIN);
    let top = ctx.s.px(TOP);
    match view.screen {
        Screen::Checking => {
            let y = heading(ctx, "System Update", "Checking for updates…", top)?;
            draw_dots(ctx, margin, y + ctx.s.px(36.0), view.t)?;
        }
        Screen::Notice(n) => draw_notice(ctx, n, top)?,
        Screen::UpdateAvailable(a) => draw_available(ctx, a, top)?,
        Screen::Progress => draw_progress(ctx, view, top)?,
    }
    let buttons = screen_buttons(view.screen, view.status, view.stalled, view.battery_blocked);
    let hint = match view.screen {
        Screen::UpdateAvailable(_) if view.battery_blocked => Some("Plug in to update"),
        _ => None,
    };
    let focus = if view.modal.is_some() {
        None
    } else {
        view.focus
    };
    let row_y = ctx.s.h - ctx.s.px(BOTTOM) - ctx.s.px(BUTTON_H);
    let mut rects = button_row(ctx, &buttons, focus, margin, row_y, hint)?;
    if let Some(modal) = view.modal {
        rects = draw_modal(ctx, modal, view.focus)?;
    }
    Ok(rects)
}

fn draw_battery(ctx: &mut Ctx, b: Battery) -> Result<()> {
    let right = ctx.s.w - ctx.s.px(MARGIN);
    let cy = ctx.s.px(88.0);
    let left = draw::battery_glyph(ctx.canvas, &ctx.s, right, cy, b)?;
    let text = format!("{}%", b.percent);
    let (_, th) = ctx.measure(&text, SMALL_PX + 2.0, false);
    ctx.label_right(
        &text,
        SMALL_PX + 2.0,
        false,
        SECONDARY,
        left - ctx.s.px(12.0),
        cy - th as i32 / 2,
    )?;
    Ok(())
}

/// Title and subtitle; returns the y below them.
fn heading(ctx: &mut Ctx, title: &str, subtitle: &str, top: i32) -> Result<i32> {
    let margin = ctx.s.px(MARGIN);
    let (_, th) = ctx.label(title, TITLE_PX, true, PRIMARY, margin, top)?;
    let y = top + th as i32 + ctx.s.px(12.0);
    if subtitle.is_empty() {
        return Ok(y + ctx.s.px(8.0));
    }
    let fitted = ctx.fit(subtitle, SUB_PX, false, ctx.content_width());
    let (_, sh) = ctx.label(&fitted, SUB_PX, false, SECONDARY, margin, y)?;
    Ok(y + sh as i32)
}

fn draw_dots(ctx: &mut Ctx, x: i32, y: i32, t: f32) -> Result<()> {
    let d = ctx.s.px(10.0);
    let gap = ctx.s.px(10.0);
    for i in 0..3 {
        let pulse = 0.5 + 0.5 * (t * 4.0 - i as f32 * 0.9).sin();
        let alpha = (60.0 + 195.0 * pulse) as u8;
        let color = Color::RGBA(PRIMARY.r, PRIMARY.g, PRIMARY.b, alpha);
        draw::fill_rounded(
            ctx.canvas,
            Rect::new(x + i * (d + gap), y, d as u32, d as u32),
            d / 2,
            color,
        )?;
    }
    Ok(())
}

fn draw_notice(ctx: &mut Ctx, n: &Notice, top: i32) -> Result<()> {
    let margin = ctx.s.px(MARGIN);
    let width = ctx.content_width();
    let first = n.lines.first().map(String::as_str).unwrap_or("");
    let mut y = heading(ctx, &n.title, first, top)?;
    for line in n.lines.iter().skip(1) {
        y += ctx.s.px(8.0);
        let fitted = ctx.fit(line, SUB_PX, false, width);
        let (_, h) = ctx.label(&fitted, SUB_PX, false, SECONDARY, margin, y)?;
        y += h as i32;
    }
    if !n.log.is_empty() {
        y += ctx.s.px(36.0);
        draw::fill(
            ctx.canvas,
            Rect::new(margin, y, width as u32, ctx.s.px(1.0) as u32),
            HAIRLINE,
        )?;
        y += ctx.s.px(20.0);
        for line in &n.log {
            let fitted = ctx.fit(line, SMALL_PX, false, width);
            let (_, h) = ctx.label(&fitted, SMALL_PX, false, SECONDARY, margin, y)?;
            y += h as i32 + ctx.s.px(4.0);
        }
    }
    Ok(())
}

fn draw_available(ctx: &mut Ctx, a: &Available, top: i32) -> Result<()> {
    let margin = ctx.s.px(MARGIN);
    let width = ctx.content_width();
    let mut y = heading(ctx, "Update available", "", top)?;
    let value_x = margin + ctx.measure("Available", SUB_PX, false).0 as i32 + ctx.s.px(20.0);
    for (label, value) in [("Installed", &a.installed), ("Available", &a.available)] {
        ctx.label(label, SUB_PX, false, SECONDARY, margin, y)?;
        let (_, h) = ctx.label(value, SUB_PX, false, PRIMARY, value_x, y)?;
        y += h as i32 + ctx.s.px(8.0);
    }
    if !a.subjects.is_empty() {
        y += ctx.s.px(28.0);
        let dot = ctx.s.px(6.0);
        let text_x = margin + dot + ctx.s.px(16.0);
        for subject in a.subjects.iter().take(SUBJECT_LINES) {
            let fitted = ctx.fit(subject, LINE_PX, false, width - (text_x - margin));
            let (_, h) = ctx.measure(&fitted, LINE_PX, false);
            let cy = y + h as i32 / 2;
            draw::fill_rounded(
                ctx.canvas,
                Rect::new(margin, cy - dot / 2, dot as u32, dot as u32),
                dot / 2,
                ACCENT,
            )?;
            ctx.label(&fitted, LINE_PX, false, SECONDARY, text_x, y)?;
            y += h as i32 + ctx.s.px(10.0);
        }
    }
    Ok(())
}

fn draw_progress(ctx: &mut Ctx, view: &View, top: i32) -> Result<()> {
    let placeholder = Status::new(Phase::Evaluating);
    let status = view.status.unwrap_or(&placeholder);
    let margin = ctx.s.px(MARGIN);
    let width = ctx.content_width();
    let title = if view.stalled {
        "Restart needed"
    } else {
        phase_title(status.phase)
    };
    heading(ctx, title, "", top)?;

    let bar_y = ctx.s.px(BAR_Y);
    let bar = Rect::new(margin, bar_y, width as u32, ctx.s.px(BAR_H) as u32);
    let percent = if view.stalled {
        Some(100.0)
    } else {
        view.percent
    };
    draw::progress_bar(ctx.canvas, &ctx.s, bar, percent, view.t)?;
    if let Some(p) = percent {
        let text = format!("{}%", p.round() as u8);
        let (_, th) = ctx.measure(&text, SUB_PX, false);
        ctx.label_right(
            &text,
            SUB_PX,
            false,
            PRIMARY,
            margin + width,
            bar_y - ctx.s.px(16.0) - th as i32,
        )?;
    }

    let detail = if view.stalled {
        "The system did not restart on its own".to_string()
    } else if status.detail.is_empty() {
        match status.phase {
            Phase::Done => "Restarting the system".to_string(),
            _ => String::new(),
        }
    } else {
        status.detail.clone()
    };
    let mut y = bar_y + ctx.s.px(BAR_H) + ctx.s.px(24.0);
    if !detail.is_empty() {
        let fitted = ctx.fit(&detail, LINE_PX, false, width);
        let (_, h) = ctx.label(&fitted, LINE_PX, false, SECONDARY, margin, y)?;
        y += h as i32 + ctx.s.px(12.0);
    }
    if status.copies.expected > 0 || status.builds.expected > 0 {
        let counters = format!(
            "{}/{} downloads · {}/{} builds",
            status.copies.done, status.copies.expected, status.builds.done, status.builds.expected
        );
        ctx.label(&counters, SMALL_PX, false, DIM, margin, y)?;
    }
    Ok(())
}

/// A row of buttons starting at (x, y); returns their rectangles.
fn button_row(
    ctx: &mut Ctx,
    buttons: &[Button],
    focus: Option<ButtonId>,
    x: i32,
    y: i32,
    hint: Option<&str>,
) -> Result<Vec<(ButtonId, Rect)>> {
    let mut rects = Vec::new();
    let height = ctx.s.px(BUTTON_H);
    let mut cursor = x;
    for b in buttons {
        let (tw, _) = ctx.measure(b.label, BUTTON_PX, true);
        let width = (tw as i32 + 2 * ctx.s.px(BUTTON_PAD)).max(ctx.s.px(BUTTON_MIN_W));
        let rect = Rect::new(cursor, y, width as u32, height as u32);
        draw_button(ctx, b, focus == Some(b.id), rect)?;
        rects.push((b.id, rect));
        cursor += width + ctx.s.px(BUTTON_GAP);
    }
    if let Some(hint) = hint {
        let (_, th) = ctx.measure(hint, LINE_PX, false);
        ctx.label(
            hint,
            LINE_PX,
            false,
            SECONDARY,
            cursor + ctx.s.px(12.0),
            y + (height - th as i32) / 2,
        )?;
    }
    Ok(rects)
}

fn draw_button(ctx: &mut Ctx, b: &Button, focused: bool, rect: Rect) -> Result<()> {
    let (fill, text, outline) = match (b.enabled, focused, b.danger) {
        (false, _, _) => (BLACK, DIM, HAIRLINE),
        (true, true, false) => (PRIMARY, BLACK, PRIMARY),
        (true, true, true) => (DANGER, BLACK, DANGER),
        (true, false, false) => (BLACK, PRIMARY, HAIRLINE),
        (true, false, true) => (BLACK, DANGER, HAIRLINE),
    };
    draw::stroke_rounded(
        ctx.canvas,
        rect,
        ctx.s.px(BUTTON_RADIUS),
        ctx.s.px(2.0),
        outline,
        fill,
    )?;
    let (tw, th) = ctx.measure(b.label, BUTTON_PX, true);
    let x = rect.x() + (rect.width() as i32 - tw as i32) / 2;
    let y = rect.y() + (rect.height() as i32 - th as i32) / 2;
    ctx.label(b.label, BUTTON_PX, true, text, x, y)?;
    Ok(())
}

fn draw_modal(
    ctx: &mut Ctx,
    modal: &Modal,
    focus: Option<ButtonId>,
) -> Result<Vec<(ButtonId, Rect)>> {
    draw::fill(
        ctx.canvas,
        Rect::new(0, 0, ctx.s.w as u32, ctx.s.h as u32),
        Color::RGBA(0, 0, 0, 176),
    )?;
    let pad = ctx.s.px(MODAL_PAD);
    let width = ctx.s.px(MODAL_W).min(ctx.s.w - 2 * ctx.s.px(32.0));
    let inner = width - 2 * pad;
    let (_, title_h) = ctx.measure(modal.title, 34.0, true);
    let line = ctx.fit(modal.line, LINE_PX, false, inner);
    let (_, line_h) = ctx.measure(&line, LINE_PX, false);
    let accent_h = ctx.s.px(4.0);
    let height = pad
        + accent_h
        + ctx.s.px(24.0)
        + title_h as i32
        + ctx.s.px(12.0)
        + line_h as i32
        + ctx.s.px(36.0)
        + ctx.s.px(BUTTON_H)
        + pad;
    let x = (ctx.s.w - width) / 2;
    let y = (ctx.s.h - height) / 2;
    let panel = Rect::new(x, y, width as u32, height as u32);
    draw::stroke_rounded(
        ctx.canvas,
        panel,
        ctx.s.px(18.0),
        ctx.s.px(2.0),
        HAIRLINE,
        BLACK,
    )?;
    let mut cy = y + pad;
    draw::fill_rounded(
        ctx.canvas,
        Rect::new(x + pad, cy, ctx.s.px(48.0) as u32, accent_h as u32),
        accent_h / 2,
        modal.accent,
    )?;
    cy += accent_h + ctx.s.px(24.0);
    ctx.label(modal.title, 34.0, true, PRIMARY, x + pad, cy)?;
    cy += title_h as i32 + ctx.s.px(12.0);
    ctx.label(&line, LINE_PX, false, SECONDARY, x + pad, cy)?;
    cy += line_h as i32 + ctx.s.px(36.0);
    button_row(ctx, &modal.buttons, focus, x + pad, cy, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_titles() {
        assert_eq!(phase_title(Phase::Evaluating), "Preparing…");
        assert_eq!(phase_title(Phase::Downloading), "Downloading");
        assert_eq!(phase_title(Phase::Building), "Building");
        assert_eq!(phase_title(Phase::Installing), "Installing");
        assert_eq!(phase_title(Phase::Cleaning), "Cleaning up");
        assert_eq!(phase_title(Phase::Done), "Restarting…");
    }

    #[test]
    fn later_is_free_on_manual_launches() {
        for deferrals in 0..5 {
            let p = later_policy(deferrals, 2, false);
            assert!(p.show && !p.warn, "{deferrals}");
        }
    }

    #[test]
    fn later_warns_on_the_deferral_that_reaches_the_limit_then_hides() {
        assert_eq!(
            later_policy(0, 2, true),
            LaterPolicy {
                show: true,
                warn: false
            }
        );
        assert_eq!(
            later_policy(1, 2, true),
            LaterPolicy {
                show: true,
                warn: true
            }
        );
        assert_eq!(
            later_policy(2, 2, true),
            LaterPolicy {
                show: false,
                warn: false
            }
        );
        assert_eq!(
            later_policy(3, 2, true),
            LaterPolicy {
                show: false,
                warn: false
            }
        );
        assert_eq!(
            later_policy(0, 1, true),
            LaterPolicy {
                show: true,
                warn: true
            }
        );
        assert_eq!(
            later_policy(0, 0, true),
            LaterPolicy {
                show: false,
                warn: false
            }
        );
    }

    #[test]
    fn battery_gate_blocks_only_when_low_and_unplugged() {
        let low = Some(Battery {
            percent: 22,
            charging: false,
        });
        let low_charging = Some(Battery {
            percent: 22,
            charging: true,
        });
        let fine = Some(Battery {
            percent: 63,
            charging: false,
        });
        assert!(battery_blocks(low, 30));
        assert!(!battery_blocks(low_charging, 30));
        assert!(!battery_blocks(fine, 30));
        assert!(!battery_blocks(None, 30));
        assert!(!battery_blocks(low, 0));
        assert!(!battery_blocks(
            Some(Battery {
                percent: 30,
                charging: false
            }),
            30
        ));
    }

    #[test]
    fn short_revs() {
        assert_eq!(short_rev(Some("bbbc145abcdef")), "bbbc145");
        assert_eq!(short_rev(Some("abc")), "abc");
        assert_eq!(short_rev(Some("")), "unknown");
        assert_eq!(short_rev(None), "unknown");
    }

    #[test]
    fn progress_buttons_follow_phase_and_cancellability() {
        let mut s = Status::new(Phase::Downloading);
        assert_eq!(progress_buttons(&s, false)[0].id, ButtonId::Cancel);
        s.cancellable = false;
        assert!(progress_buttons(&s, false).is_empty());
        s.phase = Phase::Done;
        assert!(progress_buttons(&s, false).is_empty());
        let ids: Vec<ButtonId> = progress_buttons(&s, true).iter().map(|b| b.id).collect();
        assert_eq!(ids, vec![ButtonId::Restart, ButtonId::Close]);
        s.phase = Phase::Cleaning;
        s.cancellable = true;
        assert!(progress_buttons(&s, false).is_empty());
    }

    #[test]
    fn update_screen_hides_later_and_disables_update_on_the_gate() {
        let screen = Screen::UpdateAvailable(Available {
            installed: "a".into(),
            available: "b".into(),
            subjects: vec![],
            show_later: false,
        });
        let buttons = screen_buttons(&screen, None, false, true);
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].id, ButtonId::Update);
        assert!(!buttons[0].enabled);
    }

    #[test]
    fn failed_screen_keeps_the_last_eight_log_lines() {
        let log: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let Screen::Notice(n) = Screen::failed("boom", &log) else {
            panic!("not a notice");
        };
        assert_eq!(n.log.len(), LOG_LINES);
        assert_eq!(n.log[0], "line 12");
        assert_eq!(n.lines[0], "boom");
    }
}
