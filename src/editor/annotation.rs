//! Annotation items (`Annotation`) and the pure geometry/hit-testing/drawing
//! logic scoped to them. Only self-contained functions that don't depend on
//! `Editor`'s own state live here.

use std::rc::Rc;

use rayon::prelude::*;

use crate::ui::Canvas;
use crate::ui::blend_pixel;
use crate::ui::text::TextRenderer;

use super::{EXPORT_SENTINEL, HANDLE_HALF, SEL_COLOR};

/// Export-guide border color and thickness (world px).
const GUIDE_COLOR: u32 = 0x0000_0000;
const GUIDE_THICK: i64 = 3;

/// A rect's 8 handles: 4 corners plus 4 edge midpoints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// Mosaic's visual mode: `Pixelate` is coarse block pixelation, `Blur` is a
/// blur with random jitter mixed in (see `paint_mosaic`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum MosaicMode {
    Pixelate,
    Blur,
}

/// One item (all coordinates are absolute world coordinates; the base
/// image itself is an Image item anchored at the world origin). Each item
/// carries its own color and thickness (text uses size instead).
#[derive(Clone)]
pub(super) enum Annotation {
    /// An image item (screenshot, clipboard paste, drag-and-drop). `r` is
    /// the display rect (world coordinates, two opposite corners, kept as
    /// floats like Rect/Ellipse to avoid jitter). `src_w`/`src_h` are the
    /// source pixel dimensions. `pixels` are the source pixels
    /// (0xAARRGGBB, top byte is real alpha; `paint_image` alpha-blends
    /// them onto the background to preserve transparency). `Rc`-shared so
    /// cloning is cheap.
    Image {
        r: (f64, f64, f64, f64),
        src_w: i64,
        src_h: i64,
        pixels: Rc<Vec<u32>>,
        /// Rotation angle around the center, in radians. 0 = unrotated.
        rot: f64,
    },
    Arrow {
        a: (i64, i64),
        b: (i64, i64),
        color: u32,
        thick: i64,
    },
    /// A connected line joining points in order. Like Arrow, has no `rot`
    /// (no individual rotate handle; a multi-select group rotation instead
    /// rotates every point).
    Polyline {
        points: Vec<(i64, i64)>,
        color: u32,
        thick: i64,
    },
    /// A numbered round badge placed with a single click. `number` is
    /// auto-assigned (`next_marker_number` returns the existing items'
    /// max plus one). Always drawn upright regardless of rotation, so
    /// like Arrow/Polyline it has no `rot` (a group rotation only
    /// rotates `pos`). `size` is the radius, in world px.
    NumberMarker {
        pos: (i64, i64),
        number: u32,
        color: u32,
        size: f32,
    },
    Rect {
        /// Kept as floats, unlike other shapes, because this one rotates —
        /// rounding to integers only happens at the final moment of
        /// drawing, which structurally eliminates the 1px jitter that
        /// would otherwise appear on the opposite handle while
        /// rotate-dragging.
        r: (f64, f64, f64, f64),
        color: u32,
        thick: i64,
        /// Rotation angle around the center, in radians. 0 = unrotated.
        rot: f64,
        /// If true, fills the interior with `color` (`thick` is ignored).
        filled: bool,
    },
    /// A mosaic that obscures what's beneath it. Its shape is identical to
    /// Rect (a rotatable, resizable rect), but it has no color, thickness,
    /// or fill — instead of its own color, it paints a processed version
    /// of whatever is already drawn at that position (source image plus
    /// items below it). `mode` switches between Pixelate (coarse block
    /// pixelation) and Blur (blur with random jitter mixed in). `block` is
    /// the block side length for Pixelate, or the blur radius for Blur
    /// (both in world px). `seed` is Blur's PRNG seed, assigned once when
    /// the item is created (`random_seed`) and kept as-is through
    /// save/restore — since re-randomizing every frame would make the
    /// image flicker, it's instead derived deterministically from
    /// `(seed, x, y, ...)` (`mosaic_hash`).
    Mosaic {
        r: (f64, f64, f64, f64),
        /// Rotation angle around the center, in radians. 0 = unrotated.
        rot: f64,
        block: f32,
        mode: MosaicMode,
        seed: u32,
    },
    /// An ellipse inscribed in `r`; make `r` a square for a circle.
    Ellipse {
        /// Kept as floats for the same jitter-avoidance reason as Rect —
        /// rounding to integers only happens at the final moment of drawing.
        r: (f64, f64, f64, f64),
        color: u32,
        thick: i64,
        /// Rotation angle around the center, in radians. 0 = unrotated.
        rot: f64,
        /// If true, fills the interior with `color` (`thick` is ignored).
        filled: bool,
    },
    /// `pos` is kept as the pre-rotation local (top-left) coordinate.
    /// Unlike Rect/Ellipse/Image's `r`, there's no explicit rect, since a
    /// string's width depends on the font and content and changes
    /// whenever either does; it's computed on demand via `text_local_rect`.
    Text {
        pos: (i64, i64),
        text: String,
        color: u32,
        size: f32,
        /// Rotation angle around the center, in radians. 0 = unrotated.
        rot: f64,
    },
    /// The export-bounds guide (at most one can exist). An editor-only
    /// border that never appears in the exported image; while present, it
    /// defines the export bounds.
    Guide { r: (i64, i64, i64, i64) },
}

/// The world-to-screen camera. `scale` is zoom; `(ox, oy)` is the world
/// origin's screen position. (Set up so a future infinite-canvas mode only
/// needs pan/zoom to move these two values.)
pub(super) struct Xform {
    pub(super) scale: f64,
    pub(super) ox: f64,
    pub(super) oy: f64,
}

impl Xform {
    pub(super) fn map(&self, p: (i64, i64)) -> (i64, i64) {
        (
            (self.ox + p.0 as f64 * self.scale) as i64,
            (self.oy + p.1 as f64 * self.scale) as i64,
        )
    }
    /// Converts an item's thickness (world px) to display scale.
    pub(super) fn thick(&self, base: i64) -> i64 {
        (base as f64 * self.scale).round().max(1.0) as i64
    }
    /// Converts an item's text size (world px) to display scale.
    pub(super) fn text_size(&self, base: f32) -> f32 {
        (base * self.scale as f32).max(8.0)
    }
}

/// Draws every annotation plus the drag preview, if any (the caret for
/// text being edited is drawn by the caller). When `export` is true (baking
/// in for export), the guide isn't drawn — it's editor-only and never
/// appears in the exported image.
pub(super) fn paint_annotations(
    canvas: &mut Canvas,
    annotations: &[Annotation],
    t: &Xform,
    text: Option<&TextRenderer>,
    preview: Option<Annotation>,
    export: bool,
) {
    // The guide marks the export bounds, so it's always drawn last (on
    // top) regardless of its position in the array — that way changing
    // draw order with PageUp/PageDown never hides it behind other items.
    let is_guide = |ann: &Annotation| matches!(ann, Annotation::Guide { .. });
    for ann in annotations.iter().filter(|a| !is_guide(a)) {
        paint_one(canvas, ann, t, text, export);
    }
    if let Some(ann) = preview.as_ref().filter(|a| !is_guide(a)) {
        paint_one(canvas, ann, t, text, export);
    }
    for ann in annotations.iter().filter(|a| is_guide(a)) {
        paint_one(canvas, ann, t, text, export);
    }
    if let Some(ann) = preview.as_ref().filter(|a| is_guide(a)) {
        paint_one(canvas, ann, t, text, export);
    }
}

fn paint_one(
    canvas: &mut Canvas,
    ann: &Annotation,
    t: &Xform,
    text: Option<&TextRenderer>,
    export: bool,
) {
    match ann {
        Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot,
        } => paint_image(canvas, *r, *rot, *src_w, *src_h, pixels, t),
        Annotation::Arrow { a, b, color, thick } => {
            let th = t.thick(*thick);
            draw_arrow(canvas, t.map(*a), t.map(*b), th, *color);
        }
        Annotation::Polyline {
            points,
            color,
            thick,
        } => {
            let th = t.thick(*thick);
            let mapped: Vec<(i64, i64)> = points.iter().map(|&p| t.map(p)).collect();
            draw_polyline(canvas, &mapped, th, *color);
        }
        Annotation::Mosaic {
            r,
            rot,
            block,
            mode,
            seed,
        } => paint_mosaic(canvas, *r, *rot, *block, *mode, *seed, t),
        Annotation::Rect {
            r,
            color,
            thick,
            rot,
            filled,
        } => {
            if *filled {
                let (x0, y0, x1, y1) = rect_norm_f64(*r);
                let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                let (hw, hh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
                // t.map assumes i64, so bypass it and apply scale/offset
                // directly in float, without any extra rounding.
                let (scx, scy) = (t.ox + cx * t.scale, t.oy + cy * t.scale);
                let (shw, shh) = (hw * t.scale, hh * t.scale);
                fill_rotated_rect(canvas, scx, scy, shw, shh, *rot, *color);
            } else {
                let th = t.thick(*thick);
                let corners = rotated_rect_corners(*r, *rot).map(|p| t.map(p));
                for i in 0..4 {
                    let (x0, y0) = corners[i];
                    let (x1, y1) = corners[(i + 1) % 4];
                    canvas.line(x0, y0, x1, y1, th, *color);
                    round_join(canvas, (x0, y0), th, *color);
                }
            }
        }
        Annotation::Ellipse {
            r,
            color,
            thick,
            rot,
            filled,
        } => {
            let (x0, y0, x1, y1) = rect_norm_f64(*r);
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
            // t.map assumes i64, so bypass it and apply scale/offset
            // directly in float, without any extra rounding.
            let (scx, scy) = (t.ox + cx * t.scale, t.oy + cy * t.scale);
            let (srx, sry) = (rx * t.scale, ry * t.scale);
            if *filled {
                fill_rotated_ellipse(canvas, scx, scy, srx, sry, *rot, *color);
            } else {
                let th = t.thick(*thick);
                draw_ellipse(canvas, scx, scy, srx, sry, *rot, th, *color);
            }
        }
        Annotation::Text {
            pos,
            text: s,
            color,
            size,
            rot,
        } => {
            if let Some(tr) = text {
                if rot.abs() < 1e-9 {
                    let (x, y) = t.map(*pos);
                    let screen_size = t.text_size(*size);
                    tr.draw(
                        canvas,
                        x as f32,
                        y as f32 + screen_size,
                        s,
                        screen_size,
                        *color,
                    );
                } else {
                    paint_text_rotated(canvas, *pos, s, *color, *size, *rot, tr, t);
                }
            }
        }
        Annotation::NumberMarker {
            pos,
            number,
            color,
            size,
        } => {
            let (cx, cy) = t.map(*pos);
            let radius = (*size as f64 * t.scale).max(6.0);
            canvas.fill_circle_f(cx as f64, cy as f64, radius, *color);
            if let Some(tr) = text {
                let label = number.to_string();
                let fsize = (radius * 1.1) as f32;
                let w = tr.text_width(&label, fsize);
                let baseline = tr.baseline_for_center(cy as f32, fsize);
                let text_color = contrast_text_color(*color);
                tr.draw(
                    canvas,
                    cx as f32 - w / 2.0,
                    baseline,
                    &label,
                    fsize,
                    text_color,
                );
            }
        }
        // Not drawn on export (editor-only guide).
        Annotation::Guide { r } => {
            if !export {
                let th = t.thick(GUIDE_THICK);
                // canvas.line draws with a square brush of ±(th/2) around
                // the center, so offset the center outward by brush_r+1 to
                // make the inner edge land exactly on the boundary (no gap
                // or overhang).
                let brush_r = th.max(1) / 2;
                let half = brush_r + 1;
                let (x0, y0) = t.map((r.0, r.1));
                let (x1, y1) = t.map((r.2, r.3));
                // Offset outside the boundary so it doesn't encroach on the
                // (interior) export bounds.
                let (ox0, oy0) = (x0 - half, y0 - half);
                let (ox1, oy1) = (x1 + half, y1 + half);
                // Zoom can put coordinates far outside the canvas. canvas.line
                // still counts steps for the out-of-range portion (set_i just
                // discards the write, without cutting the iteration short),
                // so clamp each axis a line extends along to the canvas
                // bounds (plus margin) to bound the iteration count. The
                // pixels actually drawn are unaffected.
                let (cw, ch) = (canvas.w as i64, canvas.h as i64);
                let margin = th + 2;
                let cx0 = ox0.clamp(-margin, cw + margin);
                let cx1 = ox1.clamp(-margin, cw + margin);
                let cy0 = oy0.clamp(-margin, ch + margin);
                let cy1 = oy1.clamp(-margin, ch + margin);
                canvas.line(cx0, oy0, cx1, oy0, th, GUIDE_COLOR);
                canvas.line(cx0, oy1, cx1, oy1, th, GUIDE_COLOR);
                canvas.line(ox0, cy0, ox0, cy1, th, GUIDE_COLOR);
                canvas.line(ox1, cy0, ox1, cy1, th, GUIDE_COLOR);
                // Fill the 4 corner joints (even when clamped, drawing at
                // the true corner coordinates naturally does nothing if
                // it's off-screen).
                for corner in [(ox0, oy0), (ox1, oy0), (ox1, oy1), (ox0, oy1)] {
                    round_join(canvas, corner, th, GUIDE_COLOR);
                }
            }
        }
    }
}

/// Processes a run of consecutive scanlines (`region`, `w` columns each)
/// for the unrotated `paint_image` fast path. `y_base` is the first row's
/// screen y — `region` isn't necessarily the whole canvas (under
/// parallelism it's the slice a given thread owns), so the actual screen y
/// is recomputed from this. A shared helper called from both the
/// single-threaded and parallel paths.
#[allow(clippy::too_many_arguments)]
fn paint_image_rows(
    region: &mut [u32],
    w: usize,
    y_base: i64,
    scx: f64,
    scy: f64,
    shw: f64,
    shh: f64,
    inv_2shw: f64,
    inv_2shh: f64,
    src_wf: f64,
    src_hf: f64,
    sw: usize,
    sh: usize,
    x0c: i64,
    x1c: i64,
    pixels: &[u32],
) {
    let rows = region.len() / w;
    for local_y in 0..rows {
        let y = y_base + local_y as i64;
        let uy = y as f64 - scy;
        if uy < -shh || uy > shh {
            continue;
        }
        let ny = (uy + shh) * inv_2shh;
        let sy = ((ny * src_hf).round() as i64).clamp(0, sh as i64 - 1) as usize;
        let src_row = sy * sw;
        let out_row = &mut region[local_y * w..(local_y + 1) * w];
        for x in x0c..x1c {
            let ux = x as f64 - scx;
            if ux < -shw || ux > shw {
                continue;
            }
            let nx = (ux + shw) * inv_2shw;
            let sx = ((nx * src_wf).round() as i64).clamp(0, sw as i64 - 1) as usize;
            // The pixel's top byte is real alpha (0xAARRGGBB). Blend it as
            // coverage rather than overwriting, so transparent areas show
            // the background through.
            let p = pixels[src_row + sx];
            let alpha = ((p >> 24) & 0xff) as f32 / 255.0;
            blend_pixel(&mut out_row[x as usize], p & 0x00FF_FFFF, alpha);
        }
    }
}

/// The rotated counterpart of `paint_image_rows`. Heavier since `sy`/row
/// offset can't be reused per row (inverse rotation mixes `x`/`y`), but
/// each row is still independent, so it's just as parallelizable.
#[allow(clippy::too_many_arguments)]
fn paint_image_rows_rotated(
    region: &mut [u32],
    w: usize,
    y_base: i64,
    scx: f64,
    scy: f64,
    shw: f64,
    shh: f64,
    inv_2shw: f64,
    inv_2shh: f64,
    src_wf: f64,
    src_hf: f64,
    sw: usize,
    sh: usize,
    x0c: i64,
    x1c: i64,
    rsin: f64,
    rcos: f64,
    pixels: &[u32],
) {
    let rows = region.len() / w;
    for local_y in 0..rows {
        let y = y_base + local_y as i64;
        let out_row = &mut region[local_y * w..(local_y + 1) * w];
        for x in x0c..x1c {
            // Inverse-rotate back to the "as if unrotated" position (the
            // same formula as `rotate_point((x, y), (scx, scy), -rot)`,
            // expanded directly using the sin/cos the caller already computed).
            let (dx, dy) = (x as f64 - scx, y as f64 - scy);
            let ux = dx * rcos + dy * rsin;
            let uy = -dx * rsin + dy * rcos;
            if ux < -shw || ux > shw || uy < -shh || uy > shh {
                continue;
            }
            let nx = (ux + shw) * inv_2shw;
            let ny = (uy + shh) * inv_2shh;
            let sx = ((nx * src_wf).round() as i64).clamp(0, sw as i64 - 1) as usize;
            let sy = ((ny * src_hf).round() as i64).clamp(0, sh as i64 - 1) as usize;
            let p = pixels[sy * sw + sx];
            let alpha = ((p >> 24) & 0xff) as f32 / 255.0;
            blend_pixel(&mut out_row[x as usize], p & 0x00FF_FFFF, alpha);
        }
    }
}

/// Draws an image item at display scale via nearest-neighbor sampling.
#[allow(clippy::too_many_arguments)]
/// Draws the image with `r` (world coordinates, two opposite corners)
/// rotated by `rot` around its center, via nearest-neighbor sampling. Only
/// the rotated AABB on screen is scanned (the same closed-form used for
/// Ellipse's bbox); each pixel is inverse-rotated by `-rot` to test
/// whether it falls within the local (unrotated) rect, sampling the source
/// pixel by nearest-neighbor if so. The nearest index is computed with
/// `round()` — truncating via `as usize` instead produced many cases
/// (especially at 100% zoom) where a value that should be exactly an
/// integer came out as, say, 14.999999999999998 due to float error,
/// picking a pixel one column/row off. That was the cause of images
/// looking slightly blurred even at 100% zoom.
fn paint_image(
    canvas: &mut Canvas,
    r: (f64, f64, f64, f64),
    rot: f64,
    src_w: i64,
    src_h: i64,
    pixels: &[u32],
    t: &Xform,
) {
    if src_w <= 0 || src_h <= 0 {
        return;
    }
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (halfw, halfh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    if halfw <= 0.0 || halfh <= 0.0 {
        return;
    }
    // t.map assumes i64, so bypass it and apply scale/offset directly in
    // float (same reasoning as Ellipse in paint_one — avoids extra rounding).
    let (scx, scy) = (t.ox + cx * t.scale, t.oy + cy * t.scale);
    let (shw, shh) = (halfw * t.scale, halfh * t.scale);
    if shw <= 0.0 || shh <= 0.0 {
        return;
    }

    // Only process what's actually visible on screen. Zoom can make the
    // rect far larger than the canvas, so without clipping, every frame
    // during panning would be very slow.
    // The rotated AABB of a rect (not an ellipse) uses the "sum of
    // absolute values" formula — sqrt(sum of squares) is the ellipse
    // formula, and using it here would round off the corners and
    // underestimate the bounds, clipping off the rotated corners.
    // sin/cos of -rot are computed once here (`rotate_point` recomputes
    // sin_cos on every call, which was noticeably slowing down every frame
    // while panning or using other tools with a large image, e.g. the
    // screenshot itself, shown fullscreen).
    let (rsin, rcos) = rot.sin_cos();
    let hw = shw * rcos.abs() + shh * rsin.abs();
    let hh = shw * rsin.abs() + shh * rcos.abs();
    let (cw, ch) = (canvas.w as i64, canvas.h as i64);
    let x0c = ((scx - hw).floor() as i64).max(0);
    let x1c = ((scx + hw).ceil() as i64).min(cw);
    let y0c = ((scy - hh).floor() as i64).max(0);
    let y1c = ((scy + hh).ceil() as i64).min(ch);
    if x0c >= x1c || y0c >= y1c {
        return;
    }

    let (sw, sh) = (src_w as usize, src_h as usize);
    // Division is much heavier than multiplication, so compute the
    // reciprocals once outside the loop.
    let (inv_2shw, inv_2shh) = (1.0 / (2.0 * shw), 1.0 / (2.0 * shh));
    let (src_wf, src_hf) = (src_w as f64, src_h as f64);

    // For an image large enough to cover much of the screen (e.g. the
    // screenshot itself while panning/rotating), split the independent
    // scanlines across CPU cores to cut per-frame processing time. Uses
    // rayon's persistent worker pool rather than `std::thread::scope`,
    // since spawning fresh OS threads every frame would offset the gains
    // from parallelizing at all. Only parallelized when the image covers
    // enough of the screen, so small images don't pay the overhead
    // (applies whether or not it's rotated, so rotated images specifically
    // don't stay slow).
    let w = canvas.w;
    let region = &mut canvas.buf[(y0c as usize * w)..(y1c as usize * w)];
    let total_rows = region.len() / w;
    const PAR_PIXEL_THRESHOLD: i64 = 200_000;
    let n_threads = rayon::current_num_threads().min(8);
    let parallel = n_threads > 1 && (x1c - x0c) * (y1c - y0c) >= PAR_PIXEL_THRESHOLD;
    let rows_per_chunk = total_rows.div_ceil(n_threads.max(1));

    if rot == 0.0 {
        // Unrotated (the common case, e.g. the screenshot itself) skips
        // the per-pixel inverse rotation entirely, and only needs the
        // source row (sy/row) computed once per scanline.
        if parallel {
            region
                .par_chunks_mut(rows_per_chunk * w)
                .enumerate()
                .for_each(|(chunk_idx, chunk)| {
                    let y_base = y0c + (chunk_idx * rows_per_chunk) as i64;
                    paint_image_rows(
                        chunk, w, y_base, scx, scy, shw, shh, inv_2shw, inv_2shh, src_wf, src_hf,
                        sw, sh, x0c, x1c, pixels,
                    );
                });
        } else {
            paint_image_rows(
                region, w, y0c, scx, scy, shw, shh, inv_2shw, inv_2shh, src_wf, src_hf, sw, sh,
                x0c, x1c, pixels,
            );
        }
        return;
    }

    if parallel {
        region
            .par_chunks_mut(rows_per_chunk * w)
            .enumerate()
            .for_each(|(chunk_idx, chunk)| {
                let y_base = y0c + (chunk_idx * rows_per_chunk) as i64;
                paint_image_rows_rotated(
                    chunk, w, y_base, scx, scy, shw, shh, inv_2shw, inv_2shh, src_wf, src_hf, sw,
                    sh, x0c, x1c, rsin, rcos, pixels,
                );
            });
    } else {
        paint_image_rows_rotated(
            region, w, y0c, scx, scy, shw, shh, inv_2shw, inv_2shh, src_wf, src_hf, sw, sh, x0c,
            x1c, rsin, rcos, pixels,
        );
    }
}

/// Assigns Blur's PRNG seed, called once when an item is created. Avoids
/// pulling in `rand` or similar — mixes `RandomState` (the standard
/// library's idiomatic way to draw OS-backed randomness) with a monotonic
/// counter and the low bits of an `Instant`, so consecutive calls within
/// the same process rarely collide.
pub(super) fn random_seed() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let base = RandomState::new().build_hasher().finish() as u32;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let time = std::time::Instant::now().elapsed().subsec_nanos();
    base ^ counter.wrapping_mul(0x9E37_79B1) ^ time
}

/// A custom mixer (splitmix64-style) that derives a deterministic 64-bit
/// value from `(seed, x, y, k)`. Same input always gives the same output
/// (so redrawing doesn't flicker), but the distribution looks random as
/// inputs vary — used as Blur's deterministic pseudo-random source for
/// sample positions/noise, avoiding a fixed kernel.
fn mosaic_hash(seed: u32, x: i64, y: i64, k: u32) -> u64 {
    let mut h = seed as u64;
    h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(x as u64);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9).wrapping_add(y as u64);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB).wrapping_add(k as u64);
    h ^= h >> 31;
    h
}

/// Cap on Blur's Gaussian kernel radius (screen px). The separable 2-pass
/// convolution's cost scales with the radius, so this bounds it to keep
/// processing time from blowing up at extreme block values (3σ only
/// exceeds this at a very large block).
const BLUR_KERNEL_RADIUS_MAX: usize = 128;

/// Draws a mosaic (Pixelate = coarse block pixelation, Blur = a blur with
/// random jitter mixed in). Only within the rect, it reprocesses whatever
/// is already drawn at that position (source image plus items below) and
/// overwrites it **opaquely** — unlike `paint_image`, no alpha blending,
/// since this is meant to hide content and shouldn't let the original
/// pixels show through at the edges. Since the read source and write
/// destination are the same `canvas.buf`, the target area is snapshotted
/// before reading it (otherwise a neighboring pixel would resample an
/// already-processed color).
fn paint_mosaic(
    canvas: &mut Canvas,
    r: (f64, f64, f64, f64),
    rot: f64,
    block: f32,
    mode: MosaicMode,
    seed: u32,
    t: &Xform,
) {
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (halfw, halfh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    if halfw <= 0.0 || halfh <= 0.0 {
        return;
    }
    // t.map assumes i64, so bypass it and apply scale/offset directly in
    // float (same reasoning as `paint_image`/Rect's `paint_one`).
    let (scx, scy) = (t.ox + cx * t.scale, t.oy + cy * t.scale);
    let (shw, shh) = (halfw * t.scale, halfh * t.scale);
    if shw <= 0.0 || shh <= 0.0 {
        return;
    }

    let (rsin, rcos) = rot.sin_cos();
    let hw = shw * rcos.abs() + shh * rsin.abs();
    let hh = shw * rsin.abs() + shh * rcos.abs();
    let (cw, ch) = (canvas.w as i64, canvas.h as i64);
    let x0c = ((scx - hw).floor() as i64).max(0);
    let x1c = ((scx + hw).ceil() as i64).min(cw);
    let y0c = ((scy - hh).floor() as i64).max(0);
    let y1c = ((scy + hh).ceil() as i64).min(ch);
    if x0c >= x1c || y0c >= y1c {
        return;
    }

    let w = canvas.w;
    let bw = (x1c - x0c) as usize;
    let bh = (y1c - y0c) as usize;
    let mut snapshot = vec![0u32; bw * bh];
    for row in 0..bh {
        let src_start = (y0c as usize + row) * w + x0c as usize;
        snapshot[row * bw..(row + 1) * bw].copy_from_slice(&canvas.buf[src_start..src_start + bw]);
    }
    let sample = |sx: i64, sy: i64| -> u32 {
        let sx = (sx.clamp(x0c, x1c - 1) - x0c) as usize;
        let sy = (sy.clamp(y0c, y1c - 1) - y0c) as usize;
        snapshot[sy * bw + sx]
    };

    // Convert block size to screen scale (converting world px to screen px
    // before snapping, so the block's apparent coarseness doesn't change
    // with zoom). Clamped to at least 1 screen px if zoomed out enough to
    // go below it (same idea as `t.thick`).
    let block = ((block as f64).max(1.0) * t.scale).max(1.0);

    match mode {
        MosaicMode::Pixelate => {
            for y in y0c..y1c {
                for x in x0c..x1c {
                    // Inverse-rotate back to the "as if unrotated" position
                    // (same formula as `paint_image_rows_rotated`).
                    let (dx, dy) = (x as f64 - scx, y as f64 - scy);
                    let ux = dx * rcos + dy * rsin;
                    let uy = -dx * rsin + dy * rcos;
                    if ux < -shw || ux > shw || uy < -shh || uy > shh {
                        continue;
                    }
                    // Snap the local (pre-rotation, screen-unit) coordinate
                    // to a block boundary, and use that block's center as
                    // the sample point (every pixel in the same block ends
                    // up the same color — the pixelation effect).
                    let bx = (ux / block).floor() * block + block / 2.0;
                    let by = (uy / block).floor() * block + block / 2.0;
                    // Rotate forward to get back to screen coordinates.
                    let sample_dx = bx * rcos - by * rsin;
                    let sample_dy = bx * rsin + by * rcos;
                    let sx = (scx + sample_dx).round() as i64;
                    let sy = (scy + sample_dy).round() as i64;
                    canvas.buf[y as usize * w + x as usize] = sample(sx, sy);
                }
            }
        }
        MosaicMode::Blur => {
            // Drawn in two stages, designed to resist deconvolution:
            //   1) Scramble: replace each pixel in the rect with a nearby
            //      pixel (within a radius-σ disc), chosen by a
            //      seed-derived pseudo-random offset.
            //   2) Gaussian blur: apply a normal separable Gaussian
            //      convolution on top (horizontal pass, then vertical).
            // Since the final stage is a genuine Gaussian convolution, the
            // output looks fully smooth like an ordinary blur — no
            // grainy noise from the randomization remains. But even a
            // perfect deconvolution of stage 2 can only recover the image
            // as scrambled in stage 1, not the original content —
            // scrambling before blurring is the whole point.
            let sigma = block / 2.0;

            // 1) Scramble. Only inside the rect is scrambled; outside is
            //    left untouched, so the blur near the edge blends
            //    naturally with the real surrounding pixels.
            let mut scrambled = snapshot.clone();
            for y in y0c..y1c {
                for x in x0c..x1c {
                    let (dx, dy) = (x as f64 - scx, y as f64 - scy);
                    let ux = dx * rcos + dy * rsin;
                    let uy = -dx * rsin + dy * rcos;
                    if ux < -shw || ux > shw || uy < -shh || uy > shh {
                        continue;
                    }
                    let h = mosaic_hash(seed, x, y, 0);
                    let angle = (h as u32) as f64 / u32::MAX as f64 * std::f64::consts::TAU;
                    let frac = (h >> 32) as u32 as f64 / u32::MAX as f64;
                    // sqrt gives a uniform distribution over the disc (not biased toward the center).
                    let dist = frac.sqrt() * sigma;
                    let sx = (x as f64 + angle.cos() * dist).round() as i64;
                    let sy = (y as f64 + angle.sin() * dist).round() as i64;
                    scrambled[(y - y0c) as usize * bw + (x - x0c) as usize] = sample(sx, sy);
                }
            }

            // 2) Separable Gaussian convolution over the whole bbox
            //    (horizontal pass, then vertical). Kernel weights are
            //    precomputed once.
            let radius = ((sigma * 3.0).ceil() as usize).clamp(1, BLUR_KERNEL_RADIUS_MAX);
            let denom = 2.0 * sigma * sigma;
            let mut weights: Vec<f64> = (-(radius as i64)..=radius as i64)
                .map(|i| (-((i * i) as f64) / denom).exp())
                .collect();
            let wsum: f64 = weights.iter().sum();
            for wgt in &mut weights {
                *wgt /= wsum;
            }

            // Horizontal pass: scrambled -> hpass. Rows are independent, so
            // split across cores (same reasoning as `paint_image` — keeps
            // every frame fast while dragging).
            let mut hpass = vec![0u32; bw * bh];
            hpass.par_chunks_mut(bw).enumerate().for_each(|(row, out)| {
                let src = &scrambled[row * bw..(row + 1) * bw];
                for (bx, slot) in out.iter_mut().enumerate() {
                    let (mut sr, mut sg, mut sb) = (0.0f64, 0.0f64, 0.0f64);
                    for (ki, wgt) in weights.iter().enumerate() {
                        let sx = (bx as i64 + ki as i64 - radius as i64).clamp(0, bw as i64 - 1)
                            as usize;
                        let c = src[sx];
                        sr += wgt * ((c >> 16) & 0xff) as f64;
                        sg += wgt * ((c >> 8) & 0xff) as f64;
                        sb += wgt * (c & 0xff) as f64;
                    }
                    let rr = sr.round().clamp(0.0, 255.0) as u32;
                    let gg = sg.round().clamp(0.0, 255.0) as u32;
                    let bb = sb.round().clamp(0.0, 255.0) as u32;
                    *slot = (rr << 16) | (gg << 8) | bb;
                }
            });

            // Vertical pass: hpass -> canvas (writes opaquely, only inside the rect).
            let region = &mut canvas.buf[(y0c as usize * w)..(y1c as usize * w)];
            region.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
                let y = y0c + row as i64;
                for x in x0c..x1c {
                    let (dx, dy) = (x as f64 - scx, y as f64 - scy);
                    let ux = dx * rcos + dy * rsin;
                    let uy = -dx * rsin + dy * rcos;
                    if ux < -shw || ux > shw || uy < -shh || uy > shh {
                        continue;
                    }
                    let bx = (x - x0c) as usize;
                    let (mut sr, mut sg, mut sb) = (0.0f64, 0.0f64, 0.0f64);
                    for (ki, wgt) in weights.iter().enumerate() {
                        let sy = (row as i64 + ki as i64 - radius as i64).clamp(0, bh as i64 - 1)
                            as usize;
                        let c = hpass[sy * bw + bx];
                        sr += wgt * ((c >> 16) & 0xff) as f64;
                        sg += wgt * ((c >> 8) & 0xff) as f64;
                        sb += wgt * (c & 0xff) as f64;
                    }
                    let rr = sr.round().clamp(0.0, 255.0) as u32;
                    let gg = sg.round().clamp(0.0, 255.0) as u32;
                    let bb = sb.round().clamp(0.0, 255.0) as u32;
                    out[x as usize] = (rr << 16) | (gg << 8) | bb;
                }
            });
        }
    }
}

/// Composites multiple `Annotation::Image`s, each at its current
/// position/rotation, into a single bitmap, returned as one
/// `Annotation::Image` (used to merge what were separate stroke items into
/// one when leaving/reselecting the Freehand tool). Uses the same
/// "inverse-rotate then nearest-neighbor sample" as `paint_image`, but
/// bakes into a new bitmap with alpha via Porter-Duff over-compositing
/// rather than onto the screen (`Canvas::blend_i` can't be used here since
/// it always zeroes the destination's alpha).
pub(super) fn merge_images(items: &[Annotation]) -> Annotation {
    let (mut ux0, mut uy0, mut ux1, mut uy1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for it in items {
        if let Annotation::Image { r, rot, .. } = it {
            let (x0, y0, x1, y1) = rotated_rect_bbox(*r, *rot);
            ux0 = ux0.min(x0);
            uy0 = uy0.min(y0);
            ux1 = ux1.max(x1);
            uy1 = uy1.max(y1);
        }
    }
    let w = (ux1 - ux0).max(1) as usize;
    let h = (uy1 - uy0).max(1) as usize;
    let mut out = vec![0u32; w * h];

    for it in items {
        let Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot,
        } = it
        else {
            continue;
        };
        if *src_w <= 0 || *src_h <= 0 {
            continue;
        }
        let (x0, y0, x1, y1) = rect_norm_f64(*r);
        let (cx, cy) = ((x0 + x1) / 2.0 - ux0 as f64, (y0 + y1) / 2.0 - uy0 as f64);
        let (halfw, halfh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        if halfw <= 0.0 || halfh <= 0.0 {
            continue;
        }
        let hw = halfw * rot.cos().abs() + halfh * rot.sin().abs();
        let hh = halfw * rot.sin().abs() + halfh * rot.cos().abs();
        let x0c = ((cx - hw).floor() as i64).max(0);
        let x1c = ((cx + hw).ceil() as i64).min(w as i64);
        let y0c = ((cy - hh).floor() as i64).max(0);
        let y1c = ((cy + hh).ceil() as i64).min(h as i64);
        let (sw, sh) = (*src_w as usize, *src_h as usize);
        for y in y0c..y1c {
            for x in x0c..x1c {
                let (lx, ly) = rotate_point((x as f64, y as f64), (cx, cy), -*rot);
                let (lux, luy) = (lx - cx, ly - cy);
                if lux < -halfw || lux > halfw || luy < -halfh || luy > halfh {
                    continue;
                }
                let nx = (lux + halfw) / (2.0 * halfw);
                let ny = (luy + halfh) / (2.0 * halfh);
                let sx = ((nx * *src_w as f64).round() as i64).clamp(0, sw as i64 - 1) as usize;
                let sy = ((ny * *src_h as f64).round() as i64).clamp(0, sh as i64 - 1) as usize;
                let src = pixels[sy * sw + sx];
                let idx = y as usize * w + x as usize;
                out[idx] = composite_over(out[idx], src);
            }
        }
    }

    Annotation::Image {
        r: (
            ux0 as f64,
            uy0 as f64,
            (ux0 + w as i64) as f64,
            (uy0 + h as i64) as f64,
        ),
        src_w: w as i64,
        src_h: h as i64,
        pixels: Rc::new(out),
        rot: 0.0,
    }
}

/// Porter-Duff "over" compositing (layers `src` on top of `dst`). Both are
/// `0xAARRGGBB` (top byte is real alpha).
fn composite_over(dst: u32, src: u32) -> u32 {
    let sa = ((src >> 24) & 0xff) as f32 / 255.0;
    if sa <= 0.0 {
        return dst;
    }
    let da = ((dst >> 24) & 0xff) as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return 0;
    }
    let ch = |shift: u32| -> u32 {
        let s = ((src >> shift) & 0xff) as f32;
        let d = ((dst >> shift) & 0xff) as f32;
        ((s * sa + d * da * (1.0 - sa)) / out_a)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    (((out_a * 255.0).round() as u32) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Text's pre-rotation local rect (world coordinates). Unlike
/// `Rect`/`Ellipse`/`Image`, which carry `r` directly, a string's width
/// depends on the font and content and changes whenever either does — so
/// Text has no explicit rect, and it's computed here on demand wherever
/// needed (bbox, hit-testing, selection outline, drawing).
pub(super) fn text_local_rect(
    pos: (i64, i64),
    s: &str,
    size: f32,
    text: Option<&TextRenderer>,
) -> (f64, f64, f64, f64) {
    let w = text.map(|t| t.text_width(s, size)).unwrap_or(0.0).ceil() as f64;
    let h = size.ceil() as f64;
    (
        pos.0 as f64,
        pos.1 as f64,
        pos.0 as f64 + w.max(1.0),
        pos.1 as f64 + h,
    )
}

/// Draws rotated text. Since `ab_glyph` doesn't support rotated drawing,
/// this first draws the glyphs at rot=0 into a small temp buffer, then
/// bakes that onto the real canvas using the same "inverse-rotate then
/// nearest-neighbor sample" as `paint_image`. Using the same sentinel idea
/// as `render_export`/`compose_rgba`, undrawn pixels in the buffer
/// (outside the glyphs) are skipped rather than baked in, letting the
/// background show through. Drawn on a black background — the glyphs' AA
/// blends into black, giving a natural edge for the same reason as the
/// existing export path.
#[allow(clippy::too_many_arguments)]
fn paint_text_rotated(
    canvas: &mut Canvas,
    pos: (i64, i64),
    s: &str,
    color: u32,
    world_size: f32,
    rot: f64,
    tr: &TextRenderer,
    t: &Xform,
) {
    let (x0, y0, x1, y1) = text_local_rect(pos, s, world_size, Some(tr));
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (scx, scy) = (t.ox + cx * t.scale, t.oy + cy * t.scale);

    let screen_size = t.text_size(world_size);
    let bw = ((tr.text_width(s, screen_size).ceil() as usize).max(1)) + 2;
    let bh = (screen_size.ceil() as usize).max(1) + 2;
    let baseline = 1.0 + screen_size;

    const SENTINEL: u32 = 0xDEAD_BEEF;
    let mut color_buf = vec![0u32; bw * bh];
    {
        let mut tmp = Canvas {
            buf: &mut color_buf,
            w: bw,
            h: bh,
            scale: 1.0,
        };
        tr.draw(&mut tmp, 1.0, baseline, s, screen_size, color);
    }
    let mut mask_buf = vec![SENTINEL; bw * bh];
    {
        let mut tmp = Canvas {
            buf: &mut mask_buf,
            w: bw,
            h: bh,
            scale: 1.0,
        };
        tr.draw(&mut tmp, 1.0, baseline, s, screen_size, color);
    }

    let (halfw, halfh) = (bw as f64 / 2.0, bh as f64 / 2.0);
    // The rotated AABB of a rect uses the "sum of absolute values" formula
    // (same reasoning as paint_image).
    let hw = halfw * rot.cos().abs() + halfh * rot.sin().abs();
    let hh = halfw * rot.sin().abs() + halfh * rot.cos().abs();
    let (cw, ch) = (canvas.w as i64, canvas.h as i64);
    let x0c = ((scx - hw).floor() as i64).max(0);
    let x1c = ((scx + hw).ceil() as i64).min(cw);
    let y0c = ((scy - hh).floor() as i64).max(0);
    let y1c = ((scy + hh).ceil() as i64).min(ch);
    if x0c >= x1c || y0c >= y1c {
        return;
    }

    for y in y0c..y1c {
        for x in x0c..x1c {
            let (lx, ly) = rotate_point((x as f64, y as f64), (scx, scy), -rot);
            let bx = lx - scx + halfw;
            let by = ly - scy + halfh;
            if bx < 0.0 || by < 0.0 || bx >= bw as f64 || by >= bh as f64 {
                continue;
            }
            let bxi = (bx.round() as i64).clamp(0, bw as i64 - 1) as usize;
            let byi = (by.round() as i64).clamp(0, bh as i64 - 1) as usize;
            let idx = byi * bw + bxi;
            if mask_buf[idx] == SENTINEL {
                continue; // A pixel not touched by any glyph lets the background show through.
            }
            canvas.set_i(x, y, color_buf[idx]);
        }
    }
}

/// Fills the interior of a rotated rect (anti-aliased). `cx`/`cy` is the
/// center, `hw`/`hh` is half-width/half-height (both in screen
/// coordinates, after scale), `rot` is in radians. A box-shaped variant of
/// `Canvas::fill_circle_f`'s "distance function -> coverage -> `blend_i`"
/// approach (local coordinates are already in pixel units, so they're used
/// directly as the 0.5px AA band).
fn fill_rotated_rect(
    canvas: &mut Canvas,
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot: f64,
    color: u32,
) {
    let (cos, sin) = (rot.cos(), rot.sin());
    let extent = hw.hypot(hh).ceil() as i64 + 2;
    let (icx, icy) = (cx.round() as i64, cy.round() as i64);
    for py in (icy - extent)..=(icy + extent) {
        for px in (icx - extent)..=(icx + extent) {
            let dx = px as f64 + 0.5 - cx;
            let dy = py as f64 + 0.5 - cy;
            let lx = dx * cos + dy * sin;
            let ly = -dx * sin + dy * cos;
            let d = (lx.abs() - hw).max(ly.abs() - hh);
            let coverage = (0.5 - d).clamp(0.0, 1.0) as f32;
            if coverage > 0.0 {
                canvas.blend_i(px, py, color, coverage);
            }
        }
    }
}

/// Fills the interior of a rotated ellipse (anti-aliased). `cx`/`cy` is
/// the center, `rx`/`ry` is the radii (screen coordinates, after scale),
/// `rot` is in radians. Dividing the normalized distance
/// `d=sqrt((lx/rx)^2+(ly/ry)^2)` by the magnitude of its gradient gives AA
/// based on actual physical pixel distance regardless of aspect ratio (an
/// implicit-function gradient approximation).
fn fill_rotated_ellipse(
    canvas: &mut Canvas,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    rot: f64,
    color: u32,
) {
    let (rx, ry) = (rx.max(0.1), ry.max(0.1));
    let (cos, sin) = (rot.cos(), rot.sin());
    // Bounding rect's half-width/half-height (same closed form as item_bbox).
    let hw = ((rx * cos).powi(2) + (ry * sin).powi(2)).sqrt();
    let hh = ((rx * sin).powi(2) + (ry * cos).powi(2)).sqrt();
    let x0 = (cx - hw).floor() as i64 - 1;
    let x1 = (cx + hw).ceil() as i64 + 1;
    let y0 = (cy - hh).floor() as i64 - 1;
    let y1 = (cy + hh).ceil() as i64 + 1;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f64 + 0.5 - cx;
            let dy = py as f64 + 0.5 - cy;
            let lx = dx * cos + dy * sin;
            let ly = -dx * sin + dy * cos;
            let d = ((lx / rx).powi(2) + (ly / ry).powi(2)).sqrt();
            let grad = ((lx / (rx * rx)).powi(2) + (ly / (ry * ry)).powi(2))
                .sqrt()
                .max(1e-6);
            let coverage = ((1.0 - d) / grad).clamp(0.0, 1.0) as f32;
            if coverage > 0.0 {
                canvas.blend_i(px, py, color, coverage);
            }
        }
    }
}

/// Fills the joint (where adjacent segments meet) of a polyline with
/// thickness `th`, using a round cap. `Canvas::line` is anti-aliased with
/// flat caps (square ends), so joining segments of different angles
/// directly leaves a gap at the joint that looks jagged. This fills that
/// gap with a round join.
fn round_join(canvas: &mut Canvas, p: (i64, i64), th: i64, color: u32) {
    // Rounding the radius to an integer would make it slightly larger or
    // smaller than the line's actual half-thickness, making only the
    // joint look bulged or pinched — so `fill_circle_f` is used to match
    // the half-thickness exactly.
    let r = (th.max(1) as f64) / 2.0;
    canvas.fill_circle_f(p.0 as f64, p.1 as f64, r, color);
}

/// Length of the arrowhead's two angled edges (world px), derived from
/// line thickness `th`; used by both `draw_arrow` and `arrow_barb_tips`.
fn arrow_head_len(th: i64) -> f64 {
    (th as f64 * 5.0).max(10.0)
}

/// The two arrowhead tip coordinates (world px, float), computed with the
/// exact same formula `draw_arrow` uses to actually draw them.
/// `item_export_bbox` uses this to include only the area the arrowhead
/// actually reaches in the bounding rect — expanding uniformly around `b`
/// by `arrow_head_len` would waste space in directions nothing is drawn,
/// depending on the arrow's angle. `None` in the degenerate case where
/// `a`->`b` has near-zero length, where no arrowhead is drawn (same as
/// `draw_arrow`).
fn arrow_barb_tips(a: (i64, i64), b: (i64, i64), th: i64) -> Option<[(f64, f64); 2]> {
    let (dx, dy) = ((b.0 - a.0) as f64, (b.1 - a.1) as f64);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = arrow_head_len(th);
    let ang = 0.5_f64;
    let mut tips = [(0.0, 0.0); 2];
    for (i, &s) in [ang, -ang].iter().enumerate() {
        let (c, sn) = (s.cos(), s.sin());
        // Rotate the reversed vector (-u) by ±ang.
        let rx = -ux * c - (-uy) * sn;
        let ry = -ux * sn + (-uy) * c;
        tips[i] = (b.0 as f64 + rx * head, b.1 as f64 + ry * head);
    }
    Some(tips)
}

/// Draws an arrow (arrowhead at tip `b`).
fn draw_arrow(canvas: &mut Canvas, a: (i64, i64), b: (i64, i64), th: i64, color: u32) {
    canvas.line(a.0, a.1, b.0, b.1, th, color);
    let Some(tips) = arrow_barb_tips(a, b, th) else {
        return;
    };
    for (ex, ey) in tips {
        canvas.line(b.0, b.1, ex as i64, ey as i64, th, color);
    }
    // Fills the joint where the shaft and both barbs meet at b.
    round_join(canvas, b, th, color);
}

/// Draws a connected line joining points in order (screen coordinates).
/// Interior vertices (excluding the first/last) get a `round_join` so
/// adjacent segment joints don't look jagged.
fn draw_polyline(canvas: &mut Canvas, points: &[(i64, i64)], th: i64, color: u32) {
    for w in points.windows(2) {
        canvas.line(w[0].0, w[0].1, w[1].0, w[1].1, th, color);
    }
    for &p in &points[1..points.len().saturating_sub(1)] {
        round_join(canvas, p, th, color);
    }
}

/// Returns a freehand stroke (`points`) as a rasterized bitmap
/// `Annotation::Image`, rather than a Polyline holding the raw points (so
/// selecting it doesn't show a huge pile of vertex handles — afterward it
/// can only be moved/resized/rotated via its bounding rect).
///
/// Draws with `draw_polyline` in solid white (`0x00FF_FFFF`) onto a
/// black-background temp buffer, and uses its luminance directly as the
/// alpha value — `Canvas::blend_i`'s coverage blending becomes a linear
/// mix with white, so the result's luminance equals coverage (the same
/// idea `paint_text_rotated` uses).
pub(super) fn rasterize_freehand(points: &[(i64, i64)], thick: i64, color: u32) -> Annotation {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for &(x, y) in points {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    // Padding for the line's half-thickness plus anti-aliasing.
    let pad = thick.max(1) / 2 + 2;
    let (ox, oy) = (x0 - pad, y0 - pad);
    let w = ((x1 - x0) + pad * 2).max(1) as usize;
    let h = ((y1 - y0) + pad * 2).max(1) as usize;
    let local: Vec<(i64, i64)> = points.iter().map(|&(px, py)| (px - ox, py - oy)).collect();

    let mut buf = vec![0u32; w * h];
    {
        let mut tmp = Canvas {
            buf: &mut buf,
            w,
            h,
            scale: 1.0,
        };
        draw_polyline(&mut tmp, &local, thick, 0x00FF_FFFF);
    }
    let pixels: Vec<u32> = buf
        .iter()
        .map(|&px| {
            let a = px & 0xff; // Drawn in white, so R=G=B=coverage*255.
            (a << 24) | (color & 0x00FF_FFFF)
        })
        .collect();

    Annotation::Image {
        r: (
            ox as f64,
            oy as f64,
            (ox + w as i64) as f64,
            (oy + h as i64) as f64,
        ),
        src_w: w as i64,
        src_h: h as i64,
        pixels: Rc::new(pixels),
        rot: 0.0,
    }
}

/// Draws an ellipse's ring (center `(cx, cy)`, radii `rx`/`ry`, screen
/// coordinates) as a polyline of short chords.
#[allow(clippy::too_many_arguments)]
fn draw_ellipse(
    canvas: &mut Canvas,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    rot: f64,
    th: i64,
    color: u32,
) {
    if rx < 0.5 && ry < 0.5 {
        return;
    }
    // Vary the segment count with size (not too few, not unboundedly many).
    let steps = ((rx + ry) as usize).clamp(24, 720);
    let mut prev: Option<(i64, i64)> = None;
    for i in 0..=steps {
        let ang = (i as f64 / steps as f64) * std::f64::consts::TAU;
        let (s, co) = ang.sin_cos();
        let local = (cx + rx * co, cy + ry * s);
        let (wx, wy) = rotate_point(local, (cx, cy), rot);
        let p = (wx.round() as i64, wy.round() as i64);
        if let Some(pp) = prev {
            canvas.line(pp.0, pp.1, p.0, p.1, th, color);
            round_join(canvas, p, th, color);
        }
        prev = Some(p);
    }
}

/// Whether two points are within `tol` on each axis (used to hit-test grabbing a handle/endpoint).
pub(super) fn near(p: (i64, i64), q: (i64, i64), tol: f64) -> bool {
    ((p.0 - q.0) as f64).abs() <= tol && ((p.1 - q.1) as f64).abs() <= tol
}

/// The float version of `near`, used only for Rect's rotation handling.
pub(super) fn near_f64(p: (f64, f64), q: (f64, f64), tol: f64) -> bool {
    (p.0 - q.0).abs() <= tol && (p.1 - q.1).abs() <= tol
}

/// Normalizes a rect (x0<x1, y0<y1).
pub(super) fn rect_norm(r: (i64, i64, i64, i64)) -> (i64, i64, i64, i64) {
    (r.0.min(r.2), r.1.min(r.3), r.0.max(r.2), r.1.max(r.3))
}

/// The float version of `rect_norm`, used only for Rect's rotation handling.
pub(super) fn rect_norm_f64(r: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    (r.0.min(r.2), r.1.min(r.3), r.0.max(r.2), r.1.max(r.3))
}

/// A rect's center (world coordinates). Kept as floats since Rect is the
/// one shape that rotates — rounding to integers only happens at the
/// final moment of drawing.
fn rect_center(r: (f64, f64, f64, f64)) -> (f64, f64) {
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
}

/// Rotates point `p` around center `c` by angle `ang` (radians).
fn rotate_point(p: (f64, f64), c: (f64, f64), ang: f64) -> (f64, f64) {
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    let (s, co) = ang.sin_cos();
    (c.0 + dx * co - dy * s, c.1 + dx * s + dy * co)
}

/// Rounds an angle (radians) to the nearest 45° (`π/4`) step; used for
/// rotation snapping while Shift is held.
pub(super) fn snap_angle_45(angle: f64) -> f64 {
    let step = std::f64::consts::FRAC_PI_4;
    (angle / step).round() * step
}

/// A rotated rect's 4 corners (world coordinates, top-left clockwise).
/// Rounding to integers only happens here, at the final moment of drawing.
fn rotated_rect_corners(r: (f64, f64, f64, f64), rot: f64) -> [(i64, i64); 4] {
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    let c = rect_center(r);
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| {
        let (rx, ry) = rotate_point((x, y), c, rot);
        (rx.round() as i64, ry.round() as i64)
    })
}

/// Rotates a world point by `-rot` around rect `r`'s center, converting it
/// back to local (unrotated) coordinates. Lets rotated-rect handle
/// hit-testing/resizing reuse axis-aligned logic. The input `p` stays
/// integer since it's only used for hit-testing, but the return value
/// isn't rounded (the caller may feed it into further rotate/translate
/// steps, so rounding only happens once, at the caller's final step).
pub(super) fn to_local(p: (i64, i64), r: (f64, f64, f64, f64), rot: f64) -> (f64, f64) {
    let c = rect_center(r);
    rotate_point((p.0 as f64, p.1 as f64), c, -rot)
}

/// Rotates a local coordinate forward to world space (used to compute the
/// display position of rotate/resize handles). Rounding to integers only
/// happens here, at the final moment of drawing.
fn from_local(p: (f64, f64), r: (f64, f64, f64, f64), rot: f64) -> (i64, i64) {
    let c = rect_center(r);
    let (wx, wy) = rotate_point(p, c, rot);
    (wx.round() as i64, wy.round() as i64)
}

/// Distance from the rect's top-edge center to the rotate handle (world
/// px). `pub(super)` so a multi-select bounding rect's rotate handle looks
/// the same.
pub(super) const ROTATE_HANDLE_DIST: f64 = 24.0;

/// The rotate handle's local coordinate (above the rect's top-edge center by `ROTATE_HANDLE_DIST`).
pub(super) fn rotate_handle_local(r: (f64, f64, f64, f64)) -> (f64, f64) {
    let (x0, y0, x1, _) = rect_norm_f64(r);
    ((x0 + x1) / 2.0, y0 - ROTATE_HANDLE_DIST)
}

/// A rect's 8 handle positions (corners first, then edges — so grabbing prioritizes corners).
fn rect_handles(r: (i64, i64, i64, i64)) -> [(Handle, (i64, i64)); 8] {
    let (x0, y0, x1, y1) = rect_norm(r);
    let mx = (x0 + x1) / 2;
    let my = (y0 + y1) / 2;
    [
        (Handle::TopLeft, (x0, y0)),
        (Handle::TopRight, (x1, y0)),
        (Handle::BottomRight, (x1, y1)),
        (Handle::BottomLeft, (x0, y1)),
        (Handle::Top, (mx, y0)),
        (Handle::Right, (x1, my)),
        (Handle::Bottom, (mx, y1)),
        (Handle::Left, (x0, my)),
    ]
}

/// The float version of `rect_handles`, used only for Rect's rotation handling.
fn rect_handles_f64(r: (f64, f64, f64, f64)) -> [(Handle, (f64, f64)); 8] {
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    let mx = (x0 + x1) / 2.0;
    let my = (y0 + y1) / 2.0;
    [
        (Handle::TopLeft, (x0, y0)),
        (Handle::TopRight, (x1, y0)),
        (Handle::BottomRight, (x1, y1)),
        (Handle::BottomLeft, (x0, y1)),
        (Handle::Top, (mx, y0)),
        (Handle::Right, (x1, my)),
        (Handle::Bottom, (mx, y1)),
        (Handle::Left, (x0, my)),
    ]
}

/// The rect handle within grab range `tol` of point `p`, if any (corners first).
pub(super) fn hit_rect_handle(r: (i64, i64, i64, i64), p: (i64, i64), tol: f64) -> Option<Handle> {
    rect_handles(r)
        .into_iter()
        .find(|&(_, q)| near(p, q, tol))
        .map(|(h, _)| h)
}

/// The float version of `hit_rect_handle`, used only for Rect's rotation handling.
pub(super) fn hit_rect_handle_f64(
    r: (f64, f64, f64, f64),
    p: (f64, f64),
    tol: f64,
) -> Option<Handle> {
    rect_handles_f64(r)
        .into_iter()
        .find(|&(_, q)| near_f64(p, q, tol))
        .map(|(h, _)| h)
}

/// The new rect after dragging handle `h` to `p` (moves only the
/// corresponding edges, then renormalizes — flipping is allowed).
pub(super) fn resize_rect(
    orig: (i64, i64, i64, i64),
    h: Handle,
    p: (i64, i64),
) -> (i64, i64, i64, i64) {
    let (mut x0, mut y0, mut x1, mut y1) = orig;
    match h {
        Handle::TopLeft | Handle::Left | Handle::BottomLeft => x0 = p.0,
        Handle::TopRight | Handle::Right | Handle::BottomRight => x1 = p.0,
        Handle::Top | Handle::Bottom => {}
    }
    match h {
        Handle::TopLeft | Handle::Top | Handle::TopRight => y0 = p.1,
        Handle::BottomLeft | Handle::Bottom | Handle::BottomRight => y1 = p.1,
        Handle::Left | Handle::Right => {}
    }
    rect_norm((x0, y0, x1, y1))
}

/// Resizes a rotated rect via handle `h`, keeping the rotation. `orig` is
/// the local rect at drag start, `rot` is the rotation angle (unchanged
/// during the drag), `p` is the cursor position in world coordinates (all
/// floats).
///
/// Since `Annotation::Rect.r` is kept as floats, this never rounds to
/// integers. It used to round the final result to integers when saving,
/// which let the rotation correction's rounding drift independently of
/// "the rounding of the original handle position" — so even with smooth
/// cursor movement, the opposite edge (which should stay fixed) would
/// jitter back and forth by 1px. (Hysteresis was tried to mitigate it, but
/// that only treated the symptom.) Rounding to integers now only happens
/// at the final moment of drawing (`rotated_rect_corners`/`from_local`),
/// so the stored data itself always changes smoothly, eliminating the
/// jitter at the source.
///
/// `resize_rect` just moves an edge at the point localized relative to
/// `orig`'s center, so the resulting rect's center often shifts too (e.g.
/// pulling just one corner leaves the opposite corner fixed but moves the
/// center). Since drawing always rotates around "the rect's own current
/// center," leaving that shift unaddressed would make a corner that
/// shouldn't have moved appear to move on screen (making the resize feel
/// wrong). To cancel it, once the center has moved by `Δ=c1-c0`, the whole
/// rect is translated by `(rot(Δ) - Δ)` — this relies on rotation being a
/// linear map: translating local coordinates uniformly by `d` translates
/// the result of rotating around its own center by that same uniform `d`.
pub(super) fn resize_rotated_rect(
    orig: (f64, f64, f64, f64),
    h: Handle,
    p: (f64, f64),
    rot: f64,
) -> (f64, f64, f64, f64) {
    let c0 = rect_center(orig);
    let local_p = rotate_point(p, c0, -rot);
    let (mut x0, mut y0, mut x1, mut y1) = orig;
    match h {
        Handle::TopLeft | Handle::Left | Handle::BottomLeft => x0 = local_p.0,
        Handle::TopRight | Handle::Right | Handle::BottomRight => x1 = local_p.0,
        Handle::Top | Handle::Bottom => {}
    }
    match h {
        Handle::TopLeft | Handle::Top | Handle::TopRight => y0 = local_p.1,
        Handle::BottomLeft | Handle::Bottom | Handle::BottomRight => y1 = local_p.1,
        Handle::Left | Handle::Right => {}
    }
    // Normalize (min/max per axis).
    let (nx0, nx1) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    let (ny0, ny1) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };

    if rot == 0.0 {
        return (nx0, ny0, nx1, ny1);
    }

    let c1 = ((nx0 + nx1) / 2.0, (ny0 + ny1) / 2.0);
    let delta = (c1.0 - c0.0, c1.1 - c0.1);
    let d = {
        let rotated = rotate_point(delta, (0.0, 0.0), rot);
        (rotated.0 - delta.0, rotated.1 - delta.1)
    };
    (nx0 + d.0, ny0 + d.1, nx1 + d.0, ny1 + d.1)
}

/// The aspect-ratio-locked version of `resize_rotated_rect` (used for
/// resizing an Image while Shift is held). Converts the cursor position to
/// local coordinates via the item's own rotation, computes a rect that
/// preserves ratio `ar = w/h` with the opposite corner (or edge) as the
/// fixed point, then applies the same "keep the opposite corner's world
/// position fixed" correction as `resize_rotated_rect`.
pub(super) fn resize_rotated_rect_aspect(
    orig: (f64, f64, f64, f64),
    h: Handle,
    p: (f64, f64),
    rot: f64,
    ar: f64,
) -> (f64, f64, f64, f64) {
    let c0 = rect_center(orig);
    let local_p = rotate_point(p, c0, -rot);
    let (x0, y0, x1, y1) = rect_norm_f64(orig);
    let ar = if ar > 0.0 { ar } else { 1.0 };
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let (nx0, ny0, nx1, ny1) = match h {
        Handle::TopLeft | Handle::TopRight | Handle::BottomLeft | Handle::BottomRight => {
            let (ax, ay) = match h {
                Handle::TopLeft => (x1, y1),
                Handle::TopRight => (x0, y1),
                Handle::BottomLeft => (x1, y0),
                _ => (x0, y0), // BottomRight
            };
            let fw = (local_p.0 - ax).abs();
            let fh = (local_p.1 - ay).abs();
            let (w, hgt) = if fw / ar > fh {
                (fw.max(1.0), (fw / ar).max(1.0))
            } else {
                ((fh * ar).max(1.0), fh.max(1.0))
            };
            let sx = if local_p.0 >= ax { 1.0 } else { -1.0 };
            let sy = if local_p.1 >= ay { 1.0 } else { -1.0 };
            rect_norm_f64((ax, ay, ax + sx * w, ay + sy * hgt))
        }
        Handle::Left | Handle::Right => {
            let ax = if matches!(h, Handle::Left) { x1 } else { x0 };
            let w = (local_p.0 - ax).abs().max(1.0);
            let hgt = (w / ar).max(1.0);
            let sx = if local_p.0 >= ax { 1.0 } else { -1.0 };
            rect_norm_f64((ax, cy - hgt / 2.0, ax + sx * w, cy + hgt / 2.0))
        }
        Handle::Top | Handle::Bottom => {
            let ay = if matches!(h, Handle::Top) { y1 } else { y0 };
            let hgt = (local_p.1 - ay).abs().max(1.0);
            let w = (hgt * ar).max(1.0);
            let sy = if local_p.1 >= ay { 1.0 } else { -1.0 };
            rect_norm_f64((cx - w / 2.0, ay, cx + w / 2.0, ay + sy * hgt))
        }
    };

    if rot == 0.0 {
        return (nx0, ny0, nx1, ny1);
    }
    let c1 = ((nx0 + nx1) / 2.0, (ny0 + ny1) / 2.0);
    let delta = (c1.0 - c0.0, c1.1 - c0.1);
    let d = {
        let rotated = rotate_point(delta, (0.0, 0.0), rot);
        (rotated.0 - delta.0, rotated.1 - delta.1)
    };
    (nx0 + d.0, ny0 + d.1, nx1 + d.0, ny1 + d.1)
}

/// Returns a copy of the item translated by `(dx, dy)` (style unchanged).
pub(super) fn translate_annotation(ann: &Annotation, dx: i64, dy: i64) -> Annotation {
    match ann {
        Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot,
        } => Annotation::Image {
            r: (
                r.0 + dx as f64,
                r.1 + dy as f64,
                r.2 + dx as f64,
                r.3 + dy as f64,
            ),
            src_w: *src_w,
            src_h: *src_h,
            pixels: pixels.clone(),
            rot: *rot,
        },
        Annotation::Arrow { a, b, color, thick } => Annotation::Arrow {
            a: (a.0 + dx, a.1 + dy),
            b: (b.0 + dx, b.1 + dy),
            color: *color,
            thick: *thick,
        },
        Annotation::Polyline {
            points,
            color,
            thick,
        } => Annotation::Polyline {
            points: points.iter().map(|p| (p.0 + dx, p.1 + dy)).collect(),
            color: *color,
            thick: *thick,
        },
        Annotation::Rect {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Rect {
            r: (
                r.0 + dx as f64,
                r.1 + dy as f64,
                r.2 + dx as f64,
                r.3 + dy as f64,
            ),
            color: *color,
            thick: *thick,
            rot: *rot,
            filled: *filled,
        },
        Annotation::Mosaic {
            r,
            rot,
            block,
            mode,
            seed,
        } => Annotation::Mosaic {
            r: (
                r.0 + dx as f64,
                r.1 + dy as f64,
                r.2 + dx as f64,
                r.3 + dy as f64,
            ),
            rot: *rot,
            block: *block,
            mode: *mode,
            seed: *seed,
        },
        Annotation::Ellipse {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Ellipse {
            r: (
                r.0 + dx as f64,
                r.1 + dy as f64,
                r.2 + dx as f64,
                r.3 + dy as f64,
            ),
            color: *color,
            thick: *thick,
            rot: *rot,
            filled: *filled,
        },
        Annotation::Text {
            pos,
            text,
            color,
            size,
            rot,
        } => Annotation::Text {
            pos: (pos.0 + dx, pos.1 + dy),
            text: text.clone(),
            color: *color,
            size: *size,
            rot: *rot,
        },
        Annotation::NumberMarker {
            pos,
            number,
            color,
            size,
        } => Annotation::NumberMarker {
            pos: (pos.0 + dx, pos.1 + dy),
            number: *number,
            color: *color,
            size: *size,
        },
        Annotation::Guide { r } => Annotation::Guide {
            r: (r.0 + dx, r.1 + dy, r.2 + dx, r.3 + dy),
        },
    }
}

/// Multi-select group resize: applies the mapping from bounding rect
/// `orig_rect` to `new_rect` as a per-axis linear transform on the item's
/// geometry, relative to `orig_rect`'s edges (style unchanged). If
/// `orig_rect` has zero width/height along an axis (a selection with no
/// extent on that axis), that axis falls back to a scale of 1.0 to avoid
/// division by zero.
pub(super) fn scale_annotation(
    ann: &Annotation,
    orig_rect: (i64, i64, i64, i64),
    new_rect: (i64, i64, i64, i64),
) -> Annotation {
    let (ox0, oy0, ox1, oy1) = orig_rect;
    let (nx0, ny0, nx1, ny1) = new_rect;
    let (ow, oh) = ((ox1 - ox0) as f64, (oy1 - oy0) as f64);
    let sx = if ow.abs() > f64::EPSILON {
        (nx1 - nx0) as f64 / ow
    } else {
        1.0
    };
    let sy = if oh.abs() > f64::EPSILON {
        (ny1 - ny0) as f64 / oh
    } else {
        1.0
    };
    // Transform in float (for Rect); rounding only happens for i64-typed values, at the end.
    let map_xf = |x: f64| nx0 as f64 + (x - ox0 as f64) * sx;
    let map_yf = |y: f64| ny0 as f64 + (y - oy0 as f64) * sy;
    let map_x = |x: i64| map_xf(x as f64).round() as i64;
    let map_y = |y: i64| map_yf(y as f64).round() as i64;

    match ann {
        Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot,
        } => Annotation::Image {
            r: (map_xf(r.0), map_yf(r.1), map_xf(r.2), map_yf(r.3)),
            src_w: *src_w,
            src_h: *src_h,
            pixels: pixels.clone(),
            rot: *rot,
        },
        Annotation::Arrow { a, b, color, thick } => Annotation::Arrow {
            a: (map_x(a.0), map_y(a.1)),
            b: (map_x(b.0), map_y(b.1)),
            color: *color,
            thick: *thick,
        },
        Annotation::Polyline {
            points,
            color,
            thick,
        } => Annotation::Polyline {
            points: points.iter().map(|p| (map_x(p.0), map_y(p.1))).collect(),
            color: *color,
            thick: *thick,
        },
        Annotation::Rect {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Rect {
            r: (map_xf(r.0), map_yf(r.1), map_xf(r.2), map_yf(r.3)),
            color: *color,
            thick: *thick,
            rot: *rot,
            filled: *filled,
        },
        Annotation::Mosaic {
            r,
            rot,
            block,
            mode,
            seed,
        } => Annotation::Mosaic {
            r: (map_xf(r.0), map_yf(r.1), map_xf(r.2), map_yf(r.3)),
            rot: *rot,
            block: *block,
            mode: *mode,
            seed: *seed,
        },
        Annotation::Ellipse {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Ellipse {
            r: (map_xf(r.0), map_yf(r.1), map_xf(r.2), map_yf(r.3)),
            color: *color,
            thick: *thick,
            rot: *rot,
            filled: *filled,
        },
        Annotation::Text {
            pos,
            text,
            color,
            size,
            rot,
        } => Annotation::Text {
            pos: (map_x(pos.0), map_y(pos.1)),
            text: text.clone(),
            color: *color,
            size: (*size as f64 * (sx + sy) / 2.0) as f32,
            rot: *rot,
        },
        Annotation::NumberMarker {
            pos,
            number,
            color,
            size,
        } => Annotation::NumberMarker {
            pos: (map_x(pos.0), map_y(pos.1)),
            number: *number,
            color: *color,
            size: (*size as f64 * (sx + sy) / 2.0) as f32,
        },
        // Never actually included in a multi-select, but provided as an
        // identity transform for exhaustiveness.
        Annotation::Guide { r } => Annotation::Guide {
            r: (map_x(r.0), map_y(r.1), map_x(r.2), map_y(r.3)),
        },
    }
}

/// A linear mapping in axis-aligned coordinates (extracted from
/// `scale_annotation`'s mapping so it can be reused given local
/// coordinates directly).
fn scale_point_local(
    p: (f64, f64),
    orig: (f64, f64, f64, f64),
    new: (f64, f64, f64, f64),
) -> (f64, f64) {
    let (ox0, oy0, ox1, oy1) = orig;
    let (nx0, ny0, nx1, ny1) = new;
    let (ow, oh) = (ox1 - ox0, oy1 - oy0);
    let sx = if ow.abs() > f64::EPSILON {
        (nx1 - nx0) / ow
    } else {
        1.0
    };
    let sy = if oh.abs() > f64::EPSILON {
        (ny1 - ny0) / oh
    } else {
        1.0
    };
    (nx0 + (p.0 - ox0) * sx, ny0 + (p.1 - oy0) * sy)
}

/// Applies the group's rotated resize transform `map` (world coordinates
/// to world coordinates) to the two opposite corners `(r.0,r.1)`-`(r.2,r.3)`
/// while accounting for the item's own rotation `item_rot`, returning new
/// local coordinates (shared by `Rect`/`Ellipse`). Since `r` itself is a
/// local coordinate — "rotated by `item_rot` around the item's own
/// center," not a world coordinate — the corners are first converted to
/// world space via the item's own rotation and passed through `map`; the
/// midpoint of the two transformed corners (the rotation center always
/// stays at that midpoint) becomes the new center, and the result is
/// rotated back and converted to local coordinates.
fn transform_rotated_corners(
    r: (f64, f64, f64, f64),
    item_rot: f64,
    map: impl Fn((f64, f64)) -> (f64, f64),
) -> (f64, f64, f64, f64) {
    let item_center = rect_center(r);
    let world_p0 = rotate_point((r.0, r.1), item_center, item_rot);
    let world_p1 = rotate_point((r.2, r.3), item_center, item_rot);
    let new_world_p0 = map(world_p0);
    let new_world_p1 = map(world_p1);
    let new_center = (
        (new_world_p0.0 + new_world_p1.0) / 2.0,
        (new_world_p0.1 + new_world_p1.1) / 2.0,
    );
    let nr0 = rotate_point(new_world_p0, new_center, -item_rot);
    let nr1 = rotate_point(new_world_p1, new_center, -item_rot);
    (nr0.0, nr0.1, nr1.0, nr1.1)
}

/// The rotation-aware version of `scale_annotation`. When the multi-select
/// bounding rect is displayed rotated by `rot` (see `group_frame`),
/// applies the resize in that local coordinate system. Each item's
/// position fields go through 3 steps: convert to `orig_rect`'s local
/// coordinates, apply an axis-aligned linear scale, then convert back to
/// world space relative to `new_rect` (same idea as
/// `Annotation::Rect`'s own `resize_rotated_rect`).
pub(super) fn scale_annotation_rotated(
    ann: &Annotation,
    orig_rect: (f64, f64, f64, f64),
    new_rect: (f64, f64, f64, f64),
    rot: f64,
) -> Annotation {
    if rot == 0.0 {
        let to_i = |r: (f64, f64, f64, f64)| {
            (
                r.0.round() as i64,
                r.1.round() as i64,
                r.2.round() as i64,
                r.3.round() as i64,
            )
        };
        return scale_annotation(ann, to_i(orig_rect), to_i(new_rect));
    }
    let c0 = rect_center(orig_rect);
    let c1 = rect_center(new_rect);
    let map = |p: (f64, f64)| -> (f64, f64) {
        let lp = rotate_point(p, c0, -rot);
        let sp = scale_point_local(lp, orig_rect, new_rect);
        rotate_point(sp, c1, rot)
    };
    let map_i = |p: (i64, i64)| -> (i64, i64) {
        let (x, y) = map((p.0 as f64, p.1 as f64));
        (x.round() as i64, y.round() as i64)
    };
    // sx/sy are only used to scale size-related fields (w/h, text size) —
    // position is already handled correctly by map/map_i above.
    let (ow, oh) = (orig_rect.2 - orig_rect.0, orig_rect.3 - orig_rect.1);
    let sx = if ow.abs() > f64::EPSILON {
        (new_rect.2 - new_rect.0) / ow
    } else {
        1.0
    };
    let sy = if oh.abs() > f64::EPSILON {
        (new_rect.3 - new_rect.1) / oh
    } else {
        1.0
    };

    match ann {
        Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot: item_rot,
        } => Annotation::Image {
            r: transform_rotated_corners(*r, *item_rot, map),
            src_w: *src_w,
            src_h: *src_h,
            pixels: pixels.clone(),
            rot: *item_rot,
        },
        Annotation::Arrow { a, b, color, thick } => Annotation::Arrow {
            a: map_i(*a),
            b: map_i(*b),
            color: *color,
            thick: *thick,
        },
        Annotation::Polyline {
            points,
            color,
            thick,
        } => Annotation::Polyline {
            points: points.iter().map(|&p| map_i(p)).collect(),
            color: *color,
            thick: *thick,
        },
        Annotation::Rect {
            r,
            color,
            thick,
            rot: item_rot,
            filled,
        } => Annotation::Rect {
            r: transform_rotated_corners(*r, *item_rot, map),
            color: *color,
            thick: *thick,
            rot: *item_rot,
            filled: *filled,
        },
        Annotation::Mosaic {
            r,
            rot: item_rot,
            block,
            mode,
            seed,
        } => Annotation::Mosaic {
            r: transform_rotated_corners(*r, *item_rot, map),
            rot: *item_rot,
            block: *block,
            mode: *mode,
            seed: *seed,
        },
        Annotation::Ellipse {
            r,
            color,
            thick,
            rot: item_rot,
            filled,
        } => Annotation::Ellipse {
            r: transform_rotated_corners(*r, *item_rot, map),
            color: *color,
            thick: *thick,
            rot: *item_rot,
            filled: *filled,
        },
        Annotation::Text {
            pos,
            text,
            color,
            size,
            rot: item_rot,
        } => Annotation::Text {
            pos: map_i(*pos),
            text: text.clone(),
            color: *color,
            size: (*size as f64 * (sx + sy) / 2.0) as f32,
            rot: *item_rot,
        },
        Annotation::NumberMarker {
            pos,
            number,
            color,
            size,
        } => Annotation::NumberMarker {
            pos: map_i(*pos),
            number: *number,
            color: *color,
            size: (*size as f64 * (sx + sy) / 2.0) as f32,
        },
        // Never actually included in a multi-select, but provided as an
        // identity transform for exhaustiveness.
        Annotation::Guide { r } => Annotation::Guide { r: *r },
    }
}

/// Returns the local coordinates after orbiting `r`'s own center around
/// `center` (an external point, e.g. a group's rotation center) by
/// `delta` radians (shared by `Rect`/`Ellipse`). Only moves the position
/// along the orbit, without changing the shape's own orientation — adding
/// `delta` to `rot` is left to the caller.
fn rotate_shape_r(r: (f64, f64, f64, f64), center: (f64, f64), delta: f64) -> (f64, f64, f64, f64) {
    let rc = rect_center(r);
    let (nx, ny) = rotate_point(rc, center, delta);
    let (dx, dy) = (nx - rc.0, ny - rc.1);
    (r.0 + dx, r.1 + dy, r.2 + dx, r.3 + dy)
}

/// Multi-select group rotation: returns a copy rotated by `delta` radians
/// around `center` (style unchanged). `Rect`/`Ellipse`/`Image`/`Text`
/// update both their center position and `rot`, so their orientation
/// actually rotates too (Text computes its local rect on demand from
/// `pos`, so it takes `text: Option<&TextRenderer>`). `Arrow` has no `rot`
/// field, so rotating both endpoints individually rotates its orientation
/// correctly.
pub(super) fn rotate_annotation_around(
    ann: &Annotation,
    center: (f64, f64),
    delta: f64,
    text: Option<&TextRenderer>,
) -> Annotation {
    let rot_i = |p: (i64, i64)| -> (i64, i64) {
        let (x, y) = rotate_point((p.0 as f64, p.1 as f64), center, delta);
        (x.round() as i64, y.round() as i64)
    };
    match ann {
        Annotation::Image {
            r,
            src_w,
            src_h,
            pixels,
            rot,
        } => Annotation::Image {
            r: rotate_shape_r(*r, center, delta),
            src_w: *src_w,
            src_h: *src_h,
            pixels: pixels.clone(),
            rot: rot + delta,
        },
        Annotation::Arrow { a, b, color, thick } => Annotation::Arrow {
            a: rot_i(*a),
            b: rot_i(*b),
            color: *color,
            thick: *thick,
        },
        Annotation::Polyline {
            points,
            color,
            thick,
        } => Annotation::Polyline {
            points: points.iter().map(|&p| rot_i(p)).collect(),
            color: *color,
            thick: *thick,
        },
        Annotation::Rect {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Rect {
            r: rotate_shape_r(*r, center, delta),
            color: *color,
            thick: *thick,
            rot: rot + delta,
            filled: *filled,
        },
        Annotation::Mosaic {
            r,
            rot,
            block,
            mode,
            seed,
        } => Annotation::Mosaic {
            r: rotate_shape_r(*r, center, delta),
            rot: rot + delta,
            block: *block,
            mode: *mode,
            seed: *seed,
        },
        Annotation::Ellipse {
            r,
            color,
            thick,
            rot,
            filled,
        } => Annotation::Ellipse {
            r: rotate_shape_r(*r, center, delta),
            color: *color,
            thick: *thick,
            rot: rot + delta,
            filled: *filled,
        },
        Annotation::Text {
            pos,
            text: s,
            color,
            size,
            rot,
        } => {
            let local_r = text_local_rect(*pos, s, *size, text);
            let new_r = rotate_shape_r(local_r, center, delta);
            Annotation::Text {
                pos: (new_r.0.round() as i64, new_r.1.round() as i64),
                text: s.clone(),
                color: *color,
                size: *size,
                rot: rot + delta,
            }
        }
        Annotation::NumberMarker {
            pos,
            number,
            color,
            size,
        } => Annotation::NumberMarker {
            pos: rot_i(*pos),
            number: *number,
            color: *color,
            size: *size,
        },
        // Never actually included in a multi-select, but provided as an
        // identity transform for exhaustiveness.
        Annotation::Guide { r } => Annotation::Guide { r: *r },
    }
}

/// Moves every selected item one step forward (`forward=true`) or
/// backward in draw order, preserving their relative order. An item
/// already at the boundary, or whose neighbor is also in the selection
/// (i.e. it should move together as part of the same block), is skipped
/// for that step — processing in descending order (toward front) or
/// ascending order (toward back) makes adjacent selected items correctly
/// move together as one block. Works correctly for a single selection
/// too. Returns the updated selection indices (ascending).
pub(super) fn reorder_selection(
    annotations: &mut [Annotation],
    selected: &[usize],
    forward: bool,
) -> Vec<usize> {
    let mut set: std::collections::BTreeSet<usize> = selected.iter().copied().collect();
    let mut order: Vec<usize> = set.iter().copied().collect();
    if forward {
        order.reverse();
    }
    for i in order {
        let neighbor = if forward {
            i.checked_add(1)
        } else {
            i.checked_sub(1)
        };
        if let Some(j) = neighbor
            && j < annotations.len()
            && !set.contains(&j)
        {
            annotations.swap(i, j);
            set.remove(&i);
            set.insert(j);
        }
    }
    set.into_iter().collect()
}

/// Distance from a point to a line segment (used to hit-test the arrow shaft).
fn dist_point_seg(p: (i64, i64), a: (i64, i64), b: (i64, i64)) -> f64 {
    let (px, py) = (p.0 as f64, p.1 as f64);
    let (ax, ay) = (a.0 as f64, a.1 as f64);
    let (bx, by) = (b.0 as f64, b.1 as f64);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    if len2 <= f64::EPSILON {
        return (px - ax).hypot(py - ay);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

/// The next number marker's number (existing markers' max + 1, or 1 if
/// none). Used instead of a dedicated counter on `Editor` — since
/// undo/redo only stacks `Vec<Annotation>` snapshots, a dedicated counter
/// would drift out of sync after undo. Deriving it from existing items
/// every time keeps it always consistent.
pub(super) fn next_marker_number(annotations: &[Annotation]) -> u32 {
    annotations
        .iter()
        .filter_map(|a| match a {
            Annotation::NumberMarker { number, .. } => Some(*number),
            _ => None,
        })
        .max()
        .map_or(1, |m| m + 1)
}

/// Picks a readable text color (white or black) for background `bg`
/// (0x00RRGGBB), via a standard luminance formula — keeps a number
/// marker's digit readable regardless of the chosen circle color.
fn contrast_text_color(bg: u32) -> u32 {
    let r = ((bg >> 16) & 0xff) as f64;
    let g = ((bg >> 8) & 0xff) as f64;
    let b = (bg & 0xff) as f64;
    let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
    if luminance > 140.0 {
        0x0000_0000
    } else {
        0x00FF_FFFF
    }
}

/// The axis-aligned bounding rect of a rotated rect (local `r` plus
/// `rot`) — min/max over the 4 actual corners. Shared by
/// `Rect`/`Image`/`Text` (the latter via the rect from `text_local_rect`).
fn rotated_rect_bbox(r: (f64, f64, f64, f64), rot: f64) -> (i64, i64, i64, i64) {
    let corners = rotated_rect_corners(r, rot);
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0, y0, x1, y1)
}

/// An item's bounding box (world coordinates); approximated for text. A
/// tight rect for cases where the UI wants it to hug the visual shape
/// exactly — the selection outline, marquee-selection hit-testing, the
/// group-transform reference — and does not include line thickness,
/// arrowhead overhang, etc. Using this for export bounds could clip the
/// outside of a line; use `item_export_bbox` there instead.
pub(super) fn item_bbox(ann: &Annotation, text: Option<&TextRenderer>) -> (i64, i64, i64, i64) {
    match ann {
        // Image/Rect/Mosaic share the same "rect plus rotation angle" shape, so they share this formula.
        Annotation::Image { r, rot, .. }
        | Annotation::Rect { r, rot, .. }
        | Annotation::Mosaic { r, rot, .. } => rotated_rect_bbox(*r, *rot),
        Annotation::Arrow { a, b, .. } => (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)),
        Annotation::Polyline { points, .. } => {
            let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
            for &(x, y) in points {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            (x0, y0, x1, y1)
        }
        Annotation::Text {
            pos,
            text: s,
            size,
            rot,
            ..
        } => rotated_rect_bbox(text_local_rect(*pos, s, *size, text), *rot),
        Annotation::NumberMarker { pos, size, .. } => {
            let r = *size as i64;
            (pos.0 - r, pos.1 - r, pos.0 + r, pos.1 + r)
        }
        Annotation::Guide { r } => rect_norm(*r),
        Annotation::Ellipse { r, rot, .. } => {
            // A rotated ellipse's axis-aligned bounding rect has a closed
            // form: for radii rx,ry and rotation rot, the bounding rect's
            // half-width is hw=sqrt((rx*cos(rot))^2+(ry*sin(rot))^2), and
            // half-height similarly.
            let (x0, y0, x1, y1) = rect_norm_f64(*r);
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
            let hw = ((rx * rot.cos()).powi(2) + (ry * rot.sin()).powi(2)).sqrt();
            let hh = ((rx * rot.sin()).powi(2) + (ry * rot.cos()).powi(2)).sqrt();
            (
                (cx - hw).round() as i64,
                (cy - hh).round() as i64,
                (cx + hw).round() as i64,
                (cy + hh).round() as i64,
            )
        }
    }
}

/// `item_bbox` plus a safety margin for how far actual drawing extends
/// beyond it (line thickness, arrowhead tips, etc.). Used only for export
/// bounds (`export_bounds` via `annotations_bounds`) — using this padded
/// rect for the selection outline or marquee-selection hit-testing would
/// make the selection box look unnaturally larger than the shape
/// (especially noticeable for a thick arrow with a large head), so those
/// still use the tight `item_bbox`.
pub(super) fn item_export_bbox(
    ann: &Annotation,
    text: Option<&TextRenderer>,
) -> (i64, i64, i64, i64) {
    match ann {
        // When filled, `thick` is ignored at draw time, so no padding is
        // needed; but an outline-only shape is drawn ±(thick/2) around the
        // center, extending beyond the rect by that much.
        Annotation::Rect { thick, filled, .. } | Annotation::Ellipse { thick, filled, .. }
            if !*filled =>
        {
            let (x0, y0, x1, y1) = item_bbox(ann, text);
            let pad = ((*thick).max(1) + 1) / 2;
            (x0 - pad, y0 - pad, x1 + pad, y1 + pad)
        }
        // Tightly bounds the two actual arrowhead tips (via
        // `arrow_barb_tips`, the exact same formula `draw_arrow` uses).
        // Expanding uniformly around `b` by `arrow_head_len` would waste
        // space in directions nothing is drawn, since the barbs only ever
        // extend toward `a` (the reverse direction). Finally, the whole
        // rect is expanded by the line's half-thickness, since both the
        // shaft and barbs are drawn at the same thickness.
        Annotation::Arrow { a, b, thick, .. } => {
            let (mut x0, mut y0, mut x1, mut y1) =
                (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
            if let Some(tips) = arrow_barb_tips(*a, *b, *thick) {
                for (tx, ty) in tips {
                    x0 = x0.min(tx.floor() as i64);
                    y0 = y0.min(ty.floor() as i64);
                    x1 = x1.max(tx.ceil() as i64);
                    y1 = y1.max(ty.ceil() as i64);
                }
            }
            let pad = ((*thick).max(1) + 1) / 2;
            (x0 - pad, y0 - pad, x1 + pad, y1 + pad)
        }
        Annotation::Polyline { thick, .. } => {
            // Add padding for the line/joints being drawn ±(thick/2) around the center.
            let (x0, y0, x1, y1) = item_bbox(ann, text);
            let pad = ((*thick).max(1) + 1) / 2;
            (x0 - pad, y0 - pad, x1 + pad, y1 + pad)
        }
        _ => item_bbox(ann, text),
    }
}

/// The rect of the sole guide, if any (world coordinates, normalized).
pub(super) fn guide_bounds(annotations: &[Annotation]) -> Option<(i64, i64, i64, i64)> {
    annotations.iter().find_map(|a| match a {
        Annotation::Guide { r } => Some(rect_norm(*r)),
        _ => None,
    })
}

/// If a guide exists (committed, or a drag preview), dims everything
/// outside it — a live-display-only visual cue that this area won't be
/// exported. A no-op if `guide` is `None`.
pub(super) fn dim_outside_guide(
    canvas: &mut Canvas,
    guide: Option<(i64, i64, i64, i64)>,
    t: &Xform,
) {
    let Some(r) = guide else {
        return;
    };
    let (gx0, gy0) = t.map((r.0, r.1));
    let (gx1, gy1) = t.map((r.2, r.3));
    let (gx0, gx1) = (gx0.min(gx1), gx0.max(gx1));
    let (gy0, gy1) = (gy0.min(gy1), gy0.max(gy1));

    let (cw, ch) = (canvas.w as i64, canvas.h as i64);
    for y in 0..ch {
        // r.2/r.3 (x1/y1) are exclusive bounds like other Rect-style fields (not included in export bounds).
        let inside_y = y >= gy0 && y < gy1;
        for x in 0..cw {
            if inside_y && x >= gx0 && x < gx1 {
                continue;
            }
            dim_pixel(canvas, x, y);
        }
    }
}

/// Dims one pixel (each RGB channel to 55% brightness).
fn dim_pixel(canvas: &mut Canvas, x: i64, y: i64) {
    let idx = y as usize * canvas.w + x as usize;
    let c = canvas.buf[idx];
    let dim = |v: u32| v * 55 / 100;
    let r = dim((c >> 16) & 0xff);
    let g = dim((c >> 8) & 0xff);
    let b = dim(c & 0xff);
    canvas.buf[idx] = (r << 16) | (g << 8) | b;
}

/// The rect containing every item (world coordinates); `None` if there are
/// none. Used for export bounds, so it uses `item_export_bbox` (the tight
/// `item_bbox` plus a safety margin) to avoid clipping line thickness or
/// arrowhead overhang.
pub(super) fn annotations_bounds(
    annotations: &[Annotation],
    text: Option<&TextRenderer>,
) -> Option<(i64, i64, i64, i64)> {
    annotations
        .iter()
        .map(|a| item_export_bbox(a, text))
        .reduce(|acc, b| {
            (
                acc.0.min(b.0),
                acc.1.min(b.1),
                acc.2.max(b.2),
                acc.3.max(b.3),
            )
        })
}

/// The bounding rect containing only the selected items (a subset of
/// indices in `selected`), used as the reference for a multi-select group
/// transform. The subset-only version of `annotations_bounds`.
pub(super) fn selection_bounds(
    annotations: &[Annotation],
    selected: &[usize],
    text: Option<&TextRenderer>,
) -> Option<(i64, i64, i64, i64)> {
    selected
        .iter()
        .filter_map(|&i| annotations.get(i))
        .map(|a| item_bbox(a, text))
        .reduce(|acc, b| {
            (
                acc.0.min(b.0),
                acc.1.min(b.1),
                acc.2.max(b.2),
                acc.3.max(b.3),
            )
        })
}

/// The selected items' effective rotation angle (`rot` for rotatable
/// shapes, 0.0 for others) if they all agree, else 0.0 (axis-aligned).
/// The multi-select bounding rect's angle recomputes this each time the
/// selection changes, then keeps it until the selection changes again
/// (`Editor.group_rot`/`recompute_group_rot`) — so the bounding rect's
/// orientation stays put even while an item's `rot` hasn't updated yet
/// mid-drag, or unsupported types are mixed in, as long as the selection
/// itself hasn't changed.
pub(super) fn common_rotation(annotations: &[Annotation], selected: &[usize]) -> f64 {
    let mut common: Option<f64> = None;
    for &i in selected {
        let Some(a) = annotations.get(i) else {
            continue;
        };
        let r = match a {
            Annotation::Rect { rot, .. }
            | Annotation::Ellipse { rot, .. }
            | Annotation::Image { rot, .. }
            | Annotation::Mosaic { rot, .. }
            | Annotation::Text { rot, .. } => *rot,
            _ => 0.0,
        };
        match common {
            None => common = Some(r),
            Some(c) if (c - r).abs() < 1e-6 => {}
            Some(_) => return 0.0,
        }
    }
    common.unwrap_or(0.0)
}

/// The selected items' bounding rect, in the same local-coordinate sense
/// as `Annotation::Rect` (so `rotated_rect_corners`/`to_local`/
/// `from_local`/`hit_rect_handle_f64`/`resize_rotated_rect` can be reused
/// directly). `rot` is the value the caller already holds (computed by
/// `common_rotation` whenever the selection changes) — this function
/// itself never detects the rotation angle; that's `common_rotation`'s job.
pub(super) fn group_rect_for_rotation(
    annotations: &[Annotation],
    selected: &[usize],
    rot: f64,
    text: Option<&TextRenderer>,
) -> Option<(f64, f64, f64, f64)> {
    let items: Vec<&Annotation> = selected
        .iter()
        .filter_map(|&i| annotations.get(i))
        .collect();
    if items.is_empty() {
        return None;
    }

    let aabb = selection_bounds(annotations, selected, text)?;
    if rot == 0.0 {
        return Some((aabb.0 as f64, aabb.1 as f64, aabb.2 as f64, aabb.3 as f64));
    }

    // Using the axis-aligned bbox's center as the pivot, rotate each
    // item's actual 4 corners (Rect's own rotation-aware corners; others
    // use the bbox's 4 corners) by -rot into local coordinates, and take
    // their AABB. Following the same jitter-avoidance policy as
    // elsewhere, this stays in float without rounding
    // (`rotated_rect_corners` isn't used here since it rounds for final
    // drawing).
    let pivot = (
        (aabb.0 + aabb.2) as f64 / 2.0,
        (aabb.1 + aabb.3) as f64 / 2.0,
    );
    let (mut lx0, mut ly0, mut lx1, mut ly1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for a in &items {
        let corners: [(f64, f64); 4] = match a {
            // An ellipse is also approximated as its bounding rect
            // rotated (since group_rect_for_rotation is itself just "a
            // tight oriented-bounding-rect estimate," this level of
            // precision is an acceptable tradeoff). For Image/Mosaic,
            // which really are rects, this formula applies exactly.
            Annotation::Rect { r, rot: ir, .. }
            | Annotation::Ellipse { r, rot: ir, .. }
            | Annotation::Image { r, rot: ir, .. }
            | Annotation::Mosaic { r, rot: ir, .. } => {
                let (x0, y0, x1, y1) = rect_norm_f64(*r);
                let c = rect_center(*r);
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| rotate_point((x, y), c, *ir))
            }
            Annotation::Text {
                pos,
                text: s,
                size,
                rot: ir,
                ..
            } => {
                let r = text_local_rect(*pos, s, *size, text);
                let (x0, y0, x1, y1) = rect_norm_f64(r);
                let c = rect_center(r);
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| rotate_point((x, y), c, *ir))
            }
            _ => {
                let (x0, y0, x1, y1) = item_bbox(a, text);
                [
                    (x0 as f64, y0 as f64),
                    (x1 as f64, y0 as f64),
                    (x1 as f64, y1 as f64),
                    (x0 as f64, y1 as f64),
                ]
            }
        };
        for (x, y) in corners {
            let (lx, ly) = rotate_point((x, y), pivot, -rot);
            lx0 = lx0.min(lx);
            ly0 = ly0.min(ly);
            lx1 = lx1.max(lx);
            ly1 = ly1.max(ly);
        }
    }
    // rotated_rect_corners rotates around the rect's own center, so
    // translate the local rect so its center exactly matches pivot
    // (otherwise the drawing axis would be off).
    let lc = ((lx0 + lx1) / 2.0, (ly0 + ly1) / 2.0);
    let (ox, oy) = (pivot.0 - lc.0, pivot.1 - lc.1);
    Some((lx0 + ox, ly0 + oy, lx1 + ox, ly1 + oy))
}

/// A common property's display value across a multi-select. Even `Mixed`
/// carries the first item's value (used as the starting value when a
/// numeric field is focused for editing; retrieved via `.value()`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum PropVal<T> {
    /// No selection (the default), or the value agrees across all selected items.
    Uniform(T),
    /// The value differs across selected items.
    Mixed(T),
}

impl<T: Copy> PropVal<T> {
    pub(super) fn value(self) -> T {
        match self {
            PropVal::Uniform(v) | PropVal::Mixed(v) => v,
        }
    }
}

/// The common style across selected items (`Uniform` with the default if
/// there are none, `Uniform` if they all agree, `Mixed` if they differ).
/// Color applies to Arrow/Rect/Ellipse/Text, thickness to
/// Arrow/Rect/Ellipse, text size only to Text.
pub(super) type StyleVals = (
    PropVal<u32>,
    PropVal<i64>,
    PropVal<f32>,
    PropVal<bool>,
    PropVal<f32>,
    PropVal<bool>,
);

pub(super) fn common_style(
    items: &[&Annotation],
    defaults: (u32, i64, f32, bool, f32, bool),
) -> StyleVals {
    fn fold<T: PartialEq + Copy>(vals: impl Iterator<Item = T>, default: T) -> PropVal<T> {
        let mut it = vals;
        let Some(first) = it.next() else {
            return PropVal::Uniform(default);
        };
        if it.all(|v| v == first) {
            PropVal::Uniform(first)
        } else {
            PropVal::Mixed(first)
        }
    }
    let color = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Arrow { color, .. }
            | Annotation::Polyline { color, .. }
            | Annotation::Rect { color, .. }
            | Annotation::Ellipse { color, .. }
            | Annotation::Text { color, .. }
            | Annotation::NumberMarker { color, .. } => Some(*color),
            _ => None,
        }),
        defaults.0,
    );
    let thick = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Arrow { thick, .. }
            | Annotation::Polyline { thick, .. }
            | Annotation::Rect { thick, .. }
            | Annotation::Ellipse { thick, .. } => Some(*thick),
            _ => None,
        }),
        defaults.1,
    );
    let size = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Text { size, .. } | Annotation::NumberMarker { size, .. } => Some(*size),
            _ => None,
        }),
        defaults.2,
    );
    let filled = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Rect { filled, .. } | Annotation::Ellipse { filled, .. } => Some(*filled),
            _ => None,
        }),
        defaults.3,
    );
    let block = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Mosaic { block, .. } => Some(*block),
            _ => None,
        }),
        defaults.4,
    );
    let is_blur = fold(
        items.iter().filter_map(|a| match a {
            Annotation::Mosaic { mode, .. } => Some(matches!(mode, MosaicMode::Blur)),
            _ => None,
        }),
        defaults.5,
    );
    (color, thick, size, filled, block, is_blur)
}

/// Builds RGBA8 from a color buffer plus a coverage sentinel buffer.
/// Pixels still at the sentinel (untouched by any item) become fully transparent.
pub(super) fn compose_rgba(color: &[u32], mask: &[u32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(color.len() * 4);
    for (c, m) in color.iter().zip(mask.iter()) {
        if *m == EXPORT_SENTINEL {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            rgba.push(((c >> 16) & 0xff) as u8);
            rgba.push(((c >> 8) & 0xff) as u8);
            rgba.push((c & 0xff) as u8);
            rgba.push(255);
        }
    }
    rgba
}

/// The frontmost item's index (checked last-first) under the cursor.
/// Tested against the line segment for Arrow/Polyline, the bbox for
/// others. The guide is never selectable via the Select tool (adjusted
/// only through the Guide tool).
pub(super) fn hit_item(
    annotations: &[Annotation],
    p: (i64, i64),
    tol: f64,
    text: Option<&TextRenderer>,
) -> Option<usize> {
    for (i, ann) in annotations.iter().enumerate().rev() {
        let inside = match ann {
            Annotation::Guide { .. } => false,
            Annotation::Arrow { a, b, .. } => dist_point_seg(p, *a, *b) <= tol,
            Annotation::Polyline { points, .. } => points
                .windows(2)
                .any(|w| dist_point_seg(p, w[0], w[1]) <= tol),
            // Tested against a rotated Rect/Image/Mosaic's actual rotated
            // outline, not its axis-aligned bounding rect (which extends
            // beyond the visible shape) — so clickable area matches what's
            // shown. Converting the click point to the item's own
            // unrotated local coordinates reduces this to a simple
            // axis-aligned range check (same technique as handle
            // hit-testing). Image/Mosaic are actually rects, so they share
            // this test.
            Annotation::Rect { r, rot, .. }
            | Annotation::Image { r, rot, .. }
            | Annotation::Mosaic { r, rot, .. } => {
                let lp = to_local(p, *r, *rot);
                let (x0, y0, x1, y1) = rect_norm_f64(*r);
                lp.0 >= x0 - tol && lp.0 <= x1 + tol && lp.1 >= y0 - tol && lp.1 <= y1 + tol
            }
            // Likewise, a rotated ellipse is tested against the actual
            // ellipse equation, not the bounding rect — same idea of
            // converting the click point to local coordinates first as
            // Rect, but using the ellipse equation instead of a range check.
            Annotation::Ellipse { r, rot, .. } => {
                let lp = to_local(p, *r, *rot);
                let (x0, y0, x1, y1) = rect_norm_f64(*r);
                let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                let (rx, ry) = (
                    ((x1 - x0) / 2.0 + tol).max(0.1),
                    ((y1 - y0) / 2.0 + tol).max(0.1),
                );
                (((lp.0 - cx) / rx).powi(2) + ((lp.1 - cy) / ry).powi(2)) <= 1.0
            }
            // Text uses the same "convert to local coordinates then
            // axis-aligned range check," but its rect is built on demand
            // from a font measurement (since it has no `r`).
            Annotation::Text {
                pos,
                text: s,
                size,
                rot,
                ..
            } => {
                let r = text_local_rect(*pos, s, *size, text);
                let lp = to_local(p, r, *rot);
                let (x0, y0, x1, y1) = rect_norm_f64(r);
                lp.0 >= x0 - tol && lp.0 <= x1 + tol && lp.1 >= y0 - tol && lp.1 <= y1 + tol
            }
            // Always a true circle, so rotation doesn't matter — distance from the center is enough.
            Annotation::NumberMarker { pos, size, .. } => {
                let d = ((p.0 - pos.0) as f64).hypot((p.1 - pos.1) as f64);
                d <= *size as f64 + tol
            }
        };
        if inside {
            return Some(i);
        }
    }
    None
}

/// Indices of non-guide items intersecting `rect`, ascending (for
/// marquee selection). Arrow is tested against its bbox like other types
/// here (unlike `hit_item`'s line-segment test).
pub(super) fn hit_items_in_rect(
    annotations: &[Annotation],
    rect: (i64, i64, i64, i64),
    text: Option<&TextRenderer>,
) -> Vec<usize> {
    let (rx0, ry0, rx1, ry1) = rect;
    annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| !matches!(a, Annotation::Guide { .. }))
        .filter(|(_, a)| match a {
            // Tested against a rotated Rect/Image/Mosaic's actual rotated
            // outline, not its axis-aligned bounding rect (which extends
            // beyond the visible shape) — so the marquee's selectable area
            // matches what's shown. An ellipse is approximated as its
            // bounding rect rotated (an exact circle-vs-rotated-rect
            // intersection test would get too complex, so this settles
            // for the existing rect-approximation precision). Image/Mosaic
            // are actually rects, so they share this test.
            Annotation::Rect { r, rot, .. }
            | Annotation::Ellipse { r, rot, .. }
            | Annotation::Image { r, rot, .. }
            | Annotation::Mosaic { r, rot, .. } => rotated_rect_intersects_rect(*r, *rot, rect),
            // Text builds its local rect on demand from a font
            // measurement, then reuses the same test.
            Annotation::Text {
                pos,
                text: s,
                size,
                rot,
                ..
            } => {
                let r = text_local_rect(*pos, s, *size, text);
                rotated_rect_intersects_rect(r, *rot, rect)
            }
            _ => {
                let (x0, y0, x1, y1) = item_bbox(a, text);
                x0 < rx1 && x1 > rx0 && y0 < ry1 && y1 > ry0
            }
        })
        .map(|(i, _)| i)
        .collect()
}

/// Tests whether a rotated rect `r`(+`rot`) intersects an axis-aligned
/// rect `rect`, via the Separating Axis Theorem (SAT). Since these are two
/// convex quadrilaterals (one axis-aligned, one at an arbitrary angle),
/// checking the 4 candidate separating axes from their edge normals — the
/// axis-aligned side's (1,0)/(0,1), and the rotated side's own angle plus
/// its perpendicular — is sufficient.
fn rotated_rect_intersects_rect(
    r: (f64, f64, f64, f64),
    rot: f64,
    rect: (i64, i64, i64, i64),
) -> bool {
    let (x0, y0, x1, y1) = rect_norm_f64(r);
    let c = rect_center(r);
    let item_corners =
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| rotate_point((x, y), c, rot));
    let (rx0, ry0, rx1, ry1) = rect;
    let rect_corners = [
        (rx0 as f64, ry0 as f64),
        (rx1 as f64, ry0 as f64),
        (rx1 as f64, ry1 as f64),
        (rx0 as f64, ry1 as f64),
    ];
    let project = |pts: &[(f64, f64); 4], axis: (f64, f64)| -> (f64, f64) {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for &(px, py) in pts {
            let d = px * axis.0 + py * axis.1;
            lo = lo.min(d);
            hi = hi.max(d);
        }
        (lo, hi)
    };
    let axes = [
        (1.0, 0.0),
        (0.0, 1.0),
        (rot.cos(), rot.sin()),
        (-rot.sin(), rot.cos()),
    ];
    for axis in axes {
        let (a_lo, a_hi) = project(&item_corners, axis);
        let (b_lo, b_hi) = project(&rect_corners, axis);
        if a_hi < b_lo || b_hi < a_lo {
            return false;
        }
    }
    true
}

/// The resize cursor shape for a handle.
pub(super) fn handle_cursor(h: Handle) -> winit::window::CursorIcon {
    use winit::window::CursorIcon;
    match h {
        Handle::TopLeft | Handle::BottomRight => CursorIcon::NwseResize,
        Handle::TopRight | Handle::BottomLeft => CursorIcon::NeswResize,
        Handle::Top | Handle::Bottom => CursorIcon::NsResize,
        Handle::Left | Handle::Right => CursorIcon::EwResize,
    }
}

/// Draws the bounding rect's outline plus rotate handle (and all 8
/// resize handles too, if `with_resize_handles`). `Rect`/`Ellipse`/`Image`
/// draw all 8 handles; `Text` draws only the rotate handle, since its
/// size is changed via the "Text size" numeric field, not a drag.
fn paint_rotated_selection_chrome(
    canvas: &mut Canvas,
    r: (f64, f64, f64, f64),
    rot: f64,
    t: &Xform,
    with_resize_handles: bool,
) {
    let corners = rotated_rect_corners(r, rot).map(|p| t.map(p));
    for i in 0..4 {
        let (x0, y0) = corners[i];
        let (x1, y1) = corners[(i + 1) % 4];
        canvas.line(x0, y0, x1, y1, 1, SEL_COLOR);
        round_join(canvas, (x0, y0), 1, SEL_COLOR);
    }
    if with_resize_handles {
        for (_, q) in rect_handles_f64(r) {
            draw_handle_circle(canvas, t.map(from_local(q, r, rot)));
        }
    }
    // The rotate handle, and its connecting line from the top-edge center.
    let (x0, y0, x1, _) = rect_norm_f64(r);
    let top_mid = from_local(((x0 + x1) / 2.0, y0), r, rot);
    let handle = from_local(rotate_handle_local(r), r, rot);
    let (tx, ty) = t.map(top_mid);
    let (hx, hy) = t.map(handle);
    canvas.line(tx, ty, hx, hy, 1, SEL_COLOR);
    draw_handle_circle(canvas, (hx, hy));
}

/// Draws the selection outline (border plus handles, accent color), projected to screen coordinates.
pub(super) fn paint_selection(
    canvas: &mut Canvas,
    ann: &Annotation,
    t: &Xform,
    text: Option<&TextRenderer>,
) {
    match ann {
        Annotation::Arrow { a, b, .. } => {
            draw_handle_circle(canvas, t.map(*a));
            draw_handle_circle(canvas, t.map(*b));
        }
        Annotation::Polyline { points, .. } => {
            for &p in points {
                draw_handle_circle(canvas, t.map(p));
            }
        }
        // Rect/Ellipse/Image/Mosaic share the same "local rect plus
        // rotation angle" selection outline (bounding rect, 8 handles,
        // rotate handle).
        Annotation::Rect { r, rot, .. }
        | Annotation::Ellipse { r, rot, .. }
        | Annotation::Image { r, rot, .. }
        | Annotation::Mosaic { r, rot, .. } => {
            paint_rotated_selection_chrome(canvas, *r, *rot, t, true);
        }
        Annotation::Guide { r } => {
            paint_bbox(canvas, rect_norm(*r), t);
            for (_, q) in rect_handles(*r) {
                draw_handle_circle(canvas, t.map(q));
            }
        }
        // Text has no resize handles (size is changed via the numeric
        // field), so only the rotate handle. Its local rect is built on
        // demand from a font measurement.
        Annotation::Text {
            pos,
            text: s,
            size,
            rot,
            ..
        } => {
            let r = text_local_rect(*pos, s, *size, text);
            paint_rotated_selection_chrome(canvas, r, *rot, t, false);
        }
        // No handles shown — there's no resize/rotate handle, and since
        // the marker is already drawn as a circle, nothing extra needs
        // drawing for its selection outline.
        Annotation::NumberMarker { .. } => {}
    }
}

/// Draws a bbox's thin (1px) outline; also reused for the marquee-selection rect preview.
pub(super) fn paint_bbox(canvas: &mut Canvas, r: (i64, i64, i64, i64), t: &Xform) {
    let (x0, y0) = t.map((r.0, r.1));
    let (x1, y1) = t.map((r.2, r.3));
    canvas.line(x0, y0, x1, y0, 1, SEL_COLOR);
    canvas.line(x0, y1, x1, y1, 1, SEL_COLOR);
    canvas.line(x0, y0, x0, y1, 1, SEL_COLOR);
    canvas.line(x1, y0, x1, y1, 1, SEL_COLOR);
    for corner in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        round_join(canvas, corner, 1, SEL_COLOR);
    }
}

/// Draws the 8 resize handles plus rotate handle on the multi-select
/// bounding rect (the local rect plus rotation angle returned by
/// `group_frame`) — drawn exactly like a single `Rect`'s selection
/// outline, so the outline itself appears rotated when `rot` is nonzero.
/// The actual transform is distributed proportionally to each item based
/// on this bounding rect (`scale_annotation_rotated`/`rotate_annotation_around`).
pub(super) fn paint_group_handles(
    canvas: &mut Canvas,
    rect: (f64, f64, f64, f64),
    rot: f64,
    t: &Xform,
) {
    let corners = rotated_rect_corners(rect, rot).map(|p| t.map(p));
    for i in 0..4 {
        let (x0, y0) = corners[i];
        let (x1, y1) = corners[(i + 1) % 4];
        canvas.line(x0, y0, x1, y1, 1, SEL_COLOR);
        round_join(canvas, (x0, y0), 1, SEL_COLOR);
    }
    for (_, q) in rect_handles_f64(rect) {
        draw_handle_circle(canvas, t.map(from_local(q, rect, rot)));
    }
    // The rotate handle, and its connecting line from the top-edge center.
    let (x0, y0, x1, _) = rect_norm_f64(rect);
    let top_mid = from_local(((x0 + x1) / 2.0, y0), rect, rot);
    let handle = from_local(rotate_handle_local(rect), rect, rot);
    let (tx, ty) = t.map(top_mid);
    let (hx, hy) = t.map(handle);
    canvas.line(tx, ty, hx, hy, 1, SEL_COLOR);
    draw_handle_circle(canvas, (hx, hy));
}

/// Draws a handle's small circle (white fill, accent border) centered at
/// screen coordinate `c`. Using a circle instead of a square lets a
/// rotated item's handles be drawn the same way, without needing to
/// rotate each handle's own shape.
fn draw_handle_circle(canvas: &mut Canvas, c: (i64, i64)) {
    canvas.fill_circle(c.0, c.1, HANDLE_HALF, SEL_COLOR);
    canvas.fill_circle(c.0, c.1, HANDLE_HALF - 1, 0x00FF_FFFF);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{DEFAULT_BLOCK, DEFAULT_COLOR, DEFAULT_THICK};

    #[test]
    fn hit_rect_handle_picks_corner_and_edge() {
        let r = (10, 20, 30, 40);
        assert_eq!(hit_rect_handle(r, (10, 20), 3.0), Some(Handle::TopLeft));
        assert_eq!(hit_rect_handle(r, (20, 40), 3.0), Some(Handle::Bottom));
        assert_eq!(hit_rect_handle(r, (30, 30), 3.0), Some(Handle::Right));
        // Near the center hits no handle.
        assert_eq!(hit_rect_handle(r, (20, 30), 3.0), None);
    }

    #[test]
    fn resize_rect_moves_matching_edges_and_renormalizes() {
        let r = (10, 20, 30, 40);
        // Only the right edge moves.
        assert_eq!(resize_rect(r, Handle::Right, (50, 5)), (10, 20, 50, 40));
        // The top-left corner moves x0/y0.
        assert_eq!(resize_rect(r, Handle::TopLeft, (5, 8)), (5, 8, 30, 40));
        // Dragging the right edge past the left edge flips and renormalizes.
        assert_eq!(resize_rect(r, Handle::Right, (0, 5)), (0, 20, 10, 40));
    }

    #[test]
    fn resize_rotated_rect_keeps_opposite_corner_fixed_in_world_space() {
        // Dragging a rotated rect's BottomRight shouldn't move the
        // opposite corner (TopLeft) on screen (if it did, the resize
        // would look wrong). The correction shifts the whole rect, so
        // TopLeft's local coordinates can change, but its world position
        // after "rotating around the rect's own center" should stay fixed.
        let orig = (0.0, 0.0, 40.0, 20.0);
        let rot = 0.4_f64;
        let anchor_world_before = rotate_point((0.0, 0.0), rect_center(orig), rot);

        let new_r = resize_rotated_rect(orig, Handle::BottomRight, (90.0, 70.0), rot);
        let new_anchor_local = (new_r.0, new_r.1);
        let anchor_world_after = rotate_point(new_anchor_local, rect_center(new_r), rot);
        // Now computed entirely in float, so no rounding-error tolerance
        // should be needed (only a tiny float arithmetic error tolerance).
        assert!(
            (anchor_world_before.0 - anchor_world_after.0).abs() < 1e-6
                && (anchor_world_before.1 - anchor_world_after.1).abs() < 1e-6,
            "before={anchor_world_before:?} after={anchor_world_after:?}"
        );
    }

    #[test]
    fn resize_rotated_rect_matches_plain_resize_when_unrotated() {
        let orig_i = (10, 20, 30, 40);
        let orig_f = (10.0, 20.0, 30.0, 40.0);
        let plain = resize_rect(orig_i, Handle::Right, (50, 5));
        let via_rotated = resize_rotated_rect(orig_f, Handle::Right, (50.0, 5.0), 0.0);
        assert_eq!(
            via_rotated,
            (
                plain.0 as f64,
                plain.1 as f64,
                plain.2 as f64,
                plain.3 as f64
            )
        );
    }

    #[test]
    fn resize_rotated_rect_stays_continuous_for_a_smooth_drag() {
        // Now that this stays in float, verify the output also changes
        // continuously for continuous cursor input (no non-finite values,
        // no large discontinuities), brute-forced across every handle and
        // rotation angle (a regression test for the jitter fix).
        let handles = [
            Handle::TopLeft,
            Handle::TopRight,
            Handle::BottomRight,
            Handle::BottomLeft,
            Handle::Top,
            Handle::Right,
            Handle::Bottom,
            Handle::Left,
        ];
        let orig = (0.0, 0.0, 40.0, 20.0);
        let mut failures = Vec::new();
        for &h in &handles {
            for rot_i in 0..48 {
                let rot = rot_i as f64 * std::f64::consts::PI / 24.0;
                let mut prev: Option<(f64, f64, f64, f64)> = None;
                for step in 0..300 {
                    let t = step as f64 * 0.07;
                    let p = (60.0 + t * 1.31, 45.0 + t * 0.71);
                    let r = resize_rotated_rect(orig, h, p, rot);
                    assert!(
                        r.0.is_finite() && r.1.is_finite() && r.2.is_finite() && r.3.is_finite(),
                        "非有限な値が出た: handle={h:?} rot={rot} r={r:?}"
                    );
                    if let Some(pr) = prev {
                        // The input moves ~0.1px per step, so a change
                        // exceeding 1.0px is treated as a discontinuous jump.
                        let jump = (r.0 - pr.0)
                            .abs()
                            .max((r.1 - pr.1).abs())
                            .max((r.2 - pr.2).abs())
                            .max((r.3 - pr.3).abs());
                        if jump > 1.0 {
                            failures.push((h, rot, step, pr, r));
                        }
                    }
                    prev = Some(r);
                }
            }
        }
        assert!(
            failures.is_empty(),
            "不連続なジャンプが {} 件見つかった。最初の数件: {:#?}",
            failures.len(),
            &failures[..failures.len().min(10)]
        );
    }

    #[test]
    fn resize_rotated_rect_aspect_keeps_ratio_when_unrotated() {
        // Original 20x10 (ar=2.0), dragged far on BottomRight. Keeps
        // ar=2 even at a position where height would otherwise dominate
        // (at rot=0, resize_rotated_rect_aspect should behave the same as
        // the old resize_rect_aspect).
        let r = (0.0, 0.0, 20.0, 10.0);
        let out = resize_rotated_rect_aspect(r, Handle::BottomRight, (30.0, 40.0), 0.0, 2.0);
        let (x0, y0, x1, y1) = out;
        let (w, h) = (x1 - x0, y1 - y0);
        assert!((w / h - 2.0).abs() < 0.05, "比率が保たれていない: {w}x{h}");
        // The fixed point (top-left) doesn't move.
        assert!((x0 - 0.0).abs() < 1e-6 && (y0 - 0.0).abs() < 1e-6);
    }

    #[test]
    fn resize_rotated_rect_aspect_edge_centers_other_axis_when_unrotated() {
        // Drag the right edge: height is derived from width via ar, keeping the vertical center (original center y=50).
        let r = (0.0, 40.0, 10.0, 60.0); // center y=50, height 20
        let out = resize_rotated_rect_aspect(r, Handle::Right, (40.0, 999.0), 0.0, 2.0);
        let (x0, y0, x1, y1) = out;
        let (w, h) = (x1 - x0, y1 - y0);
        assert!((w / h - 2.0).abs() < 0.05);
        assert!((x0 - 0.0).abs() < 1e-6); // opposite edge (left) is fixed
        assert!(((y0 + y1) / 2.0 - 50.0).abs() < 1e-6); // vertical center unchanged
    }

    #[test]
    fn resize_rotated_rect_aspect_keeps_opposite_corner_fixed_in_world_space_when_rotated() {
        // Same verification approach as Rect's own resize_rotated_rect:
        // resizing a rotated rect with aspect ratio locked should still
        // leave the opposite corner's (TopLeft) world coordinates unchanged.
        let orig = (0.0, 0.0, 40.0, 20.0); // ar = 2.0
        let rot = 0.4_f64;
        let anchor_world_before = rotate_point((0.0, 0.0), rect_center(orig), rot);

        let new_r = resize_rotated_rect_aspect(orig, Handle::BottomRight, (90.0, 50.0), rot, 2.0);
        let anchor_world_after = rotate_point((new_r.0, new_r.1), rect_center(new_r), rot);
        assert!(
            (anchor_world_before.0 - anchor_world_after.0).abs() < 1e-6
                && (anchor_world_before.1 - anchor_world_after.1).abs() < 1e-6,
            "before={anchor_world_before:?} after={anchor_world_after:?}"
        );
        // The ratio should be preserved too.
        let (w, h) = (new_r.2 - new_r.0, new_r.3 - new_r.1);
        assert!((w / h - 2.0).abs() < 0.05, "比率が保たれていない: {w}x{h}");
    }

    #[test]
    fn dist_point_seg_matches_geometry() {
        // Horizontal segment (0,0)-(10,0).
        assert!((dist_point_seg((5, 3), (0, 0), (10, 0)) - 3.0).abs() < 1e-9);
        // Beyond an endpoint, distance is to that endpoint.
        assert!((dist_point_seg((-3, 0), (0, 0), (10, 0)) - 3.0).abs() < 1e-9);
        // On the segment, it's 0.
        assert!(dist_point_seg((5, 0), (0, 0), (10, 0)) < 1e-9);
    }

    #[test]
    fn rotate_point_90deg_maps_axis_point_predictably() {
        // A point in the +x direction from center (0,0), rotated 90°,
        // ends up in the +y direction (per this rotation convention, on a
        // screen coordinate system with y pointing down).
        let (x, y) = rotate_point((10.0, 0.0), (0.0, 0.0), std::f64::consts::FRAC_PI_2);
        assert!((x - 0.0).abs() < 1e-6, "x={x}");
        assert!((y - 10.0).abs() < 1e-6, "y={y}");
    }

    #[test]
    fn snap_angle_45_rounds_to_nearest_45_degree_step() {
        let deg = |d: f64| d.to_radians();
        let step = std::f64::consts::FRAC_PI_4;
        // Exact 45° steps pass through unchanged.
        assert!((snap_angle_45(0.0) - 0.0).abs() < 1e-9);
        assert!((snap_angle_45(step) - step).abs() < 1e-9);
        assert!((snap_angle_45(2.0 * step) - 2.0 * step).abs() < 1e-9);
        // 44°/46° each round to 45°.
        assert!((snap_angle_45(deg(44.0)) - step).abs() < 1e-9);
        assert!((snap_angle_45(deg(46.0)) - step).abs() < 1e-9);
        // Negative angles round the same way.
        assert!((snap_angle_45(deg(-44.0)) - (-step)).abs() < 1e-9);
        // An angle crossing the 360° boundary (e.g. 359°) also rounds to the nearest 45° step (=360°/0°).
        assert!((snap_angle_45(deg(359.0)) - deg(360.0)).abs() < 1e-9);
    }

    #[test]
    fn to_local_and_from_local_are_inverses() {
        let r = (10.0, 10.0, 50.0, 30.0);
        let rot = 0.6_f64;
        let p = (37, 5);
        let local = to_local(p, r, rot);
        let back = from_local(local, r, rot);
        // Round-trips, allowing for rounding error.
        assert!(near(back, p, 1.0), "back={back:?} p={p:?}");
    }

    #[test]
    fn item_bbox_rect_rotated_returns_aabb_of_corners() {
        let flat = rect_item((0, 0, 40, 20));
        let (fx0, fy0, fx1, fy1) = item_bbox(&flat, None);

        let rotated = Annotation::Rect {
            r: (0.0, 0.0, 40.0, 20.0),
            color: 0,
            thick: 1,
            rot: std::f64::consts::FRAC_PI_4,
            filled: false,
        };
        let (rx0, ry0, rx1, ry1) = item_bbox(&rotated, None);

        // A 45° rotation makes the bounding rect larger than the original (unless it's a square).
        assert!(rx1 - rx0 > fx1 - fx0 || ry1 - ry0 > fy1 - fy0);
        // The center doesn't change.
        assert_eq!((fx0 + fx1) / 2, (rx0 + rx1) / 2);
        assert_eq!((fy0 + fy1) / 2, (ry0 + ry1) / 2);
    }

    #[test]
    fn item_bbox_image_rotated_returns_aabb_of_corners() {
        // Image has exactly the same "rect plus rotation angle" shape as Rect, so it should behave the same.
        let flat = image_item((0, 0), 40, 20);
        let (fx0, fy0, fx1, fy1) = item_bbox(&flat, None);

        let rotated = Annotation::Image {
            r: (0.0, 0.0, 40.0, 20.0),
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot: std::f64::consts::FRAC_PI_4,
        };
        let (rx0, ry0, rx1, ry1) = item_bbox(&rotated, None);

        // A 45° rotation makes the bounding rect larger than the original.
        assert!(rx1 - rx0 > fx1 - fx0 || ry1 - ry0 > fy1 - fy0);
        // The center doesn't change.
        assert_eq!((fx0 + fx1) / 2, (rx0 + rx1) / 2);
        assert_eq!((fy0 + fy1) / 2, (ry0 + ry1) / 2);
    }

    #[test]
    fn item_bbox_text_rotated_returns_aabb_of_corners() {
        // Text builds its local rect on demand from a font measurement, then uses the same formula.
        let flat = Annotation::Text {
            pos: (0, 0),
            text: "Hi".into(),
            color: DEFAULT_COLOR,
            size: 20.0,
            rot: 0.0,
        };
        let (fx0, fy0, fx1, fy1) = item_bbox(&flat, None);

        let rotated = Annotation::Text {
            pos: (0, 0),
            text: "Hi".into(),
            color: DEFAULT_COLOR,
            size: 20.0,
            rot: std::f64::consts::FRAC_PI_4,
        };
        let (rx0, ry0, rx1, ry1) = item_bbox(&rotated, None);

        assert!(rx1 - rx0 > fx1 - fx0 || ry1 - ry0 > fy1 - fy0);
        assert_eq!((fx0 + fx1) / 2, (rx0 + rx1) / 2);
        assert_eq!((fy0 + fy1) / 2, (ry0 + ry1) / 2);
    }

    #[test]
    fn item_bbox_number_marker_is_a_square_around_pos_sized_by_radius() {
        let ann = Annotation::NumberMarker {
            pos: (10, 20),
            number: 1,
            color: DEFAULT_COLOR,
            size: 8.0,
        };
        assert_eq!(item_bbox(&ann, None), (2, 12, 18, 28));
    }

    #[test]
    fn item_bbox_ellipse_rotated_matches_closed_form_and_sampled_outline() {
        // An ellipse with rx=40, ry=20, rotated 30°. The bounding rect
        // should be given by the closed form
        // hw=sqrt((rx*cos)^2+(ry*sin)^2), hh=sqrt((rx*sin)^2+(ry*cos)^2),
        // and that value should closely match the min/max measured by
        // finely sampling the outline.
        let (cx, cy) = (100.0, 50.0);
        let (rx, ry) = (40.0, 20.0);
        let rot = 30.0_f64.to_radians();
        let ann = Annotation::Ellipse {
            r: (cx - rx, cy - ry, cx + rx, cy + ry),
            color: DEFAULT_COLOR,
            thick: 1,
            rot,
            filled: false,
        };
        let (bx0, by0, bx1, by1) = item_bbox(&ann, None);

        let mut sx0 = f64::MAX;
        let mut sy0 = f64::MAX;
        let mut sx1 = f64::MIN;
        let mut sy1 = f64::MIN;
        let steps = 3600;
        for i in 0..steps {
            let ang = i as f64 / steps as f64 * std::f64::consts::TAU;
            let local = (cx + rx * ang.cos(), cy + ry * ang.sin());
            let (wx, wy) = rotate_point(local, (cx, cy), rot);
            sx0 = sx0.min(wx);
            sy0 = sy0.min(wy);
            sx1 = sx1.max(wx);
            sy1 = sy1.max(wy);
        }

        assert!((bx0 as f64 - sx0).abs() <= 1.0, "bx0={bx0} sx0={sx0}");
        assert!((by0 as f64 - sy0).abs() <= 1.0, "by0={by0} sy0={sy0}");
        assert!((bx1 as f64 - sx1).abs() <= 1.0, "bx1={bx1} sx1={sx1}");
        assert!((by1 as f64 - sy1).abs() <= 1.0, "by1={by1} sy1={sy1}");
    }

    #[test]
    fn item_bbox_polyline_returns_min_max_of_all_points() {
        // item_bbox stays tight (for the selection outline/hit-testing).
        let ann = Annotation::Polyline {
            points: vec![(10, 50), (30, 5), (100, 20)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        assert_eq!(item_bbox(&ann, None), (10, 5, 100, 50));
    }

    #[test]
    fn item_export_bbox_polyline_is_padded_by_half_thickness() {
        let ann = Annotation::Polyline {
            points: vec![(10, 50), (30, 5), (100, 20)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        // Since the line is drawn ±(thick/2) around the center, it extends
        // beyond the points' min/max by that much (DEFAULT_THICK=4 ->
        // pad=2, so the outer half of the line isn't clipped on export).
        // This padding is only added by the export-only item_export_bbox
        // (item_bbox itself stays tight).
        let pad = DEFAULT_THICK / 2;
        assert_eq!(
            item_export_bbox(&ann, None),
            (10 - pad, 5 - pad, 100 + pad, 50 + pad)
        );
    }

    #[test]
    fn item_export_bbox_arrow_tightly_bounds_the_actual_barb_tips() {
        let a = (0, 0);
        let b = (100, 0);
        let thick = DEFAULT_THICK;
        let ann = Annotation::Arrow {
            a,
            b,
            color: DEFAULT_COLOR,
            thick,
        };
        // item_bbox (for the selection outline/hit-testing) stays the tight min/max of a/b.
        assert_eq!(item_bbox(&ann, None), (0, 0, 100, 0));

        let (bx0, by0, bx1, by1) = item_export_bbox(&ann, None);
        let pad = (thick.max(1) + 1) / 2;

        // Both arrowhead tips are actually included in the bounding rect (not clipped).
        let tips = arrow_barb_tips(a, b, thick).unwrap();
        for (tx, ty) in tips {
            assert!(
                tx >= (bx0 + pad) as f64 && tx <= (bx1 - pad) as f64,
                "tx={tx}"
            );
            assert!(
                ty >= (by0 + pad) as f64 && ty <= (by1 - pad) as f64,
                "ty={ty}"
            );
        }
        // The barbs only ever extend toward `a` (the reverse direction),
        // so no extra padding should appear beyond `b` in the opposite
        // direction (+x for this arrow) — this should be tighter than the
        // previous implementation, which expanded uniformly around `b` by
        // `arrow_head_len`.
        let head_pad = arrow_head_len(thick).ceil() as i64;
        assert!(
            bx1 < b.0 + head_pad,
            "bx1={bx1} は矢の延長線上まで無駄に広がっている"
        );
    }

    #[test]
    fn item_export_bbox_rect_filled_has_no_stroke_padding() {
        // A filled shape ignores thick when drawing, so the bounding rect needs no padding either.
        let ann = Annotation::Rect {
            r: (0.0, 0.0, 40.0, 20.0),
            color: DEFAULT_COLOR,
            thick: 20,
            rot: 0.0,
            filled: true,
        };
        assert_eq!(item_export_bbox(&ann, None), (0, 0, 40, 20));
    }

    #[test]
    fn draw_ellipse_paints_a_ring_wider_than_tall_for_wide_bbox() {
        const SENTINEL: u32 = 0x0012_3456;
        let ann = Annotation::Ellipse {
            r: (5.0, 15.0, 35.0, 25.0), // width 30 x height 10.
            color: 0x0000_0000,
            thick: 1,
            rot: 0.0,
            filled: false,
        };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, false);
        }
        assert!(buf.iter().any(|&px| px != SENTINEL), "何か描かれるはず");

        let mut min_x = usize::MAX;
        let mut max_x = 0usize;
        let mut min_y = usize::MAX;
        let mut max_y = 0usize;
        for y in 0..40 {
            for x in 0..40 {
                if buf[y * 40 + x] != SENTINEL {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(
            max_x - min_x > max_y - min_y,
            "横長のbboxなら描いたリングも横長のはず"
        );
    }

    #[test]
    fn fill_rotated_rect_center_is_full_color_and_far_outside_is_untouched() {
        const SENTINEL: u32 = 0x0012_3456;
        const FG: u32 = 0x00ff_0000;
        let (w, h) = (40, 40);
        let mut buf = vec![SENTINEL; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            fill_rotated_rect(&mut canvas, 20.0, 20.0, 8.0, 5.0, 0.0, FG);
        }
        assert_eq!(buf[20 * w + 20], FG, "中心は完全に前景色");
        assert_eq!(
            buf[20 * w + 2],
            SENTINEL,
            "半径から十分離れた点は塗られないまま"
        );
    }

    #[test]
    fn fill_rotated_ellipse_center_is_full_color_and_far_outside_is_untouched() {
        const SENTINEL: u32 = 0x0012_3456;
        const FG: u32 = 0x00ff_0000;
        let (w, h) = (40, 40);
        let mut buf = vec![SENTINEL; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w,
                h,
                scale: 1.0,
            };
            fill_rotated_ellipse(&mut canvas, 20.0, 20.0, 8.0, 5.0, 0.0, FG);
        }
        assert_eq!(buf[20 * w + 20], FG, "中心は完全に前景色");
        assert_eq!(
            buf[20 * w + 2],
            SENTINEL,
            "半径から十分離れた点は塗られないまま"
        );
    }

    #[test]
    fn paint_one_filled_rect_paints_solid_interior_unfilled_stays_a_ring() {
        const SENTINEL: u32 = 0x0012_3456;
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };

        let filled = Annotation::Rect {
            r: (5.0, 5.0, 35.0, 35.0),
            color: 0x0000_0000,
            thick: 1,
            rot: 0.0,
            filled: true,
        };
        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &filled, &t, None, false);
        }
        assert_ne!(
            buf[20 * 40 + 20],
            SENTINEL,
            "filled: true なら中心も塗られる"
        );

        let unfilled = Annotation::Rect {
            r: (5.0, 5.0, 35.0, 35.0),
            color: 0x0000_0000,
            thick: 1,
            rot: 0.0,
            filled: false,
        };
        let mut buf2 = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf2,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &unfilled, &t, None, false);
        }
        assert_eq!(
            buf2[20 * 40 + 20],
            SENTINEL,
            "filled: false は従来通り中心は塗られないリングのまま"
        );
    }

    #[test]
    fn paint_one_filled_ellipse_paints_solid_interior_unfilled_stays_a_ring() {
        const SENTINEL: u32 = 0x0012_3456;
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };

        let filled = Annotation::Ellipse {
            r: (5.0, 5.0, 35.0, 35.0),
            color: 0x0000_0000,
            thick: 1,
            rot: 0.0,
            filled: true,
        };
        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &filled, &t, None, false);
        }
        assert_ne!(
            buf[20 * 40 + 20],
            SENTINEL,
            "filled: true なら中心も塗られる"
        );

        let unfilled = Annotation::Ellipse {
            r: (5.0, 5.0, 35.0, 35.0),
            color: 0x0000_0000,
            thick: 1,
            rot: 0.0,
            filled: false,
        };
        let mut buf2 = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf2,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &unfilled, &t, None, false);
        }
        assert_eq!(
            buf2[20 * 40 + 20],
            SENTINEL,
            "filled: false は従来通り中心は塗られないリングのまま"
        );
    }

    fn rect_item(r: (i64, i64, i64, i64)) -> Annotation {
        Annotation::Rect {
            r: (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        }
    }

    fn mosaic_item(r: (i64, i64, i64, i64)) -> Annotation {
        Annotation::Mosaic {
            r: (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64),
            rot: 0.0,
            block: DEFAULT_BLOCK,
            mode: MosaicMode::Pixelate,
            seed: 0,
        }
    }

    #[test]
    fn hit_item_prefers_topmost() {
        // Two overlapping rects; the last one (frontmost) wins.
        let items = vec![rect_item((0, 0, 100, 100)), rect_item((10, 10, 50, 50))];
        assert_eq!(hit_item(&items, (20, 20), 4.0, None), Some(1));
        // Outside the front one but inside the back one hits the back one.
        assert_eq!(hit_item(&items, (80, 80), 4.0, None), Some(0));
        // Outside both is None.
        assert_eq!(hit_item(&items, (200, 200), 4.0, None), None);
    }

    #[test]
    fn hit_item_uses_actual_rotated_outline_not_its_aabb() {
        // A square rotated 45° (a diamond). Its axis-aligned bounding rect
        // (AABB) extends beyond the actual outline, so clicking in that
        // "gap" shouldn't select it.
        let items = vec![Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: std::f64::consts::FRAC_PI_4,
            filled: false,
        }];
        // The AABB's (roughly (-2,-2)..(12,12)) top-left corner is outside the diamond's outline.
        assert_eq!(hit_item(&items, (-1, -1), 0.0, None), None);
        // The center (inside the diamond) is of course selected.
        assert_eq!(hit_item(&items, (5, 5), 0.0, None), Some(0));
    }

    #[test]
    fn hit_item_image_uses_actual_rotated_outline_not_its_aabb() {
        // Image shares exactly the same test as Rect, so it's the same diamond scenario.
        let items = vec![Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot: std::f64::consts::FRAC_PI_4,
        }];
        assert_eq!(hit_item(&items, (-1, -1), 0.0, None), None);
        assert_eq!(hit_item(&items, (5, 5), 0.0, None), Some(0));
    }

    #[test]
    fn hit_item_text_uses_actual_rotated_outline_not_its_aabb() {
        // With no font measurement (`text: None`), the local rect falls
        // back to a thin 1px-wide, 20px-tall rect. Even so, the AABB's
        // corner should be outside the outline, and near the center
        // should be inside it (values pre-computed).
        let items = vec![Annotation::Text {
            pos: (0, 0),
            text: String::new(),
            color: DEFAULT_COLOR,
            size: 20.0,
            rot: std::f64::consts::FRAC_PI_4,
        }];
        assert_eq!(hit_item(&items, (-6, 4), 0.0, None), None);
        assert_eq!(hit_item(&items, (0, 10), 0.0, None), Some(0));
    }

    #[test]
    fn hit_item_number_marker_hits_within_radius_and_misses_outside() {
        let items = vec![Annotation::NumberMarker {
            pos: (10, 10),
            number: 1,
            color: DEFAULT_COLOR,
            size: 5.0,
        }];
        assert_eq!(hit_item(&items, (12, 10), 0.0, None), Some(0));
        assert_eq!(hit_item(&items, (20, 10), 0.0, None), None);
        // Also hits slightly outside, within the tolerance tol.
        assert_eq!(hit_item(&items, (16, 10), 1.0, None), Some(0));
    }

    #[test]
    fn hit_item_ellipse_uses_actual_rotated_outline_not_its_aabb() {
        // An ellipse with rx=10,ry=8, rotated 45° (center (10,8)). Its
        // axis-aligned bounding rect (AABB) extends beyond the actual
        // ellipse, so clicking in that "gap" (the AABB's corner)
        // shouldn't select it.
        let items = vec![Annotation::Ellipse {
            r: (0.0, 0.0, 20.0, 16.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: std::f64::consts::FRAC_PI_4,
            filled: false,
        }];
        // The AABB's (roughly (0.9,-1.1)..(19.1,17.1)) top-left corner is outside the ellipse's outline.
        assert_eq!(hit_item(&items, (1, -1), 0.0, None), None);
        // The center (inside the ellipse) is of course selected.
        assert_eq!(hit_item(&items, (10, 8), 0.0, None), Some(0));
    }

    #[test]
    fn hit_item_never_selects_guide_even_when_topmost() {
        // The guide is never selectable via the Select tool (Guide-tool
        // only), to prevent it from being accidentally selected when
        // trying to select another item.
        let items = vec![
            rect_item((0, 0, 100, 100)),
            Annotation::Guide {
                r: (0, 0, 100, 100),
            },
        ];
        assert_eq!(hit_item(&items, (20, 20), 4.0, None), Some(0));
        // With only a guide present, None.
        let guide_only = vec![Annotation::Guide {
            r: (0, 0, 100, 100),
        }];
        assert_eq!(hit_item(&guide_only, (20, 20), 4.0, None), None);
    }

    #[test]
    fn hit_item_polyline_hits_interior_segment_but_misses_off_segment() {
        let items = vec![Annotation::Polyline {
            points: vec![(0, 0), (10, 0), (10, 10)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        }];
        // A point on the second segment ((10,0)-(10,10)).
        assert_eq!(hit_item(&items, (10, 5), 0.0, None), Some(0));
        // A point far from every segment.
        assert_eq!(hit_item(&items, (5, 5), 0.0, None), None);
    }

    #[test]
    fn hit_items_in_rect_finds_contained_and_overlapping_but_not_outside() {
        let items = vec![
            rect_item((10, 10, 20, 20)),     // fully contained
            rect_item((15, 15, 200, 200)),   // partially overlapping
            rect_item((500, 500, 600, 600)), // fully outside
        ];
        let mut hits = hit_items_in_rect(&items, (0, 0, 100, 100), None);
        hits.sort_unstable();
        assert_eq!(hits, vec![0, 1]);
    }

    #[test]
    fn hit_items_in_rect_excludes_guide_even_when_overlapping() {
        let items = vec![
            rect_item((10, 10, 20, 20)),
            Annotation::Guide {
                r: (0, 0, 100, 100),
            },
        ];
        assert_eq!(hit_items_in_rect(&items, (0, 0, 100, 100), None), vec![0]);
    }

    #[test]
    fn hit_items_in_rect_uses_actual_rotated_outline_not_its_aabb() {
        // A square rotated 45° (a diamond). Its axis-aligned bounding rect
        // (AABB) extends beyond the actual outline, so a marquee touching
        // only that "gap" shouldn't select it.
        let item = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: std::f64::consts::FRAC_PI_4,
            filled: false,
        };
        let items = vec![item];
        // A marquee touching the AABB's (roughly (-2,-2)..(12,12))
        // top-left corner, but not the diamond's outline.
        assert_eq!(
            hit_items_in_rect(&items, (-2, -2, 0, 0), None),
            Vec::<usize>::new(),
            "AABB の隙間だけに触れても選択されてはいけない"
        );
        // A marquee touching near the center (inside the diamond) selects it.
        assert_eq!(hit_items_in_rect(&items, (3, 3, 7, 7), None), vec![0]);
    }

    #[test]
    fn hit_items_in_rect_ellipse_uses_its_own_rotated_bbox() {
        // Marquee selection approximates an ellipse as its bounding rect
        // rotated (the same (0,0,20,16) rot=45° ellipse as in item_bbox).
        let item = Annotation::Ellipse {
            r: (0.0, 0.0, 20.0, 16.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: std::f64::consts::FRAC_PI_4,
            filled: false,
        };
        let items = vec![item];
        // A marquee outside the AABB (roughly (0.9,-1.1)..(19.1,17.1)), past the origin.
        assert_eq!(
            hit_items_in_rect(&items, (-10, -10, -5, -5), None),
            Vec::<usize>::new()
        );
        // A marquee touching near the center selects it.
        assert_eq!(hit_items_in_rect(&items, (8, 6, 12, 10), None), vec![0]);
    }

    #[test]
    fn hit_items_in_rect_image_uses_actual_rotated_outline_not_its_aabb() {
        // Image shares exactly the same test as Rect, so it's the same diamond scenario.
        let item = Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot: std::f64::consts::FRAC_PI_4,
        };
        let items = vec![item];
        assert_eq!(
            hit_items_in_rect(&items, (-2, -2, 0, 0), None),
            Vec::<usize>::new(),
            "AABB の隙間だけに触れても選択されてはいけない"
        );
        assert_eq!(hit_items_in_rect(&items, (3, 3, 7, 7), None), vec![0]);
    }

    #[test]
    fn hit_items_in_rect_text_uses_its_own_rotated_local_rect() {
        // The same scenario as
        // hit_item_text_uses_actual_rotated_outline_not_its_aabb: a thin
        // 1px-wide, 20px-tall fallback rect, rotated 45°.
        let item = Annotation::Text {
            pos: (0, 0),
            text: String::new(),
            color: DEFAULT_COLOR,
            size: 20.0,
            rot: std::f64::consts::FRAC_PI_4,
        };
        let items = vec![item];
        // A marquee touching only near the AABB's corner (not the actual thin outline).
        assert_eq!(
            hit_items_in_rect(&items, (-7, 3, -6, 4), None),
            Vec::<usize>::new()
        );
        // A marquee touching near the center.
        assert_eq!(hit_items_in_rect(&items, (-1, 9, 1, 11), None), vec![0]);
    }

    #[test]
    fn hit_items_in_rect_polyline_uses_bbox_of_all_points() {
        let item = Annotation::Polyline {
            points: vec![(0, 0), (10, 0), (10, 10)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        let items = vec![item];
        // A marquee entirely outside the bbox (0,0)-(10,10).
        assert_eq!(
            hit_items_in_rect(&items, (20, 20, 30, 30), None),
            Vec::<usize>::new()
        );
        // A marquee overlapping the bbox.
        assert_eq!(hit_items_in_rect(&items, (5, 5, 15, 15), None), vec![0]);
    }

    fn image_item(pos: (i64, i64), w: i64, h: i64) -> Annotation {
        Annotation::Image {
            r: (
                pos.0 as f64,
                pos.1 as f64,
                (pos.0 + w) as f64,
                (pos.1 + h) as f64,
            ),
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot: 0.0,
        }
    }

    #[test]
    fn image_bbox_uses_display_size() {
        let a = image_item((10, 20), 30, 40);
        assert_eq!(item_bbox(&a, None), (10, 20, 40, 60));
    }

    #[test]
    fn paint_image_at_100_percent_zoom_samples_every_column_exactly() {
        // At 100% zoom (dw==src_w), column dx should always sample the
        // same column sx=dx, even with float error — naively truncating
        // `(dx/dw)*src_w` via `as usize` caused a bug where, e.g. column
        // 15 of a 22-wide image rounded 14.999999999999998 down to 14 due
        // to float error, shifting the sampled column by one.
        const SRC_W: usize = 22;
        // A different color per column, so a shift is immediately detectable
        // (top byte set to 0xFF to mark opacity).
        let pixels: Vec<u32> = (0..SRC_W as u32).map(|v| 0xFF00_0000 | v).collect();
        let mut buf = vec![0u32; SRC_W];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: SRC_W,
                h: 1,
                scale: 1.0,
            };
            let t = Xform {
                scale: 1.0,
                ox: 0.0,
                oy: 0.0,
            };
            paint_image(
                &mut canvas,
                (0.0, 0.0, SRC_W as f64, 1.0),
                0.0,
                SRC_W as i64,
                1,
                &pixels,
                &t,
            );
        }
        for (x, &px) in buf.iter().enumerate() {
            assert_eq!(px, x as u32, "列 {x} が別の列の画素を拾っている");
        }
    }

    #[test]
    fn paint_image_huge_zoom_clips_to_canvas() {
        // Even at an extreme zoom (dw/dh far larger than the canvas), it
        // should clip and only paint within the canvas — without clipping
        // the iteration count would explode and the test would never finish.
        // Top byte set to 0xFF to mark opacity.
        let pixels = [0xFFFF_0000u32, 0xFF00_FF00, 0xFF00_00FF, 0xFFFF_FFFF];
        const SENTINEL: u32 = 0x0012_3456;
        let mut buf = vec![SENTINEL; 4 * 4];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 4,
                h: 4,
                scale: 1.0,
            };
            let t = Xform {
                scale: 100_000.0,
                ox: 0.0,
                oy: 0.0,
            };
            paint_image(&mut canvas, (0.0, 0.0, 2.0, 2.0), 0.0, 2, 2, &pixels, &t);
        }
        // The image covers the origin area, so the whole canvas should be overwritten from the sentinel color.
        assert!(buf.iter().all(|&px| px != SENTINEL));
        assert_eq!(buf[0], 0x00FF_0000); // (0,0) samples the top-left (red) via nearest-neighbor.
    }

    #[test]
    fn paint_image_offscreen_is_skipped() {
        let pixels = [0xFFFF_0000u32, 0xFF00_FF00, 0xFF00_00FF, 0xFFFF_FFFF];
        const SENTINEL: u32 = 0x0012_3456;
        let mut buf = vec![SENTINEL; 4 * 4];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 4,
                h: 4,
                scale: 1.0,
            };
            let t = Xform {
                scale: 1.0,
                ox: 0.0,
                oy: 0.0,
            };
            // Placed far outside the canvas's bottom-right.
            paint_image(
                &mut canvas,
                (1000.0, 1000.0, 1002.0, 1002.0),
                0.0,
                2,
                2,
                &pixels,
                &t,
            );
        }
        assert!(buf.iter().all(|&px| px == SENTINEL));
    }

    #[test]
    fn paint_image_rotated_square_is_not_cropped_at_corners() {
        // Rotating a square image by 45° makes the diamond's tips extend
        // beyond the pre-rotation rect. Underestimating the rotated AABB
        // caused a bug where that extended area fell outside the scan
        // range and was never drawn, looking "cropped" — the rect's
        // bounding-box calculation had mistakenly reused the ellipse's
        // closed form sqrt(sum of squares) instead of the correct "sum of
        // absolute values," so at 45° rotation it didn't expand at all
        // and the tips never got drawn.
        let pixels = [0xFFAA_BBCCu32];
        const SENTINEL: u32 = 0x0012_3456;
        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            let t = Xform {
                scale: 1.0,
                ox: 0.0,
                oy: 0.0,
            };
            paint_image(
                &mut canvas,
                (0.0, 0.0, 20.0, 20.0),
                std::f64::consts::FRAC_PI_4,
                1,
                1,
                &pixels,
                &t,
            );
        }
        // A point 12 units from the center (10,10) toward the rotated tip
        // (+x axis). Outside the pre-rotation rect (half-width 10), but
        // should be inside the rotated diamond (half-width ≈14.14).
        assert_eq!(
            buf[10 * 40 + 22],
            0x00AA_BBCC,
            "回転後の菱形の内側なのに塗られていない（クロップ回帰）"
        );
    }

    #[test]
    fn paint_image_blends_semi_transparent_pixels_and_skips_fully_transparent_ones() {
        // A pasted or drag-and-dropped image's alpha (pixels' top byte)
        // should be respected and correctly alpha-blended with the
        // existing background (this used to always overwrite as opaque,
        // losing the transparency).
        const BG: u32 = 0x0010_2030;
        let mut buf = vec![BG; 3];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 3,
                h: 1,
                scale: 1.0,
            };
            let t = Xform {
                scale: 1.0,
                ox: 0.0,
                oy: 0.0,
            };
            // Column 1: fully opaque white, column 2: semi-transparent
            // (alpha=128) white, column 3: fully transparent white.
            let pixels = [0xFFFF_FFFFu32, 0x80FF_FFFF, 0x00FF_FFFF];
            paint_image(&mut canvas, (0.0, 0.0, 3.0, 1.0), 0.0, 3, 1, &pixels, &t);
        }
        assert_eq!(buf[0], 0x00FF_FFFF, "不透明画素は背景を完全に覆うはず");
        assert_eq!(buf[2], BG, "完全透明画素は背景を一切変えないはず");
        let mixed = buf[1];
        let mix_channel = |bg: u32, fg: u32| -> u32 {
            (bg as f32 * (1.0 - 128.0 / 255.0) + fg as f32 * (128.0 / 255.0)) as u32
        };
        let expect_r = mix_channel((BG >> 16) & 0xff, 0xff);
        let expect_g = mix_channel((BG >> 8) & 0xff, 0xff);
        let expect_b = mix_channel(BG & 0xff, 0xff);
        assert_eq!(
            mixed,
            (expect_r << 16) | (expect_g << 8) | expect_b,
            "半透明画素は背景と線形補間されるはず"
        );
    }

    #[test]
    fn paint_mosaic_uniform_block_flattens_interior_to_one_color_and_leaves_outside_untouched() {
        // Applies a large block to a checkerboard background. A block
        // boundary always runs through the rect's center (the local
        // origin), so the whole rect won't be one color, but the quadrant
        // matching the center (here, bottom-right: x and y both >= center)
        // fits entirely within one block, so it should become uniform.
        // Outside the rect (past the checkerboard's boundary) is unchanged.
        let mut buf = vec![0u32; 40 * 40];
        for y in 0..40 {
            for x in 0..40 {
                buf[y * 40 + x] = if (x + y) % 2 == 0 {
                    0x00FF_0000
                } else {
                    0x0000_00FF
                };
            }
        }
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            // Rect: (10,10)-(30,30), center (20,20). With block=100, the
            // bottom-right quadrant [20,30)x[20,30) falls within local
            // coordinates [0,10), so it snaps to the same block.
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                100.0,
                MosaicMode::Pixelate,
                0,
                &t,
            );
        }
        let inside = buf[25 * 40 + 25];
        for y in 20..30 {
            for x in 20..30 {
                assert_eq!(
                    buf[y * 40 + x],
                    inside,
                    "中心から見て同じ象限は1ブロックに収まるので単一色になるはず (x={x}, y={y})"
                );
            }
        }
        // Outside the rect (past the checkerboard's boundary) is unchanged.
        assert_eq!(buf[0], 0x00FF_0000, "矩形外は元の市松のままのはず");
        assert_eq!(buf[1], 0x0000_00FF, "矩形外は元の市松のままのはず");
    }

    #[test]
    fn paint_mosaic_rotated_only_affects_the_rotated_footprint() {
        // Rotating by 45° should change only the interior of the rotated
        // diamond, not the axis-aligned rect area as-is — the corners (of
        // the axis-aligned rect) fall outside the rotated diamond, so they
        // stay unchanged. Against the checkerboard, confirms "repainted"
        // by checking that two adjacent pixels inside the diamond (which
        // would normally have alternating checkerboard colors) become the
        // same color after snapping to the same block.
        let mut buf = vec![0u32; 40 * 40];
        for y in 0..40 {
            for x in 0..40 {
                buf[y * 40 + x] = if (x + y) % 2 == 0 {
                    0x00FF_0000
                } else {
                    0x0000_00FF
                };
            }
        }
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                std::f64::consts::FRAC_PI_4,
                4.0,
                MosaicMode::Pixelate,
                0,
                &t,
            );
        }
        // The axis-aligned rect's corner (10,10) is outside the rotated
        // diamond, so it follows the original checkerboard pattern unchanged.
        assert_eq!(
            buf[10 * 40 + 10],
            0x00FF_0000,
            "回転前矩形の角は回転後の菱形の外なので変化しないはず"
        );
        // The center (20,20) and its neighbor (21,20) are both inside the
        // diamond. They'd normally alternate colors in the checkerboard,
        // but the mosaic should repaint them to the same block, making
        // them the same color.
        assert_eq!(
            buf[20 * 40 + 20],
            buf[20 * 40 + 21],
            "菱形の内側は隣接画素でも同じブロックに塗り替えられて同色になるはず"
        );
    }

    fn checkerboard_buf() -> Vec<u32> {
        let mut buf = vec![0u32; 40 * 40];
        for y in 0..40 {
            for x in 0..40 {
                buf[y * 40 + x] = if (x + y) % 2 == 0 {
                    0x00FF_0000
                } else {
                    0x0000_00FF
                };
            }
        }
        buf
    }

    #[test]
    fn paint_mosaic_blur_changes_pixels_inside_but_not_outside_the_shape() {
        let mut buf = checkerboard_buf();
        let original_outside = buf[0];
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                8.0,
                MosaicMode::Blur,
                12345,
                &t,
            );
        }
        // Outside the rect (far top-left) is unchanged.
        assert_eq!(buf[0], original_outside, "矩形外は変化しないはず");
        // Inside the rect, the checkerboard's alternating pattern should
        // be smoothed, with at least one case of adjacent pixels matching
        // exactly (evidence of blurring).
        let mut any_smoothed = false;
        for y in 11..29 {
            for x in 11..29 {
                let here = buf[y * 40 + x];
                let right = buf[y * 40 + x + 1];
                // In the original checkerboard, adjacent pixels always
                // differed. If they match after blurring, that's evidence
                // averaging took effect.
                if here == right {
                    any_smoothed = true;
                }
            }
        }
        assert!(
            any_smoothed,
            "Blur は市松の互い違いパターンを均すので、内側のどこかで隣接画素が同色になるはず"
        );
    }

    #[test]
    fn paint_mosaic_blur_same_seed_is_deterministic() {
        // With the same seed, the output should be exactly identical no
        // matter how many times it's drawn (so redrawing doesn't flicker).
        let base = checkerboard_buf();
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf1 = base.clone();
        let mut buf2 = base;
        {
            let mut canvas = Canvas {
                buf: &mut buf1,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                8.0,
                MosaicMode::Blur,
                777,
                &t,
            );
        }
        {
            let mut canvas = Canvas {
                buf: &mut buf2,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                8.0,
                MosaicMode::Blur,
                777,
                &t,
            );
        }
        assert_eq!(buf1, buf2, "同じシードなら出力は完全に一致するはず");
    }

    #[test]
    fn paint_mosaic_blur_different_seeds_produce_different_output() {
        // With a different seed, sample positions are random instead of a
        // fixed kernel, so the output should differ even for the same
        // input (this is the property that resists reconstruction).
        let base = checkerboard_buf();
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf1 = base.clone();
        let mut buf2 = base;
        {
            let mut canvas = Canvas {
                buf: &mut buf1,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                8.0,
                MosaicMode::Blur,
                1,
                &t,
            );
        }
        {
            let mut canvas = Canvas {
                buf: &mut buf2,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_mosaic(
                &mut canvas,
                (10.0, 10.0, 30.0, 30.0),
                0.0,
                8.0,
                MosaicMode::Blur,
                2,
                &t,
            );
        }
        assert_ne!(buf1, buf2, "シードが違えば出力も変わるはず");
    }

    #[test]
    fn guide_paints_when_live_but_not_when_exporting() {
        const SENTINEL: u32 = 0x0012_3456;
        // The border is drawn offset outside the boundary, so leave enough margin to stay within the canvas.
        let ann = Annotation::Guide {
            r: (10, 10, 20, 20),
        };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };

        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, false);
        }
        assert!(
            buf.iter().any(|&px| px != SENTINEL),
            "ライブ表示ではガイドが描かれるはず"
        );

        let mut buf = vec![SENTINEL; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, true);
        }
        assert!(
            buf.iter().all(|&px| px == SENTINEL),
            "書き出し時はガイドを描かないはず"
        );
    }

    #[test]
    fn guide_border_does_not_intrude_into_export_area() {
        let ann = Annotation::Guide { r: (2, 2, 6, 6) };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 10 * 10];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 10,
                h: 10,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, false);
        }
        // The border doesn't overflow into the export bounds (interior) [2,6)x[2,6).
        for y in 2..6 {
            for x in 2..6 {
                assert_eq!(
                    buf[y * 10 + x],
                    0x00FF_FFFF,
                    "枠が内側 ({x},{y}) へ食い込んでいる"
                );
            }
        }
        // The border (black) is actually drawn somewhere outside.
        assert!(buf.contains(&0x0000_0000));
    }

    #[test]
    fn guide_border_hugs_the_boundary_without_a_gap() {
        // The border sits flush just outside the boundary (1px out), with no gap between them.
        let ann = Annotation::Guide {
            r: (10, 10, 20, 20),
        };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, false);
        }
        // 1px outside the top edge's boundary (y=10, so y=9); horizontally, near the top edge's midpoint.
        assert_eq!(buf[9 * 40 + 15], 0x0000_0000, "境界のすぐ外側に隙間がある");
    }

    #[test]
    fn guide_border_renders_correctly_when_far_larger_than_canvas() {
        // Even when the guide's bottom-right is far outside the canvas (an
        // actual scenario when zoomed in), the visible portion of the
        // border should still draw correctly (and quickly).
        let ann = Annotation::Guide {
            r: (10, 10, 100_000, 100_000),
        };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_one(&mut canvas, &ann, &t, None, false);
        }
        // 1px outside the top edge's boundary (y=10, so y=9) still has the border drawn.
        assert_eq!(
            buf[9 * 40 + 15],
            0x0000_0000,
            "遠く離れた右下によりクランプが正しく効いていない"
        );
        // Near the guide's actual bottom-right (off-canvas), nothing is drawn, as expected.
        assert_eq!(buf[39 * 40 + 39], 0x00FF_FFFF);
    }

    #[test]
    fn paint_annotations_always_draws_guide_on_top_regardless_of_order() {
        // Even if the guide is first in the array (i.e. behind other
        // items), it should always draw on top, unaffected by
        // PageUp/PageDown changing draw order.
        let guide = Annotation::Guide {
            r: (10, 10, 20, 20),
        };
        // Places a red rect overlapping the guide's border (just outside
        // the boundary, passing through (9,15)) after the guide (i.e. in
        // front, in array order).
        let rect = Annotation::Rect {
            r: (9.0, 0.0, 11.0, 40.0),
            color: 0x00FF_0000,
            thick: 1,
            rot: 0.0,
            filled: false,
        };
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 40 * 40];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 40,
                h: 40,
                scale: 1.0,
            };
            paint_annotations(&mut canvas, &[guide, rect], &t, None, None, false);
        }
        assert_eq!(
            buf[9 * 40 + 9],
            GUIDE_COLOR,
            "配列上は背面でも、ガイドの枠が矩形に隠れず見えるはず"
        );
    }

    #[test]
    fn dim_outside_guide_darkens_outside_and_keeps_inside() {
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 4 * 4]; // all white
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 4,
                h: 4,
                scale: 1.0,
            };
            dim_outside_guide(&mut canvas, Some((1, 1, 3, 3)), &t);
        }
        // Inside the guide (1,1)-(2,2) is unchanged.
        assert_eq!(buf[4 + 1], 0x00FF_FFFF);
        assert_eq!(buf[2 * 4 + 2], 0x00FF_FFFF);
        // Outside the guide is dimmed.
        assert_ne!(buf[0], 0x00FF_FFFF);
        assert_ne!(buf[3 * 4 + 3], 0x00FF_FFFF);
    }

    #[test]
    fn dim_outside_guide_noop_without_guide() {
        let t = Xform {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let mut buf = vec![0x00FF_FFFFu32; 4 * 4];
        {
            let mut canvas = Canvas {
                buf: &mut buf,
                w: 4,
                h: 4,
                scale: 1.0,
            };
            dim_outside_guide(&mut canvas, None, &t);
        }
        assert!(buf.iter().all(|&px| px == 0x00FF_FFFF));
    }

    #[test]
    fn annotations_bounds_covers_all_items() {
        let items = vec![image_item((0, 0), 100, 50), rect_item((-10, 20, 40, 200))];
        // x: -10..100, y: 0..200, plus rect_item's (unfilled, DEFAULT_THICK=4)
        // outline padding (pad=2).
        assert_eq!(annotations_bounds(&items, None), Some((-12, 0, 100, 202)));
        assert_eq!(annotations_bounds(&[], None), None);
    }

    #[test]
    fn selection_bounds_covers_only_the_selected_subset() {
        let items = vec![
            image_item((0, 0), 100, 50),
            rect_item((-10, 20, 40, 200)),
            image_item((1000, 1000), 10, 10), // not selected
        ];
        // Used as the group-transform reference (the selection outline's
        // bounding rect), so unlike annotations_bounds (for export, with
        // line-padding included), this stays the tight item_bbox range.
        assert_eq!(
            selection_bounds(&items, &[0, 1], None),
            Some((-10, 0, 100, 200))
        );
        assert_eq!(
            selection_bounds(&items, &[2], None),
            Some((1000, 1000, 1010, 1010))
        );
        assert_eq!(selection_bounds(&items, &[], None), None);
    }

    #[test]
    fn common_rotation_matches_when_all_selected_rects_agree() {
        let a = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.3,
            filled: false,
        };
        let items = vec![a.clone(), a];
        assert!((common_rotation(&items, &[0, 1]) - 0.3).abs() < 1e-9);
    }

    #[test]
    fn common_rotation_falls_back_to_zero_when_rotations_differ() {
        let a = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.3,
            filled: false,
        };
        let b = Annotation::Rect {
            r: (20.0, 0.0, 30.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.5,
            filled: false,
        };
        let items = vec![a, b];
        assert_eq!(common_rotation(&items, &[0, 1]), 0.0);
    }

    #[test]
    fn common_rotation_treats_non_rotatable_items_as_effectively_unrotated() {
        // Types without a rotation field, like Arrow/Guide, are treated as
        // agreeing at an effective rotation of 0.0 (Image/Text are excluded
        // here since they became rotatable).
        let items = vec![
            Annotation::Arrow {
                a: (0, 0),
                b: (10, 0),
                color: DEFAULT_COLOR,
                thick: DEFAULT_THICK,
            },
            Annotation::Guide { r: (0, 0, 10, 10) },
        ];
        assert_eq!(common_rotation(&items, &[0, 1]), 0.0);
    }

    #[test]
    fn common_rotation_considers_ellipse_rotation_too() {
        // Ellipse, like Rect, has rotation, so it's reflected in the effective rotation.
        let a = Annotation::Ellipse {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.4,
            filled: false,
        };
        let items = vec![a.clone(), a];
        assert!((common_rotation(&items, &[0, 1]) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn common_rotation_considers_image_and_text_rotation_too() {
        // Image/Text have also become rotatable, so it's reflected in the effective rotation.
        let img = Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot: 0.4,
        };
        let txt = Annotation::Text {
            pos: (0, 0),
            text: "hi".into(),
            color: DEFAULT_COLOR,
            size: 20.0,
            rot: 0.4,
        };
        let items = vec![img, txt];
        assert!((common_rotation(&items, &[0, 1]) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn group_rect_for_rotation_matches_item_local_rect_when_uniform() {
        // Selecting two identical Rects: the group's bounding rect should
        // exactly match the item's own local rect (`rot` is the known
        // value the caller passes in).
        let a = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.3,
            filled: false,
        };
        let items = vec![a.clone(), a];
        let rect = group_rect_for_rotation(&items, &[0, 1], 0.3, None).unwrap();
        assert!(
            (rect.0 - 0.0).abs() < 1e-6
                && (rect.1 - 0.0).abs() < 1e-6
                && (rect.2 - 10.0).abs() < 1e-6
                && (rect.3 - 10.0).abs() < 1e-6,
            "rect={rect:?}"
        );
    }

    #[test]
    fn group_rect_for_rotation_is_axis_aligned_bbox_when_rot_is_zero() {
        let a = rect_item((0, 0, 10, 10));
        let b = image_item((20, 0), 10, 10);
        let items = vec![a, b];
        let rect = group_rect_for_rotation(&items, &[0, 1], 0.0, None).unwrap();
        let aabb = selection_bounds(&items, &[0, 1], None).unwrap();
        assert_eq!(
            rect,
            (aabb.0 as f64, aabb.1 as f64, aabb.2 as f64, aabb.3 as f64)
        );
    }

    #[test]
    fn scale_annotation_rotated_matches_scale_annotation_when_unrotated() {
        let orig_rect = (0.0, 0.0, 10.0, 10.0);
        let new_rect = (0.0, 0.0, 20.0, 30.0);
        let item = Annotation::Ellipse {
            r: (2.0, 2.0, 8.0, 8.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        };
        let via_rotated = scale_annotation_rotated(&item, orig_rect, new_rect, 0.0);
        let via_plain = scale_annotation(&item, (0, 0, 10, 10), (0, 0, 20, 30));
        match (via_rotated, via_plain) {
            (Annotation::Ellipse { r: r1, .. }, Annotation::Ellipse { r: r2, .. }) => {
                assert_eq!(r1, r2)
            }
            _ => panic!("Ellipse のはず"),
        }
    }

    #[test]
    fn scale_annotation_rotated_keeps_opposite_corner_fixed_in_world_space() {
        // Resizing a rotated bounding rect should still leave the opposite
        // corner's (TopLeft) world coordinates unchanged (same
        // verification approach as Rect's own resize_rotated_rect).
        let orig_rect = (0.0, 0.0, 40.0, 20.0);
        let rot = 0.4_f64;
        let anchor_world_before = rotate_point((0.0, 0.0), rect_center(orig_rect), rot);

        let new_rect = resize_rotated_rect(orig_rect, Handle::BottomRight, (90.0, 70.0), rot);

        let item = Annotation::Rect {
            r: orig_rect,
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot,
            filled: false,
        };
        let scaled = scale_annotation_rotated(&item, orig_rect, new_rect, rot);
        let Annotation::Rect {
            r: new_r,
            rot: new_rot,
            ..
        } = scaled
        else {
            panic!("Rect のはず");
        };
        assert_eq!(new_rot, rot, "グループのリサイズだけでは rot は変わらない");

        let anchor_world_after = rotate_point((new_r.0, new_r.1), rect_center(new_r), new_rot);
        assert!(
            (anchor_world_before.0 - anchor_world_after.0).abs() < 1e-6
                && (anchor_world_before.1 - anchor_world_after.1).abs() < 1e-6,
            "before={anchor_world_before:?} after={anchor_world_after:?}"
        );
    }

    #[test]
    fn scale_annotation_rotated_keeps_opposite_corner_fixed_in_world_space_for_ellipse() {
        // The exact same verification as Rect, done for Ellipse too (both
        // go through the same transform_rotated_corners helper, so they
        // should behave the same).
        let orig_rect = (0.0, 0.0, 40.0, 20.0);
        let rot = 0.4_f64;
        let anchor_world_before = rotate_point((0.0, 0.0), rect_center(orig_rect), rot);

        let new_rect = resize_rotated_rect(orig_rect, Handle::BottomRight, (90.0, 70.0), rot);

        let item = Annotation::Ellipse {
            r: orig_rect,
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot,
            filled: false,
        };
        let scaled = scale_annotation_rotated(&item, orig_rect, new_rect, rot);
        let Annotation::Ellipse {
            r: new_r,
            rot: new_rot,
            ..
        } = scaled
        else {
            panic!("Ellipse のはず");
        };
        assert_eq!(new_rot, rot, "グループのリサイズだけでは rot は変わらない");

        let anchor_world_after = rotate_point((new_r.0, new_r.1), rect_center(new_r), new_rot);
        assert!(
            (anchor_world_before.0 - anchor_world_after.0).abs() < 1e-6
                && (anchor_world_before.1 - anchor_world_after.1).abs() < 1e-6,
            "before={anchor_world_before:?} after={anchor_world_after:?}"
        );
    }

    #[test]
    fn scale_annotation_rotated_keeps_opposite_corner_fixed_in_world_space_for_image() {
        // The exact same verification as Rect/Ellipse, done for Image too
        // (all go through the same transform_rotated_corners helper, so
        // they should behave the same).
        let orig_rect = (0.0, 0.0, 40.0, 20.0);
        let rot = 0.4_f64;
        let anchor_world_before = rotate_point((0.0, 0.0), rect_center(orig_rect), rot);

        let new_rect = resize_rotated_rect(orig_rect, Handle::BottomRight, (90.0, 70.0), rot);

        let item = Annotation::Image {
            r: orig_rect,
            src_w: 2,
            src_h: 2,
            pixels: Rc::new(vec![1, 2, 3, 4]),
            rot,
        };
        let scaled = scale_annotation_rotated(&item, orig_rect, new_rect, rot);
        let Annotation::Image {
            r: new_r,
            rot: new_rot,
            ..
        } = scaled
        else {
            panic!("Image のはず");
        };
        assert_eq!(new_rot, rot, "グループのリサイズだけでは rot は変わらない");

        let anchor_world_after = rotate_point((new_r.0, new_r.1), rect_center(new_r), new_rot);
        assert!(
            (anchor_world_before.0 - anchor_world_after.0).abs() < 1e-6
                && (anchor_world_before.1 - anchor_world_after.1).abs() < 1e-6,
            "before={anchor_world_before:?} after={anchor_world_after:?}"
        );
    }

    #[test]
    fn guide_bounds_finds_the_only_guide_and_normalizes_it() {
        let items = vec![
            rect_item((0, 0, 10, 10)),
            Annotation::Guide { r: (40, 30, 5, 8) }, // unnormalized (x0>x1, y0>y1)
            image_item((100, 100), 20, 20),
        ];
        assert_eq!(guide_bounds(&items), Some((5, 8, 40, 30)));
    }

    #[test]
    fn guide_bounds_none_when_no_guide_present() {
        let items = vec![rect_item((0, 0, 10, 10)), image_item((0, 0), 20, 20)];
        assert_eq!(guide_bounds(&items), None);
        assert_eq!(guide_bounds(&[]), None);
    }

    #[test]
    fn translate_image_shifts_pos_keeps_pixels() {
        let a = image_item((10, 20), 30, 40);
        match translate_annotation(&a, 5, -3) {
            Annotation::Image { r, pixels, .. } => {
                assert_eq!(r, (15.0, 17.0, 45.0, 57.0));
                assert_eq!(&*pixels, &[1, 2, 3, 4]);
            }
            _ => panic!("Image のはず"),
        }
    }

    #[test]
    fn translate_annotation_mosaic_shifts_r_keeps_rot_and_block() {
        let a = Annotation::Mosaic {
            r: (10.0, 20.0, 30.0, 40.0),
            rot: 0.3,
            block: 12.0,
            mode: MosaicMode::Pixelate,
            seed: 42,
        };
        match translate_annotation(&a, 5, -3) {
            Annotation::Mosaic {
                r,
                rot,
                block,
                mode,
                seed,
            } => {
                assert_eq!(r, (15.0, 17.0, 35.0, 37.0));
                assert_eq!(rot, 0.3);
                assert_eq!(block, 12.0);
                assert_eq!(mode, MosaicMode::Pixelate);
                assert_eq!(seed, 42);
            }
            _ => panic!("Mosaic のはず"),
        }
    }

    #[test]
    fn scale_annotation_mosaic_maps_r_keeps_block() {
        let a = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Blur,
            seed: 7,
        };
        match scale_annotation(&a, (0, 0, 10, 10), (0, 0, 20, 20)) {
            Annotation::Mosaic {
                r,
                block,
                mode,
                seed,
                ..
            } => {
                assert_eq!(r, (0.0, 0.0, 20.0, 20.0));
                assert_eq!(block, 8.0);
                assert_eq!(mode, MosaicMode::Blur);
                assert_eq!(seed, 7);
            }
            _ => panic!("Mosaic のはず"),
        }
    }

    #[test]
    fn scale_annotation_rotated_mosaic_keeps_opposite_corner_fixed_in_world_space() {
        let a = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Pixelate,
            seed: 3,
        };
        let orig = (0.0, 0.0, 10.0, 10.0);
        let dragged = (0.0, 0.0, 20.0, 10.0);
        match scale_annotation_rotated(&a, orig, dragged, 0.0) {
            Annotation::Mosaic {
                r,
                rot,
                block,
                mode,
                seed,
            } => {
                assert_eq!(r, (0.0, 0.0, 20.0, 10.0));
                assert_eq!(rot, 0.0);
                assert_eq!(block, 8.0);
                assert_eq!(mode, MosaicMode::Pixelate);
                assert_eq!(seed, 3);
            }
            _ => panic!("Mosaic のはず"),
        }
    }

    #[test]
    fn rotate_annotation_around_mosaic_spins_r_and_adds_delta_to_rot() {
        let a = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Blur,
            seed: 99,
        };
        let center = (5.0, 5.0);
        match rotate_annotation_around(&a, center, std::f64::consts::FRAC_PI_2, None) {
            Annotation::Mosaic {
                r,
                rot,
                block,
                mode,
                seed,
            } => {
                // Even after a 90° rotation, since it's around the center, the rect itself (corner coordinates) is unchanged.
                assert_eq!(r, (0.0, 0.0, 10.0, 10.0));
                assert!((rot - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
                assert_eq!(block, 8.0);
                assert_eq!(mode, MosaicMode::Blur);
                assert_eq!(seed, 99);
            }
            _ => panic!("Mosaic のはず"),
        }
    }

    #[test]
    fn item_bbox_mosaic_rotated_returns_aabb_of_corners() {
        let flat = mosaic_item((0, 0, 40, 20));
        let (fx0, fy0, fx1, fy1) = item_bbox(&flat, None);

        let rotated = Annotation::Mosaic {
            r: (0.0, 0.0, 40.0, 20.0),
            rot: std::f64::consts::FRAC_PI_4,
            block: DEFAULT_BLOCK,
            mode: MosaicMode::Pixelate,
            seed: 0,
        };
        let (rx0, ry0, rx1, ry1) = item_bbox(&rotated, None);

        assert!(rx1 - rx0 > fx1 - fx0 || ry1 - ry0 > fy1 - fy0);
        assert_eq!((fx0 + fx1) / 2, (rx0 + rx1) / 2);
        assert_eq!((fy0 + fy1) / 2, (ry0 + ry1) / 2);
    }

    #[test]
    fn item_export_bbox_mosaic_matches_item_bbox_since_it_has_no_stroke() {
        // Mosaic has no thickness, so its export bbox should match the regular bbox.
        let a = mosaic_item((5, 5, 35, 35));
        assert_eq!(item_bbox(&a, None), item_export_bbox(&a, None));
    }

    #[test]
    fn compose_rgba_marks_sentinel_transparent() {
        // pixel 0: drawn black (stays opaque), pixel 1: sentinel (transparent), pixel 2: colored (opaque).
        let color = [0x0000_0000, 0x0011_2233, 0x00E0_3030];
        let mask = [0x0000_0000, EXPORT_SENTINEL, 0x00E0_3030];
        let rgba = compose_rgba(&color, &mask);
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]); // a black item isn't made transparent
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]); // 番兵は完全透明
        assert_eq!(&rgba[8..12], &[0xE0, 0x30, 0x30, 255]);
    }

    #[test]
    fn translate_annotation_preserves_style() {
        let a = Annotation::Rect {
            r: (10.0, 20.0, 30.0, 40.0),
            color: 0x0012_3456,
            thick: 7,
            rot: 0.3,
            filled: false,
        };
        match translate_annotation(&a, 5, -3) {
            Annotation::Rect {
                r,
                color,
                thick,
                rot,
                ..
            } => {
                assert_eq!(r, (15.0, 17.0, 35.0, 37.0));
                assert_eq!(color, 0x0012_3456);
                assert_eq!(thick, 7);
                assert_eq!(rot, 0.3);
            }
            _ => panic!("Rect のはず"),
        }
    }

    #[test]
    fn translate_annotation_number_marker_shifts_pos_keeps_number_and_size() {
        let a = Annotation::NumberMarker {
            pos: (10, 20),
            number: 3,
            color: 0x0012_3456,
            size: 8.0,
        };
        match translate_annotation(&a, 5, -3) {
            Annotation::NumberMarker {
                pos,
                number,
                color,
                size,
            } => {
                assert_eq!(pos, (15, 17));
                assert_eq!(number, 3);
                assert_eq!(color, 0x0012_3456);
                assert_eq!(size, 8.0);
            }
            _ => panic!("NumberMarker のはず"),
        }
    }

    #[test]
    fn scale_annotation_number_marker_maps_pos_and_scales_size() {
        let a = Annotation::NumberMarker {
            pos: (10, 10),
            number: 1,
            color: DEFAULT_COLOR,
            size: 10.0,
        };
        // A bounding-rect transform that scales up 2x.
        let scaled = scale_annotation(&a, (0, 0, 10, 10), (0, 0, 20, 20));
        match scaled {
            Annotation::NumberMarker { pos, size, .. } => {
                assert_eq!(pos, (20, 20));
                assert_eq!(size, 20.0);
            }
            _ => panic!("NumberMarker のはず"),
        }
    }

    #[test]
    fn scale_annotation_rotated_number_marker_matches_scale_annotation_when_unrotated() {
        let a = Annotation::NumberMarker {
            pos: (10, 10),
            number: 1,
            color: DEFAULT_COLOR,
            size: 10.0,
        };
        let plain = scale_annotation(&a, (0, 0, 10, 10), (0, 0, 20, 20));
        let rotated =
            scale_annotation_rotated(&a, (0.0, 0.0, 10.0, 10.0), (0.0, 0.0, 20.0, 20.0), 0.0);
        match (plain, rotated) {
            (
                Annotation::NumberMarker {
                    pos: p1, size: s1, ..
                },
                Annotation::NumberMarker {
                    pos: p2, size: s2, ..
                },
            ) => {
                assert_eq!(p1, p2);
                assert_eq!(s1, s2);
            }
            _ => panic!("NumberMarker のはず"),
        }
    }

    #[test]
    fn rotate_annotation_around_number_marker_only_repositions_pos() {
        let a = Annotation::NumberMarker {
            pos: (10, 0),
            number: 1,
            color: DEFAULT_COLOR,
            size: 5.0,
        };
        // A point in the +x direction from center (0,0), rotated 90°, ends up in the +y direction.
        match rotate_annotation_around(&a, (0.0, 0.0), std::f64::consts::FRAC_PI_2, None) {
            Annotation::NumberMarker {
                pos, number, size, ..
            } => {
                assert_eq!(pos, (0, 10));
                assert_eq!(number, 1);
                assert_eq!(size, 5.0);
            }
            _ => panic!("NumberMarker のはず"),
        }
    }

    #[test]
    fn next_marker_number_starts_at_1_and_continues_from_existing_max() {
        assert_eq!(next_marker_number(&[]), 1);
        let items = vec![
            Annotation::NumberMarker {
                pos: (0, 0),
                number: 1,
                color: DEFAULT_COLOR,
                size: 5.0,
            },
            Annotation::NumberMarker {
                pos: (0, 0),
                number: 3,
                color: DEFAULT_COLOR,
                size: 5.0,
            },
            Annotation::Rect {
                r: (0.0, 0.0, 1.0, 1.0),
                color: DEFAULT_COLOR,
                thick: DEFAULT_THICK,
                rot: 0.0,
                filled: false,
            },
        ];
        assert_eq!(next_marker_number(&items), 4);
    }

    #[test]
    fn contrast_text_color_picks_black_on_light_and_white_on_dark() {
        assert_eq!(contrast_text_color(0x00FF_FFFF), 0x0000_0000);
        assert_eq!(contrast_text_color(0x0000_0000), 0x00FF_FFFF);
    }

    #[test]
    fn translate_annotation_polyline_shifts_all_points_equally() {
        let a = Annotation::Polyline {
            points: vec![(10, 20), (30, 40), (50, 20)],
            color: 0x0012_3456,
            thick: 7,
        };
        match translate_annotation(&a, 5, -3) {
            Annotation::Polyline {
                points,
                color,
                thick,
            } => {
                assert_eq!(points, vec![(15, 17), (35, 37), (55, 17)]);
                assert_eq!(color, 0x0012_3456);
                assert_eq!(thick, 7);
            }
            _ => panic!("Polyline のはず"),
        }
    }

    #[test]
    fn scale_annotation_keeps_anchor_corner_fixed_and_scales_proportionally() {
        // Bounding rect (0,0,10,10) -> (0,0,20,30) (top-left is the fixed anchor, width 2x, height 3x).
        let orig_rect = (0, 0, 10, 10);
        let new_rect = (0, 0, 20, 30);

        // Ellipse: each coordinate scales proportionally by sx=2, sy=3.
        let ellipse = Annotation::Ellipse {
            r: (2.0, 2.0, 8.0, 8.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        };
        match scale_annotation(&ellipse, orig_rect, new_rect) {
            Annotation::Ellipse { r, .. } => assert_eq!(r, (4.0, 6.0, 16.0, 24.0)),
            _ => panic!("Ellipse のはず"),
        }

        // Rect: transforms proportionally while staying f64; rot is unchanged.
        let rect = Annotation::Rect {
            r: (2.0, 2.0, 8.0, 8.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.5,
            filled: false,
        };
        match scale_annotation(&rect, orig_rect, new_rect) {
            Annotation::Rect { r, rot, .. } => {
                assert_eq!(r, (4.0, 6.0, 16.0, 24.0));
                assert_eq!(rot, 0.5);
            }
            _ => panic!("Rect のはず"),
        }

        // Image: like Rect, r transforms proportionally.
        let image = Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 10,
            src_h: 10,
            pixels: Rc::new(vec![0; 400]),
            rot: 0.0,
        };
        match scale_annotation(&image, orig_rect, new_rect) {
            Annotation::Image { r, .. } => assert_eq!(r, (0.0, 0.0, 20.0, 30.0)),
            _ => panic!("Image のはず"),
        }
    }

    #[test]
    fn scale_annotation_falls_back_to_unit_scale_on_zero_width_axis() {
        // Even when the selection's bounding rect is a vertical line (width 0), no panic or division by zero.
        let orig_rect = (5, 0, 5, 10);
        let new_rect = (5, 0, 5, 40);
        let a = Annotation::Arrow {
            a: (5, 2),
            b: (5, 8),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        match scale_annotation(&a, orig_rect, new_rect) {
            Annotation::Arrow { a, b, .. } => {
                // The x axis (width 0) stays 1:1, and only the y axis scales 4x.
                assert_eq!(a, (5, 8));
                assert_eq!(b, (5, 32));
            }
            _ => panic!("Arrow のはず"),
        }
    }

    #[test]
    fn scale_annotation_polyline_scales_all_points_proportionally() {
        let orig_rect = (0, 0, 10, 10);
        let new_rect = (0, 0, 20, 30);
        let a = Annotation::Polyline {
            points: vec![(2, 2), (8, 8), (2, 8)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        match scale_annotation(&a, orig_rect, new_rect) {
            Annotation::Polyline { points, .. } => {
                assert_eq!(points, vec![(4, 6), (16, 24), (4, 24)]);
            }
            _ => panic!("Polyline のはず"),
        }
    }

    #[test]
    fn scale_annotation_rotated_polyline_matches_scale_annotation_when_unrotated() {
        let orig_rect = (0.0, 0.0, 10.0, 10.0);
        let new_rect = (0.0, 0.0, 20.0, 30.0);
        let item = Annotation::Polyline {
            points: vec![(2, 2), (8, 8)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        let via_rotated = scale_annotation_rotated(&item, orig_rect, new_rect, 0.0);
        let via_plain = scale_annotation(&item, (0, 0, 10, 10), (0, 0, 20, 30));
        match (via_rotated, via_plain) {
            (Annotation::Polyline { points: p1, .. }, Annotation::Polyline { points: p2, .. }) => {
                assert_eq!(p1, p2)
            }
            _ => panic!("Polyline のはず"),
        }
    }

    #[test]
    fn rotate_annotation_around_spins_rect_and_arrow_but_only_repositions_others() {
        let center = (0.0, 0.0);
        let delta = std::f64::consts::FRAC_PI_2;

        // Rect: the center orbits, and rot increases by delta.
        let rect = Annotation::Rect {
            r: (5.0, -5.0, 15.0, 5.0), // center (10,0)
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.1,
            filled: false,
        };
        match rotate_annotation_around(&rect, center, delta, None) {
            Annotation::Rect { r, rot, .. } => {
                let new_center = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
                assert!((new_center.0 - 0.0).abs() < 1e-6, "cx={}", new_center.0);
                assert!((new_center.1 - 10.0).abs() < 1e-6, "cy={}", new_center.1);
                assert!((rot - (0.1 + delta)).abs() < 1e-9);
            }
            _ => panic!("Rect のはず"),
        }

        // Arrow: both endpoints orbit the center individually (orientation rotates correctly too).
        let arrow = Annotation::Arrow {
            a: (10, 0),
            b: (20, 0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        match rotate_annotation_around(&arrow, center, delta, None) {
            Annotation::Arrow { a, b, .. } => {
                assert_eq!(a, (0, 10));
                assert_eq!(b, (0, 20));
            }
            _ => panic!("Arrow のはず"),
        }

        // Polyline: like Arrow, every point orbits the center individually (has no `rot`).
        let polyline = Annotation::Polyline {
            points: vec![(10, 0), (20, 0), (20, 10)],
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
        };
        match rotate_annotation_around(&polyline, center, delta, None) {
            Annotation::Polyline { points, .. } => {
                assert_eq!(points, vec![(0, 10), (0, 20), (-10, 20)]);
            }
            _ => panic!("Polyline のはず"),
        }

        // Ellipse/Image: like Rect, the center orbits and rot increases by
        // delta (this changed from the earlier "orbits without spinning"
        // behavior once both became rotatable).
        let ellipse = Annotation::Ellipse {
            r: (5.0, -5.0, 15.0, 5.0), // center (10,0)
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.1,
            filled: false,
        };
        match rotate_annotation_around(&ellipse, center, delta, None) {
            Annotation::Ellipse { r, rot, .. } => {
                let new_center = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
                assert!((new_center.0 - 0.0).abs() < 1e-6, "cx={}", new_center.0);
                assert!((new_center.1 - 10.0).abs() < 1e-6, "cy={}", new_center.1);
                assert!((rot - (0.1 + delta)).abs() < 1e-9);
            }
            _ => panic!("Ellipse のはず"),
        }

        let image = Annotation::Image {
            r: (5.0, -5.0, 15.0, 5.0), // center (10,0)
            src_w: 10,
            src_h: 10,
            pixels: Rc::new(vec![0; 400]),
            rot: 0.1,
        };
        match rotate_annotation_around(&image, center, delta, None) {
            Annotation::Image { r, rot, .. } => {
                let new_center = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
                assert!((new_center.0 - 0.0).abs() < 1e-6, "cx={}", new_center.0);
                assert!((new_center.1 - 10.0).abs() < 1e-6, "cy={}", new_center.1);
                assert!((rot - (0.1 + delta)).abs() < 1e-9);
            }
            _ => panic!("Image のはず"),
        }

        // Text: its own rot also increases by delta (its local rect is
        // built on demand from a font measurement, so only the rot update
        // is verified here).
        let text_ann = Annotation::Text {
            pos: (10, -5),
            text: String::new(),
            color: DEFAULT_COLOR,
            size: 10.0,
            rot: 0.1,
        };
        match rotate_annotation_around(&text_ann, center, delta, None) {
            Annotation::Text { rot, .. } => {
                assert!((rot - (0.1 + delta)).abs() < 1e-9);
            }
            _ => panic!("Text のはず"),
        }
    }

    #[test]
    fn reorder_selection_moves_whole_set_forward_when_room_exists() {
        let mut items: Vec<Annotation> = (0..7).map(|i| rect_item((i, 0, i + 1, 1))).collect();
        let selected = reorder_selection(&mut items, &[2, 3, 5], true);
        assert_eq!(selected, vec![3, 4, 6]);
    }

    #[test]
    fn reorder_selection_moves_whole_set_backward_when_room_exists() {
        let mut items: Vec<Annotation> = (0..7).map(|i| rect_item((i, 0, i + 1, 1))).collect();
        let selected = reorder_selection(&mut items, &[2, 3, 5], false);
        assert_eq!(selected, vec![1, 2, 4]);
    }

    #[test]
    fn reorder_selection_adjacent_pair_moves_as_a_block() {
        let mut items: Vec<Annotation> = (0..5).map(|i| rect_item((i, 0, i + 1, 1))).collect();
        let selected = reorder_selection(&mut items, &[2, 3], true);
        assert_eq!(selected, vec![3, 4]);
    }

    #[test]
    fn reorder_selection_blocked_group_at_boundary_does_not_move() {
        let mut items: Vec<Annotation> = (0..5).map(|i| rect_item((i, 0, i + 1, 1))).collect();
        // A group already at the front boundary (3,4 are last) doesn't move further forward.
        let selected = reorder_selection(&mut items, &[3, 4], true);
        assert_eq!(selected, vec![3, 4]);
    }

    #[test]
    fn reorder_selection_single_item_behaves_like_the_old_page_up_down() {
        let mut items: Vec<Annotation> = (0..3).map(|i| rect_item((i, 0, i + 1, 1))).collect();
        assert_eq!(reorder_selection(&mut items, &[1], true), vec![2]);
        assert_eq!(reorder_selection(&mut items, &[2], false), vec![1]);
    }

    #[test]
    fn common_style_single_item_is_always_uniform() {
        let a = rect_item((0, 0, 10, 10));
        let (color, thick, size, filled, block, is_blur) = common_style(
            &[&a],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(color, PropVal::Uniform(DEFAULT_COLOR));
        assert_eq!(thick, PropVal::Uniform(DEFAULT_THICK));
        // Rect has no text size/block/blur, so those stay Uniform at their defaults.
        assert_eq!(size, PropVal::Uniform(24.0));
        assert_eq!(filled, PropVal::Uniform(false));
        assert_eq!(block, PropVal::Uniform(16.0));
        assert_eq!(is_blur, PropVal::Uniform(false));
    }

    #[test]
    fn common_style_differing_colors_report_mixed_with_first_as_representative() {
        let a = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: 0x00FF_0000,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        };
        let b = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: 0x0000_FF00,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        };
        let (color, thick, _, _, _, _) = common_style(
            &[&a, &b],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(color, PropVal::Mixed(0x00FF_0000));
        // Thickness agrees, so it stays Uniform.
        assert_eq!(thick, PropVal::Uniform(DEFAULT_THICK));
    }

    #[test]
    fn common_style_reports_mixed_filled_when_rect_and_ellipse_differ() {
        let filled = Annotation::Rect {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: true,
        };
        let unfilled = Annotation::Ellipse {
            r: (0.0, 0.0, 10.0, 10.0),
            color: DEFAULT_COLOR,
            thick: DEFAULT_THICK,
            rot: 0.0,
            filled: false,
        };
        let (_, _, _, f, _, _) = common_style(
            &[&filled, &unfilled],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(f, PropVal::Mixed(true));

        // Uniform when they all agree.
        let (_, _, _, f2, _, _) = common_style(
            &[&filled, &filled],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(f2, PropVal::Uniform(true));
    }

    #[test]
    fn common_style_falls_back_to_defaults_when_no_applicable_item() {
        let img = Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 10,
            src_h: 10,
            pixels: Rc::new(vec![0; 400]),
            rot: 0.0,
        };
        let guide = Annotation::Guide { r: (0, 0, 10, 10) };
        // When only types without color/thickness are selected, or nothing
        // is selected, everything falls back to Uniform at its default.
        let (color, thick, size, filled, block, is_blur) = common_style(
            &[&img, &guide],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(color, PropVal::Uniform(DEFAULT_COLOR));
        assert_eq!(thick, PropVal::Uniform(DEFAULT_THICK));
        assert_eq!(size, PropVal::Uniform(24.0));
        assert_eq!(filled, PropVal::Uniform(false));
        assert_eq!(block, PropVal::Uniform(16.0));
        assert_eq!(is_blur, PropVal::Uniform(false));

        let (color, thick, size, filled, block, is_blur) = common_style(
            &[],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(color, PropVal::Uniform(DEFAULT_COLOR));
        assert_eq!(thick, PropVal::Uniform(DEFAULT_THICK));
        assert_eq!(size, PropVal::Uniform(24.0));
        assert_eq!(filled, PropVal::Uniform(false));
        assert_eq!(block, PropVal::Uniform(16.0));
        assert_eq!(is_blur, PropVal::Uniform(false));
    }

    #[test]
    fn common_style_reports_mixed_block_when_mosaics_differ_uniform_when_same() {
        let a = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Pixelate,
            seed: 0,
        };
        let b = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 16.0,
            mode: MosaicMode::Pixelate,
            seed: 0,
        };
        let (_, _, _, _, block, _) = common_style(
            &[&a, &b],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(block, PropVal::Mixed(8.0));

        let (_, _, _, _, block2, _) = common_style(
            &[&a, &a],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(block2, PropVal::Uniform(8.0));
    }

    #[test]
    fn common_style_reports_is_blur_uniform_and_mixed() {
        let pixelate = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Pixelate,
            seed: 0,
        };
        let blur = Annotation::Mosaic {
            r: (0.0, 0.0, 10.0, 10.0),
            rot: 0.0,
            block: 8.0,
            mode: MosaicMode::Blur,
            seed: 0,
        };
        let (.., is_blur) = common_style(
            &[&pixelate, &blur],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(is_blur, PropVal::Mixed(false));

        let (.., is_blur2) = common_style(
            &[&blur, &blur],
            (DEFAULT_COLOR, DEFAULT_THICK, 24.0, false, 16.0, false),
        );
        assert_eq!(is_blur2, PropVal::Uniform(true));
    }

    #[test]
    fn rasterize_freehand_returns_image_with_padded_bbox_and_expected_alpha() {
        // A vertical line (0,0)-(0,20), thickness 4.
        let ann = rasterize_freehand(&[(0, 0), (0, 20)], 4, DEFAULT_COLOR);
        match ann {
            Annotation::Image {
                r,
                src_w,
                src_h,
                pixels,
                rot,
            } => {
                // pad = thick.max(1)/2 + 2 = 4.
                assert_eq!(r, (-4.0, -4.0, 4.0, 24.0));
                assert_eq!(src_w, 8);
                assert_eq!(src_h, 28);
                assert_eq!(rot, 0.0);
                // The stroke's center (local coordinates directly over the line) is nearly opaque.
                let center = pixels[12 * src_w as usize + 4];
                assert!((center >> 24) & 0xff >= 250, "center alpha too low");
                assert_eq!(center & 0x00FF_FFFF, DEFAULT_COLOR & 0x00FF_FFFF);
                // A padding corner (far from the line) is fully transparent.
                let corner = pixels[0];
                assert_eq!((corner >> 24) & 0xff, 0);
            }
            _ => panic!("Image のはず"),
        }
    }

    #[test]
    fn composite_over_opaque_src_fully_replaces_dst() {
        let dst = 0xFF10_2030;
        let src = 0xFFFF_FFFF;
        assert_eq!(composite_over(dst, src), src);
    }

    #[test]
    fn composite_over_transparent_src_leaves_dst_unchanged() {
        let dst = 0xFF10_2030;
        let src = 0x00FF_0000; // alpha 0 = fully transparent
        assert_eq!(composite_over(dst, src), dst);
    }

    #[test]
    fn composite_over_half_alpha_src_blends_toward_dst() {
        // Layers semi-transparent red (alpha=128) onto opaque black.
        let dst = 0xFF00_0000;
        let src = 0x80FF_0000;
        let out = composite_over(dst, src);
        assert_eq!((out >> 24) & 0xff, 255);
        // (255*0.5 + 0*1*0.5)/1 = 127.5 -> rounds to 128.
        assert_eq!((out >> 16) & 0xff, 128);
        assert_eq!((out >> 8) & 0xff, 0);
        assert_eq!(out & 0xff, 0);
    }

    #[test]
    fn merge_images_composites_non_overlapping_items_at_their_own_positions() {
        let red = Annotation::Image {
            r: (0.0, 0.0, 10.0, 10.0),
            src_w: 1,
            src_h: 1,
            pixels: Rc::new(vec![0xFFFF_0000]),
            rot: 0.0,
        };
        let green = Annotation::Image {
            r: (20.0, 0.0, 30.0, 10.0),
            src_w: 1,
            src_h: 1,
            pixels: Rc::new(vec![0xFF00_FF00]),
            rot: 0.0,
        };
        let merged = merge_images(&[red, green]);
        match merged {
            Annotation::Image {
                r,
                src_w,
                src_h,
                pixels,
                rot,
            } => {
                assert_eq!(r, (0.0, 0.0, 30.0, 10.0));
                assert_eq!(src_w, 30);
                assert_eq!(src_h, 10);
                assert_eq!(rot, 0.0);
                // Inside the red item's area.
                assert_eq!(pixels[5 * src_w as usize + 5], 0xFFFF_0000);
                // Inside the green item's area.
                assert_eq!(pixels[5 * src_w as usize + 25], 0xFF00_FF00);
                // A gap belonging to neither area stays transparent.
                assert_eq!((pixels[5 * src_w as usize + 15] >> 24) & 0xff, 0);
            }
            _ => panic!("Image のはず"),
        }
    }
}
