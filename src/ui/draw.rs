//! Drawing primitives on the SDL canvas: the layout unit, colors, rounded rects,
//! small polygons, the progress bar and the battery glyph.

use crate::battery::Battery;
use anyhow::{Error, Result};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::WindowCanvas;

pub const BLACK: Color = Color::RGB(0x00, 0x00, 0x00);
pub const PRIMARY: Color = Color::RGB(0xF2, 0xF2, 0xF2);
pub const SECONDARY: Color = Color::RGB(0x9A, 0x9A, 0x9A);
pub const DIM: Color = Color::RGB(0x55, 0x55, 0x55);
pub const HAIRLINE: Color = Color::RGB(0x2A, 0x2A, 0x2A);
pub const ACCENT: Color = Color::RGB(0x4F, 0xC3, 0xF7);
pub const DANGER: Color = Color::RGB(0xFF, 0x6B, 0x6B);

/// Reference design size; one unit is one pixel on a Steam Deck.
const REF_W: f32 = 1280.0;
const REF_H: f32 = 800.0;

/// Pixels per layout unit for a canvas of the given size.
pub fn unit_scale(width: u32, height: u32) -> f32 {
    (width as f32 / REF_W).min(height as f32 / REF_H)
}

/// Filled width of a bar `total` pixels wide at `percent`.
pub fn bar_fill_width(total: i32, percent: u8) -> i32 {
    (total as f32 * percent.min(100) as f32 / 100.0).round() as i32
}

/// Left edge of the indeterminate sweep at time `t` seconds for a bar `total` wide
/// whose sweep segment is `segment` wide; the segment slides in from the left and out
/// to the right every 1.6 s with an ease.
pub fn sweep_offset(total: i32, segment: i32, t: f32) -> i32 {
    let phase = (t / 1.6).fract();
    let eased = phase * phase * (3.0 - 2.0 * phase);
    (-segment as f32 + (total + segment) as f32 * eased).round() as i32
}

/// Canvas size and the unit scale derived from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    pub w: i32,
    pub h: i32,
    pub u: f32,
}

impl Scale {
    pub fn new(width: u32, height: u32) -> Scale {
        Scale {
            w: width as i32,
            h: height as i32,
            u: unit_scale(width, height),
        }
    }

    /// Units to whole pixels, never less than one for a positive input.
    pub fn px(&self, units: f32) -> i32 {
        let px = (units * self.u).round() as i32;
        if units > 0.0 {
            px.max(1)
        } else {
            px
        }
    }

    /// Units to a font size in pixels.
    pub fn font(&self, units: f32) -> u32 {
        self.px(units).max(1) as u32
    }
}

pub fn sdl_err(e: String) -> Error {
    Error::msg(e)
}

pub fn fill(canvas: &mut WindowCanvas, rect: Rect, color: Color) -> Result<()> {
    if rect.width() == 0 || rect.height() == 0 {
        return Ok(());
    }
    canvas.set_draw_color(color);
    canvas.fill_rect(rect).map_err(sdl_err)
}

/// Filled rectangle with rounded corners, corner edges anti-aliased.
pub fn fill_rounded(
    canvas: &mut WindowCanvas,
    rect: Rect,
    radius: i32,
    color: Color,
) -> Result<()> {
    let w = rect.width() as i32;
    let h = rect.height() as i32;
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    let r = radius.min(w / 2).min(h / 2).max(0);
    if r == 0 {
        return fill(canvas, rect, color);
    }
    fill(
        canvas,
        Rect::new(rect.x(), rect.y() + r, w as u32, (h - 2 * r) as u32),
        color,
    )?;
    let rf = r as f32;
    for i in 0..r {
        let dy = rf - (i as f32 + 0.5);
        let inset = rf - (rf * rf - dy * dy).sqrt();
        let whole = inset.floor() as i32;
        let edge = Color::RGBA(
            color.r,
            color.g,
            color.b,
            ((1.0 - inset.fract()) * color.a as f32) as u8,
        );
        for y in [rect.y() + i, rect.y() + h - 1 - i] {
            let span = w - 2 * whole - 2;
            if span > 0 {
                fill(
                    canvas,
                    Rect::new(rect.x() + whole + 1, y, span as u32, 1),
                    color,
                )?;
            }
            fill(canvas, Rect::new(rect.x() + whole, y, 1, 1), edge)?;
            fill(canvas, Rect::new(rect.x() + w - 1 - whole, y, 1, 1), edge)?;
        }
    }
    Ok(())
}

/// Rounded outline `thickness` wide with the inside painted `inner`.
pub fn stroke_rounded(
    canvas: &mut WindowCanvas,
    rect: Rect,
    radius: i32,
    thickness: i32,
    color: Color,
    inner: Color,
) -> Result<()> {
    fill_rounded(canvas, rect, radius, color)?;
    let t = thickness.max(1);
    let inner_rect = Rect::new(
        rect.x() + t,
        rect.y() + t,
        (rect.width() as i32 - 2 * t).max(0) as u32,
        (rect.height() as i32 - 2 * t).max(0) as u32,
    );
    fill_rounded(canvas, inner_rect, (radius - t).max(0), inner)
}

/// Scanline fill of a simple polygon given in pixel coordinates.
pub fn fill_polygon(canvas: &mut WindowCanvas, points: &[(f32, f32)], color: Color) -> Result<()> {
    if points.len() < 3 {
        return Ok(());
    }
    let min_y = points.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor() as i32;
    let max_y = points.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i32;
    let mut crossings = Vec::new();
    for y in min_y..max_y {
        let yc = y as f32 + 0.5;
        crossings.clear();
        for i in 0..points.len() {
            let (x0, y0) = points[i];
            let (x1, y1) = points[(i + 1) % points.len()];
            if (y0 <= yc) != (y1 <= yc) {
                crossings.push(x0 + (yc - y0) / (y1 - y0) * (x1 - x0));
            }
        }
        crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for [left, right] in crossings.as_chunks::<2>().0 {
            let x0 = left.round() as i32;
            let x1 = right.round() as i32;
            if x1 > x0 {
                fill(canvas, Rect::new(x0, y, (x1 - x0) as u32, 1), color)?;
            }
        }
    }
    Ok(())
}

/// Thin progress bar: hairline track, accent fill or a sweeping segment when indeterminate.
pub fn progress_bar(
    canvas: &mut WindowCanvas,
    s: &Scale,
    rect: Rect,
    percent: Option<f32>,
    t: f32,
) -> Result<()> {
    let radius = s.px(3.0);
    fill_rounded(canvas, rect, radius, HAIRLINE)?;
    let total = rect.width() as i32;
    match percent {
        Some(p) => {
            let w = bar_fill_width(total, p.round().clamp(0.0, 100.0) as u8);
            if w > 0 {
                fill_rounded(
                    canvas,
                    Rect::new(rect.x(), rect.y(), w as u32, rect.height()),
                    radius,
                    ACCENT,
                )?;
            }
        }
        None => {
            let segment = total / 4;
            let x = sweep_offset(total, segment, t);
            let x0 = x.max(0);
            let x1 = (x + segment).min(total);
            if x1 > x0 {
                fill_rounded(
                    canvas,
                    Rect::new(rect.x() + x0, rect.y(), (x1 - x0) as u32, rect.height()),
                    radius,
                    ACCENT,
                )?;
            }
        }
    }
    Ok(())
}

/// Battery outline, charge fill and a bolt when charging. `right` is the glyph's right
/// edge and `cy` its vertical center; returns the glyph's left edge.
pub fn battery_glyph(
    canvas: &mut WindowCanvas,
    s: &Scale,
    right: i32,
    cy: i32,
    b: Battery,
) -> Result<i32> {
    let body_w = s.px(38.0);
    let body_h = s.px(18.0);
    let nub_w = s.px(3.0);
    let nub_h = s.px(8.0);
    let line = s.px(2.0);
    let gap = s.px(2.0);
    let body_x = right - nub_w - body_w;
    let body_y = cy - body_h / 2;
    let body = Rect::new(body_x, body_y, body_w as u32, body_h as u32);
    stroke_rounded(canvas, body, s.px(4.0), line, SECONDARY, BLACK)?;
    fill_rounded(
        canvas,
        Rect::new(body_x + body_w, cy - nub_h / 2, nub_w as u32, nub_h as u32),
        s.px(1.0),
        SECONDARY,
    )?;
    let inner_w = body_w - 2 * (line + gap);
    let inner_h = body_h - 2 * (line + gap);
    let fill_w = bar_fill_width(inner_w, b.percent);
    let fill_color = if b.charging {
        ACCENT
    } else if b.percent <= 20 {
        DANGER
    } else {
        PRIMARY
    };
    if fill_w > 0 {
        fill_rounded(
            canvas,
            Rect::new(
                body_x + line + gap,
                body_y + line + gap,
                fill_w as u32,
                inner_h as u32,
            ),
            s.px(2.0),
            fill_color,
        )?;
    }
    if b.charging {
        let cx = (body_x + body_w / 2) as f32;
        let cyf = cy as f32;
        let bolt = |scale: f32| -> Vec<(f32, f32)> {
            [
                (1.0, -5.0),
                (-2.5, 0.5),
                (0.0, 0.5),
                (-1.0, 5.0),
                (2.5, -0.5),
                (0.0, -0.5),
            ]
            .iter()
            .map(|(x, y)| (cx + x * scale, cyf + y * scale))
            .collect()
        };
        fill_polygon(canvas, &bolt(1.6 * s.u), BLACK)?;
        fill_polygon(canvas, &bolt(1.2 * s.u), PRIMARY)?;
    }
    Ok(body_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_is_one_on_a_deck_and_follows_the_tighter_axis() {
        assert_eq!(unit_scale(1280, 800), 1.0);
        assert!((unit_scale(1920, 1080) - 1.35).abs() < 1e-6);
        assert!((unit_scale(3840, 2160) - 2.7).abs() < 1e-6);
        assert_eq!(unit_scale(2560, 1600), 2.0);
        assert_eq!(unit_scale(800, 1280), 0.625);
    }

    #[test]
    fn px_rounds_and_never_drops_a_positive_size_to_zero() {
        let s = Scale::new(1280, 800);
        assert_eq!(s.px(12.0), 12);
        let tiny = Scale::new(320, 200);
        assert_eq!(tiny.px(1.0), 1);
        assert_eq!(tiny.px(0.0), 0);
        assert_eq!(Scale::new(3840, 2160).px(10.0), 27);
    }

    #[test]
    fn bar_width_is_proportional_and_clamped() {
        assert_eq!(bar_fill_width(1000, 0), 0);
        assert_eq!(bar_fill_width(1000, 42), 420);
        assert_eq!(bar_fill_width(1000, 100), 1000);
        assert_eq!(bar_fill_width(1000, 150), 1000);
        assert_eq!(bar_fill_width(3, 50), 2);
    }

    #[test]
    fn sweep_covers_the_whole_bar_over_a_cycle() {
        assert_eq!(sweep_offset(1000, 250, 0.0), -250);
        assert_eq!(sweep_offset(1000, 250, 1.6), -250);
        assert_eq!(sweep_offset(1000, 250, 0.8), 375);
        let near_end = sweep_offset(1000, 250, 1.599);
        assert!(near_end > 990, "{near_end}");
    }
}
