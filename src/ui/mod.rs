//! Shared UI primitives: `Canvas`, which draws directly into a
//! softbuffer raw buffer (0x00RRGGBB), and the `Rect` rectangle. Used by
//! multiple modules including overlay/editor.

pub mod text;

use text::TextRenderer;

/// A normalized rect (`x0..x1` and `y0..y1` are exclusive bounds).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub fn width(&self) -> usize {
        self.x1 - self.x0
    }
    pub fn height(&self) -> usize {
        self.y1 - self.y0
    }
}

/// Alpha-blends `color` into `dst` by `coverage` (exactly what
/// `Canvas::blend_i` does after its bounds check). Kept as a free
/// function independent of `Canvas` so it also works for parallel
/// rendering that can't go through a whole `Canvas` — e.g. splitting rows
/// into separate `&mut [u32]` slices handed off to multiple threads.
pub fn blend_pixel(dst: &mut u32, color: u32, coverage: f32) {
    if coverage <= 0.0 {
        return;
    }
    if coverage >= 1.0 {
        *dst = color;
        return;
    }
    let bg = *dst;
    let mix = |b: u32, f: u32| -> u32 {
        (b as f32 * (1.0 - coverage) + f as f32 * coverage) as u32 & 0xff
    };
    let r = mix((bg >> 16) & 0xff, (color >> 16) & 0xff);
    let g = mix((bg >> 8) & 0xff, (color >> 8) & 0xff);
    let b = mix(bg & 0xff, color & 0xff);
    *dst = (r << 16) | (g << 8) | b;
}

/// A drawing target: a softbuffer raw buffer paired with its width/height.
pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub w: usize,
    pub h: usize,
    /// Physical-per-logical-pixel ratio applied by the shape-drawing
    /// methods below (`fill`/`stroke`/`line`*/`fill_circle`*/`blit_scaled`*)
    /// to their own geometric inputs, once, before doing their normal
    /// per-physical-pixel work — so callers can keep passing the same
    /// fixed logical-pixel layout values regardless of the window's DPI.
    /// `1.0` (the default for every caller except the Settings window) is
    /// an exact identity transform. `set`/`set_i`/`blend_i` are the raw
    /// per-physical-pixel primitives and are deliberately NOT scaled here
    /// (the methods above already convert to physical coordinates before
    /// calling them — scaling twice would leave gaps).
    pub scale: f64,
}

impl Canvas<'_> {
    fn scaled_rect(&self, r: Rect) -> Rect {
        let s = |v: usize| ((v as f64) * self.scale).round() as usize;
        Rect {
            x0: s(r.x0),
            y0: s(r.y0),
            x1: s(r.x1),
            y1: s(r.y1),
        }
    }

    pub fn set(&mut self, x: usize, y: usize, color: u32) {
        if x < self.w && y < self.h {
            self.buf[y * self.w + x] = color;
        }
    }

    /// A signed-coordinate version (ignores out-of-range values).
    pub fn set_i(&mut self, x: i64, y: i64, color: u32) {
        if x >= 0 && y >= 0 {
            self.set(x as usize, y as usize, color);
        }
    }

    /// Alpha-blends `color` into the existing pixel by `coverage`
    /// (0.0-1.0, how much the shape covers the pixel) — the shared
    /// anti-aliasing primitive, matching the same blending idea used by
    /// `text.rs`'s glyph rendering.
    pub fn blend_i(&mut self, x: i64, y: i64, color: u32, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.w || y >= self.h {
            return;
        }
        blend_pixel(&mut self.buf[y * self.w + x], color, coverage);
    }

    /// Fills the inside of a rect.
    pub fn fill(&mut self, r: Rect, color: u32) {
        let r = self.scaled_rect(r);
        for y in r.y0..r.y1.min(self.h) {
            let row = y * self.w;
            for x in r.x0..r.x1.min(self.w) {
                self.buf[row + x] = color;
            }
        }
    }

    /// Draws a rect's 1px outline.
    pub fn stroke(&mut self, r: Rect, color: u32) {
        let r = self.scaled_rect(r);
        for x in r.x0..r.x1 {
            self.set(x, r.y0, color);
            if r.y1 > 0 {
                self.set(x, r.y1 - 1, color);
            }
        }
        for y in r.y0..r.y1 {
            self.set(r.x0, y, color);
            if r.x1 > 0 {
                self.set(r.x1 - 1, y, color);
            }
        }
    }

    /// Draws a line of thickness `thickness` (flat caps, anti-aliased edges).
    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, thickness: i64, color: u32) {
        self.line_impl(x0, y0, x1, y1, thickness, color, None);
    }

    /// `line` with a rect clip added; nothing is drawn outside `clip`
    /// (used so a scrolling icon etc. appears cut off at an arbitrary
    /// viewport boundary rather than the whole canvas).
    #[allow(clippy::too_many_arguments)]
    pub fn line_clipped(
        &mut self,
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        thickness: i64,
        color: u32,
        clip: Rect,
    ) {
        self.line_impl(x0, y0, x1, y1, thickness, color, Some(clip));
    }

    /// Draws a `thickness`-thick segment by computing coverage from the
    /// signed distance to a flat-capped rect (square ends), giving
    /// smooth diagonals and edges.
    #[allow(clippy::too_many_arguments)]
    fn line_impl(
        &mut self,
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        thickness: i64,
        color: u32,
        clip: Option<Rect>,
    ) {
        let s = |v: i64| ((v as f64) * self.scale).round() as i64;
        let (x0, y0, x1, y1) = (s(x0), s(y0), s(x1), s(y1));
        let thickness = (((thickness as f64) * self.scale).round() as i64).max(1);
        let clip = clip.map(|c| self.scaled_rect(c));
        let half = (thickness.max(1) as f64) / 2.0;
        let (fx0, fy0, fx1, fy1) = (x0 as f64, y0 as f64, x1 as f64, y1 as f64);
        let (dx, dy) = (fx1 - fx0, fy1 - fy0);
        let len = (dx * dx + dy * dy).sqrt();

        // The bounding box (each endpoint ± (half-thickness+1)), clamped
        // to canvas/clip bounds before iterating (so the iteration count
        // doesn't explode when coordinates extend far outside the canvas).
        let margin = half.ceil() as i64 + 1;
        let mut bx0 = x0.min(x1) - margin;
        let mut by0 = y0.min(y1) - margin;
        let mut bx1 = x0.max(x1) + margin;
        let mut by1 = y0.max(y1) + margin;
        bx0 = bx0.max(0);
        by0 = by0.max(0);
        bx1 = bx1.min(self.w as i64 - 1);
        by1 = by1.min(self.h as i64 - 1);
        if let Some(c) = clip {
            bx0 = bx0.max(c.x0 as i64);
            by0 = by0.max(c.y0 as i64);
            bx1 = bx1.min(c.x1 as i64 - 1);
            by1 = by1.min(c.y1 as i64 - 1);
        }

        for py in by0..=by1 {
            for px in bx0..=bx1 {
                let (qx, qy) = (px as f64 - fx0, py as f64 - fy0);
                let d = if len < 1e-6 {
                    // Zero length (a point): direction is undefined, so
                    // treat it as an axis-aligned square (matches how the
                    // old Bresenham version stamped a square in a single step).
                    qx.abs().max(qy.abs()) - half
                } else {
                    let (ux, uy) = (dx / len, dy / len);
                    let along = qx * ux + qy * uy;
                    let across = qx * -uy + qy * ux;
                    // Signed distance to a rect centered on the midpoint
                    // along the line's direction (a box SDF).
                    let ax = (along - len / 2.0).abs() - len / 2.0;
                    let ay = across.abs() - half;
                    ax.max(0.0).hypot(ay.max(0.0)) + ax.max(ay).min(0.0)
                };
                let coverage = (0.5 - d).clamp(0.0, 1.0) as f32;
                if coverage > 0.0 {
                    self.blend_i(px, py, color, coverage);
                }
            }
        }
    }

    /// Draws a filled circle (used for icon placeholders, handles, etc).
    /// Turns distance from the edge into coverage for a smooth boundary.
    pub fn fill_circle(&mut self, cx: i64, cy: i64, r: i64, color: u32) {
        self.fill_circle_f(cx as f64, cy as f64, r as f64, color);
    }

    /// A floating-point version of `fill_circle`, drawable without
    /// rounding the radius/center to integers — used for round joins
    /// (`round_join`) that need to match a line's half-thickness exactly.
    /// Rounding the radius to an integer would make it slightly larger/
    /// smaller than the line's actual half-thickness, causing the joint
    /// alone to look bulged (or pinched).
    pub fn fill_circle_f(&mut self, cx: f64, cy: f64, r: f64, color: u32) {
        self.fill_circle_f_impl(cx, cy, r, color, None);
    }

    /// `fill_circle_f` with a rect clip added; nothing is drawn outside
    /// `clip` (same purpose as `line_clipped` — keeps a joint circle from
    /// spilling past the clip range at a scroll boundary).
    pub fn fill_circle_f_clipped(&mut self, cx: f64, cy: f64, r: f64, color: u32, clip: Rect) {
        self.fill_circle_f_impl(cx, cy, r, color, Some(clip));
    }

    fn fill_circle_f_impl(&mut self, cx: f64, cy: f64, r: f64, color: u32, clip: Option<Rect>) {
        let (cx, cy, r) = (cx * self.scale, cy * self.scale, r * self.scale);
        let clip = clip.map(|c| self.scaled_rect(c));
        let margin = 1;
        let mut x0 = (cx - r).floor() as i64 - margin;
        let mut x1 = (cx + r).ceil() as i64 + margin;
        let mut y0 = (cy - r).floor() as i64 - margin;
        let mut y1 = (cy + r).ceil() as i64 + margin;
        if let Some(c) = clip {
            x0 = x0.max(c.x0 as i64);
            y0 = y0.max(c.y0 as i64);
            x1 = x1.min(c.x1 as i64 - 1);
            y1 = y1.min(c.y1 as i64 - 1);
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = (x as f64 - cx).hypot(y as f64 - cy) - r;
                let coverage = (0.5 - d).clamp(0.0, 1.0) as f32;
                if coverage > 0.0 {
                    self.blend_i(x, y, color, coverage);
                }
            }
        }
    }

    /// Scales `src` (`src_w x src_h`, packed pixels) to fit rect `dst`
    /// via nearest-neighbor sampling (for simple blits like thumbnails).
    pub fn blit_scaled(&mut self, dst: Rect, src_w: usize, src_h: usize, src: &[u32]) {
        let dst = self.scaled_rect(dst);
        if src_w == 0 || src_h == 0 || dst.width() == 0 || dst.height() == 0 {
            return;
        }
        for dy in 0..dst.height() {
            let sy = (dy * src_h / dst.height()).min(src_h - 1);
            for dx in 0..dst.width() {
                let sx = (dx * src_w / dst.width()).min(src_w - 1);
                self.set(dst.x0 + dx, dst.y0 + dy, src[sy * src_w + sx]);
            }
        }
    }

    /// An alpha-blending version of `blit_scaled`, using each `src`
    /// pixel's top byte (alpha) as coverage when blending into `dst`
    /// (used to draw an icon image with a transparent background over a
    /// button's background).
    pub fn blit_scaled_alpha(&mut self, dst: Rect, src_w: usize, src_h: usize, src: &[u32]) {
        let dst = self.scaled_rect(dst);
        if src_w == 0 || src_h == 0 || dst.width() == 0 || dst.height() == 0 {
            return;
        }
        for dy in 0..dst.height() {
            let sy = (dy * src_h / dst.height()).min(src_h - 1);
            for dx in 0..dst.width() {
                let sx = (dx * src_w / dst.width()).min(src_w - 1);
                let px = src[sy * src_w + sx];
                let coverage = ((px >> 24) & 0xff) as f32 / 255.0;
                self.blend_i(
                    (dst.x0 + dx) as i64,
                    (dst.y0 + dy) as i64,
                    px & 0x00FF_FFFF,
                    coverage,
                );
            }
        }
    }
}

/// The shared look of a square button: an icon on top (currently a
/// placeholder circle) and an uppercase label below. Shared by the
/// post-selection action menu and the recording control bar.
pub fn draw_icon_button(
    canvas: &mut Canvas,
    rect: Rect,
    bg: u32,
    icon_color: u32,
    label: &str,
    text_color: u32,
    text: &TextRenderer,
) {
    canvas.fill(rect, bg);

    let w = rect.width() as f32;
    let h = rect.height() as f32;
    // Draws a placeholder circle in the icon area (top 55%).
    let icon_h = h * 0.55;
    let icon_cx = rect.x0 as f32 + w / 2.0;
    let icon_cy = rect.y0 as f32 + icon_h / 2.0;
    let r = (icon_h.min(w) * 0.28) as i64;
    canvas.fill_circle(icon_cx as i64, icon_cy as i64, r, icon_color);

    // The label (uppercase, centered in the bottom 45%). Font size
    // tracks the button size (measured: at a 56px button, the longest
    // label "DESKTOP" fits in a 46.4px width at 15px) — no upper clamp,
    // so a caller drawing intentionally larger buttons (e.g. DPI-scaled)
    // gets proportionally larger, still-fitting text rather than a
    // fixed-size label that looks undersized inside a bigger button.
    let label_font = (h * 0.27).max(11.0);
    let label = label.to_uppercase();
    let tw = text.text_width(&label, label_font);
    let lx = rect.x0 as f32 + (w - tw) / 2.0;
    let label_cy = rect.y0 as f32 + icon_h + (h - icon_h) / 2.0;
    let baseline = text.baseline_for_center(label_cy, label_font);
    text.draw(canvas, lx, baseline, &label, label_font, text_color);
}

/// Draws a "refresh" icon (an arc plus an arrowhead, like a typical
/// reload button) with a rect clip. Nothing is drawn outside `clip` (so
/// while scrolling, the icon appears cut off smoothly at the boundary
/// without changing size — recomputing the center/radius from the
/// clamped rect would make it shrink while being cut off, so the caller
/// must always pass the pre-clamp center and a fixed radius).
#[allow(clippy::too_many_arguments)]
pub fn draw_refresh_icon_clipped(
    canvas: &mut Canvas,
    center: (i64, i64),
    radius: i64,
    thickness: i64,
    color: u32,
    clip: Rect,
) {
    draw_refresh_icon_impl(canvas, center, radius, thickness, color, Some(clip));
}

#[allow(clippy::too_many_arguments)]
fn draw_refresh_icon_impl(
    canvas: &mut Canvas,
    center: (i64, i64),
    radius: i64,
    thickness: i64,
    color: u32,
    clip: Option<Rect>,
) {
    let line = |canvas: &mut Canvas, x0: i64, y0: i64, x1: i64, y1: i64| match clip {
        Some(c) => canvas.line_clipped(x0, y0, x1, y1, thickness, color, c),
        None => canvas.line(x0, y0, x1, y1, thickness, color),
    };
    // Since `line` has flat, anti-aliased caps, fills the joint between
    // segments at different angles with a circle to avoid jaggedness
    // (round join). Rounding the radius to an integer would drift from
    // the line's actual half-thickness and make just the joint look
    // bulged, so `fill_circle_f` is used to match it exactly.
    let join = |canvas: &mut Canvas, x: i64, y: i64| {
        let r = (thickness.max(1) as f64) / 2.0;
        match clip {
            Some(c) => canvas.fill_circle_f_clipped(x as f64, y as f64, r, color, c),
            None => canvas.fill_circle_f(x as f64, y as f64, r, color),
        }
    };

    let (cx, cy) = (center.0 as f64, center.1 as f64);
    let r = radius as f64;
    // Draws roughly a 270-degree clockwise arc, leaving the rest open for the arrowhead.
    let start = -110.0_f64.to_radians();
    let end = 160.0_f64.to_radians();
    const STEPS: usize = 28;
    let pts: Vec<(f64, f64)> = (0..=STEPS)
        .map(|i| {
            let t = start + (end - start) * (i as f64 / STEPS as f64);
            (cx + r * t.cos(), cy + r * t.sin())
        })
        .collect();
    for w in pts.windows(2) {
        line(
            canvas,
            w[0].0.round() as i64,
            w[0].1.round() as i64,
            w[1].0.round() as i64,
            w[1].1.round() as i64,
        );
        join(canvas, w[1].0.round() as i64, w[1].1.round() as i64);
    }
    // The arrowhead: a triangle at the arc's end, pointing along the tangent (direction of travel).
    let (bx, by) = pts[pts.len() - 2];
    let (ex, ey) = pts[pts.len() - 1];
    let (dx, dy) = (ex - bx, ey - by);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (dx, dy) = (dx / len, dy / len);
    let (px, py) = (-dy, dx); // A vector perpendicular to the direction of travel.
    let head = r * 0.9;
    let tip = (ex + dx * head * 0.6, ey + dy * head * 0.6);
    let w1 = (
        ex - dx * head * 0.4 + px * head * 0.55,
        ey - dy * head * 0.4 + py * head * 0.55,
    );
    let w2 = (
        ex - dx * head * 0.4 - px * head * 0.55,
        ey - dy * head * 0.4 - py * head * 0.55,
    );
    line(
        canvas,
        w1.0.round() as i64,
        w1.1.round() as i64,
        tip.0.round() as i64,
        tip.1.round() as i64,
    );
    line(
        canvas,
        w2.0.round() as i64,
        w2.1.round() as i64,
        tip.0.round() as i64,
        tip.1.round() as i64,
    );
    join(canvas, tip.0.round() as i64, tip.1.round() as i64);
}

/// Converts RGBA8 (top-to-bottom, left-to-right) into a packed 0x00RRGGBB array.
pub fn rgba_to_packed(w: usize, h: usize, rgba: &[u8]) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let r = rgba[i * 4] as u32;
            let g = rgba[i * 4 + 1] as u32;
            let b = rgba[i * 4 + 2] as u32;
            (r << 16) | (g << 8) | b
        })
        .collect()
}

/// Converts RGBA8 (top-to-bottom, left-to-right) into a packed
/// 0xAARRGGBB array, preserving alpha unlike `rgba_to_packed` (keeps
/// transparency for pasted/dropped images).
pub fn rgba_to_packed_alpha(w: usize, h: usize, rgba: &[u8]) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let r = rgba[i * 4] as u32;
            let g = rgba[i * 4 + 1] as u32;
            let b = rgba[i * 4 + 2] as u32;
            let a = rgba[i * 4 + 3] as u32;
            (a << 24) | (r << 16) | (g << 8) | b
        })
        .collect()
}

/// The reverse of `rgba_to_packed_alpha`, using the top byte's real alpha as-is.
pub fn packed_to_rgba_alpha(w: usize, h: usize, pixels: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for &p in &pixels[..w * h] {
        out.push(((p >> 16) & 0xff) as u8);
        out.push(((p >> 8) & 0xff) as u8);
        out.push((p & 0xff) as u8);
        out.push(((p >> 24) & 0xff) as u8);
    }
    out
}

/// The drag target for a color picker (an SV square + hue bar). Shared
/// by the Editor's annotation color picker and the Settings click-ripple color picker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerPart {
    Sv,
    Hue,
}

/// HSV(0..360, 0..1, 0..1) -> 0x00RRGGBB.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

/// 0x00RRGGBB -> HSV(0..360, 0..1, 0..1).
pub fn rgb_to_hsv(c: u32) -> (f32, f32, f32) {
    let r = ((c >> 16) & 0xff) as f32 / 255.0;
    let g = ((c >> 8) & 0xff) as f32 / 255.0;
    let b = (c & 0xff) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

/// The color picker's current-value marker (a hollow white square).
pub fn marker(canvas: &mut Canvas, cx: i64, cy: i64) {
    let s = canvas.scale;
    let cx = ((cx as f64) * s).round() as i64;
    let cy = ((cy as f64) * s).round() as i64;
    let half = ((4.0 * s).round() as i64).max(1);
    for d in -half..=half {
        canvas.set_i(cx + d, cy - half, 0x00FF_FFFF);
        canvas.set_i(cx + d, cy + half, 0x00FF_FFFF);
        canvas.set_i(cx - half, cy + d, 0x00FF_FFFF);
        canvas.set_i(cx + half, cy + d, 0x00FF_FFFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: u32 = 0x00FF_FFFF;
    const FG: u32 = 0x0000_0000;

    #[test]
    fn blend_i_interpolates_by_coverage() {
        let mut buf = vec![BG];
        let mut canvas = Canvas {
            buf: &mut buf,
            w: 1,
            h: 1,
            scale: 1.0,
        };
        canvas.blend_i(0, 0, FG, 0.0);
        assert_eq!(canvas.buf[0], BG, "カバレッジ0なら変化しない");
        canvas.blend_i(0, 0, FG, 1.0);
        assert_eq!(canvas.buf[0], FG, "カバレッジ1なら完全に上書き");
        canvas.buf[0] = BG;
        canvas.blend_i(0, 0, FG, 0.5);
        let ch = (canvas.buf[0] >> 16) & 0xff;
        assert!(
            (120..=135).contains(&ch),
            "中間のブレンドになるはず: ch={ch}"
        );
    }

    #[test]
    fn fill_circle_center_is_full_color_edge_is_blended_and_outside_is_background() {
        let (w, h) = (20, 20);
        let mut buf = vec![BG; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            canvas.fill_circle(10, 10, 5, FG);
        }
        assert_eq!(buf[10 * w + 10], FG, "中心は完全に前景色");
        let edge = buf[10 * w + (10 + 5)];
        assert_ne!(edge, BG, "境界はハードな背景色のままにならない");
        assert_ne!(edge, FG, "境界はハードな前景色にもならない");
        assert_eq!(buf[10 * w + (10 + 8)], BG, "十分外側は背景のまま");
    }

    #[test]
    fn fill_circle_f_supports_fractional_radius_without_rounding() {
        // A round join needs to exactly match a line's half-thickness
        // (which can be fractional), so this separately verifies the
        // version that doesn't round the radius to an integer.
        let (w, h) = (20, 20);
        let mut buf = vec![BG; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            canvas.fill_circle_f(10.0, 10.0, 1.5, FG);
        }
        assert_eq!(buf[10 * w + 10], FG, "中心は完全に前景色");
        // At distance 1.0 (inside radius 1.5), should be full foreground color.
        assert_eq!(buf[10 * w + 11], FG);
        // At distance 3.0 (well outside radius 1.5), stays background.
        assert_eq!(buf[10 * w + 13], BG);
    }

    #[test]
    fn fill_circle_f_clipped_leaves_pixels_outside_clip_untouched() {
        // Verifies that when a hotkey row's reset icon (a round join)
        // reaches the viewport boundary while scrolling, the circle isn't
        // drawn spilling past `clip` (the unclipped version draws
        // unconditionally, so without this the drawing would leak into button rows outside the boundary).
        let (w, h) = (20, 20);
        let mut buf = vec![BG; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            let clip = Rect {
                x0: 0,
                y0: 0,
                x1: 20,
                y1: 10,
            };
            // Center (10,10), radius 3: a circle extending just past clip (y1=10).
            canvas.fill_circle_f_clipped(10.0, 10.0, 3.0, FG, clip);
        }
        assert_eq!(buf[9 * w + 10], FG, "clip 内側（中心付近）は塗られる");
        assert_eq!(buf[12 * w + 10], BG, "clip の外側は塗られないまま");
    }

    #[test]
    fn thick_line_center_is_full_color_edge_is_blended_and_outside_is_background() {
        let (w, h) = (20, 25);
        let mut buf = vec![BG; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            canvas.line(5, 10, 15, 10, 4, FG); // thickness=4 -> half-thickness 2.0
        }
        assert_eq!(buf[10 * w + 10], FG, "中心線上は完全に前景色");
        let edge = buf[12 * w + 10]; // the boundary at exactly the half-thickness
        assert_ne!(edge, BG, "境界はハードな背景色のままにならない");
        assert_ne!(edge, FG, "境界はハードな前景色にもならない");
        assert_eq!(buf[13 * w + 10], BG, "十分外側は背景のまま");
    }

    #[test]
    fn rgba_to_packed_drops_alpha() {
        // 2px: red (opaque) and green (semi-transparent).
        let rgba = [0xE0, 0x30, 0x30, 0xFF, 0x33, 0xA8, 0x52, 0x80];
        assert_eq!(rgba_to_packed(2, 1, &rgba), vec![0x00E0_3030, 0x0033_A852]);
    }

    #[test]
    fn rgba_to_packed_alpha_preserves_alpha_byte() {
        // 2px: red (opaque) and green (semi-transparent). Unlike
        // rgba_to_packed, alpha is kept as-is in the top byte.
        let rgba = [0xE0, 0x30, 0x30, 0xFF, 0x33, 0xA8, 0x52, 0x80];
        assert_eq!(
            rgba_to_packed_alpha(2, 1, &rgba),
            vec![0xFFE0_3030, 0x8033_A852]
        );
    }

    #[test]
    fn packed_to_rgba_alpha_round_trips_including_transparency() {
        let packed = vec![0xFFE0_3030u32, 0x8033_A852, 0x0000_0000];
        let rgba = packed_to_rgba_alpha(3, 1, &packed);
        assert_eq!(
            rgba,
            vec![0xE0, 0x30, 0x30, 0xFF, 0x33, 0xA8, 0x52, 0x80, 0, 0, 0, 0]
        );
        assert_eq!(
            rgba_to_packed_alpha(3, 1, &rgba),
            packed,
            "往復して一致するはず"
        );
    }

    #[test]
    fn blit_scaled_nearest_neighbor_fills_destination_rect() {
        // Scales a 2x1 packed-pixel image (left=red, right=green) up into a 4x2 rect.
        let src = vec![0x00FF_0000u32, 0x0000_FF00];
        let (w, h) = (6, 4);
        let mut buf = vec![0u32; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            let dst = Rect {
                x0: 1,
                y0: 1,
                x1: 5,
                y1: 3,
            };
            canvas.blit_scaled(dst, 2, 1, &src);
        }
        // The left half should be red, the right half green.
        assert_eq!(buf[w + 1], 0x00FF_0000);
        assert_eq!(buf[w + 2], 0x00FF_0000);
        assert_eq!(buf[w + 3], 0x0000_FF00);
        assert_eq!(buf[w + 4], 0x0000_FF00);
        // Nothing outside the rect is touched.
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn blit_scaled_alpha_blends_by_source_alpha_and_leaves_fully_transparent_pixels_untouched() {
        // 3 pixels: opaque red, 50%-transparent green, and fully transparent.
        let src = vec![0xFFFF_0000u32, 0x8000_FF00, 0x0000_00FF];
        let (w, h) = (3, 1);
        let mut buf = vec![0x00AA_AAAAu32; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            let dst = Rect {
                x0: 0,
                y0: 0,
                x1: 3,
                y1: 1,
            };
            canvas.blit_scaled_alpha(dst, 3, 1, &src);
        }
        // The opaque pixel overwrites outright.
        assert_eq!(buf[0], 0x00FF_0000);
        // The semi-transparent pixel mixes with the background
        // (0xAAAAAA) — a value roughly between both components.
        let g = buf[1];
        let gr = (g >> 16) & 0xff;
        let gg = (g >> 8) & 0xff;
        assert!(gr > 0 && gr < 0xAA, "背景よりは暗いが0ではない: {gr:#x}");
        assert!(
            gg > 0 && gg < 0xFF,
            "背景よりは明るいが緑単色ではない: {gg:#x}"
        );
        // Fully transparent (alpha 0) leaves the background untouched.
        assert_eq!(buf[2], 0x00AA_AAAA);
    }

    #[test]
    fn hsv_rgb_round_trip() {
        for &c in &[
            0x00E0_3030u32,
            0x0033_A852,
            0x004D_A6FF,
            0x00FF_FFFF,
            0x0000_0000,
        ] {
            let (h, s, v) = rgb_to_hsv(c);
            let back = hsv_to_rgb(h, s, v);
            let diff = |sh: u32| (((c >> sh) & 0xff) as i32 - ((back >> sh) & 0xff) as i32).abs();
            assert!(
                diff(16) <= 1 && diff(8) <= 1 && diff(0) <= 1,
                "{c:06X} != {back:06X}"
            );
        }
    }
}
