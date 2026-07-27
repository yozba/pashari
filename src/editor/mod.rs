//! Annotation editor.
//!
//! Edits the image cropped from a selected region. A tool stays active
//! after one use, rather than snapping back to Select. Self-drawn via
//! softbuffer, running in the same single event loop as overlay. Save
//! writes a PNG; Copy puts the annotated image on the clipboard.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId, WindowLevel};

use crate::export::Shot;
use crate::localkey::LocalKey;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, PickerPart, Rect, hsv_to_rgb, marker, rgb_to_hsv, rgba_to_packed_alpha};

mod annotation;
mod icons;
mod session;
use icons::IconSet;

use annotation::{
    Annotation, Handle, MosaicMode, PropVal, StyleVals, Xform, annotations_bounds, common_rotation,
    common_style, compose_rgba, dim_outside_guide, group_rect_for_rotation, guide_bounds,
    handle_cursor, hit_item, hit_items_in_rect, hit_rect_handle, hit_rect_handle_f64, item_bbox,
    merge_images, near, near_f64, next_marker_number, paint_annotations, paint_bbox,
    paint_group_handles, paint_selection, random_seed, rasterize_freehand, rect_norm,
    rect_norm_f64, reorder_selection, resize_rect, resize_rotated_rect, resize_rotated_rect_aspect,
    rotate_annotation_around, rotate_handle_local, scale_annotation_rotated, snap_angle_45,
    text_local_rect, to_local, translate_annotation,
};

/// Top bar height (Pin/Save/Copy laid out horizontally).
const TOOLBAR_H: usize = 46;
/// Left bar width (Arrow/Rect/Text tools laid out vertically).
const TOOLBAR_W: usize = 56;
/// Toolbar buttons (left bar's drawing tools, top bar's Pin/Save/Copy) are
/// all 1:1 square icon buttons.
const BTN_SIZE: usize = 40;
/// Padding around the icon drawn inside a button (keeps it inset from the square edge).
const ICON_PAD: usize = 6;
const BTN_GAP: usize = 6;
/// Gap between Pin and Save in the top bar.
const GROUP_GAP: usize = 18;
/// Minimum layout size (so the top bar's properties plus Pin/Save/Copy fit).
const MIN_W: usize = 470;
const MIN_H: usize = 300;
/// Default canvas size when opened blank (no image).
const BLANK_DISP_W: usize = 800;
const BLANK_DISP_H: usize = 500;

/// The checkerboard pattern marking transparency (two low-contrast colors) and its cell size.
const CHECK_A: u32 = 0x001C_1C1C;
const CHECK_B: u32 = 0x0026_2626;
const CHECK_SIZE: usize = 10;
/// Sentinel for coverage testing on export; never matches any item's color/pixel (top byte 0).
const EXPORT_SENTINEL: u32 = 0xDEAD_BEEF;

const TOOLBAR_BG: u32 = 0x0025_2525;
const BTN_BG: u32 = 0x0033_3333;
const BTN_HOVER: u32 = 0x0045_4545;
const BTN_ACTIVE: u32 = 0x004D_A6FF;
const TEXT_COLOR: u32 = 0x00EA_EAEA;

/// Default style for a new item (drawing/text/palette initial values).
const DEFAULT_COLOR: u32 = 0x00E0_3030;
const DEFAULT_THICK: i64 = 4;
const DEFAULT_SIZE: f32 = 24.0;
const DEFAULT_BLOCK: f32 = 16.0;

/// Allowed range for thickness/text size (px).
const THICK_RANGE: (f64, f64) = (1.0, 200.0);
const SIZE_RANGE: (f64, f64) = (4.0, 400.0);
/// Allowed range for a mosaic block's side length (world px).
const BLOCK_RANGE: (f64, f64) = (4.0, 200.0);
/// Allowed range for a number marker's "next number to place."
const NUMBER_RANGE: (f64, f64) = (1.0, 9999.0);

/// Cap on the export canvas's side length (px), so allocation doesn't
/// blow up even if items are scattered far apart.
const EXPORT_MAX: usize = 20000;

/// Toolbar UI font size (px), matched to a typical editor workbench UI.
const UI_FONT_SIZE: f32 = 13.0;

/// Zoom range and the multiplier per wheel notch.
const ZOOM_MIN: f64 = 0.05;
const ZOOM_MAX: f64 = 16.0;
const ZOOM_STEP: f64 = 1.1;

/// Dimensions of the top bar's left-side properties (color swatch, label,
/// -/+ stepper, numeric field).
const SWATCH: usize = 26;
/// Width of the color field (wider than the square swatch, to fit the
/// "Mixed" label text too).
const COLOR_W: usize = 64;
const FIELD_W: usize = 48;
const FIELD_H: usize = 26;
const FIELD_BG: u32 = 0x0033_3333;
/// Gap between a label ("Color"/"Width"/"Text size") and the control to
/// its right. The label's own width is measured (`label_col_w`), so this
/// gap stays consistent regardless of label length.
const LABEL_GAP: usize = 10;
/// Rough fallback label width when no font is available (e.g. in tests).
const COLOR_LABEL_W_FALLBACK: usize = 40;
/// Gap from the color field to the next label (Width/Text size). Wider
/// than `LABEL_GAP` so the color field and label don't look cramped together.
const COLOR_FIELD_GAP: usize = 24;
/// Rough fallback field-label width when no font is available (e.g. in
/// tests); sized to fit "Text size".
const LABEL_W_FALLBACK: usize = 72;
const STEP_W: usize = 22;
/// Width of the Fill toggle button, and its gap from the Width field before it.
const FILL_W: usize = 52;
const FILL_GAP: usize = 16;
/// Width of the Blur toggle button, and its gap from the Block field before it.
const BLUR_W: usize = 52;
const BLUR_GAP: usize = 16;

/// Color picker popup dimensions and colors.
const PICK_SV: usize = 128;
const PICK_HUE_H: usize = 14;
const PICK_PAD: usize = 8;
const PICK_BG: u32 = 0x0022_2222;

/// Selection outline (border/handles) color.
const SEL_COLOR: u32 = 0x004D_A6FF;
/// Drawn radius of a handle circle (px).
const HANDLE_HALF: i64 = 6;
/// Grab tolerance for items/handles (screen px; converted to world
/// coordinates via /scale).
const SELECT_TOL: f64 = 6.0;
/// During a Freehand drag, the minimum distance (world px) from the last
/// accepted point before the next point is accepted. `CursorMoved` fires
/// densely, so without this thinning a single stroke would accumulate
/// far too many points.
const FREEHAND_MIN_STEP: f64 = 2.0;
/// Offset (world px) applied when pasting with Ctrl+V, so the paste
/// doesn't land exactly on top of the copied item.
const PASTE_OFFSET: i64 = 16;

/// An editing tool. Select is for selecting/editing; the rest draw
/// continuously. Guide places the export-bounds guide (at most one; placing
/// a new one replaces the existing one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Select,
    Arrow,
    Polyline,
    Draw,
    Rect,
    Ellipse,
    Text,
    NumberMarker,
    Guide,
    Mosaic,
}

/// Whether the distance from `last` to `p` is at least `FREEHAND_MIN_STEP` (for freehand point thinning).
fn should_sample_freehand_point(last: (i64, i64), p: (i64, i64)) -> bool {
    let (dx, dy) = ((p.0 - last.0) as f64, (p.1 - last.1) as f64);
    dx * dx + dy * dy >= FREEHAND_MIN_STEP * FREEHAND_MIN_STEP
}

/// Whether local coordinate `lp` is within `tol` of rect `rect`'s (the
/// bounding rect drawn as a selection outline) border — tested as a
/// `tol`-wide band on both the inside and outside, using the same
/// tolerance as handle hit-testing. The band is hollow inside, so clicking
/// near the rect's center never matches — this enforces the requirement
/// that dragging only works on the bounding rect's outline itself.
fn near_rect_outline_local(rect: (f64, f64, f64, f64), lp: (f64, f64), tol: f64) -> bool {
    let (x0, y0, x1, y1) = rect_norm_f64(rect);
    let outer = lp.0 >= x0 - tol && lp.0 <= x1 + tol && lp.1 >= y0 - tol && lp.1 <= y1 + tol;
    let inner = lp.0 >= x0 + tol && lp.0 <= x1 - tol && lp.1 >= y0 + tol && lp.1 <= y1 - tol;
    outer && !inner
}

/// An in-progress edit drag (Select tool). Coordinates are world coordinates.
enum EditDrag {
    None,
    /// Whole-body drag moving the entire item (`grab` = grabbed position, `orig` = item at grab time).
    Move {
        grab: (i64, i64),
        orig: Annotation,
    },
    /// Rect-handle resize (`orig` = normalized rect at grab time). Guide
    /// only (stays integer, since it has no rotation).
    RectHandle {
        h: Handle,
        orig: (i64, i64, i64, i64),
    },
    /// Handle resize for Rect/Ellipse/Image (rotatable shapes). To avoid
    /// jitter, integer rounding (the source of jitter) is avoided, and the
    /// rect at grab time is also kept as floats (Image's aspect lock while
    /// Shift is held is handled separately in apply_edit).
    RotatedHandle {
        h: Handle,
        orig: (f64, f64, f64, f64),
    },
    /// Dragging an arrow's endpoint (`end_b`=true for the b end).
    ArrowEnd {
        end_b: bool,
    },
    /// Dragging a polyline vertex (`index` = the vertex being dragged).
    PolylinePoint {
        index: usize,
    },
    /// Rotating Rect/Ellipse/Image/Text (`center` = rotation center, fixed
    /// at the rect's center when grabbed).
    Rotate {
        center: (f64, f64),
    },
    /// Dragging a multi-selection together. `origs` = each item (with
    /// index) at grab time.
    GroupMove {
        grab: (i64, i64),
        origs: Vec<(usize, Annotation)>,
    },
    /// Resizing via a multi-selection's bounding-rect handle. `orig_rect` =
    /// the bounding rect at grab time (local coordinates from
    /// `group_rect_for_rotation`), `rot` = its rotation angle (fixed during the drag).
    GroupHandle {
        h: Handle,
        orig_rect: (f64, f64, f64, f64),
        rot: f64,
        origs: Vec<(usize, Annotation)>,
    },
    /// Rotating via a multi-selection's bounding-rect rotate handle.
    /// `center` = the bounding rect's center (fixed); `orig_rect`/`orig_rot`
    /// = the bounding rect at grab time; `start_angle` = the center-to-cursor
    /// angle at grab time (not an absolute angle — the delta from it is
    /// applied to every item each frame). `orig_rect`/`orig_rot` aren't
    /// applied to the items themselves, only used to display the bounding
    /// rect (`draw()`) — during the drag, `group_rect_for_rotation` isn't
    /// recomputed every frame; it's instead derived directly from this
    /// fixed value plus the delta angle, avoiding a bug where the selected
    /// items' bounding-rect estimate (based on the axis-aligned bbox) falls
    /// out of sync with the in-progress rotation and appears to drift.
    GroupRotate {
        center: (f64, f64),
        start_angle: f64,
        orig_rect: (f64, f64, f64, f64),
        orig_rot: f64,
        origs: Vec<(usize, Annotation)>,
    },
}

/// A toolbar button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditorBtn {
    Tool(Tool),
    /// Always-on-top toggle (default off).
    Pin,
    Save,
    Copy,
}

/// A numeric input field (thickness / text size / next number marker to place).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Line,
    Size,
    /// NumberMarker tool only. Edits tool-level state — the next marker's
    /// number to place — rather than an existing item's property.
    Number,
    /// Mosaic only. The block's side length.
    Block,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Line => "Width",
            Field::Size => "Size",
            Field::Number => "Number",
            Field::Block => "Block",
        }
    }
}

/// A top-bar-left property control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PropCtrl {
    Color,
    /// A numeric input field (click to focus).
    Field(Field),
    /// A ±stepper (`true` = increase).
    Step(Field, bool),
    /// Fill toggle (shown only for Rect/Ellipse).
    Fill,
    /// Pixelate/Blur toggle (shown only for Mosaic).
    Blur,
}

/// Undo/Redo history. `T` is a full snapshot of state at a point in time
/// (not a diff). Simpler than a diff-based implementation, and only
/// designed for cases where copying a snapshot is cheap (`Annotation`'s
/// image pixels are `Rc`-shared, so cloning a `Vec<Annotation>` is cheap).
struct UndoStack<T> {
    undo: Vec<T>,
    redo: Vec<T>,
}

impl<T: Clone> UndoStack<T> {
    fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Pushes the pre-change state `current` onto the undo history and
    /// clears the redo history (a new change makes any later redo meaningless).
    fn push(&mut self, current: T) {
        self.undo.push(current);
        self.redo.clear();
    }

    /// Returns the previous state, pushing `current` (the state before
    /// reverting) onto the redo history. `None` and a no-op if there's no history.
    fn undo(&mut self, current: T) -> Option<T> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        Some(prev)
    }

    /// Redoes one undo. `None` and a no-op if there's no history.
    fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }
}

/// A pre-rendered cache of committed items plus the checkerboard
/// background, used only during a Freehand drag. During a drag, it's
/// safe to assume neither the committed items nor the Xform (scale/offset)
/// change, so a large screenshot doesn't need resampling every frame. If
/// the size or Xform ever mismatches the current frame (window resize, or
/// rarely a wheel zoom mid-drag), that frame falls back to normal
/// rendering and rebuilds the cache.
struct FreehandBgCache {
    w: usize,
    h: usize,
    scale: f64,
    ox: f64,
    oy: f64,
    buf: Vec<u32>,
}

pub struct Editor {
    window: Option<Rc<Window>>,
    _context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// The window's logical (DPI-independent) size, used for all layout,
    /// hit-testing, and the image zoom/pan math below — see `dpi`.
    surface_size: (usize, usize),
    /// The window/surface's actual physical pixel size.
    physical_size: (usize, usize),
    /// The window's DPI scale factor (`window.scale_factor()`), used to
    /// convert between logical (`surface_size`) and physical
    /// (`physical_size`) — see the module doc in `src/settings/mod.rs` for
    /// the general approach this mirrors. Named `dpi` (not `scale`) to
    /// avoid colliding with the image zoom/pan `scale` below, a separate,
    /// user-controlled concept that operates entirely in logical pixels.
    dpi: f64,

    /// Display scale (zoom) and the world origin's offset within the
    /// window. `f64` since zoom/pan can go negative.
    scale: f64,
    offset: (f64, f64),
    /// Reset target for Ctrl+0 (the initial fit-to-window scale and offset).
    home_scale: f64,
    home_offset: (f64, f64),
    /// Last cursor position during a middle-button pan (`None` = not panning).
    pan: Option<(f64, f64)>,

    tool: Tool,
    cursor: (f64, f64),
    drag_start: Option<(i64, i64)>,
    dragging: bool,
    /// Vertices accumulated for the Polyline tool before committing
    /// (empty = inactive). Kept separate from `drag_start`/`dragging`
    /// since points are added per-click rather than via a drag.
    polyline_pts: Vec<(i64, i64)>,
    /// Points accumulated (with thinning) during a Freehand drag (empty =
    /// inactive). Used together with `dragging`/`drag_start`; rasterized
    /// into an `Annotation::Image` on release (`rasterize_freehand`).
    freehand_pts: Vec<(i64, i64)>,
    /// Indices of committed Freehand items not yet merged. Merged into
    /// one via `merge_images` by `finalize_freehand_batch` whenever the
    /// tool changes (including reselecting the same Freehand button).
    freehand_batch: Vec<usize>,
    /// Background cache during a Freehand drag (`Some` only while dragging).
    freehand_bg_cache: Option<FreehandBgCache>,
    annotations: Vec<Annotation>,
    /// Items copied with Ctrl+C (Copy with no selection copies an image to
    /// the OS clipboard instead, and doesn't use this). Ctrl+V pastes
    /// these as items if non-empty, falling back to pasting a clipboard
    /// image if empty.
    item_clipboard: Vec<Annotation>,
    /// Indices of selected items (the Select tool shows their
    /// handles/outline). Always ascending, no duplicates. A multi-selection
    /// shows no handles, only a lightweight bbox outline.
    selected: Vec<usize>,
    /// The multi-selection's bounding rect (local coordinates) and
    /// rotation angle. Recomputed from the selected items' current
    /// positions via `recompute_group_frame` whenever `self.selected`
    /// changes; once a group resize/rotate drag finishes, its final
    /// result is written back as-is (re-deriving from the axis-aligned
    /// bbox would fall out of sync with the rotation center used during
    /// the drag, making the bounding rect appear to drift). Otherwise
    /// (e.g. single-selection operations), it's kept as-is.
    group_rect: (f64, f64, f64, f64),
    group_rot: f64,
    /// The in-progress edit drag.
    edit: EditDrag,
    /// Undo/redo history (snapshots of `annotations`).
    history: UndoStack<Vec<Annotation>>,
    /// Default style for new items (changed via property controls).
    cur_color: u32,
    cur_thick: i64,
    cur_size: f32,
    /// Block side length (world px) used for a new mosaic.
    cur_block: f32,
    /// The next number marker's number to place. Incremented each time a
    /// marker is placed; can also be edited directly via the property
    /// field (`Field::Number`). Excluded from undo/redo — it's tool-level
    /// state (the next value to use), not part of `annotations`' content.
    next_marker_num: u32,
    /// Whether a new Rect/Ellipse is created filled (changed via the Fill toggle).
    cur_filled: bool,
    /// Whether a new Mosaic is created in Blur mode (changed via the Blur toggle).
    cur_blur: bool,
    /// PRNG seed used only while drag-creating a new Mosaic (assigned in
    /// `on_press`; the same value is used for both the preview and the
    /// committed item). Transient state excluded from undo/redo, even more
    /// short-lived than `next_marker_num`.
    new_item_seed: u32,
    /// Focus and edit buffer for a numeric input field.
    focus: Option<Field>,
    buf: String,
    /// Color picker (`Some` = open, holding HSV) and its drag target.
    picker: Option<(f32, f32, f32)>,
    picker_drag: Option<PickerPart>,
    /// (position, in-progress string) while editing text.
    editing: Option<((i64, i64), String)>,
    /// Uncommitted string from IME composition (`Ime::Preedit`), for the pre-commit preview.
    ime_preedit: String,

    hover: Option<EditorBtn>,
    pressed: Option<EditorBtn>,
    /// Whether always-on-top is active (the Pin toggle, default false).
    pinned: bool,
    ctrl: bool,
    /// Whether Shift is held (also used to lock aspect ratio when resizing an image).
    shift: bool,
    /// Whether Alt is held (for hotkey detection).
    alt: bool,
    /// This editor's local key bindings (changeable via the settings GUI's
    /// Hotkeys tab).
    keys: EditorKeys,
    text: Option<TextRenderer>,
    /// Toolbar button icons (decoded once at startup).
    icons: IconSet,
}

/// This editor's set of local key bindings (Undo/Redo are shared with the
/// region-selection overlay). Escape, Delete/Backspace, and
/// paste/save/copy (fixed to Ctrl+V/S/C) are excluded and always fixed.
/// Everything else can have multiple keys bound to one action, so each is
/// kept as a `Vec` (empty = unbound).
struct EditorKeys {
    undo: Vec<LocalKey>,
    redo: Vec<LocalKey>,
    reset_zoom: Vec<LocalKey>,
    tool_select: Vec<LocalKey>,
    tool_arrow: Vec<LocalKey>,
    tool_polyline: Vec<LocalKey>,
    tool_draw: Vec<LocalKey>,
    tool_rect: Vec<LocalKey>,
    tool_ellipse: Vec<LocalKey>,
    tool_text: Vec<LocalKey>,
    tool_number_marker: Vec<LocalKey>,
}

impl EditorKeys {
    fn from_config(cfg: &crate::store::hotkeys::HotkeyConfig) -> Self {
        let get = |specs: &[String]| -> Vec<LocalKey> {
            specs
                .iter()
                .filter_map(|s| crate::localkey::parse(s))
                .collect()
        };
        Self {
            undo: get(&cfg.hotkey_undo),
            redo: get(&cfg.hotkey_redo),
            reset_zoom: get(&cfg.hotkey_editor_reset_zoom),
            tool_select: get(&cfg.hotkey_editor_tool_select),
            tool_arrow: get(&cfg.hotkey_editor_tool_arrow),
            tool_polyline: get(&cfg.hotkey_editor_tool_polyline),
            tool_draw: get(&cfg.hotkey_editor_tool_draw),
            tool_rect: get(&cfg.hotkey_editor_tool_rect),
            tool_ellipse: get(&cfg.hotkey_editor_tool_ellipse),
            tool_text: get(&cfg.hotkey_editor_tool_text),
            tool_number_marker: get(&cfg.hotkey_editor_tool_number_marker),
        }
    }
}

/// The scale factor to fit an image `(w, h)` within `(max_w, max_h)` (never upscales).
fn fit_scale(w: usize, h: usize, max_w: usize, max_h: usize) -> f64 {
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    s.clamp(0.05, 1.0)
}

/// `Editor::new`'s initial state. `Blank` = empty; `Shot` = the
/// just-captured image as the sole Image item (the original behavior);
/// `Session` = opens directly with the item set restored from a saved session.
enum EditorInit {
    Blank,
    Shot(Shot),
    Session {
        width: usize,
        height: usize,
        annotations: Vec<Annotation>,
    },
}

impl Editor {
    /// Creates the editor from a cropped image and its window. If `init`
    /// is `Blank`, the editor opens empty (launched from the tray's
    /// Editor entry; an image item can be added later via clipboard paste
    /// or file drag-and-drop). If `Session`, restores the item set from a
    /// saved session as-is.
    fn new(event_loop: &ActiveEventLoop, init: EditorInit, monitor: (usize, usize, f64)) -> Self {
        let keys = EditorKeys::from_config(&crate::store::hotkeys::snapshot());

        let (img_size, annotations) = match init {
            EditorInit::Blank => (None, Vec::new()),
            EditorInit::Shot(shot) => {
                let (img_w, img_h) = (shot.width as usize, shot.height as usize);
                // Converts the screenshot to 0xAARRGGBB and places it as an
                // Image item at the world origin (a screen capture is
                // always opaque, but paint_image interprets alpha, so
                // rgba_to_packed_alpha is still needed).
                let base = rgba_to_packed_alpha(img_w, img_h, &shot.rgba);
                let item = Annotation::Image {
                    r: (0.0, 0.0, img_w as f64, img_h as f64),
                    src_w: img_w as i64,
                    src_h: img_h as i64,
                    pixels: Rc::new(base),
                    rot: 0.0,
                };
                (Some((img_w, img_h)), vec![item])
            }
            EditorInit::Session {
                width,
                height,
                annotations,
            } => (Some((width, height)), annotations),
        };

        // `monitor` is physical; convert to logical up front so every
        // constant below (TOOLBAR_W/H, MIN_W/H, ...) can keep meaning a
        // fixed logical-pixel count, same as every other window in this
        // codebase — the window itself is created with a logical size
        // below, and the final on-screen `Canvas` (in `draw()`) is what
        // converts back to physical for DPI-crisp rendering.
        let monitor = (
            ((monitor.0 as f64) / monitor.2).round() as usize,
            ((monitor.1 as f64) / monitor.2).round() as usize,
        );

        // A horizontal bar on top, a vertical bar on the left; the image
        // sits inside them (bottom-right). Uses the default size if blank.
        let max_w = (monitor.0 * 9 / 10).saturating_sub(TOOLBAR_W);
        let max_h = (monitor.1 * 9 / 10).saturating_sub(TOOLBAR_H);
        let (scale, disp_w, disp_h) = match img_size {
            Some((img_w, img_h)) => {
                let scale = fit_scale(img_w, img_h, max_w.max(1), max_h.max(1));
                (
                    scale,
                    ((img_w as f64 * scale) as usize).max(1),
                    ((img_h as f64 * scale) as usize).max(1),
                )
            }
            None => (1.0, BLANK_DISP_W, BLANK_DISP_H),
        };

        let win_w = (TOOLBAR_W + disp_w).max(MIN_W);
        let win_h = (TOOLBAR_H + disp_h).max(MIN_H);
        // Centers the image within the canvas area (right of the left bar, below the top bar).
        let offset = (
            (TOOLBAR_W + (win_w - TOOLBAR_W - disp_w) / 2) as f64,
            (TOOLBAR_H + (win_h - TOOLBAR_H - disp_h) / 2) as f64,
        );

        let pos_x = (monitor.0.saturating_sub(win_w) / 2) as i32;
        let pos_y = (monitor.1.saturating_sub(win_h) / 2) as i32;

        let attrs = Window::default_attributes()
            .with_title("pashari Editor")
            .with_resizable(true)
            .with_min_inner_size(LogicalSize::new(MIN_W as f64, MIN_H as f64))
            .with_window_level(WindowLevel::Normal)
            .with_position(LogicalPosition::new(pos_x as f64, pos_y as f64))
            .with_inner_size(LogicalSize::new(win_w as f64, win_h as f64));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("エディタウィンドウ生成に失敗"),
        );

        let context = softbuffer::Context::new(window.clone()).expect("editor context");
        let mut surface =
            softbuffer::Surface::new(&context, window.clone()).expect("editor surface");
        let size = window.inner_size();
        let (pw, ph) = (size.width.max(1), size.height.max(1));
        surface
            .resize(NonZeroU32::new(pw).unwrap(), NonZeroU32::new(ph).unwrap())
            .expect("editor resize");
        window.request_redraw();

        let dpi = window.scale_factor();
        let surface_size = (
            ((pw as f64) / dpi).round().max(1.0) as usize,
            ((ph as f64) / dpi).round().max(1.0) as usize,
        );

        // If markers already exist (e.g. restored from a session), continue numbering from there.
        let next_marker_num = next_marker_number(&annotations);

        Self {
            window: Some(window),
            _context: Some(context),
            surface: Some(surface),
            surface_size,
            physical_size: (pw as usize, ph as usize),
            dpi,
            scale,
            offset,
            home_scale: scale,
            home_offset: offset,
            pan: None,
            tool: Tool::Select,
            cursor: (0.0, 0.0),
            drag_start: None,
            dragging: false,
            polyline_pts: Vec::new(),
            freehand_pts: Vec::new(),
            freehand_batch: Vec::new(),
            freehand_bg_cache: None,
            annotations,
            item_clipboard: Vec::new(),
            selected: Vec::new(),
            group_rect: (0.0, 0.0, 0.0, 0.0),
            group_rot: 0.0,
            edit: EditDrag::None,
            history: UndoStack::new(),
            cur_color: DEFAULT_COLOR,
            cur_thick: DEFAULT_THICK,
            cur_size: DEFAULT_SIZE,
            cur_block: DEFAULT_BLOCK,
            next_marker_num,
            cur_filled: false,
            cur_blur: false,
            new_item_seed: 0,
            focus: None,
            buf: String::new(),
            picker: None,
            picker_drag: None,
            editing: None,
            ime_preedit: String::new(),
            hover: None,
            pressed: None,
            pinned: false,
            ctrl: false,
            shift: false,
            alt: false,
            keys,
            text: TextRenderer::load(),
            icons: IconSet::load(),
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Called before a change, pushing the current `annotations` onto the
    /// undo history wholesale (the redo history is cleared, since this is
    /// a new change). Not called for continuous updates during a drag
    /// (e.g. per-pixel movement); the caller should only call this at the
    /// start of an operation.
    fn push_undo(&mut self) {
        self.history.push(self.annotations.clone());
    }

    /// Cleanup after undo/redo (resets selection, in-progress edit, and
    /// text input, then redraws).
    fn restore_annotations(&mut self, snapshot: Vec<Annotation>) {
        self.annotations = snapshot;
        self.selected.clear();
        // Since annotations is replaced wholesale, the tracked Freehand
        // batch's indices no longer mean anything — just discard them,
        // erring on the safe side. The only effect is that strokes up to
        // that point stay as separate items, which is harmless.
        self.freehand_batch.clear();
        self.recompute_group_frame();
        self.edit = EditDrag::None;
        self.editing = None;
        self.disable_ime();
        self.request_redraw();
    }

    /// Merges committed Freehand strokes accumulated up to a tool change
    /// (including reselecting the same Freehand button) into one item.
    /// With fewer than 2 strokes there's nothing to merge, so this just
    /// clears the tracking. The merged result isn't selected (no handles
    /// shown; the outline drawn during the stroke reads `freehand_batch`
    /// directly each frame).
    fn finalize_freehand_batch(&mut self) {
        let mut idxs = std::mem::take(&mut self.freehand_batch);
        idxs.retain(|&i| i < self.annotations.len());
        idxs.sort_unstable();
        idxs.dedup();
        if idxs.len() < 2 {
            return;
        }
        let items: Vec<Annotation> = idxs.iter().map(|&i| self.annotations[i].clone()).collect();
        let merged = merge_images(&items);
        self.push_undo();
        // Removing from the front would shift later indices, so remove in descending order.
        for &i in idxs.iter().rev() {
            self.annotations.remove(i);
        }
        self.annotations.push(merged);
    }

    /// Called whenever `self.selected` changes: recomputes the
    /// multi-selection's bounding rect (`group_rect`/`group_rot`) entirely
    /// from the selected items' current positions. The rotation angle is
    /// the effective rotation if it agrees, else 0.0. Not called otherwise
    /// (mid-drag, or single-selection operations) — the bounding rect is
    /// kept as-is until the selection changes (once a group resize/rotate
    /// drag finishes, its result is written back directly instead, via
    /// `active_group_frame`).
    fn recompute_group_frame(&mut self) {
        self.group_rot = common_rotation(&self.annotations, &self.selected);
        self.group_rect = group_rect_for_rotation(
            &self.annotations,
            &self.selected,
            self.group_rot,
            self.text.as_ref(),
        )
        .unwrap_or((0.0, 0.0, 0.0, 0.0));
    }

    /// The bounding rect at the current cursor position, for an
    /// in-progress group transform (move/resize/rotate) drag — used both
    /// for display and to write back once the drag commits. `None` if not dragging.
    fn active_group_frame(&self) -> Option<((f64, f64, f64, f64), f64)> {
        match &self.edit {
            EditDrag::GroupMove { grab, .. } => {
                let p = self.to_world(self.cursor.0, self.cursor.1);
                let (dx, dy) = ((p.0 - grab.0) as f64, (p.1 - grab.1) as f64);
                let r = self.group_rect;
                Some(((r.0 + dx, r.1 + dy, r.2 + dx, r.3 + dy), self.group_rot))
            }
            EditDrag::GroupHandle {
                h, orig_rect, rot, ..
            } => {
                let p_f = self.to_world_f64(self.cursor.0, self.cursor.1);
                Some((resize_rotated_rect(*orig_rect, *h, p_f, *rot), *rot))
            }
            EditDrag::GroupRotate {
                center,
                start_angle,
                orig_rect,
                orig_rot,
                ..
            } => {
                let p = self.to_world(self.cursor.0, self.cursor.1);
                let cur_angle = (p.1 as f64 - center.1).atan2(p.0 as f64 - center.0);
                let delta = cur_angle - start_angle;
                Some((*orig_rect, orig_rot + delta))
            }
            // While dragging a Polyline vertex, there's no fixed "local
            // rect plus rotation angle" reference (rotation is always 0 —
            // axis-aligned), so unlike Rect's rotation drag there's no
            // risk of display drift, and the current bbox can just be
            // recomputed plainly every frame.
            EditDrag::PolylinePoint { .. } => {
                let i = *self.selected.first()?;
                let ann = self.annotations.get(i)?;
                let (x0, y0, x1, y1) = item_bbox(ann, self.text.as_ref());
                Some(((x0 as f64, y0 as f64, x1 as f64, y1 as f64), 0.0))
            }
            // A single selected Polyline also shows its bounding-rect
            // handles during a whole-body (Move) drag (the single_polyline
            // path in `begin_select_drag`), so like the vertex drag, the
            // bbox needs recomputing every frame. Other shapes
            // (Rect/Arrow/Text/etc.) draw handles directly from their own
            // r/rot without using group_rect, so `None` is fine here for them.
            EditDrag::Move { .. } => {
                let i = *self.selected.first()?;
                let ann = self.annotations.get(i)?;
                if !matches!(ann, Annotation::Polyline { .. }) {
                    return None;
                }
                let (x0, y0, x1, y1) = item_bbox(ann, self.text.as_ref());
                Some(((x0 as f64, y0 as f64, x1 as f64, y1 as f64), 0.0))
            }
            _ => None,
        }
    }

    /// `Ctrl+Z`: reverts to the previous state. A no-op mid-drag/resize/
    /// move/picker operation, since the target would be ambiguous (use
    /// after finishing the operation).
    fn undo(&mut self) {
        if self.dragging || !matches!(self.edit, EditDrag::None) || self.picker_drag.is_some() {
            return;
        }
        if let Some(prev) = self.history.undo(self.annotations.clone()) {
            self.restore_annotations(prev);
        }
    }

    /// `Ctrl+Shift+Z`: redoes one undo.
    fn redo(&mut self) {
        if self.dragging || !matches!(self.edit, EditDrag::None) || self.picker_drag.is_some() {
            return;
        }
        if let Some(next) = self.history.redo(self.annotations.clone()) {
            self.restore_annotations(next);
        }
    }

    fn button_at(&self, x: usize, y: usize) -> Option<EditorBtn> {
        let (_, top_btns) = top_layout(
            self.surface_size.0,
            self.active_field(),
            self.active_fill_checkbox(),
            self.text.as_ref(),
        );
        tool_buttons()
            .into_iter()
            .chain(top_btns)
            .find(|(_, r)| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
            .map(|(b, _)| b)
    }

    /// Hit-tests the top-bar-left property controls.
    fn prop_at(&self, x: usize, y: usize) -> Option<PropCtrl> {
        let (props, _) = top_layout(
            self.surface_size.0,
            self.active_field(),
            self.active_fill_checkbox(),
            self.text.as_ref(),
        );
        props
            .into_iter()
            .find(|(_, r)| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
            .map(|(c, _)| c)
    }

    /// The property field currently being edited (selected item takes
    /// priority, falling back to the current tool if nothing's selected).
    /// `None` for the Select tool with nothing selected (no Width/Text
    /// size shown). With a multi-selection, judged across the whole
    /// selection: `Size` if everything is Text, `Line` if any is
    /// Arrow/Rect/Ellipse, `None` otherwise (only Image/Guide). The
    /// NumberMarker tool is an exception — since it edits tool-level state
    /// (the next number to place), it always returns `Field::Number` with
    /// top priority, unaffected by the selection (even the marker just placed).
    fn active_field(&self) -> Option<Field> {
        if self.tool == Tool::NumberMarker {
            return Some(Field::Number);
        }
        let items: Vec<&Annotation> = self
            .selected
            .iter()
            .filter_map(|&i| self.annotations.get(i))
            .collect();
        if !items.is_empty() {
            if items
                .iter()
                .all(|a| matches!(a, Annotation::Text { .. } | Annotation::NumberMarker { .. }))
            {
                return Some(Field::Size);
            }
            if items.iter().all(|a| matches!(a, Annotation::Mosaic { .. })) {
                return Some(Field::Block);
            }
            if items.iter().any(|a| {
                matches!(
                    a,
                    Annotation::Arrow { .. }
                        | Annotation::Polyline { .. }
                        | Annotation::Rect { .. }
                        | Annotation::Ellipse { .. }
                )
            }) {
                return Some(Field::Line);
            }
            return None;
        }
        match self.tool {
            Tool::Arrow | Tool::Polyline | Tool::Draw | Tool::Rect | Tool::Ellipse => {
                Some(Field::Line)
            }
            Tool::Text => Some(Field::Size),
            // Never actually reached (already returned early at the top of the function).
            Tool::NumberMarker => Some(Field::Number),
            Tool::Mosaic => Some(Field::Block),
            Tool::Select | Tool::Guide => None,
        }
    }

    /// Whether the Fill toggle should be shown (selected item takes
    /// priority, falling back to the current tool if nothing's selected).
    /// Only applies to Rect/Ellipse (Arrow/Polyline/Text have no "fill" concept).
    fn active_fill_checkbox(&self) -> bool {
        let items: Vec<&Annotation> = self
            .selected
            .iter()
            .filter_map(|&i| self.annotations.get(i))
            .collect();
        if !items.is_empty() {
            return items
                .iter()
                .any(|a| matches!(a, Annotation::Rect { .. } | Annotation::Ellipse { .. }));
        }
        matches!(self.tool, Tool::Rect | Tool::Ellipse)
    }

    /// Window coordinates to world (absolute) coordinates. Never clamped, assuming an infinite canvas.
    fn to_world(&self, wx: f64, wy: f64) -> (i64, i64) {
        let x = ((wx - self.offset.0) / self.scale).round() as i64;
        let y = ((wy - self.offset.1) / self.scale).round() as i64;
        (x, y)
    }

    /// The unrounded version of `to_world`. Use this when passing to
    /// something that rotates the coordinate before rounding, like
    /// resizing a rotated rect (`resize_rotated_rect`). Rounding to
    /// integers first and then rotating would round again after rotation
    /// mixes x/y together, causing handles to jitter even with smooth
    /// cursor movement.
    fn to_world_f64(&self, wx: f64, wy: f64) -> (f64, f64) {
        (
            (wx - self.offset.0) / self.scale,
            (wy - self.offset.1) / self.scale,
        )
    }

    /// Grab tolerance (world coordinates); divided by scale so it stays a fixed px value on screen.
    fn grab_tol(&self) -> f64 {
        SELECT_TOL / self.scale
    }

    /// Commits the in-progress text and pushes it as an annotation (at the current color/size).
    fn commit_text(&mut self) {
        if let Some((pos, buf)) = self.editing.take() {
            if !buf.is_empty() {
                self.push_undo();
                self.annotations.push(Annotation::Text {
                    pos,
                    text: buf,
                    color: self.cur_color,
                    size: self.cur_size,
                    rot: 0.0,
                });
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
            }
            self.disable_ime();
        }
    }

    /// Enables the IME and positions its candidate window near where editing started.
    fn enable_ime_at(&mut self, world_pos: (i64, i64)) {
        let Some(w) = self.window.as_ref() else {
            return;
        };
        let t = Xform {
            scale: self.scale,
            ox: self.offset.0,
            oy: self.offset.1,
        };
        let (sx, sy) = t.map(world_pos);
        w.set_ime_allowed(true);
        w.set_ime_cursor_area(
            PhysicalPosition::new(sx.max(0) as i32, sy.max(0) as i32),
            PhysicalSize::new(1u32, t.text_size(self.cur_size) as u32),
        );
    }

    /// Disables the IME and clears any in-progress composition string too.
    fn disable_ime(&mut self) {
        self.ime_preedit.clear();
        if let Some(w) = self.window.as_ref() {
            w.set_ime_allowed(false);
        }
    }

    /// What happens when a button is pressed (the Ctrl+S/Ctrl+C shortcuts
    /// call this too). Save/Copy directly save/copy here; the editor doesn't close.
    fn activate(&mut self, btn: EditorBtn) {
        match btn {
            EditorBtn::Tool(t) => {
                self.commit_text();
                // Leaving/reselecting a drawing tool merges any Freehand
                // strokes accumulated so far into one item (not selected
                // afterward — only the bounding rect is drawn during the
                // stroke, with no handles once committed).
                self.finalize_freehand_batch();
                self.tool = t;
                // Changing tools clears selection/editing (handles only show in Select).
                self.selected.clear();
                self.recompute_group_frame();
                self.edit = EditDrag::None;
                // Also discard an in-progress polyline/freehand stroke on a tool change.
                self.polyline_pts.clear();
                self.freehand_pts.clear();
                self.freehand_bg_cache = None;
                self.request_redraw();
            }
            EditorBtn::Pin => {
                self.pinned = !self.pinned;
                if let Some(w) = self.window.as_ref() {
                    w.set_window_level(if self.pinned {
                        WindowLevel::AlwaysOnTop
                    } else {
                        WindowLevel::Normal
                    });
                }
                self.request_redraw();
            }
            EditorBtn::Save => {
                self.commit_text();
                let shot = self.render_export();
                match crate::export::save_png(&shot) {
                    Ok(p) => {
                        println!("saved: {}", p.display());
                        // On a separate thread, so the first call's COM
                        // cold start doesn't block the UI.
                        std::thread::spawn(move || crate::shell::reveal_and_select(&p));
                    }
                    Err(e) => eprintln!("保存に失敗: {e}"),
                }
            }
            EditorBtn::Copy => {
                self.commit_text();
                if self.selected.is_empty() {
                    // With no selection, copies everything as an image to the OS clipboard.
                    let shot = self.render_export();
                    match crate::export::copy_to_clipboard(&shot) {
                        Ok(()) => println!("copied: {}x{}", shot.width, shot.height),
                        Err(e) => eprintln!("コピーに失敗: {e}"),
                    }
                } else {
                    // With a selection, copies as items (not flattened to
                    // an image, so Ctrl+V pastes them back as editable items).
                    self.item_clipboard = self
                        .selected
                        .iter()
                        .filter_map(|&i| self.annotations.get(i).cloned())
                        .collect();
                    println!("copied {} item(s)", self.item_clipboard.len());
                }
            }
        }
    }

    /// Handles an event; returns true when the editor should close (Cancel/closing the window).
    pub fn handle_event(&mut self, _event_loop: &ActiveEventLoop, event: WindowEvent) -> bool {
        match event {
            WindowEvent::CloseRequested => {
                self.save_session();
                return true;
            }

            WindowEvent::Resized(size) => {
                if let Some(surface) = self.surface.as_mut() {
                    let (pw, ph) = (size.width.max(1), size.height.max(1));
                    let _ =
                        surface.resize(NonZeroU32::new(pw).unwrap(), NonZeroU32::new(ph).unwrap());
                    self.physical_size = (pw as usize, ph as usize);
                    self.surface_size = (
                        ((pw as f64) / self.dpi).round().max(1.0) as usize,
                        ((ph as f64) / self.dpi).round().max(1.0) as usize,
                    );
                }
                self.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dpi = scale_factor;
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.ctrl = mods.state().control_key();
                self.shift = mods.state().shift_key();
                self.alt = mods.state().alt_key();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    return self.on_key(&event);
                }
            }

            // IME (input methods needing composition, e.g. Japanese). Only
            // enabled while editing (`enable_ime_at`/`disable_ime`), so
            // this only arrives during text editing.
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _range) => self.ime_preedit = text,
                    Ime::Commit(text) => {
                        self.ime_preedit.clear();
                        if let Some((_, buf)) = self.editing.as_mut() {
                            buf.push_str(&text);
                        }
                    }
                    Ime::Enabled => {}
                    Ime::Disabled => self.ime_preedit.clear(),
                }
                self.request_redraw();
            }

            WindowEvent::MouseInput { state, button, .. } => match button {
                MouseButton::Left => return self.on_mouse(state),
                // Pan via a middle-button drag.
                MouseButton::Middle => match state {
                    ElementState::Pressed => {
                        self.pan = Some(self.cursor);
                        if let Some(w) = self.window.as_ref() {
                            w.set_cursor(CursorIcon::Grabbing);
                        }
                    }
                    ElementState::Released => {
                        self.pan = None;
                        self.update_cursor();
                    }
                },
                _ => {}
            },

            WindowEvent::CursorMoved { position, .. } => {
                // `position` is always physical; the rest of this module
                // (including the image zoom/pan math) works in logical
                // coordinates (see the `dpi` field doc), so convert once here.
                let x = position.x / self.dpi;
                let y = position.y / self.dpi;
                // While panning, shift the camera by the cursor's movement.
                if let Some((lx, ly)) = self.pan {
                    self.offset.0 += x - lx;
                    self.offset.1 += y - ly;
                    self.pan = Some((x, y));
                    self.cursor = (x, y);
                    self.request_redraw();
                    return false;
                }
                self.cursor = (x, y);
                if self.picker_drag.is_some() {
                    // Follow the color picker drag.
                    self.apply_picker(x, y);
                } else if !matches!(self.edit, EditDrag::None) {
                    // Follow the item while moving/resizing.
                    self.apply_edit();
                    self.request_redraw();
                } else {
                    if self.dragging && self.tool == Tool::Draw {
                        // During a freehand drag: accumulate stroke points, thinning as it goes.
                        let p = self.to_world(x, y);
                        let should_push = match self.freehand_pts.last() {
                            Some(&last) => should_sample_freehand_point(last, p),
                            None => true,
                        };
                        if should_push {
                            self.freehand_pts.push(p);
                        }
                    }
                    let h = self.button_at(x as usize, y as usize);
                    if h != self.hover {
                        self.hover = h;
                        self.request_redraw();
                    } else if self.dragging
                        || (self.tool == Tool::Polyline && !self.polyline_pts.is_empty())
                    {
                        // Before a Polyline is committed, redraw every time
                        // so the rubber-band preview line follows the
                        // cursor even without clicking.
                        self.request_redraw();
                    }
                    self.update_cursor();
                }
            }

            WindowEvent::RedrawRequested => self.draw(),

            // Wheel zooms in/out centered on the cursor position.
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                if dy != 0.0 {
                    self.zoom_at(self.cursor.0, self.cursor.1, dy);
                }
            }

            // Dropping an image file adds it as an item at the cursor position.
            WindowEvent::DroppedFile(path) => self.drop_file(&path),

            _ => {}
        }
        false
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        // While drawing a polyline: Enter commits it (if at least 2
        // points), Escape discards it. Takes priority over everything else.
        if !self.polyline_pts.is_empty() {
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => {
                    if self.polyline_pts.len() >= 2 {
                        self.push_undo();
                        let points = std::mem::take(&mut self.polyline_pts);
                        let (color, thick) = (self.cur_color, self.cur_thick);
                        self.annotations.push(Annotation::Polyline {
                            points,
                            color,
                            thick,
                        });
                        self.selected = vec![self.annotations.len() - 1];
                        self.recompute_group_frame();
                    } else {
                        self.polyline_pts.clear();
                    }
                    self.request_redraw();
                    return false;
                }
                Key::Named(NamedKey::Escape) => {
                    self.polyline_pts.clear();
                    self.request_redraw();
                    return false;
                }
                _ => {}
            }
        }

        // Escape during a freehand drag discards it (a safety net for
        // canceling before releasing the mouse button; normally it just
        // commits on release).
        if self.dragging
            && self.tool == Tool::Draw
            && matches!(event.logical_key, Key::Named(NamedKey::Escape))
        {
            self.dragging = false;
            self.drag_start = None;
            self.freehand_pts.clear();
            self.freehand_bg_cache = None;
            self.request_redraw();
            return false;
        }

        // While editing text, key input is treated as characters.
        if let Some((_, buf)) = self.editing.as_mut() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.editing = None;
                    self.disable_ime();
                }
                Key::Named(NamedKey::Enter) => self.commit_text(),
                Key::Named(NamedKey::Backspace) => {
                    buf.pop();
                }
                Key::Named(NamedKey::Space) => buf.push(' '),
                Key::Character(s) => buf.push_str(s),
                _ => {}
            }
            self.request_redraw();
            return false;
        }

        // While a numeric field is focused, only digits are accepted.
        if self.focus.is_some() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.focus = None;
                    self.buf.clear();
                }
                Key::Named(NamedKey::Enter) => self.commit_field(),
                Key::Named(NamedKey::Backspace) => {
                    self.buf.pop();
                }
                Key::Character(s) if s.chars().all(|c| c.is_ascii_digit()) => {
                    self.buf.push_str(s);
                }
                _ => {}
            }
            self.request_redraw();
            return false;
        }

        match &event.logical_key {
            // Esc closes an open picker. Esc never closes the editor
            // itself (only the window's ✕ does).
            Key::Named(NamedKey::Escape) => {
                if self.picker.take().is_some() {
                    self.request_redraw();
                }
                return false;
            }
            // Deletes every selected item.
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if !self.selected.is_empty() {
                    self.push_undo();
                    let mut idx = std::mem::take(&mut self.selected);
                    // Removing from the front would shift later indices, so remove in descending order.
                    idx.sort_unstable_by(|a, b| b.cmp(a));
                    for i in idx {
                        if i < self.annotations.len() {
                            self.annotations.remove(i);
                        }
                    }
                    // Deletion shifts indices, so the tracked Freehand
                    // batch is discarded, erring on the safe side (harmless — see #restore_annotations).
                    self.freehand_batch.clear();
                    self.recompute_group_frame();
                    self.edit = EditDrag::None;
                    self.request_redraw();
                }
            }
            // Moves every selected item one step forward/backward in draw
            // order (last = frontmost), preserving relative order. Works
            // the same way for a single selection too.
            Key::Named(NamedKey::PageUp) => {
                if !self.selected.is_empty() {
                    self.push_undo();
                    self.selected = reorder_selection(&mut self.annotations, &self.selected, true);
                    // Reordering shifts indices, so the tracked Freehand
                    // batch is discarded, erring on the safe side (harmless).
                    self.freehand_batch.clear();
                    self.request_redraw();
                }
            }
            Key::Named(NamedKey::PageDown) => {
                if !self.selected.is_empty() {
                    self.push_undo();
                    self.selected = reorder_selection(&mut self.annotations, &self.selected, false);
                    self.freehand_batch.clear();
                    self.request_redraw();
                }
            }
            Key::Character(s) => {
                if let Some(ch) = s.chars().next() {
                    let pressed = LocalKey::new(self.ctrl, self.shift, self.alt, ch);
                    if self.keys.redo.contains(&pressed) {
                        self.redo();
                    } else if self.keys.undo.contains(&pressed) {
                        self.undo();
                    } else if pressed == LocalKey::new(true, false, false, 'v') {
                        // Paste is always fixed to Ctrl+V (not configurable in Settings).
                        self.paste_items_or_clipboard_image();
                    } else if pressed == LocalKey::new(true, false, false, 's') {
                        // Save is always fixed to Ctrl+S (same path as activate; the editor doesn't close).
                        self.activate(EditorBtn::Save);
                    } else if pressed == LocalKey::new(true, false, false, 'c') {
                        // Copy is always fixed to Ctrl+C (same path as activate; the editor doesn't close).
                        self.activate(EditorBtn::Copy);
                    } else if self.keys.reset_zoom.contains(&pressed) {
                        // Resets zoom/pan to the initial fit-to-window display.
                        self.scale = self.home_scale;
                        self.offset = self.home_offset;
                        self.update_cursor();
                        self.request_redraw();
                    } else if self.keys.tool_select.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Select));
                    } else if self.keys.tool_arrow.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Arrow));
                    } else if self.keys.tool_polyline.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Polyline));
                    } else if self.keys.tool_draw.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Draw));
                    } else if self.keys.tool_rect.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Rect));
                    } else if self.keys.tool_ellipse.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Ellipse));
                    } else if self.keys.tool_text.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::Text));
                    } else if self.keys.tool_number_marker.contains(&pressed) {
                        self.activate(EditorBtn::Tool(Tool::NumberMarker));
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn on_mouse(&mut self, state: ElementState) -> bool {
        let (wx, wy) = self.cursor;
        match state {
            ElementState::Pressed => self.on_press(wx, wy),
            ElementState::Released => {
                if self.picker_drag.take().is_some() {
                    return false;
                }
                if let Some(btn) = self.pressed.take() {
                    if self.button_at(wx as usize, wy as usize) == Some(btn) {
                        self.activate(btn);
                    }
                } else if !matches!(self.edit, EditDrag::None) {
                    // Commits the move/resize (geometry was already
                    // applied via CursorMoved). Once a group resize/rotate
                    // finishes, its final bounding rect is written back
                    // as-is (and kept until the selection changes).
                    // Re-deriving it here from the axis-aligned bbox
                    // (`recompute_group_frame`) would fall out of sync
                    // with the rotation center used during the drag,
                    // making the bounding rect appear to drift — so the
                    // drag's own final value must always be used.
                    if let Some((rect, rot)) = self.active_group_frame() {
                        self.group_rect = rect;
                        self.group_rot = rot;
                    }
                    self.edit = EditDrag::None;
                    self.request_redraw();
                } else if self.dragging {
                    self.dragging = false;
                    if self.tool == Tool::Draw {
                        self.drag_start = None;
                        let points = std::mem::take(&mut self.freehand_pts);
                        if points.len() >= 2 {
                            self.push_undo();
                            let (color, thick) = (self.cur_color, self.cur_thick);
                            self.annotations
                                .push(rasterize_freehand(&points, thick, color));
                            // No handles shown per stroke (not selected) —
                            // just queued for merging until the tool is left/reselected.
                            self.freehand_batch.push(self.annotations.len() - 1);
                        }
                        // The drag ended (annotations changed), so the background cache is discarded.
                        self.freehand_bg_cache = None;
                        self.request_redraw();
                    } else if let Some(a) = self.drag_start.take() {
                        let b = self.to_world(wx, wy);
                        self.push_shape(a, b);
                        self.request_redraw();
                    }
                }
            }
        }
        false
    }

    fn on_press(&mut self, wx: f64, wy: f64) {
        let sw = self.surface_size.0;
        // 1. Picker open: click inside starts a drag; outside closes it and continues.
        if self.picker.is_some() {
            let (_popup, sv, hue) = picker_geom(sw, self.text.as_ref());
            if inside(sv, wx, wy) {
                if !self.selected.is_empty() {
                    self.push_undo();
                }
                self.picker_drag = Some(PickerPart::Sv);
                self.apply_picker(wx, wy);
                return;
            }
            if inside(hue, wx, wy) {
                if !self.selected.is_empty() {
                    self.push_undo();
                }
                self.picker_drag = Some(PickerPart::Hue);
                self.apply_picker(wx, wy);
                return;
            }
            // Anything else (margin, swatch, canvas, etc.) just closes it
            // — the next click is a separate action (this also prevents
            // it from immediately reopening if the swatch is clicked again).
            self.picker = None;
            self.request_redraw();
            return;
        }
        // 2. Clicking elsewhere while a numeric field is focused commits it.
        self.commit_field();

        // 3. Top bar: properties, or Pin/Save/Copy.
        if wy < TOOLBAR_H as f64 {
            if let Some(pc) = self.prop_at(wx as usize, wy as usize) {
                match pc {
                    PropCtrl::Color => self.toggle_picker(),
                    PropCtrl::Field(f) => self.focus_field(f),
                    PropCtrl::Step(f, up) => self.step_field(f, up),
                    PropCtrl::Fill => self.toggle_fill(),
                    PropCtrl::Blur => self.toggle_blur(),
                }
            } else {
                self.pressed = self.button_at(wx as usize, wy as usize);
                self.request_redraw();
            }
            return;
        }
        // 4. Left bar: tools.
        if wx < TOOLBAR_W as f64 {
            self.pressed = self.button_at(wx as usize, wy as usize);
            return;
        }
        // 5. Canvas: if text is being edited, commit it before acting.
        self.commit_text();
        let p = self.to_world(wx, wy);
        if self.tool == Tool::Select {
            self.begin_select_drag(p);
        } else if self.tool == Tool::Guide {
            self.begin_guide_drag(p);
        } else if !self.selected.is_empty() && self.try_begin_edit_on_current_selection(p) {
            // Hit a handle on the just-created shape, so resize/rotate it
            // instead of switching tools.
        } else {
            // No handle hit (whether clicking inside or outside the body):
            // clear the selection and proceed to the tool's normal
            // creation behavior (dragging inside the bounding rect
            // prioritizes drawing a new shape over moving).
            if !self.selected.is_empty() {
                self.selected.clear();
                self.recompute_group_frame();
                self.request_redraw();
            }
            if self.tool == Tool::Text {
                self.editing = Some((p, String::new()));
                self.enable_ime_at(p);
                self.request_redraw();
            } else if self.tool == Tool::Polyline {
                self.polyline_pts.push(p);
                self.request_redraw();
            } else if self.tool == Tool::Draw {
                self.drag_start = Some(p);
                self.dragging = true;
                self.freehand_pts = vec![p];
                // The previous stroke's commit changed annotations, so
                // discard the old background cache before starting a new stroke.
                self.freehand_bg_cache = None;
                self.request_redraw();
            } else if self.tool == Tool::NumberMarker {
                // Commits immediately on a single click, no drag (unlike
                // Text/Polyline/Freehand, drag_start/dragging aren't used).
                self.push_undo();
                let number = self.next_marker_num;
                self.annotations.push(Annotation::NumberMarker {
                    pos: p,
                    number,
                    color: self.cur_color,
                    size: self.cur_size,
                });
                self.next_marker_num = self.next_marker_num.saturating_add(1);
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
                self.request_redraw();
            } else if self.tool == Tool::Mosaic {
                // The preview and the committed item use the same seed for
                // one drag gesture (assigned here and reused after).
                self.new_item_seed = random_seed();
                self.drag_start = Some(p);
                self.dragging = true;
            } else {
                self.drag_start = Some(p);
                self.dragging = true;
            }
        }
    }

    /// Press handling for the Guide tool: an existing guide's handle
    /// resizes it, its body moves it, and anything else (no guide, or
    /// outside it) drags out a new replacement. Since the guide isn't
    /// selectable via the Select tool, adjusting it only happens through
    /// this dedicated path (avoiding it being accidentally selected while
    /// trying to select another annotation).
    fn begin_guide_drag(&mut self, p: (i64, i64)) {
        let tol = self.grab_tol();
        if let Some(i) = self
            .annotations
            .iter()
            .position(|a| matches!(a, Annotation::Guide { .. }))
            && let Some(Annotation::Guide { r }) = self.annotations.get(i)
        {
            if let Some(h) = hit_rect_handle(*r, p, tol) {
                let orig = rect_norm(*r);
                self.push_undo();
                self.selected = vec![i];
                self.edit = EditDrag::RectHandle { h, orig };
                return;
            }
            let (x0, y0, x1, y1) = rect_norm(*r);
            if p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1 {
                self.push_undo();
                self.selected = vec![i];
                self.edit = EditDrag::Move {
                    grab: p,
                    orig: self.annotations[i].clone(),
                };
                return;
            }
        }
        self.drag_start = Some(p);
        self.dragging = true;
    }

    /// Press handling for the Select tool: a handle resizes; an item's
    /// body selects and moves it, or toggles selection with Shift; empty
    /// space starts a marquee-selection drag.
    fn begin_select_drag(&mut self, p: (i64, i64)) {
        let tol = self.grab_tol();
        // 0) With a multi-selection, or a single selected Polyline, the
        // bounding rect's handles/rotate handle are checked first (the
        // exact same pattern as a single Rect's rotated-handle test:
        // convert to the bounding rect's local coordinates via to_local,
        // then test). Polyline has no "local rect plus rotation angle,"
        // and the transform that scales/rotates a set of vertices together
        // (`scale_annotation_rotated`/`rotate_annotation_around`) already
        // exists via the multi-selection group path, so a single selection
        // rides that same path too (as a one-item set).
        let single_polyline = matches!(
            self.selected[..],
            [i] if matches!(self.annotations.get(i), Some(Annotation::Polyline { .. }))
        );
        if self.selected.len() > 1 || single_polyline {
            let (rect, rot) = (self.group_rect, self.group_rot);
            let lp = to_local(p, rect, rot);
            if near_f64(lp, rotate_handle_local(rect), tol) {
                self.push_undo();
                let origs = self
                    .selected
                    .iter()
                    .filter_map(|&i| self.annotations.get(i).map(|a| (i, a.clone())))
                    .collect();
                let center = ((rect.0 + rect.2) / 2.0, (rect.1 + rect.3) / 2.0);
                let start_angle = (p.1 as f64 - center.1).atan2(p.0 as f64 - center.0);
                self.edit = EditDrag::GroupRotate {
                    center,
                    start_angle,
                    orig_rect: rect,
                    orig_rot: rot,
                    origs,
                };
                self.request_redraw();
                return;
            }
            if let Some(h) = hit_rect_handle_f64(rect, lp, tol) {
                self.push_undo();
                let origs = self
                    .selected
                    .iter()
                    .filter_map(|&i| self.annotations.get(i).map(|a| (i, a.clone())))
                    .collect();
                self.edit = EditDrag::GroupHandle {
                    h,
                    orig_rect: rect,
                    rot,
                    origs,
                };
                self.request_redraw();
                return;
            }
        }
        // 1) Handle hit-testing only with a single selection (Rect=8, Arrow=2 endpoints).
        if let [i] = self.selected[..]
            && let Some(ann) = self.annotations.get(i)
        {
            match ann {
                // Rect/Ellipse/Image share the same "local rect plus
                // rotation angle" handle test (Image's aspect lock while
                // Shift is held is handled separately in apply_edit).
                Annotation::Rect { r, rot, .. }
                | Annotation::Ellipse { r, rot, .. }
                | Annotation::Image { r, rot, .. }
                | Annotation::Mosaic { r, rot, .. } => {
                    let lp = to_local(p, *r, *rot);
                    if near_f64(lp, rotate_handle_local(*r), tol) {
                        let (x0, y0, x1, y1) = rect_norm_f64(*r);
                        self.push_undo();
                        self.edit = EditDrag::Rotate {
                            center: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                        };
                        return;
                    }
                    if let Some(h) = hit_rect_handle_f64(*r, lp, tol) {
                        let orig = rect_norm_f64(*r);
                        self.push_undo();
                        self.edit = EditDrag::RotatedHandle { h, orig };
                        return;
                    }
                }
                Annotation::Arrow { a, b, .. } => {
                    if near(p, *b, tol) {
                        self.push_undo();
                        self.edit = EditDrag::ArrowEnd { end_b: true };
                        return;
                    }
                    if near(p, *a, tol) {
                        self.push_undo();
                        self.edit = EditDrag::ArrowEnd { end_b: false };
                        return;
                    }
                }
                Annotation::Polyline { points, .. } => {
                    for (idx, &pt) in points.iter().enumerate() {
                        if near(p, pt, tol) {
                            self.push_undo();
                            self.edit = EditDrag::PolylinePoint { index: idx };
                            return;
                        }
                    }
                }
                // Text has only a rotate handle (no resize; its local rect
                // is built on demand from a font measurement).
                Annotation::Text {
                    pos,
                    text: s,
                    size,
                    rot,
                    ..
                } => {
                    let r = text_local_rect(*pos, s, *size, self.text.as_ref());
                    let lp = to_local(p, r, *rot);
                    if near_f64(lp, rotate_handle_local(r), tol) {
                        let (x0, y0, x1, y1) = rect_norm_f64(r);
                        self.push_undo();
                        self.edit = EditDrag::Rotate {
                            center: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                        };
                        return;
                    }
                }
                // No handles (a true circle looks the same rotated or
                // not, so there's nothing to handle). Falls through to
                // step 2's "body hit moves it."
                Annotation::NumberMarker { .. } => {}
                // The guide is never selectable via Select (Guide-tool only).
                Annotation::Guide { .. } => {}
            }
        }
        // 2) The frontmost item under the cursor.
        if let Some(i) = hit_item(&self.annotations, p, tol, self.text.as_ref()) {
            if self.shift {
                // Shift+click only toggles the selection set (content is
                // unchanged, so push_undo isn't called). Doesn't start a drag.
                if let Some(pos) = self.selected.iter().position(|&x| x == i) {
                    self.selected.remove(pos);
                } else {
                    self.selected.push(i);
                    self.selected.sort_unstable();
                }
                self.recompute_group_frame();
                self.edit = EditDrag::None;
            } else if self.selected.len() > 1 && self.selected.contains(&i) {
                // Clicking a member of an existing multi-selection: keeps the selection and moves it as a group.
                self.push_undo();
                if self.alt {
                    // While Alt is held, the original items stay put and the duplicate is moved instead.
                    let clones: Vec<Annotation> = self
                        .selected
                        .iter()
                        .filter_map(|&idx| self.annotations.get(idx).cloned())
                        .collect();
                    let start = self.annotations.len();
                    self.annotations.extend(clones);
                    self.selected = (start..self.annotations.len()).collect();
                    self.recompute_group_frame();
                }
                let origs = self
                    .selected
                    .iter()
                    .filter_map(|&idx| self.annotations.get(idx).map(|a| (idx, a.clone())))
                    .collect();
                self.edit = EditDrag::GroupMove { grab: p, origs };
            } else {
                self.push_undo();
                // While Alt is held, the original item stays put and the duplicate is moved instead.
                let target = if self.alt {
                    let dup = self.annotations[i].clone();
                    self.annotations.push(dup);
                    self.annotations.len() - 1
                } else {
                    i
                };
                self.selected = vec![target];
                self.recompute_group_frame();
                self.edit = EditDrag::Move {
                    grab: p,
                    orig: self.annotations[target].clone(),
                };
            }
            self.request_redraw();
            return;
        }
        // 3) Empty click/drag start: marquee selection (committed on
        // mouse-up, via `marquee_select`). `self.selected` isn't touched here.
        self.edit = EditDrag::None;
        self.drag_start = Some(p);
        self.dragging = true;
        self.request_redraw();
    }

    /// For a drawing tool (anything but Select/Guide), tests whether the
    /// click hits the handles/bounding-rect outline of a single item still
    /// selected from just having been created, and if so starts the same
    /// edit drag (resize/rotate/move) as `begin_select_drag`. A
    /// single-selection-only rebuild of `begin_select_drag`'s "handle test
    /// with a single selection" (multi-selection, Shift-toggle, and
    /// marquee remain Select-tool-only and aren't here). **Never starts a
    /// move on a hit inside the bounding rect (the body)** — while still on
    /// a drawing tool, dragging inside should prioritize that tool's normal
    /// behavior (creating a new shape). Only a hit on the bounding rect's
    /// outline (border) allows a move (`near_rect_outline_local`).
    fn try_begin_edit_on_current_selection(&mut self, p: (i64, i64)) -> bool {
        let tol = self.grab_tol();
        let [i] = self.selected[..] else {
            return false;
        };
        let Some(ann) = self.annotations.get(i).cloned() else {
            return false;
        };
        let ann = &ann;

        // A single Polyline rides the same bounding-rect handle path as a
        // multi-selection (as a one-item set).
        if matches!(ann, Annotation::Polyline { .. }) {
            let (rect, rot) = (self.group_rect, self.group_rot);
            let lp = to_local(p, rect, rot);
            if near_f64(lp, rotate_handle_local(rect), tol) {
                self.push_undo();
                let center = ((rect.0 + rect.2) / 2.0, (rect.1 + rect.3) / 2.0);
                let start_angle = (p.1 as f64 - center.1).atan2(p.0 as f64 - center.0);
                self.edit = EditDrag::GroupRotate {
                    center,
                    start_angle,
                    orig_rect: rect,
                    orig_rot: rot,
                    origs: vec![(i, ann.clone())],
                };
                self.request_redraw();
                return true;
            }
            if let Some(h) = hit_rect_handle_f64(rect, lp, tol) {
                self.push_undo();
                self.edit = EditDrag::GroupHandle {
                    h,
                    orig_rect: rect,
                    rot,
                    origs: vec![(i, ann.clone())],
                };
                self.request_redraw();
                return true;
            }
        }

        match ann {
            Annotation::Rect { r, rot, .. }
            | Annotation::Ellipse { r, rot, .. }
            | Annotation::Image { r, rot, .. }
            | Annotation::Mosaic { r, rot, .. } => {
                let lp = to_local(p, *r, *rot);
                if near_f64(lp, rotate_handle_local(*r), tol) {
                    let (x0, y0, x1, y1) = rect_norm_f64(*r);
                    self.push_undo();
                    self.edit = EditDrag::Rotate {
                        center: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                    };
                    self.request_redraw();
                    return true;
                }
                if let Some(h) = hit_rect_handle_f64(*r, lp, tol) {
                    let orig = rect_norm_f64(*r);
                    self.push_undo();
                    self.edit = EditDrag::RotatedHandle { h, orig };
                    self.request_redraw();
                    return true;
                }
                // On the bounding rect's outline (border), move (excluded
                // inside it, so the tool's normal creation behavior wins there).
                if near_rect_outline_local(*r, lp, tol) {
                    self.push_undo();
                    self.edit = EditDrag::Move {
                        grab: p,
                        orig: ann.clone(),
                    };
                    self.request_redraw();
                    return true;
                }
            }
            Annotation::Arrow { a, b, .. } => {
                if near(p, *b, tol) {
                    self.push_undo();
                    self.edit = EditDrag::ArrowEnd { end_b: true };
                    self.request_redraw();
                    return true;
                }
                if near(p, *a, tol) {
                    self.push_undo();
                    self.edit = EditDrag::ArrowEnd { end_b: false };
                    self.request_redraw();
                    return true;
                }
            }
            Annotation::Polyline { points, .. } => {
                for (idx, &pt) in points.iter().enumerate() {
                    if near(p, pt, tol) {
                        self.push_undo();
                        self.edit = EditDrag::PolylinePoint { index: idx };
                        self.request_redraw();
                        return true;
                    }
                }
                // Not on a vertex, but on the bounding rect's (shared with
                // the group path) outline: move.
                let (rect, rot) = (self.group_rect, self.group_rot);
                let lp = to_local(p, rect, rot);
                if near_rect_outline_local(rect, lp, tol) {
                    self.push_undo();
                    self.edit = EditDrag::Move {
                        grab: p,
                        orig: ann.clone(),
                    };
                    self.request_redraw();
                    return true;
                }
            }
            Annotation::Text {
                pos,
                text: s,
                size,
                rot,
                ..
            } => {
                let r = text_local_rect(*pos, s, *size, self.text.as_ref());
                let lp = to_local(p, r, *rot);
                if near_f64(lp, rotate_handle_local(r), tol) {
                    let (x0, y0, x1, y1) = rect_norm_f64(r);
                    self.push_undo();
                    self.edit = EditDrag::Rotate {
                        center: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                    };
                    self.request_redraw();
                    return true;
                }
                if near_rect_outline_local(r, lp, tol) {
                    self.push_undo();
                    self.edit = EditDrag::Move {
                        grab: p,
                        orig: ann.clone(),
                    };
                    self.request_redraw();
                    return true;
                }
            }
            // This tool always commits immediately on a single click (no
            // drag creates a new one), so unlike Rect etc. there's no
            // reason to prioritize the tool inside the bounding rect —
            // inside the circle can just move it (naturally consistent
            // with on_press's branch, where clicking outside adds a new marker).
            Annotation::NumberMarker { pos, size, .. } => {
                let d = ((p.0 - pos.0) as f64).hypot((p.1 - pos.1) as f64);
                if d <= *size as f64 + tol {
                    self.push_undo();
                    self.edit = EditDrag::Move {
                        grab: p,
                        orig: ann.clone(),
                    };
                    self.request_redraw();
                    return true;
                }
            }
            Annotation::Guide { .. } => {}
        }

        false
    }

    /// Commits a marquee selection (treated as a plain click if the drag distance is small).
    fn marquee_select(&mut self, a: (i64, i64), b: (i64, i64)) {
        let tol = self.grab_tol();
        if (a.0 - b.0).abs() as f64 <= tol && (a.1 - b.1).abs() as f64 <= tol {
            // Just a click (small drag distance). Skips hit-testing
            // entirely — a zero-width rect could otherwise coincidentally
            // "intersect" an item right on the boundary — and only clears
            // the selection, as a plain empty click would.
            if !self.shift {
                self.selected.clear();
                self.recompute_group_frame();
            }
            self.request_redraw();
            return;
        }
        let rect = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        let hits = hit_items_in_rect(&self.annotations, rect, self.text.as_ref());
        if self.shift {
            for i in hits {
                if !self.selected.contains(&i) {
                    self.selected.push(i);
                }
            }
            self.selected.sort_unstable();
        } else {
            self.selected = hits;
        }
        self.recompute_group_frame();
        self.request_redraw();
    }

    /// Applies `f` to every selected item (a no-op if there are none).
    fn for_each_selected(&mut self, mut f: impl FnMut(&mut Annotation)) {
        for &i in &self.selected {
            if let Some(ann) = self.annotations.get_mut(i) {
                f(ann);
            }
        }
    }

    /// Sets the current color (default plus every selected item, all at once).
    fn set_color(&mut self, c: u32) {
        self.cur_color = c;
        self.for_each_selected(|a| match a {
            Annotation::Arrow { color, .. }
            | Annotation::Polyline { color, .. }
            | Annotation::Rect { color, .. }
            | Annotation::Ellipse { color, .. }
            | Annotation::Text { color, .. }
            | Annotation::NumberMarker { color, .. } => *color = c,
            Annotation::Image { .. } | Annotation::Guide { .. } | Annotation::Mosaic { .. } => {}
        });
    }

    /// Sets thickness (default plus every selected Arrow/Polyline/Rect/Ellipse, all at once).
    fn set_thick(&mut self, th: i64) {
        self.cur_thick = th;
        self.for_each_selected(|a| match a {
            Annotation::Arrow { thick, .. }
            | Annotation::Polyline { thick, .. }
            | Annotation::Rect { thick, .. }
            | Annotation::Ellipse { thick, .. } => *thick = th,
            Annotation::Text { .. }
            | Annotation::Image { .. }
            | Annotation::Guide { .. }
            | Annotation::NumberMarker { .. }
            | Annotation::Mosaic { .. } => {}
        });
    }

    /// Sets size (default plus every selected Text/NumberMarker, all at once).
    fn set_size(&mut self, sz: f32) {
        self.cur_size = sz;
        self.for_each_selected(|a| match a {
            Annotation::Text { size, .. } | Annotation::NumberMarker { size, .. } => *size = sz,
            _ => {}
        });
    }

    /// Sets block size (default plus every selected Mosaic, all at once).
    fn set_block(&mut self, b: f32) {
        self.cur_block = b;
        self.for_each_selected(|a| {
            if let Annotation::Mosaic { block, .. } = a {
                *block = b;
            }
        });
    }

    /// Sets filled (default plus every selected Rect/Ellipse, all at once).
    fn set_filled(&mut self, f: bool) {
        self.cur_filled = f;
        self.for_each_selected(|a| match a {
            Annotation::Rect { filled, .. } | Annotation::Ellipse { filled, .. } => *filled = f,
            _ => {}
        });
    }

    /// Handles clicking the Fill toggle (default plus selected items, all flipped).
    fn toggle_fill(&mut self) {
        self.commit_field();
        let filled = !self.active_style().3.value();
        if !self.selected.is_empty() {
            self.push_undo();
        }
        self.set_filled(filled);
        self.request_redraw();
    }

    /// Sets Blur mode (default plus every selected Mosaic, all at once).
    fn set_blur(&mut self, b: bool) {
        self.cur_blur = b;
        self.for_each_selected(|a| {
            if let Annotation::Mosaic { mode, .. } = a {
                *mode = if b {
                    MosaicMode::Blur
                } else {
                    MosaicMode::Pixelate
                };
            }
        });
    }

    /// Handles clicking the Blur toggle (default plus selected items, all flipped).
    fn toggle_blur(&mut self) {
        self.commit_field();
        let blur = !self.active_style().5.value();
        if !self.selected.is_empty() {
            self.push_undo();
        }
        self.set_blur(blur);
        self.request_redraw();
    }

    /// The common style across selected items (default if none selected,
    /// `Uniform` if they agree, `Mixed` if they differ; used for display
    /// and the picker's initial value).
    fn active_style(&self) -> StyleVals {
        let items: Vec<&Annotation> = self
            .selected
            .iter()
            .filter_map(|&i| self.annotations.get(i))
            .collect();
        common_style(
            &items,
            (
                self.cur_color,
                self.cur_thick,
                self.cur_size,
                self.cur_filled,
                self.cur_block,
                self.cur_blur,
            ),
        )
    }

    /// Focuses a numeric field and loads its current value into the
    /// buffer (closing the picker). When `Mixed`, editing starts from the
    /// representative value (the first item's).
    fn focus_field(&mut self, field: Field) {
        self.picker = None;
        self.commit_field();
        let (_, thick, size, _, block, _) = self.active_style();
        self.buf = match field {
            Field::Line => thick.value().to_string(),
            Field::Size => (size.value().round() as i64).to_string(),
            Field::Number => self.next_marker_num.to_string(),
            Field::Block => (block.value().round() as i64).to_string(),
        };
        self.focus = Some(field);
        self.request_redraw();
    }

    /// Commits the focused numeric field (parse -> clamp -> apply).
    fn commit_field(&mut self) {
        let Some(field) = self.focus.take() else {
            return;
        };
        let (_, thick, size, _, block, _) = self.active_style();
        // Number is tool-level state (the next number to place), not a
        // property of a selected item, so push_undo is skipped even with
        // a selection (annotations' content doesn't change).
        if !self.selected.is_empty() && field != Field::Number {
            self.push_undo();
        }
        match field {
            Field::Line => {
                let v = parse_dim(&self.buf, thick.value() as f64, THICK_RANGE);
                self.set_thick(v.round() as i64);
            }
            Field::Size => {
                let v = parse_dim(&self.buf, size.value() as f64, SIZE_RANGE);
                self.set_size(v as f32);
            }
            Field::Number => {
                let v = parse_dim(&self.buf, self.next_marker_num as f64, NUMBER_RANGE);
                self.next_marker_num = v.round() as u32;
            }
            Field::Block => {
                let v = parse_dim(&self.buf, block.value() as f64, BLOCK_RANGE);
                self.set_block(v as f32);
            }
        }
        self.buf.clear();
        self.request_redraw();
    }

    /// Steps thickness/size/next-number by one via the ±stepper (default plus selected items, all at once).
    fn step_field(&mut self, field: Field, up: bool) {
        self.commit_field();
        let (_, thick, size, _, block, _) = self.active_style();
        let d = if up { 1.0 } else { -1.0 };
        if !self.selected.is_empty() && field != Field::Number {
            self.push_undo();
        }
        match field {
            Field::Line => {
                let v = (thick.value() as f64 + d).clamp(THICK_RANGE.0, THICK_RANGE.1);
                self.set_thick(v as i64);
            }
            Field::Size => {
                let v = (size.value() as f64 + d).clamp(SIZE_RANGE.0, SIZE_RANGE.1);
                self.set_size(v as f32);
            }
            Field::Number => {
                let v = (self.next_marker_num as f64 + d).clamp(NUMBER_RANGE.0, NUMBER_RANGE.1);
                self.next_marker_num = v as u32;
            }
            Field::Block => {
                let v = (block.value() as f64 + d).clamp(BLOCK_RANGE.0, BLOCK_RANGE.1);
                self.set_block(v as f32);
            }
        }
        self.request_redraw();
    }

    /// Toggles the color picker; opening derives HSV from the current
    /// color (the representative value, if `Mixed`).
    fn toggle_picker(&mut self) {
        if self.picker.is_some() {
            self.picker = None;
        } else {
            self.commit_field();
            self.picker = Some(rgb_to_hsv(self.active_style().0.value()));
        }
        self.request_redraw();
    }

    /// Updates HSV from the picker's drag position and applies the color.
    fn apply_picker(&mut self, wx: f64, wy: f64) {
        let (Some((h, s, v)), Some(part)) = (self.picker, self.picker_drag) else {
            return;
        };
        let (_, sv, hue) = picker_geom(self.surface_size.0, self.text.as_ref());
        let (nh, ns, nv) = match part {
            PickerPart::Sv => {
                let ns = ((wx - sv.x0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32;
                let nv = 1.0 - ((wy - sv.y0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32;
                (h, ns, nv)
            }
            PickerPart::Hue => {
                let nh = ((wx - hue.x0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32 * 360.0;
                (nh, s, v)
            }
        };
        self.picker = Some((nh, ns, nv));
        self.set_color(hsv_to_rgb(nh, ns, nv));
        self.request_redraw();
    }

    /// Applies the in-progress edit drag to the selected item(s) at the
    /// current cursor position. Single-item handles/move/rotate use
    /// `self.selected`'s first index. `Group*` variants use the indices
    /// in their own `origs`, so they apply to multiple items at once.
    fn apply_edit(&mut self) {
        let Some(i) = self.selected.first().copied() else {
            return;
        };
        if i >= self.annotations.len() {
            return;
        }
        let p = self.to_world(self.cursor.0, self.cursor.1);
        let p_f = self.to_world_f64(self.cursor.0, self.cursor.1);
        let shift = self.shift;
        // Only geometry is updated; style is preserved.
        match &self.edit {
            EditDrag::Move { grab, orig } => {
                let (mut dx, mut dy) = (p.0 - grab.0, p.1 - grab.1);
                if shift {
                    // Snap to a horizontal/vertical axis: keep only the axis with the larger movement.
                    if dx.abs() >= dy.abs() {
                        dy = 0;
                    } else {
                        dx = 0;
                    }
                }
                self.annotations[i] = translate_annotation(orig, dx, dy);
            }
            EditDrag::RectHandle { h, orig } => {
                if let Annotation::Guide { r } = &mut self.annotations[i] {
                    *r = resize_rect(*orig, *h, p);
                }
            }
            EditDrag::RotatedHandle { h, orig } => match &mut self.annotations[i] {
                // While Shift is held, preserves the aspect ratio from drag start.
                Annotation::Rect { r, rot, .. }
                | Annotation::Ellipse { r, rot, .. }
                | Annotation::Mosaic { r, rot, .. } => {
                    *r = if shift {
                        let (ox0, oy0, ox1, oy1) = rect_norm_f64(*orig);
                        let ar = if oy1 - oy0 > 0.0 {
                            (ox1 - ox0) / (oy1 - oy0)
                        } else {
                            1.0
                        };
                        resize_rotated_rect_aspect(*orig, *h, p_f, *rot, ar)
                    } else {
                        resize_rotated_rect(*orig, *h, p_f, *rot)
                    };
                }
                // While Shift is held, preserves the source image's ratio (src_w:src_h).
                Annotation::Image {
                    r,
                    rot,
                    src_w,
                    src_h,
                    ..
                } => {
                    *r = if shift && *src_h > 0 {
                        resize_rotated_rect_aspect(
                            *orig,
                            *h,
                            p_f,
                            *rot,
                            *src_w as f64 / *src_h as f64,
                        )
                    } else {
                        resize_rotated_rect(*orig, *h, p_f, *rot)
                    };
                }
                _ => {}
            },
            EditDrag::ArrowEnd { end_b } => {
                if let Annotation::Arrow { a, b, .. } = &mut self.annotations[i] {
                    if *end_b {
                        *b = p;
                    } else {
                        *a = p;
                    }
                }
            }
            EditDrag::PolylinePoint { index } => {
                if let Annotation::Polyline { points, .. } = &mut self.annotations[i]
                    && let Some(pt) = points.get_mut(*index)
                {
                    *pt = p;
                }
            }
            EditDrag::Rotate { center } => {
                if let Annotation::Rect { rot, .. }
                | Annotation::Ellipse { rot, .. }
                | Annotation::Image { rot, .. }
                | Annotation::Text { rot, .. }
                | Annotation::Mosaic { rot, .. } = &mut self.annotations[i]
                {
                    let (dx, dy) = (p.0 as f64 - center.0, p.1 as f64 - center.1);
                    if dx.abs() > f64::EPSILON || dy.abs() > f64::EPSILON {
                        let mut angle = dy.atan2(dx) + std::f64::consts::FRAC_PI_2;
                        if shift {
                            angle = snap_angle_45(angle);
                        }
                        *rot = angle;
                    }
                }
            }
            EditDrag::GroupMove { grab, origs } => {
                let (mut dx, mut dy) = (p.0 - grab.0, p.1 - grab.1);
                if shift {
                    if dx.abs() >= dy.abs() {
                        dy = 0;
                    } else {
                        dx = 0;
                    }
                }
                for (idx, orig) in origs {
                    if let Some(slot) = self.annotations.get_mut(*idx) {
                        *slot = translate_annotation(orig, dx, dy);
                    }
                }
            }
            EditDrag::GroupHandle {
                h,
                orig_rect,
                rot,
                origs,
            } => {
                let new_rect = if shift {
                    let (ox0, oy0, ox1, oy1) = rect_norm_f64(*orig_rect);
                    let ar = if oy1 - oy0 > 0.0 {
                        (ox1 - ox0) / (oy1 - oy0)
                    } else {
                        1.0
                    };
                    resize_rotated_rect_aspect(*orig_rect, *h, p_f, *rot, ar)
                } else {
                    resize_rotated_rect(*orig_rect, *h, p_f, *rot)
                };
                for (idx, orig) in origs {
                    if let Some(slot) = self.annotations.get_mut(*idx) {
                        *slot = scale_annotation_rotated(orig, *orig_rect, new_rect, *rot);
                    }
                }
            }
            EditDrag::GroupRotate {
                center,
                start_angle,
                origs,
                ..
            } => {
                let cur_angle = (p.1 as f64 - center.1).atan2(p.0 as f64 - center.0);
                let mut delta = cur_angle - start_angle;
                if shift {
                    // Snaps the whole group's rotation — "the amount
                    // rotated since it was grabbed" — to 45° steps (each
                    // item's relative orientation is preserved).
                    delta = snap_angle_45(delta);
                }
                for (idx, orig) in origs {
                    if let Some(slot) = self.annotations.get_mut(*idx) {
                        *slot = rotate_annotation_around(orig, *center, delta, self.text.as_ref());
                    }
                }
            }
            EditDrag::None => {}
        }
    }

    /// Updates the mouse cursor shape (resize/move) based on what's under
    /// it, for the Select tool (or the Guide tool while adjusting the
    /// guide). Other tools still show a resize/rotate cursor over a
    /// remaining selection's handles, but over the body stay at the
    /// normal drawing cursor rather than move.
    fn update_cursor(&self) {
        let Some(w) = self.window.as_ref() else {
            return;
        };
        let (wx, wy) = self.cursor;
        if wy < TOOLBAR_H as f64 || wx < TOOLBAR_W as f64 {
            w.set_cursor(CursorIcon::Default);
            return;
        }
        if self.tool == Tool::Guide {
            // Over the guide's handle/body, the same icon as the Select
            // tool; otherwise (no guide, or outside it) stays the crosshair for a new drag.
            let p = self.to_world(wx, wy);
            let tol = self.grab_tol();
            if let Some(r) = self.annotations.iter().find_map(|a| match a {
                Annotation::Guide { r } => Some(*r),
                _ => None,
            }) {
                if let Some(h) = hit_rect_handle(r, p, tol) {
                    w.set_cursor(handle_cursor(h));
                    return;
                }
                let (x0, y0, x1, y1) = rect_norm(r);
                if p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1 {
                    w.set_cursor(CursorIcon::Move);
                    return;
                }
            }
            w.set_cursor(CursorIcon::Crosshair);
            return;
        }
        if self.tool != Tool::Select && self.selected.is_empty() {
            w.set_cursor(CursorIcon::Crosshair);
            return;
        }
        let p = self.to_world(wx, wy);
        let tol = self.grab_tol();
        // With a multi-selection, shows a cursor matching the bounding
        // rect's handle/rotate handle. Anything else (a gap in the body,
        // etc.) is left to the hit_item-based check at the end (avoids
        // showing a Move cursor somewhere nothing would actually happen).
        if self.selected.len() > 1 {
            let (rect, rot) = (self.group_rect, self.group_rot);
            let lp = to_local(p, rect, rot);
            if near_f64(lp, rotate_handle_local(rect), tol) {
                w.set_cursor(CursorIcon::Grab);
                return;
            }
            if let Some(h) = hit_rect_handle_f64(rect, lp, tol) {
                w.set_cursor(handle_cursor(h));
                return;
            }
        }
        // Shows a per-handle cursor only with a single selection.
        if let [i] = self.selected[..] {
            match self.annotations.get(i) {
                Some(Annotation::Rect { r, rot, .. })
                | Some(Annotation::Ellipse { r, rot, .. })
                | Some(Annotation::Image { r, rot, .. })
                | Some(Annotation::Mosaic { r, rot, .. }) => {
                    let lp = to_local(p, *r, *rot);
                    if near_f64(lp, rotate_handle_local(*r), tol) {
                        w.set_cursor(CursorIcon::Grab);
                        return;
                    }
                    if let Some(h) = hit_rect_handle_f64(*r, lp, tol) {
                        w.set_cursor(handle_cursor(h));
                        return;
                    }
                    // For a drawing tool, only the bounding rect's outline
                    // shows Move (the Select tool already shows Move over
                    // the whole body via the hit_item check at the end,
                    // so it's left untouched here).
                    if self.tool != Tool::Select && near_rect_outline_local(*r, lp, tol) {
                        w.set_cursor(CursorIcon::Move);
                        return;
                    }
                }
                Some(Annotation::Text {
                    pos,
                    text: s,
                    size,
                    rot,
                    ..
                }) => {
                    let r = text_local_rect(*pos, s, *size, self.text.as_ref());
                    let lp = to_local(p, r, *rot);
                    if near_f64(lp, rotate_handle_local(r), tol) {
                        w.set_cursor(CursorIcon::Grab);
                        return;
                    }
                    if self.tool != Tool::Select && near_rect_outline_local(r, lp, tol) {
                        w.set_cursor(CursorIcon::Move);
                        return;
                    }
                }
                Some(Annotation::Arrow { a, b, .. }) if near(p, *a, tol) || near(p, *b, tol) => {
                    w.set_cursor(CursorIcon::Crosshair);
                    return;
                }
                Some(Annotation::Polyline { points, .. }) => {
                    if points.iter().any(|&pt| near(p, pt, tol)) {
                        w.set_cursor(CursorIcon::Crosshair);
                        return;
                    }
                    if self.tool != Tool::Select {
                        let (rect, rot) = (self.group_rect, self.group_rot);
                        let lp = to_local(p, rect, rot);
                        if near_rect_outline_local(rect, lp, tol) {
                            w.set_cursor(CursorIcon::Move);
                            return;
                        }
                    }
                }
                // Always a true circle with the whole body movable, so
                // unlike Rect etc. this isn't restricted to the bounding
                // rect's outline (the Select tool already gets this from
                // the hit_item check at the end, so only shown here for
                // non-Select tools).
                Some(Annotation::NumberMarker { pos, size, .. }) if self.tool != Tool::Select => {
                    let d = ((p.0 - pos.0) as f64).hypot((p.1 - pos.1) as f64);
                    if d <= *size as f64 + tol {
                        w.set_cursor(CursorIcon::Move);
                        return;
                    }
                }
                _ => {}
            }
        }
        if self.tool == Tool::Select {
            // Only the Select tool shows Move on a body hit (the only
            // tool where dragging actually moves an item).
            if hit_item(&self.annotations, p, tol, self.text.as_ref()).is_some() {
                w.set_cursor(CursorIcon::Move);
            } else {
                w.set_cursor(CursorIcon::Default);
            }
        } else {
            // For a drawing tool, anywhere but a handle keeps the normal
            // drawing cursor even over the body (dragging there creates a new shape, not a move).
            w.set_cursor(CursorIcon::Crosshair);
        }
    }

    /// Commits a drag by pushing an arrow/rect/etc. annotation (at the
    /// current color/thickness). Select commits a marquee selection
    /// instead (content is unchanged, so push_undo isn't called).
    fn push_shape(&mut self, a: (i64, i64), b: (i64, i64)) {
        let (color, thick, filled) = (self.cur_color, self.cur_thick, self.cur_filled);
        match self.tool {
            Tool::Arrow => {
                self.push_undo();
                self.annotations
                    .push(Annotation::Arrow { a, b, color, thick });
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
            }
            Tool::Rect => {
                self.push_undo();
                let r = (
                    a.0.min(b.0) as f64,
                    a.1.min(b.1) as f64,
                    a.0.max(b.0) as f64,
                    a.1.max(b.1) as f64,
                );
                self.annotations.push(Annotation::Rect {
                    r,
                    color,
                    thick,
                    rot: 0.0,
                    filled,
                });
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
            }
            Tool::Ellipse => {
                self.push_undo();
                let r = (
                    a.0.min(b.0) as f64,
                    a.1.min(b.1) as f64,
                    a.0.max(b.0) as f64,
                    a.1.max(b.1) as f64,
                );
                self.annotations.push(Annotation::Ellipse {
                    r,
                    color,
                    thick,
                    rot: 0.0,
                    filled,
                });
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
            }
            Tool::Guide => {
                self.push_undo();
                let r = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
                // At most one can exist, so placing a new one replaces the existing one.
                self.annotations
                    .retain(|ann| !matches!(ann, Annotation::Guide { .. }));
                self.annotations.push(Annotation::Guide { r });
            }
            Tool::Mosaic => {
                self.push_undo();
                let r = (
                    a.0.min(b.0) as f64,
                    a.1.min(b.1) as f64,
                    a.0.max(b.0) as f64,
                    a.1.max(b.1) as f64,
                );
                self.annotations.push(Annotation::Mosaic {
                    r,
                    rot: 0.0,
                    block: self.cur_block,
                    mode: if self.cur_blur {
                        MosaicMode::Blur
                    } else {
                        MosaicMode::Pixelate
                    },
                    seed: self.new_item_seed,
                });
                self.selected = vec![self.annotations.len() - 1];
                self.recompute_group_frame();
            }
            Tool::Select => self.marquee_select(a, b),
            // Polyline adds points per click, a gesture independent of
            // dragging, and Freehand commits directly in on_mouse's
            // Released branch — neither goes through push_shape (drag
            // commit). NumberMarker likewise commits immediately as a
            // single click with no drag, in on_press.
            Tool::Text | Tool::Polyline | Tool::Draw | Tool::NumberMarker => {}
        }
    }

    /// Zooms while keeping the world point at cursor position `(cx, cy)` fixed (`dir>0` = zoom in).
    fn zoom_at(&mut self, cx: f64, cy: f64, dir: f64) {
        // Ignored outside the canvas (over the toolbar).
        if cx < TOOLBAR_W as f64 || cy < TOOLBAR_H as f64 {
            return;
        }
        let factor = if dir > 0.0 {
            ZOOM_STEP
        } else {
            1.0 / ZOOM_STEP
        };
        let new_scale = (self.scale * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        if (new_scale - self.scale).abs() < f64::EPSILON {
            return;
        }
        // Finds the world point under the cursor and corrects the offset
        // so it lands at the same screen position after zooming.
        let wx = (cx - self.offset.0) / self.scale;
        let wy = (cy - self.offset.1) / self.scale;
        self.scale = new_scale;
        self.offset = (cx - wx * new_scale, cy - wy * new_scale);
        self.update_cursor();
        self.request_redraw();
    }

    /// World coordinates of the visible canvas's center (the default paste position).
    fn view_center_world(&self) -> (i64, i64) {
        let (sw, sh) = self.surface_size;
        let cx = (TOOLBAR_W + sw) as f64 / 2.0;
        let cy = (TOOLBAR_H + sh) as f64 / 2.0;
        self.to_world(cx, cy)
    }

    /// Adds an image item centered at `center` at 1:1 scale, and selects it via Select.
    fn add_image(&mut self, center: (i64, i64), src_w: i64, src_h: i64, pixels: Rc<Vec<u32>>) {
        if src_w <= 0 || src_h <= 0 {
            return;
        }
        self.push_undo();
        let pos = (center.0 - src_w / 2, center.1 - src_h / 2);
        self.annotations.push(Annotation::Image {
            r: (
                pos.0 as f64,
                pos.1 as f64,
                (pos.0 + src_w) as f64,
                (pos.1 + src_h) as f64,
            ),
            src_w,
            src_h,
            pixels,
            rot: 0.0,
        });
        // Selects it via Select so it can be moved/resized right after being added.
        self.commit_text();
        self.tool = Tool::Select;
        self.selected = vec![self.annotations.len() - 1];
        self.recompute_group_frame();
        self.edit = EditDrag::None;
        self.request_redraw();
    }

    /// Pastes the clipboard's image as a new image item (at the view's center).
    fn paste_clipboard_image(&mut self) {
        let img = match arboard::Clipboard::new().and_then(|mut c| c.get_image()) {
            Ok(i) => i,
            Err(_) => {
                eprintln!("クリップボードに画像がありません");
                return;
            }
        };
        if img.width == 0 || img.height == 0 {
            return;
        }
        let pixels = rgba_to_packed_alpha(img.width, img.height, &img.bytes);
        let center = self.view_center_world();
        self.add_image(center, img.width as i64, img.height as i64, Rc::new(pixels));
    }

    /// Ctrl+V: if there are items last copied to `item_clipboard`, pastes
    /// them slightly offset; otherwise falls back to pasting a clipboard
    /// image (both bundled under the same Ctrl+V, alongside `paste_clipboard_image`).
    fn paste_items_or_clipboard_image(&mut self) {
        if self.item_clipboard.is_empty() {
            self.paste_clipboard_image();
            return;
        }
        self.push_undo();
        let start = self.annotations.len();
        for ann in &self.item_clipboard {
            self.annotations
                .push(translate_annotation(ann, PASTE_OFFSET, PASTE_OFFSET));
        }
        self.commit_text();
        self.tool = Tool::Select;
        self.selected = (start..self.annotations.len()).collect();
        self.recompute_group_frame();
        self.edit = EditDrag::None;
        self.request_redraw();
    }

    /// Loads a dropped image file and adds it as an image item at the
    /// cursor position (or the center, if outside the canvas).
    fn drop_file(&mut self, path: &std::path::Path) {
        let img = match image::open(path) {
            Ok(i) => i.to_rgba8(),
            Err(e) => {
                eprintln!("画像の読み込みに失敗: {e}");
                return;
            }
        };
        let (w, h) = (img.width() as usize, img.height() as usize);
        if w == 0 || h == 0 {
            return;
        }
        let pixels = rgba_to_packed_alpha(w, h, img.as_raw());
        let (cx, cy) = self.cursor;
        let center = if cx >= TOOLBAR_W as f64 && cy >= TOOLBAR_H as f64 {
            self.to_world(cx, cy)
        } else {
            self.view_center_world()
        };
        self.add_image(center, w as i64, h as i64, Rc::new(pixels));
    }

    /// Export bounds (world coordinates). The guide's rect if one is
    /// placed; otherwise the rect containing every item (images are items
    /// too, so this also covers area added via paste/drag-and-drop).
    /// Shared by `render_export`/`render_thumbnail`/`save_session`.
    fn export_bounds(&self) -> (i64, i64, i64, i64) {
        guide_bounds(&self.annotations)
            .or_else(|| annotations_bounds(&self.annotations, self.text.as_ref()))
            .unwrap_or((0, 0, 1, 1))
    }

    /// Builds a `Shot` by baking the export bounds at full resolution.
    fn render_export(&self) -> Shot {
        let (x0, y0, x1, y1) = self.export_bounds();
        // Guards against allocation blowing up on an extremely large canvas.
        let w = ((x1 - x0).max(1) as usize).min(EXPORT_MAX);
        let h = ((y1 - y0).max(1) as usize).min(EXPORT_MAX);
        let t = Xform {
            scale: 1.0,
            ox: -x0 as f64,
            oy: -y0 as f64,
        };
        self.render_at(w, h, &t)
    }

    /// Builds a list thumbnail by baking at a scale that fits within `max_w x max_h`.
    /// `fit_scale` isn't used here, since its display-oriented lower clamp
    /// (5%) could exceed the thumbnail's intended max size — no lower
    /// bound is set here, and upscaling is allowed if needed.
    fn render_thumbnail(&self, max_w: usize, max_h: usize) -> Shot {
        let (x0, y0, x1, y1) = self.export_bounds();
        let bw = ((x1 - x0).max(1) as usize).min(EXPORT_MAX);
        let bh = ((y1 - y0).max(1) as usize).min(EXPORT_MAX);
        let scale = (max_w as f64 / bw as f64).min(max_h as f64 / bh as f64);
        let w = ((bw as f64 * scale) as usize).max(1);
        let h = ((bh as f64 * scale) as usize).max(1);
        let t = Xform {
            scale,
            ox: -x0 as f64 * scale,
            oy: -y0 as f64 * scale,
        };
        self.render_at(w, h, &t)
    }

    /// Bakes into a color buffer plus a sentinel coverage buffer and
    /// returns the composited `Shot` (shared by `render_export`/`render_thumbnail`).
    fn render_at(&self, w: usize, h: usize, t: &Xform) -> Shot {
        let text = self.text.as_ref();
        // Color is drawn on a black background — text's AA blends into
        // black, giving a natural edge even in transparent areas.
        let mut color = vec![0u32; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut color,
                w,
                h,
                scale: 1.0,
            };
            paint_annotations(&mut canvas, &self.annotations, t, text, None, true);
        }
        // For coverage, the same picture is drawn onto a separate buffer
        // filled with the sentinel; pixels still at the sentinel become transparent.
        let mut mask = vec![EXPORT_SENTINEL; w * h];
        {
            let mut canvas = Canvas {
                buf: &mut mask,
                w,
                h,
                scale: 1.0,
            };
            paint_annotations(&mut canvas, &self.annotations, t, text, None, true);
        }
        Shot {
            width: w as u32,
            height: h as u32,
            rgba: compose_rgba(&color, &mask),
        }
    }

    /// Records the session (image plus placed items) just before the
    /// window closes. A no-op if there are no items (closed while still blank).
    fn save_session(&self) {
        if self.annotations.is_empty() {
            return;
        }
        let (x0, y0, x1, y1) = self.export_bounds();
        let (width, height) = ((x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        let thumb = self.render_thumbnail(
            crate::session::THUMB_W as usize,
            crate::session::THUMB_H as usize,
        );
        let Ok(png) = crate::export::encode_png(&thumb) else {
            return;
        };
        let limit = crate::store::snapshot().session_history_limit;
        session::record(&self.annotations, width, height, &png, limit);
    }

    fn draw(&mut self) {
        let (sw, sh) = self.surface_size;
        let (pw, ph) = self.physical_size;
        if sw == 0 || sh == 0 {
            return;
        }
        // Finalize everything needed for drawing before borrowing surface (to split the borrows).
        let (ox, oy) = self.offset;
        let scale = self.scale;
        let show_field = self.active_field();
        let show_fill = self.active_fill_checkbox();
        let (props, top_btns) = top_layout(sw, show_field, show_fill, self.text.as_ref());
        let tools = tool_buttons();
        let icons = &self.icons;
        let hover = self.hover;
        let tool = self.tool;
        let dragging = self.dragging;
        let pinned = self.pinned;
        let selected = self.selected.clone();
        let group_rect = self.group_rect;
        let group_rot = self.group_rot;
        let (cur_color, cur_thick, cur_size) = (self.cur_color, self.cur_thick, self.cur_size);
        let cur_filled = self.cur_filled;
        let cur_block = self.cur_block;
        let cur_mosaic_mode = if self.cur_blur {
            MosaicMode::Blur
        } else {
            MosaicMode::Pixelate
        };
        let new_item_seed = self.new_item_seed;
        let next_marker_num = self.next_marker_num;
        // The active display/picker value (selected items' style, or the default if none).
        let (active_color, active_thick, active_size, active_filled, active_block, active_blur) =
            self.active_style();
        let focus = self.focus;
        let field_buf = self.buf.clone();
        let picker = self.picker;
        // The provisional shape while dragging (current style).
        let preview = if self.dragging {
            self.drag_start.and_then(|a| {
                let b = self.to_world(self.cursor.0, self.cursor.1);
                match self.tool {
                    Tool::Arrow => Some(Annotation::Arrow {
                        a,
                        b,
                        color: cur_color,
                        thick: cur_thick,
                    }),
                    Tool::Rect => Some(Annotation::Rect {
                        r: (
                            a.0.min(b.0) as f64,
                            a.1.min(b.1) as f64,
                            a.0.max(b.0) as f64,
                            a.1.max(b.1) as f64,
                        ),
                        color: cur_color,
                        thick: cur_thick,
                        rot: 0.0,
                        filled: cur_filled,
                    }),
                    Tool::Ellipse => Some(Annotation::Ellipse {
                        r: (
                            a.0.min(b.0) as f64,
                            a.1.min(b.1) as f64,
                            a.0.max(b.0) as f64,
                            a.1.max(b.1) as f64,
                        ),
                        color: cur_color,
                        thick: cur_thick,
                        rot: 0.0,
                        filled: cur_filled,
                    }),
                    Tool::Guide => Some(Annotation::Guide {
                        r: (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)),
                    }),
                    Tool::Mosaic => Some(Annotation::Mosaic {
                        r: (
                            a.0.min(b.0) as f64,
                            a.1.min(b.1) as f64,
                            a.0.max(b.0) as f64,
                            a.1.max(b.1) as f64,
                        ),
                        rot: 0.0,
                        block: cur_block,
                        mode: cur_mosaic_mode,
                        seed: new_item_seed,
                    }),
                    Tool::Draw => {
                        // Previews the accumulated stroke plus the current
                        // cursor position appended as a rubber band (same
                        // approach as Polyline's pre-commit preview).
                        let mut points = self.freehand_pts.clone();
                        points.push(b);
                        Some(Annotation::Polyline {
                            points,
                            color: cur_color,
                            thick: cur_thick,
                        })
                    }
                    Tool::Select | Tool::Text | Tool::Polyline | Tool::NumberMarker => None,
                }
            })
        } else if !self.polyline_pts.is_empty() {
            // A polyline before commit: previews the accumulated vertices
            // plus the current cursor position appended as a provisional
            // final point, as a rubber band.
            let mut points = self.polyline_pts.clone();
            points.push(self.to_world(self.cursor.0, self.cursor.1));
            Some(Annotation::Polyline {
                points,
                color: cur_color,
                thick: cur_thick,
            })
        } else {
            None
        };
        // The marquee-selection preview rect while dragging (Select tool only).
        let marquee_rect = if self.dragging && self.tool == Tool::Select {
            self.drag_start.map(|a| {
                let b = self.to_world(self.cursor.0, self.cursor.1);
                (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
            })
        } else {
            None
        };
        // During a multi-selection's group-transform drag, the bounding
        // rect is computed directly from the drag's own baseline
        // (`active_group_frame`), rather than recomputing
        // `self.group_rect`/`group_rot` on the spot. Re-deriving it from
        // the items' current positions via the axis-aligned bbox would
        // fall out of sync with the rotation center used during the drag,
        // making the bounding rect appear to drift.
        let group_drag_rect = self.active_group_frame();

        let annotations = &self.annotations;
        let text = self.text.as_ref();
        // Display info for the text being edited (committed plus IME composition plus caret).
        let ime_preedit = self.ime_preedit.as_str();
        let editing = self
            .editing
            .as_ref()
            .map(|(p, s)| (*p, format!("{s}{ime_preedit}|")));

        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };

        // During a Freehand drag, the cache can be reused if its size and
        // Xform match the current frame; if not (window resize, or rarely
        // a wheel zoom mid-drag), this simply falls back to full
        // rendering and rebuilds it below.
        // Keyed on the physical buffer size (not the logical `surface_size`)
        // since `c.buf` below is a verbatim copy of the physical-sized
        // `canvas.buf` — the two must always agree or `copy_from_slice`
        // (used when reusing the cache) would panic on a length mismatch.
        let use_cache = tool == Tool::Draw
            && dragging
            && matches!(
                &self.freehand_bg_cache,
                Some(c) if c.w == pw && c.h == ph && c.scale == scale && c.ox == ox && c.oy == oy
            );

        if !use_cache {
            // Lays down the low-contrast checkerboard marking transparency
            // (items like the screenshot sit on top). Uses the physical
            // buffer size directly (a purely decorative tiling pattern, so
            // there's no need to route it through `Canvas`'s DPI scaling).
            for y in 0..ph {
                let row = y * pw;
                for x in 0..pw {
                    buf[row + x] = if (x / CHECK_SIZE + y / CHECK_SIZE) & 1 == 0 {
                        CHECK_A
                    } else {
                        CHECK_B
                    };
                }
            }
        }

        {
            let mut canvas = Canvas {
                buf: &mut buf[..],
                w: pw,
                h: ph,
                scale: self.dpi,
            };

            let t = Xform { scale, ox, oy };
            // A new guide mid-drag takes priority (so the dimming follows
            // it before it's committed); otherwise the committed guide is used.
            let guide_rect = match &preview {
                Some(Annotation::Guide { r }) => Some(rect_norm(*r)),
                _ => guide_bounds(annotations),
            };
            if use_cache {
                // Committed items reuse what was already drawn in a
                // previous frame as-is (skipping resampling a large
                // screenshot entirely).
                if let Some(c) = &self.freehand_bg_cache {
                    canvas.buf.copy_from_slice(&c.buf);
                }
            } else {
                // The screenshot is an item too, so paint_annotations draws it along with the base image.
                paint_annotations(&mut canvas, annotations, &t, text, None, false);
                if tool == Tool::Draw && dragging {
                    // Caches only the committed state, excluding the
                    // stroke currently being drawn.
                    self.freehand_bg_cache = Some(FreehandBgCache {
                        w: pw,
                        h: ph,
                        scale,
                        ox,
                        oy,
                        buf: canvas.buf.to_vec(),
                    });
                }
            }
            // The preview of the stroke currently being drawn (when using the cache, only this is drawn on top).
            if let Some(p) = preview.clone() {
                paint_annotations(&mut canvas, &[], &t, text, Some(p), false);
            }
            // If a guide exists, dims outside it to make the excluded export area clear.
            dim_outside_guide(&mut canvas, guide_rect, &t);

            // The caret for the text being edited (at the current color/size).
            if let (Some((pos, caret)), Some(tr)) = (editing.as_ref(), text) {
                let (x, y) = t.map(*pos);
                let sz = t.text_size(cur_size);
                tr.draw(&mut canvas, x as f32, y as f32 + sz, caret, sz, cur_color);
            }

            // The selection outline: a single selection has handles; a
            // multi-selection gets a lightweight bbox outline per item
            // plus resize/rotate handles on the bounding rect (the Guide
            // tool while adjusting the guide is always a single selection).
            if tool == Tool::Guide {
                if let Some(&i) = selected.first()
                    && let Some(ann) = annotations.get(i)
                {
                    paint_selection(&mut canvas, ann, &t, text);
                }
            } else if tool == Tool::Select || !selected.is_empty() {
                // Even on a non-Select tool, if a selection remains (e.g.
                // right after drawing), the same handles are drawn (so it
                // can be resized/rotated right there).
                match selected.len() {
                    1 => {
                        if let Some(ann) = annotations.get(selected[0]) {
                            paint_selection(&mut canvas, ann, &t, text);
                            // Polyline/Freehand also gets bounding-rect
                            // handles (besides the vertex circles) so the
                            // whole line can be resized/rotated together.
                            if matches!(ann, Annotation::Polyline { .. }) {
                                let frame = group_drag_rect.or(Some((group_rect, group_rot)));
                                if let Some((rect, rot)) = frame {
                                    paint_group_handles(&mut canvas, rect, rot, &t);
                                }
                            }
                        }
                    }
                    n if n > 1 => {
                        // If not dragging, uses group_rect/group_rot as-is
                        // — kept until the selection changes (re-deriving
                        // from the axis-aligned bbox every frame would
                        // make it appear to drift).
                        let frame = group_drag_rect.or(Some((group_rect, group_rot)));
                        if let Some((rect, rot)) = frame {
                            paint_group_handles(&mut canvas, rect, rot, &t);
                        }
                    }
                    _ => {}
                }
                // The marquee-selection preview rect while dragging (Select tool only).
                if tool == Tool::Select
                    && let Some(r) = marquee_rect
                {
                    paint_bbox(&mut canvas, r, &t);
                }
            }

            // The toolbar (an L-shape: top bar plus left bar).
            canvas.fill(
                Rect {
                    x0: 0,
                    y0: 0,
                    x1: sw,
                    y1: TOOLBAR_H.min(sh),
                },
                TOOLBAR_BG,
            );
            canvas.fill(
                Rect {
                    x0: 0,
                    y0: TOOLBAR_H.min(sh),
                    x1: TOOLBAR_W.min(sw),
                    y1: sh,
                },
                TOOLBAR_BG,
            );

            // The left bar's tools, plus the top bar's right-side Pin/Save/Copy.
            for (btn, rect) in tools.iter().chain(top_btns.iter()) {
                let active = matches!(btn, EditorBtn::Tool(t) if *t == tool)
                    || (matches!(btn, EditorBtn::Pin) && pinned);
                let color = if active {
                    BTN_ACTIVE
                } else if hover == Some(*btn) {
                    BTN_HOVER
                } else {
                    BTN_BG
                };
                canvas.fill(*rect, color);
                let icon = icons.get(*btn);
                let icon_rect = Rect {
                    x0: rect.x0 + ICON_PAD,
                    y0: rect.y0 + ICON_PAD,
                    x1: rect.x1.saturating_sub(ICON_PAD),
                    y1: rect.y1.saturating_sub(ICON_PAD),
                };
                canvas.blit_scaled_alpha(icon_rect, icon.w, icon.h, &icon.pixels);
            }

            // The top bar's left-side properties. The "Color" label always
            // sits left of the color field; Width/Text size labels sit
            // left of their field depending on the selection.
            if let Some(tr) = text {
                let baseline = tr.baseline_for_center(TOOLBAR_H as f32 / 2.0, UI_FONT_SIZE);
                tr.draw(
                    &mut canvas,
                    10.0,
                    baseline,
                    "Color",
                    UI_FONT_SIZE,
                    0x00CC_CCCC,
                );
                if let Some(f) = show_field {
                    let color_label_w = label_col_w(Some(tr), "Color", COLOR_LABEL_W_FALLBACK);
                    let lx = (10 + color_label_w + COLOR_W + COLOR_FIELD_GAP) as f32;
                    tr.draw(
                        &mut canvas,
                        lx,
                        baseline,
                        f.label(),
                        UI_FONT_SIZE,
                        0x00CC_CCCC,
                    );
                }
            }
            for (pc, r) in &props {
                match pc {
                    PropCtrl::Color => {
                        match active_color {
                            PropVal::Uniform(c) => canvas.fill(*r, c),
                            PropVal::Mixed(_) => {
                                canvas.fill(*r, FIELD_BG);
                                label_center_size(&mut canvas, text, *r, "Mixed", UI_FONT_SIZE);
                            }
                        }
                        canvas.stroke(*r, 0x0080_8080);
                        if picker.is_some() {
                            canvas.stroke(*r, SEL_COLOR);
                            canvas.stroke(inset(*r, 1), SEL_COLOR);
                        }
                    }
                    PropCtrl::Step(_, up) => {
                        canvas.fill(*r, BTN_BG);
                        label_center_size(
                            &mut canvas,
                            text,
                            *r,
                            if *up { "+" } else { "-" },
                            UI_FONT_SIZE,
                        );
                    }
                    PropCtrl::Field(f) => {
                        canvas.fill(*r, FIELD_BG);
                        if focus == Some(*f) {
                            canvas.stroke(*r, SEL_COLOR);
                        }
                        let val = match f {
                            Field::Line => match active_thick {
                                PropVal::Uniform(v) => v.to_string(),
                                PropVal::Mixed(_) => "Mixed".to_string(),
                            },
                            Field::Size => match active_size {
                                PropVal::Uniform(v) => (v.round() as i64).to_string(),
                                PropVal::Mixed(_) => "Mixed".to_string(),
                            },
                            Field::Number => next_marker_num.to_string(),
                            Field::Block => match active_block {
                                PropVal::Uniform(v) => (v.round() as i64).to_string(),
                                PropVal::Mixed(_) => "Mixed".to_string(),
                            },
                        };
                        let shown = if focus == Some(*f) {
                            format!("{field_buf}|")
                        } else {
                            val
                        };
                        label_center_size(&mut canvas, text, *r, &shown, UI_FONT_SIZE);
                    }
                    PropCtrl::Fill => {
                        // Leans toward the inactive look when `Mixed`
                        // (only some are filled) — same as the Pin button,
                        // just an active/inactive fill.
                        let active = matches!(active_filled, PropVal::Uniform(true));
                        canvas.fill(*r, if active { BTN_ACTIVE } else { BTN_BG });
                        label_center_size(&mut canvas, text, *r, "Fill", UI_FONT_SIZE);
                    }
                    PropCtrl::Blur => {
                        let active = matches!(active_blur, PropVal::Uniform(true));
                        canvas.fill(*r, if active { BTN_ACTIVE } else { BTN_BG });
                        label_center_size(&mut canvas, text, *r, "Blur", UI_FONT_SIZE);
                    }
                }
            }

            // The color picker popup, layered on top of the canvas when open.
            if let Some((h, s, v)) = picker {
                let (popup, sv, hue) = picker_geom(sw, text);
                canvas.fill(popup, PICK_BG);
                canvas.stroke(popup, 0x0080_8080);
                for yy in 0..PICK_SV {
                    let vv = 1.0 - yy as f32 / PICK_SV as f32;
                    for xx in 0..PICK_SV {
                        let ss = xx as f32 / PICK_SV as f32;
                        canvas.set(sv.x0 + xx, sv.y0 + yy, hsv_to_rgb(h, ss, vv));
                    }
                }
                marker(
                    &mut canvas,
                    sv.x0 as i64 + (s * PICK_SV as f32) as i64,
                    sv.y0 as i64 + ((1.0 - v) * PICK_SV as f32) as i64,
                );
                for xx in 0..PICK_SV {
                    let col = hsv_to_rgb(xx as f32 / PICK_SV as f32 * 360.0, 1.0, 1.0);
                    for yy in 0..PICK_HUE_H {
                        canvas.set(hue.x0 + xx, hue.y0 + yy, col);
                    }
                }
                let hx = hue.x0 as i64 + (h / 360.0 * PICK_SV as f32) as i64;
                for yy in 0..PICK_HUE_H as i64 {
                    canvas.set_i(hx, hue.y0 as i64 + yy, 0x00FF_FFFF);
                    canvas.set_i(hx - 1, hue.y0 as i64 + yy, 0x0000_0000);
                }
            }
        }

        let _ = buf.present();
    }
}

/// The left (vertical) bar's tool buttons. A pure layout, independent of `self`.
fn tool_buttons() -> Vec<(EditorBtn, Rect)> {
    let lx0 = (TOOLBAR_W - BTN_SIZE) / 2;
    let mut v = Vec::with_capacity(10);
    let mut y = TOOLBAR_H + 12;
    for b in [
        EditorBtn::Tool(Tool::Select),
        EditorBtn::Tool(Tool::Arrow),
        EditorBtn::Tool(Tool::Polyline),
        EditorBtn::Tool(Tool::Draw),
        EditorBtn::Tool(Tool::Rect),
        EditorBtn::Tool(Tool::Ellipse),
        EditorBtn::Tool(Tool::Mosaic),
        EditorBtn::Tool(Tool::Text),
        EditorBtn::Tool(Tool::NumberMarker),
        EditorBtn::Tool(Tool::Guide),
    ] {
        v.push((b, mk_rect(lx0, y, BTN_SIZE, BTN_SIZE)));
        y += BTN_SIZE + BTN_GAP;
    }
    v
}

/// The top bar's left-side property layout, and right-side button layout.
type PropLayout = Vec<(PropCtrl, Rect)>;
type BtnLayout = Vec<(EditorBtn, Rect)>;

/// The top bar: left = properties (Color/Line/Size), right = Pin/Save/Copy
/// (right-aligned), based on `sw`. Measured label text width plus
/// `LABEL_GAP` (or `fallback` with no font). Routing "Color"/"Width"/"Text
/// size" through the same function keeps the label-to-control gap
/// consistent regardless of label length.
fn label_col_w(text: Option<&TextRenderer>, label: &str, fallback: usize) -> usize {
    text.map(|tr| tr.text_width(label, UI_FONT_SIZE).ceil() as usize + LABEL_GAP)
        .unwrap_or(fallback)
}

fn top_layout(
    sw: usize,
    show: Option<Field>,
    show_fill: bool,
    text: Option<&TextRenderer>,
) -> (PropLayout, BtnLayout) {
    let fy = (TOOLBAR_H - FIELD_H) / 2;
    let by = (TOOLBAR_H - BTN_SIZE) / 2;
    let sy = (TOOLBAR_H - SWATCH) / 2;

    // Left: the "Color" label plus color field, then (depending on
    // selection) a label area plus -/field/+, then (Rect/Ellipse only) the Fill toggle.
    let mut props = Vec::with_capacity(5);
    let mut x = 10 + label_col_w(text, "Color", COLOR_LABEL_W_FALLBACK);
    props.push((PropCtrl::Color, mk_rect(x, sy, COLOR_W, SWATCH)));
    x += COLOR_W + COLOR_FIELD_GAP;
    if let Some(f) = show {
        x += label_col_w(text, f.label(), LABEL_W_FALLBACK); // label area (drawn by draw())
        props.push((PropCtrl::Step(f, false), mk_rect(x, fy, STEP_W, FIELD_H)));
        x += STEP_W;
        props.push((PropCtrl::Field(f), mk_rect(x, fy, FIELD_W, FIELD_H)));
        x += FIELD_W;
        props.push((PropCtrl::Step(f, true), mk_rect(x, fy, STEP_W, FIELD_H)));
        x += STEP_W;
    }
    if show_fill {
        x += FILL_GAP;
        props.push((PropCtrl::Fill, mk_rect(x, fy, FILL_W, FIELD_H)));
    }
    // Field::Block is Mosaic-only, so "is Mosaic contextually active"
    // can be read directly from `show` (no dedicated helper needed).
    if matches!(show, Some(Field::Block)) {
        x += BLUR_GAP;
        props.push((PropCtrl::Blur, mk_rect(x, fy, BLUR_W, FIELD_H)));
    }

    // Right: from the right edge, Copy / Save / (gap) / Pin.
    let mut buttons = Vec::with_capacity(3);
    let mut rx = sw.saturating_sub(10 + BTN_SIZE);
    for b in [EditorBtn::Copy, EditorBtn::Save] {
        buttons.push((b, mk_rect(rx, by, BTN_SIZE, BTN_SIZE)));
        rx = rx.saturating_sub(BTN_SIZE + BTN_GAP);
    }
    rx = rx.saturating_sub(GROUP_GAP);
    buttons.push((EditorBtn::Pin, mk_rect(rx, by, BTN_SIZE, BTN_SIZE)));

    (props, buttons)
}

/// Parses and clamps a numeric input buffer; returns `current` if empty/invalid.
fn parse_dim(buf: &str, current: f64, range: (f64, f64)) -> f64 {
    match buf.trim().parse::<f64>() {
        Ok(v) => v.clamp(range.0, range.1),
        Err(_) => current,
    }
}

fn mk_rect(x: usize, y: usize, w: usize, h: usize) -> Rect {
    Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    }
}

/// Rects for the color picker's popup, SV square, and Hue bar (directly below the swatch).
fn picker_geom(sw: usize, text: Option<&TextRenderer>) -> (Rect, Rect, Rect) {
    let (props, _) = top_layout(sw, None, false, text);
    let px = props[0].1.x0; // The Color swatch is always first.
    let py = TOOLBAR_H + 4;
    let popup = mk_rect(
        px,
        py,
        PICK_SV + 2 * PICK_PAD,
        PICK_SV + PICK_HUE_H + 3 * PICK_PAD,
    );
    let sv = mk_rect(px + PICK_PAD, py + PICK_PAD, PICK_SV, PICK_SV);
    let hue = mk_rect(px + PICK_PAD, sv.y1 + PICK_PAD, PICK_SV, PICK_HUE_H);
    (popup, sv, hue)
}

fn inside(r: Rect, x: f64, y: f64) -> bool {
    x >= r.x0 as f64 && x < r.x1 as f64 && y >= r.y0 as f64 && y < r.y1 as f64
}

/// Shrinks a rect by `n` px on each side.
fn inset(r: Rect, n: usize) -> Rect {
    Rect {
        x0: r.x0 + n,
        y0: r.y0 + n,
        x1: r.x1.saturating_sub(n),
        y1: r.y1.saturating_sub(n),
    }
}

/// Draws a label centered in a rect, at the given font size.
fn label_center_size(
    canvas: &mut Canvas,
    text: Option<&TextRenderer>,
    r: Rect,
    label: &str,
    size: f32,
) {
    if let Some(tr) = text {
        let tw = tr.text_width(label, size);
        let lx = r.x0 as f32 + (r.width() as f32 - tw) / 2.0;
        let baseline = tr.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, size);
        tr.draw(canvas, lx, baseline, label, size, TEXT_COLOR);
    }
}

/// Checks upfront whether a session folder can actually be loaded. Used by
/// `app.rs` when opening from the Settings Recent tab, so a failure
/// doesn't close the Settings window (the actual load happens again, on
/// the separately spawned process that's launched fire-and-forget).
pub fn session_is_loadable(dir: &std::path::Path) -> Result<(), String> {
    session::load(dir).map(|_| ()).map_err(|e| e.to_string())
}

/// Runs as a separate editor process (`pashari editor [<png or session folder>]`),
/// driving its own EventLoop. With no `image_path`, opens blank (launched
/// from the tray's Editor entry). A directory is restored as a saved
/// session (opened from the Settings Recent tab).
pub fn run_standalone(image_path: Option<String>) {
    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("EventLoop の生成に失敗: {e}");
            return;
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = EditorApp {
        image_path: image_path.map(PathBuf::from),
        editor: None,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("editor error: {e}");
    }
}

/// The editor process's `ApplicationHandler`. Loads one image or one
/// session folder (blank if neither), and drives a single `Editor`.
struct EditorApp {
    /// A PNG file (the just-captured temp image) or a session folder (via the Recent tab).
    image_path: Option<PathBuf>,
    editor: Option<Editor>,
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.editor.is_some() {
            return;
        }
        let init = match &self.image_path {
            // A directory is a saved session (not a temp file, so it's not deleted).
            Some(path) if path.is_dir() => match session::load(path) {
                Ok((width, height, annotations)) => EditorInit::Session {
                    width,
                    height,
                    annotations,
                },
                Err(e) => {
                    eprintln!("セッションの読み込みに失敗: {e}");
                    // This process is launched fire-and-forget (app.rs's
                    // spawn_editor_process) with no other window, so
                    // eprintln alone shows the user nothing. Shows a
                    // dialog so the user notices, then exits.
                    crate::shell::show_error_dialog(&format!(
                        "セッションを読み込めませんでした\n{e}"
                    ));
                    event_loop.exit();
                    return;
                }
            },
            Some(path) => match crate::export::load_shot(path) {
                Ok(s) => {
                    // No longer needed once loaded — it was just a relay temp file.
                    let _ = std::fs::remove_file(path);
                    EditorInit::Shot(s)
                }
                Err(e) => {
                    eprintln!("画像の読み込みに失敗: {e}");
                    event_loop.exit();
                    return;
                }
            },
            None => EditorInit::Blank,
        };

        let monitor = event_loop
            .primary_monitor()
            .map(|m| {
                let s = m.size();
                (s.width as usize, s.height as usize, m.scale_factor())
            })
            .unwrap_or((1920, 1080, 1.0));
        self.editor = Some(Editor::new(event_loop, init, monitor));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let Some(ed) = self.editor.as_mut()
            && ed.handle_event(event_loop, event)
        {
            event_loop.exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_stack_steps_back_and_forward_through_history() {
        let mut h: UndoStack<i32> = UndoStack::new();
        // Two changes, 0 -> 1 -> 2, pushed in the same order as an actual
        // call site: "push the pre-change value, then update it."
        h.push(0);
        let mut current = 1;
        h.push(current);
        current = 2;

        current = h.undo(current).unwrap();
        assert_eq!(current, 1);
        current = h.undo(current).unwrap();
        assert_eq!(current, 0);
        // None once the history is exhausted.
        assert!(h.undo(current).is_none());

        current = h.redo(current).unwrap();
        assert_eq!(current, 1);
        current = h.redo(current).unwrap();
        assert_eq!(current, 2);
        // None once the history is exhausted.
        assert!(h.redo(current).is_none());
    }

    #[test]
    fn undo_stack_push_discards_redo_history() {
        let mut h: UndoStack<i32> = UndoStack::new();
        h.push(0);
        let current = h.undo(1).unwrap();
        assert_eq!(current, 0);

        // A new change discards the redo history that had been stashed by undo.
        h.push(current);
        assert!(h.redo(current).is_none());
    }

    #[test]
    fn fit_scale_downscales_large_and_keeps_small() {
        // A large image is downscaled.
        let s = fit_scale(4000, 2000, 1800, 900);
        assert!(s < 1.0 && (s - 0.45).abs() < 1e-9);
        // An image that already fits stays 1:1 (never upscaled).
        assert_eq!(fit_scale(400, 300, 1800, 900), 1.0);
    }

    #[test]
    fn tool_buttons_are_in_left_bar_and_stacked() {
        let v = tool_buttons();
        // Select / Arrow / Polyline / Freehand / Rect / Ellipse / Mosaic / Text / NumberMarker / Guide
        assert_eq!(v.len(), 10);
        for (b, r) in &v {
            assert!(matches!(b, EditorBtn::Tool(_)));
            assert!(r.y0 >= TOOLBAR_H && r.x1 <= TOOLBAR_W, "左バーに載るべき");
            assert_eq!(r.width(), r.height(), "1:1 (正方形) であるべき");
        }
        for w in v.windows(2) {
            assert!(w[1].1.y0 >= w[0].1.y1, "縦に重ならない");
        }
    }

    #[test]
    fn should_sample_freehand_point_uses_min_step_threshold() {
        // Below the threshold is thinned out.
        assert!(!should_sample_freehand_point((0, 0), (1, 1)));
        // Exactly at the threshold (the boundary) is accepted.
        let edge = FREEHAND_MIN_STEP as i64;
        assert!(should_sample_freehand_point((0, 0), (edge, 0)));
        // Well past the threshold is accepted.
        assert!(should_sample_freehand_point((0, 0), (100, 100)));
    }

    #[test]
    fn editor_keys_from_config_maps_default_tool_shortcuts() {
        let keys = EditorKeys::from_config(&crate::store::hotkeys::HotkeyConfig::default());
        let plain = |ch: char| LocalKey::new(false, false, false, ch);
        assert_eq!(keys.tool_select, vec![plain('v')]);
        assert_eq!(keys.tool_arrow, vec![plain('a')]);
        assert_eq!(keys.tool_polyline, vec![plain('l')]);
        assert_eq!(keys.tool_draw, vec![plain('d')]);
        assert_eq!(keys.tool_rect, vec![plain('r')]);
        assert_eq!(keys.tool_ellipse, vec![plain('c')]);
        assert_eq!(keys.tool_text, vec![plain('t')]);
        assert_eq!(keys.tool_number_marker, vec![plain('n')]);
        assert_eq!(
            keys.reset_zoom,
            vec![LocalKey::new(true, false, false, '0')]
        );
    }

    #[test]
    fn near_rect_outline_local_hits_border_band_but_misses_center_and_far_outside() {
        let rect = (0.0, 0.0, 100.0, 100.0);
        let tol = 5.0;
        // The center (the hollow interior) misses.
        assert!(!near_rect_outline_local(rect, (50.0, 50.0), tol));
        // Exactly on an edge (the border itself) hits.
        assert!(near_rect_outline_local(rect, (50.0, 0.0), tol));
        // Within tol inside/outside an edge also hits, as a band.
        assert!(near_rect_outline_local(rect, (50.0, 3.0), tol));
        assert!(near_rect_outline_local(rect, (50.0, -3.0), tol));
        // A corner also counts as part of the border.
        assert!(near_rect_outline_local(rect, (0.0, 0.0), tol));
        // Outside the band (well inside or well outside) misses.
        assert!(!near_rect_outline_local(rect, (50.0, 20.0), tol));
        assert!(!near_rect_outline_local(rect, (50.0, -20.0), tol));
    }

    #[test]
    fn top_layout_props_left_buttons_right() {
        let sw = 900;
        let (props, buttons) = top_layout(sw, Some(Field::Line), false, None);
        // Color plus (-/numeric/+) = 4.
        assert_eq!(props.len(), 4);
        assert_eq!(buttons.len(), 3); // Pin / Save / Copy
        // With no selection (None), the only property is Color.
        assert_eq!(top_layout(sw, None, false, None).0.len(), 1);
        // Everything is within the top bar (y < TOOLBAR_H).
        for (_, r) in &props {
            assert!(r.y1 <= TOOLBAR_H);
        }
        for (_, r) in &buttons {
            assert!(r.y1 <= TOOLBAR_H);
        }
        // Properties on the left (small x), buttons on the right (large x), never overlapping.
        let props_right = props.iter().map(|(_, r)| r.x1).max().unwrap();
        let btns_left = buttons.iter().map(|(_, r)| r.x0).min().unwrap();
        assert!(props_right < btns_left, "左右が重なっている");
        // The right buttons follow sw (a wider window pushes them further right).
        let (_, b2) = top_layout(1400, Some(Field::Line), false, None);
        let right_now = buttons.iter().map(|(_, r)| r.x1).max().unwrap();
        let right_wide = b2.iter().map(|(_, r)| r.x1).max().unwrap();
        assert!(right_wide > right_now);
    }

    #[test]
    fn top_layout_adds_fill_control_only_when_requested() {
        let sw = 900;
        // Not Rect/Ellipse (has a Width field but no Fill).
        let (props_no_fill, _) = top_layout(sw, Some(Field::Line), false, None);
        assert!(!props_no_fill.iter().any(|(pc, _)| *pc == PropCtrl::Fill));

        // For Rect/Ellipse: Fill is added, one item, right of the Width group.
        let (props_fill, _) = top_layout(sw, Some(Field::Line), true, None);
        assert_eq!(props_fill.len(), 5);
        let fill_rect = props_fill
            .iter()
            .find(|(pc, _)| *pc == PropCtrl::Fill)
            .map(|(_, r)| *r)
            .expect("Fill コントロールがあるはず");
        let width_field_right = props_fill
            .iter()
            .find(|(pc, _)| matches!(pc, PropCtrl::Field(Field::Line)))
            .map(|(_, r)| r.x1)
            .unwrap();
        assert!(fill_rect.x0 >= width_field_right, "Fill は Width 群の右");
        assert!(fill_rect.y1 <= TOOLBAR_H);
    }

    #[test]
    fn parse_dim_clamps_and_keeps_current() {
        assert_eq!(parse_dim("8", 4.0, THICK_RANGE), 8.0);
        assert_eq!(parse_dim("999", 4.0, THICK_RANGE), THICK_RANGE.1); // 上限クランプ
        assert_eq!(parse_dim("0", 4.0, THICK_RANGE), THICK_RANGE.0); // 下限クランプ
        assert_eq!(parse_dim("", 4.0, THICK_RANGE), 4.0); // 空は現状維持
        assert_eq!(parse_dim("abc", 5.0, SIZE_RANGE), 5.0); // 不正は現状維持
    }
}
