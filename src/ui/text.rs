//! A small rasterizer that draws text directly into a softbuffer raw
//! buffer (0x00RRGGBB). Loads standard Windows fonts at runtime (not bundled).
//!
//! UI labels etc. are drawn with the usual Latin-script font (e.g. Segoe
//! UI), falling back to a Japanese font (e.g. Yu Gothic) only for
//! characters that font lacks glyphs for (kana/kanji). Unifying to a
//! single font would make Japanese glyphs look smaller/different and
//! change the UI's appearance, so this keeps the existing look while
//! still supporting Japanese text.

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};

use super::{Canvas, Rect};

/// Which font a glyph was resolved from (kerning is only applied between glyphs from the same font).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Primary,
    Fallback,
}

/// Draws text using loaded fonts.
pub struct TextRenderer {
    /// The primary font (Latin script) used for UI labels.
    primary: FontVec,
    /// A Japanese fallback used only for glyphs missing from the primary font (kana/kanji, etc).
    fallback: Option<FontVec>,
}

impl TextRenderer {
    /// Loads the UI font and a Japanese fallback from the Windows font
    /// directory, matched to a typical editor workbench UI's font. `None`
    /// if the primary font isn't found (callers can continue without labels).
    pub fn load() -> Option<Self> {
        let primary = Self::load_one(&["segoeui.ttf", "arial.ttf"])?;
        // The kana/kanji fallback. `ab_glyph` (backed by `ttf-parser`)
        // can also read the first font out of a `.ttc` (font collection) as-is.
        let fallback = Self::load_one(&["YuGothM.ttc", "msgothic.ttc"]);
        Some(Self { primary, fallback })
    }

    fn load_one(names: &[&str]) -> Option<FontVec> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        for name in names {
            let path = std::path::Path::new(&root).join("Fonts").join(name);
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(font) = FontVec::try_from_vec(bytes)
            {
                return Some(font);
            }
        }
        None
    }

    fn font(&self, which: Which) -> &FontVec {
        match which {
            Which::Primary => &self.primary,
            Which::Fallback => self.fallback.as_ref().unwrap_or(&self.primary),
        }
    }

    /// Chooses the font to draw `ch` with and its glyph ID within that
    /// font. Falls back to the Japanese font if the primary font has no
    /// glyph (`.notdef` = `GlyphId(0)`).
    fn resolve(&self, ch: char) -> (Which, GlyphId) {
        let id = self.primary.glyph_id(ch);
        if id.0 != 0 {
            return (Which::Primary, id);
        }
        if let Some(fb) = &self.fallback {
            let fid = fb.glyph_id(ch);
            if fid.0 != 0 {
                return (Which::Fallback, fid);
            }
        }
        (Which::Primary, id)
    }

    /// Text width (px) at `size` px.
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        let scale = PxScale::from(size);
        let mut w = 0.0;
        let mut prev: Option<(Which, GlyphId)> = None;
        for ch in text.chars() {
            let (which, id) = self.resolve(ch);
            let scaled = self.font(which).as_scaled(scale);
            if let Some((pwhich, pid)) = prev
                && pwhich == which
            {
                w += scaled.kern(pid, id);
            }
            w += scaled.h_advance(id);
            prev = Some((which, id));
        }
        w
    }

    /// Returns the baseline y for centering vertically on `center_y`.
    /// Based on the primary font's line metrics (since labels are mostly Latin script).
    pub fn baseline_for_center(&self, center_y: f32, size: f32) -> f32 {
        let scaled = self.primary.as_scaled(PxScale::from(size));
        center_y + (scaled.ascent() + scaled.descent()) / 2.0
    }

    /// The font's top/bottom extent at `size` px (distance from the
    /// baseline; `ascent` positive, `descent` negative). Used to match a
    /// text cursor/selection highlight's height to the actual glyph
    /// height (based on the primary font's line metrics, same as `baseline_for_center`).
    pub fn glyph_vextent(&self, size: f32) -> (f32, f32) {
        let scaled = self.primary.as_scaled(PxScale::from(size));
        (scaled.ascent(), scaled.descent())
    }

    /// Draws `text` in `color` (0x00RRGGBB), with `(x, baseline)` as the
    /// left edge/baseline. Alpha-blends into existing pixels by coverage.
    pub fn draw(
        &self,
        canvas: &mut Canvas,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        color: u32,
    ) {
        self.draw_impl(canvas, x, baseline, text, size, color, None);
    }

    /// `draw` with a rect clip added; nothing is drawn outside `clip`
    /// (used so a scrolling row appears cut off smoothly at an arbitrary
    /// viewport boundary rather than the whole canvas).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_clipped(
        &self,
        canvas: &mut Canvas,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        color: u32,
        clip: Rect,
    ) {
        self.draw_impl(canvas, x, baseline, text, size, color, Some(clip));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_impl(
        &self,
        canvas: &mut Canvas,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        color: u32,
        clip: Option<Rect>,
    ) {
        // `x`/`baseline`/`size` (and `clip`) are logical-pixel values, same
        // as every other caller-facing coordinate in this codebase; scale
        // them to the canvas's physical resolution once here so glyphs are
        // rasterized crisply at any DPI instead of being nearest-neighbor
        // magnified after the fact.
        let dpi = canvas.scale as f32;
        let x = x * dpi;
        let baseline = baseline * dpi;
        let size = size * dpi;
        let clip = clip.map(|c| {
            let s = |v: usize| ((v as f64) * canvas.scale).round() as usize;
            Rect {
                x0: s(c.x0),
                y0: s(c.y0),
                x1: s(c.x1),
                y1: s(c.y1),
            }
        });

        let (sw, sh) = (canvas.w, canvas.h);
        let scale = PxScale::from(size);
        let fr = ((color >> 16) & 0xff) as f32;
        let fg = ((color >> 8) & 0xff) as f32;
        let fb = (color & 0xff) as f32;

        let mut pen_x = x;
        let mut prev: Option<(Which, GlyphId)> = None;
        for ch in text.chars() {
            let (which, id) = self.resolve(ch);
            let font = self.font(which);
            let scaled = font.as_scaled(scale);
            if let Some((pwhich, pid)) = prev
                && pwhich == which
            {
                pen_x += scaled.kern(pid, id);
            }
            let glyph = id.with_scale_and_position(scale, point(pen_x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 || px as usize >= sw || py as usize >= sh {
                        return;
                    }
                    if let Some(c) = clip {
                        let outside = (px as usize) < c.x0
                            || (py as usize) < c.y0
                            || (px as usize) >= c.x1
                            || (py as usize) >= c.y1;
                        if outside {
                            return;
                        }
                    }
                    let idx = py as usize * sw + px as usize;
                    let bg = canvas.buf[idx];
                    let br = ((bg >> 16) & 0xff) as f32;
                    let bgc = ((bg >> 8) & 0xff) as f32;
                    let bb = (bg & 0xff) as f32;
                    let c = coverage;
                    let r = (br * (1.0 - c) + fr * c) as u32 & 0xff;
                    let g = (bgc * (1.0 - c) + fg * c) as u32 & 0xff;
                    let b = (bb * (1.0 - c) + fb * c) as u32 & 0xff;
                    canvas.buf[idx] = (r << 16) | (g << 8) | b;
                });
            }
            pen_x += scaled.h_advance(id);
            prev = Some((which, id));
        }
    }
}
