//! Inter faces embedded in the binary, string rasterization with fontdue, and a
//! texture cache so each (text, size, weight) is rasterized once per run.

use anyhow::{anyhow, Result};
use fontdue::layout::{CoordinateSystem, GlyphPosition, Layout, LayoutSettings, TextStyle};
use fontdue::{Font, FontSettings};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{BlendMode, Texture, TextureCreator, WindowCanvas};
use sdl2::video::WindowContext;
use std::collections::HashMap;

static REGULAR: &[u8] = include_bytes!(env!("STEAM_MAINTENANCE_FONT_REGULAR"));
static BOLD: &[u8] = include_bytes!(env!("STEAM_MAINTENANCE_FONT_BOLD"));

/// Entries kept before the cache is flushed wholesale.
const CACHE_LIMIT: usize = 512;

/// A rasterized string: coverage per pixel, row-major.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

/// The regular and bold faces.
pub struct Fonts {
    regular: Font,
    bold: Font,
}

impl Fonts {
    /// Parse the embedded faces. Bold comes from the collection when it has an
    /// "Inter Bold" (or SemiBold) face, otherwise it is the regular face again.
    pub fn load() -> Result<Fonts> {
        let regular = load_face(REGULAR, 0)?;
        let bold = bold_face_index(BOLD)
            .and_then(|i| load_face(BOLD, i).ok())
            .unwrap_or_else(|| regular.clone());
        Ok(Fonts { regular, bold })
    }

    fn face(&self, bold: bool) -> &Font {
        if bold {
            &self.bold
        } else {
            &self.regular
        }
    }

    fn layout(&self, text: &str, px: f32, bold: bool) -> Layout {
        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings::default());
        layout.append(&[self.face(bold)], &TextStyle::new(text, px, 0));
        layout
    }

    /// Pixel size of `text` as `rasterize` would produce it.
    pub fn measure(&self, text: &str, px: f32, bold: bool) -> (u32, u32) {
        let layout = self.layout(text, px, bold);
        (ink_width(layout.glyphs()), layout.height().ceil() as u32)
    }

    /// Render `text` on one line into a coverage bitmap.
    pub fn rasterize(&self, text: &str, px: f32, bold: bool) -> Bitmap {
        let font = self.face(bold);
        let layout = self.layout(text, px, bold);
        let width = ink_width(layout.glyphs());
        let height = layout.height().ceil() as u32;
        let mut alpha = vec![0u8; (width * height) as usize];
        for glyph in layout.glyphs() {
            if glyph.width == 0 || glyph.height == 0 {
                continue;
            }
            let (_, coverage) = font.rasterize_config(glyph.key);
            blit(
                &mut alpha,
                width,
                height,
                glyph.x as i32,
                glyph.y as i32,
                glyph.width as u32,
                &coverage,
            );
        }
        Bitmap {
            width,
            height,
            alpha,
        }
    }

    /// `text` if it fits in `max_width` pixels, otherwise a prefix with an ellipsis that does.
    pub fn fit(&self, text: &str, px: f32, bold: bool, max_width: u32) -> String {
        if self.measure(text, px, bold).0 <= max_width {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut keep = chars.len();
        while keep > 0 {
            keep -= 1;
            let candidate: String = chars[..keep]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_string()
                + "…";
            if self.measure(&candidate, px, bold).0 <= max_width {
                return candidate;
            }
        }
        "…".to_string()
    }
}

fn load_face(data: &'static [u8], collection_index: u32) -> Result<Font> {
    let settings = FontSettings {
        collection_index,
        ..FontSettings::default()
    };
    Font::from_bytes(data, settings).map_err(|e| anyhow!("loading font: {e}"))
}

/// Index of the "Inter Bold" face in a collection, SemiBold as a fallback; 0 for a
/// plain font file; None when no suitable face exists.
fn bold_face_index(data: &[u8]) -> Option<u32> {
    let count = match ttf_parser::fonts_in_collection(data) {
        None => return Some(0),
        Some(count) => count,
    };
    let mut semibold = None;
    for index in 0..count {
        let Ok(face) = ttf_parser::Face::parse(data, index) else {
            continue;
        };
        match full_name(&face).as_deref() {
            Some("Inter Bold") => return Some(index),
            Some("Inter SemiBold") => semibold = Some(index),
            _ => {}
        }
    }
    semibold
}

fn full_name(face: &ttf_parser::Face) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|n| n.name_id == ttf_parser::name_id::FULL_NAME && n.is_unicode())
        .find_map(|n| {
            let units: Vec<u16> = n
                .name
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16(&units).ok()
        })
}

fn ink_width(glyphs: &[GlyphPosition]) -> u32 {
    glyphs
        .iter()
        .map(|g| (g.x as i32 + g.width as i32).max(0))
        .max()
        .unwrap_or(0) as u32
}

/// Max-blend a glyph's coverage into the string bitmap, clipping at the edges.
fn blit(dst: &mut [u8], dst_w: u32, dst_h: u32, x0: i32, y0: i32, src_w: u32, src: &[u8]) {
    for (row, line) in src.chunks_exact(src_w as usize).enumerate() {
        let y = y0 + row as i32;
        if y < 0 || y >= dst_h as i32 {
            continue;
        }
        for (col, &cov) in line.iter().enumerate() {
            let x = x0 + col as i32;
            if x < 0 || x >= dst_w as i32 {
                continue;
            }
            let i = (y as u32 * dst_w + x as u32) as usize;
            dst[i] = dst[i].max(cov);
        }
    }
}

/// Size, weight and color of a run of text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub px: u32,
    pub bold: bool,
    pub color: Color,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    text: String,
    px: u32,
    bold: bool,
}

struct Entry<'a> {
    texture: Texture<'a>,
    width: u32,
    height: u32,
}

/// Rasterized strings as white-with-alpha textures, tinted at draw time.
pub struct TextCache<'a> {
    creator: &'a TextureCreator<WindowContext>,
    fonts: &'a Fonts,
    entries: HashMap<Key, Entry<'a>>,
}

impl<'a> TextCache<'a> {
    pub fn new(creator: &'a TextureCreator<WindowContext>, fonts: &'a Fonts) -> TextCache<'a> {
        TextCache {
            creator,
            fonts,
            entries: HashMap::new(),
        }
    }

    pub fn fonts(&self) -> &Fonts {
        self.fonts
    }

    /// Drop everything once the cache grows past its limit; called between frames.
    pub fn trim(&mut self) {
        if self.entries.len() > CACHE_LIMIT {
            self.entries.clear();
        }
    }

    pub fn measure(&self, text: &str, px: u32, bold: bool) -> (u32, u32) {
        self.fonts.measure(text, px as f32, bold)
    }

    fn entry(&mut self, text: &str, px: u32, bold: bool) -> Result<&mut Entry<'a>> {
        let key = Key {
            text: text.to_string(),
            px,
            bold,
        };
        if !self.entries.contains_key(&key) {
            let entry = upload(self.creator, self.fonts.rasterize(text, px as f32, bold))?;
            self.entries.insert(key.clone(), entry);
        }
        Ok(self.entries.get_mut(&key).expect("just inserted"))
    }

    /// Draw `text` with its top-left corner at (x, y); returns the drawn size.
    pub fn draw(
        &mut self,
        canvas: &mut WindowCanvas,
        text: &str,
        style: Style,
        x: i32,
        y: i32,
    ) -> Result<(u32, u32)> {
        if text.is_empty() {
            return Ok((0, 0));
        }
        let entry = self.entry(text, style.px, style.bold)?;
        if entry.width == 0 || entry.height == 0 {
            return Ok((0, 0));
        }
        let color = style.color;
        entry.texture.set_color_mod(color.r, color.g, color.b);
        entry.texture.set_alpha_mod(color.a);
        let dst = Rect::new(x, y, entry.width, entry.height);
        canvas
            .copy(&entry.texture, None, dst)
            .map_err(anyhow::Error::msg)?;
        Ok((entry.width, entry.height))
    }

    /// Draw `text` with its top-right corner at (right, y).
    pub fn draw_right(
        &mut self,
        canvas: &mut WindowCanvas,
        text: &str,
        style: Style,
        right: i32,
        y: i32,
    ) -> Result<(u32, u32)> {
        let (w, _) = self.measure(text, style.px, style.bold);
        self.draw(canvas, text, style, right - w as i32, y)
    }
}

fn upload<'a>(creator: &'a TextureCreator<WindowContext>, bitmap: Bitmap) -> Result<Entry<'a>> {
    if bitmap.width == 0 || bitmap.height == 0 {
        // SDL refuses zero-sized textures; a 1x1 transparent one keeps the key cached.
        let texture = creator
            .create_texture_streaming(PixelFormatEnum::RGBA32, 1, 1)
            .map_err(|e| anyhow!("{e}"))?;
        return Ok(Entry {
            texture,
            width: 0,
            height: 0,
        });
    }
    let mut pixels = Vec::with_capacity(bitmap.alpha.len() * 4);
    for &a in &bitmap.alpha {
        pixels.extend_from_slice(&[255, 255, 255, a]);
    }
    let mut texture = creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, bitmap.width, bitmap.height)
        .map_err(|e| anyhow!("{e}"))?;
    texture
        .update(None, &pixels, bitmap.width as usize * 4)
        .map_err(|e| anyhow!("{e}"))?;
    texture.set_blend_mode(BlendMode::Blend);
    Ok(Entry {
        texture,
        width: bitmap.width,
        height: bitmap.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_face_is_found_in_the_collection() {
        let index = bold_face_index(BOLD).expect("a bold face");
        let face = ttf_parser::Face::parse(BOLD, index).unwrap();
        let name = full_name(&face).unwrap();
        assert!(name == "Inter Bold" || name == "Inter SemiBold", "{name}");
    }

    #[test]
    fn plain_font_file_uses_index_zero() {
        assert_eq!(bold_face_index(REGULAR), Some(0));
    }

    #[test]
    fn rasterized_text_has_ink() {
        let fonts = Fonts::load().unwrap();
        let bitmap = fonts.rasterize("Update", 32.0, true);
        assert!(bitmap.width > 0 && bitmap.height > 0);
        assert!(bitmap.alpha.iter().any(|&a| a > 128));
        assert_eq!(
            fonts.measure("Update", 32.0, true),
            (bitmap.width, bitmap.height)
        );
    }

    #[test]
    fn fit_ellipsizes_long_text() {
        let fonts = Fonts::load().unwrap();
        let long = "feat(smeagol): add the steam deck host and a very long explanation";
        let fitted = fonts.fit(long, 24.0, false, 200);
        assert!(fitted.ends_with('…'));
        assert!(fonts.measure(&fitted, 24.0, false).0 <= 200);
        assert_eq!(fonts.fit("short", 24.0, false, 1000), "short");
    }
}
