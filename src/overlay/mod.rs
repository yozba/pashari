//! The region-selection overlay.
//!
//! Freeze approach: captures the screen once at startup and lays that
//! still image behind an opaque, always-on-top window. The whole surface
//! is dimmed, and only the inside of the selection rect is drawn at its
//! original brightness, giving a "cutout" look.
//!
//! Flow: drag to pick a region (Selecting) -> releasing commits it and
//! shows a "Save / Copy / Edit / Upload / Video / Quit" menu near the
//! selection (Menu). A button click or hotkey (S / C / E / U / V / Q)
//! commits an action, returning the cropped `Shot` and chosen `Action` to
//! the caller. Esc or Q cancels. The R key reuses the last-used region at
//! any time (replaces the current selection if any, no button for it),
//! and the X key clears the selection to redraw it (also no button).
//!
//! While selecting, the wheel drives a cursor-centered magnifier
//! ([`view::View`]). The cursor stays at its 1:1 full-screen position, and
//! only the surrounding area is magnified, so the cursor can still be
//! moved to pick any pixel precisely even while zoomed in.

mod menu;
pub(crate) mod snap;
mod view;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{CursorIcon, Window, WindowId, WindowLevel};

use raw_window_handle::HasWindowHandle;
use rayon::prelude::*;

use crate::app::UserEvent;
use crate::capture;
use crate::export::Shot;
use crate::localkey::LocalKey;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect};
use view::View;

/// Side length (px) of a square button, shared by the action menu and the
/// recording control bar.
pub(super) const ACTION_BTN: usize = 56;
/// Padding for the recording control bar, and the bar size it implies.
const CTRL_PAD: usize = 12;
const CTRL_BTN_X: usize = CTRL_PAD;
/// The bar width matches the widest configuration (setup: Start/Format/
/// Fps/Desktop/Mic/Quit, 6 buttons). Buttons are packed tightly for now
/// (see `control_buttons`), but a little slack is kept so spacing them
/// out later won't need a resize.
const CTRL_MAX_BTNS: usize = 6;
const CTRL_RESERVE_W: usize = 60;
const CONTROL_W: usize = CTRL_PAD * 2 + ACTION_BTN * CTRL_MAX_BTNS + CTRL_RESERVE_W;
const CONTROL_H: usize = CTRL_PAD * 2 + ACTION_BTN;
/// Redraw interval for the control bar while recording. The elapsed-time
/// display alone would only need 1Hz, but this is shorter so the
/// Desktop/Mic level meters animate smoothly.
const CONTROL_REDRAW_INTERVAL: Duration = Duration::from_millis(50);
/// Display gain for the level meters. At normal volume the peak barely
/// moves and is hard to read, so it's boosted before display (clamped to
/// 0.0..=1.0 in `draw_level_meter`).
const METER_GAIN: f32 = 3.0;
/// Region border thickness (px).
const BORDER_STRIP: usize = 3;

/// Accent color (selection border). 0x00RRGGBB.
const ACCENT: u32 = 0x004D_A6FF;
/// Max drag distance (src px) still counted as a click (i.e. a snap
/// commit). Beyond this it's treated as a manual rect selection.
const SNAP_CLICK_TOL: f64 = 5.0;
/// Frame thickness (px).
const BORDER: usize = 2;
/// Edge-grab hit tolerance (screen px; divided by zoom to test in src
/// space when magnified).
const HANDLE_GRAB: f64 = 9.0;
/// Zoom range and the multiplier per wheel notch (continuous zoom).
const ZOOM_MIN: f64 = 1.0;
const ZOOM_MAX: f64 = 32.0;
const ZOOM_FACTOR: f64 = 1.2;

/// An action selectable after committing a selection.
#[derive(Clone, Copy)]
pub enum Action {
    Save,
    Copy,
    Edit,
    /// Hand off to an external editor (Shift+E; hotkey-only, no button).
    EditExternal,
    Upload,
    Record,
    /// Discards the selection and ends the capture session (same as Esc).
    Quit,
}

/// The overlay's result.
pub enum Outcome {
    /// A still-image action (Save / Copy / Edit) with the cropped region.
    Captured { action: Action, shot: Shot },
    /// The path of the mp4 saved after stopping a recording.
    Recorded(PathBuf),
    /// The path of the png saved to the "save as" (Shift+S) dialog's chosen location.
    Saved(PathBuf),
}

/// The last-used selection region (for reuse via the R key). Kept only
/// in-process, not persisted to the config file, since its coordinates
/// only make sense for the current monitor layout.
static LAST_REGION: Mutex<Option<Rect>> = Mutex::new(None);

fn save_last_region(r: Rect) {
    *LAST_REGION.lock().unwrap() = Some(r);
}

fn last_region() -> Option<Rect> {
    *LAST_REGION.lock().unwrap()
}

/// The overlay's operating mode.
enum Mode {
    /// Selecting a region.
    Selecting,
    /// Preparing to record (freeze image hidden, region border + Start/
    /// audio toggles shown).
    RecordSetup,
    /// Recording (region border + Stop button + elapsed time shown).
    Recording,
}

/// A button on the control bar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CtrlBtn {
    /// Start / Stop.
    Primary,
    /// Output format toggle (MP4 / GIF).
    Format,
    /// Recording FPS (cycles through the config's preset list).
    Fps,
    /// Desktop audio toggle.
    Desktop,
    /// Mic toggle.
    Mic,
    /// End (cancels during setup, stops and saves while recording;
    /// provided as an explicit button less error-prone than Esc).
    Quit,
}

/// Resize direction for the selection rect (4 corners + 4 edges). Edges
/// aren't limited to 8 fixed grab points — grabbing anywhere along an
/// edge moves just that axis (see `hit_handle`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// The operation in progress during the menu (transform) phase.
enum Adjust {
    /// Nothing in progress.
    Idle,
    /// Moving the whole region via an inside drag (`anchor` = the src
    /// position grabbed, `orig` = the rect at grab time).
    Moving { anchor: (f64, f64), orig: Rect },
    /// Resizing via a handle drag.
    Resizing(Handle),
}

/// The still image captured at startup (physical pixels, all monitors composited into one).
struct Frozen {
    width: usize,
    height: usize,
    /// Each pixel at its original brightness, as 0x00RRGGBB.
    bright: Vec<u32>,
    /// `bright` dimmed to roughly half brightness.
    dim: Vec<u32>,
    /// The absolute position of `(0,0)` in virtual-desktop coordinates
    /// (can be negative depending on monitor layout).
    origin: (i32, i32),
    /// Each monitor's bounding rect (src coordinates). With mismatched
    /// monitor heights/layouts, clamping against the whole composited
    /// canvas could place a button in a "blind spot" belonging to no
    /// monitor, so button placement clamps against this instead — the
    /// range actually showing a screen.
    monitors: Vec<Rect>,
    /// Each monitor's own DPI scale factor, same order/index as
    /// `monitors`. A multi-monitor setup can mix DPIs, and this window
    /// spans every monitor at once, so there's no single "the window's
    /// scale factor" that's correct everywhere — anything sized by DPI
    /// (currently just the post-selection action menu) has to look up the
    /// specific monitor a selection is on.
    monitor_dpis: Vec<f64>,
}

/// Computes the bounding rect `(min_x, min_y, max_x, max_y)` containing
/// all monitors, from a list of `(x, y, w, h)` monitor rects (used for the
/// composited canvas size and origin; an OS-independent pure function).
fn monitors_bounds(rects: &[(i32, i32, u32, u32)]) -> Option<(i32, i32, i32, i32)> {
    rects
        .iter()
        .map(|&(x, y, w, h)| (x, y, x + w as i32, y + h as i32))
        .reduce(|acc, r| {
            (
                acc.0.min(r.0),
                acc.1.min(r.1),
                acc.2.max(r.2),
                acc.3.max(r.3),
            )
        })
}

/// Index of the monitor that actually shows rect `sel` (`monitors` in src
/// coordinates). If `sel` isn't fully contained by any one monitor (e.g. a
/// selection spanning multiple monitors), falls back to whichever monitor
/// overlaps it most. `None` if there are no monitors (an OS-independent
/// pure function; shared by `containing_monitor` and the per-monitor DPI
/// lookup, so both agree on which monitor a selection belongs to).
fn containing_monitor_index(monitors: &[Rect], sel: Rect) -> Option<usize> {
    monitors
        .iter()
        .position(|m| m.x0 <= sel.x0 && m.y0 <= sel.y0 && m.x1 >= sel.x1 && m.y1 >= sel.y1)
        .or_else(|| {
            monitors
                .iter()
                .enumerate()
                .max_by_key(|(_, m)| {
                    let iw = m.x1.min(sel.x1).saturating_sub(m.x0.max(sel.x0));
                    let ih = m.y1.min(sel.y1).saturating_sub(m.y0.max(sel.y0));
                    iw * ih
                })
                .map(|(i, _)| i)
        })
}

/// Returns the monitor rect that actually shows rect `sel` (`monitors` in
/// src coordinates). Falls back to the whole composited canvas (`canvas`)
/// if there are no monitors (an OS-independent pure function).
fn containing_monitor(monitors: &[Rect], sel: Rect, canvas: Rect) -> Rect {
    containing_monitor_index(monitors, sel)
        .map(|i| monitors[i])
        .unwrap_or(canvas)
}

/// One monitor as reported by the windowing system: absolute position and
/// size (both physical pixels) plus its own DPI scale factor.
type MonitorInfo = ((i32, i32), (u32, u32), f64);

/// Pairs each monitor rect (src coordinates; `origin` is where `(0,0)`
/// sits in virtual-desktop coordinates) with a scale factor, by finding
/// the `handles` entry covering that rect's center. Falls back to 1.0 for
/// anything unmatched (an OS-independent pure function).
fn match_monitor_dpis(origin: (i32, i32), monitors: &[Rect], handles: &[MonitorInfo]) -> Vec<f64> {
    monitors
        .iter()
        .map(|m| {
            let cx = origin.0 + m.x0 as i32 + (m.x1 - m.x0) as i32 / 2;
            let cy = origin.1 + m.y0 as i32 + (m.y1 - m.y0) as i32 / 2;
            handles
                .iter()
                .find(|&&((hx, hy), (hw, hh), _)| {
                    cx >= hx && cx < hx + hw as i32 && cy >= hy && cy < hy + hh as i32
                })
                .map(|&(.., scale)| scale)
                .unwrap_or(1.0)
        })
        .collect()
}

impl Frozen {
    /// Captures all monitors and composites them into one virtual-desktop-wide image.
    fn capture(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        use xcap::Monitor;

        let monitors = Monitor::all()?;
        let rects: Vec<(i32, i32, u32, u32)> = monitors
            .iter()
            .map(|m| (m.x(), m.y(), m.width(), m.height()))
            .collect();
        // Scale factors come from winit, not xcap: xcap derives its own
        // from a device context's logical-vs-physical width ratio, which
        // is always 1 in a DPI-aware process like this one (nothing is
        // virtualized for it), whereas winit reports each monitor's real
        // per-monitor DPI.
        let handles: Vec<MonitorInfo> = event_loop
            .available_monitors()
            .map(|m| {
                let p = m.position();
                let s = m.size();
                ((p.x, p.y), (s.width, s.height), m.scale_factor())
            })
            .collect();
        let (min_x, min_y, max_x, max_y) =
            monitors_bounds(&rects).ok_or("モニタが見つかりません")?;
        let width = (max_x - min_x).max(1) as usize;
        let height = (max_y - min_y).max(1) as usize;
        let monitor_rects: Vec<Rect> = rects
            .iter()
            .map(|&(x, y, w, h)| Rect {
                x0: (x - min_x) as usize,
                y0: (y - min_y) as usize,
                x1: (((x - min_x) as i64 + w as i64).clamp(0, width as i64)) as usize,
                y1: (((y - min_y) as i64 + h as i64).clamp(0, height as i64)) as usize,
            })
            .collect();
        let monitor_dpis = match_monitor_dpis((min_x, min_y), &monitor_rects, &handles);

        let mut bright = vec![0u32; width * height];
        let mut dim = vec![0u32; width * height];

        for m in &monitors {
            let image = match m.capture_image() {
                Ok(img) => img,
                Err(e) => {
                    // Warn and skip rather than failing the whole capture for one monitor.
                    eprintln!("モニタ '{}' のキャプチャに失敗（スキップ）: {e}", m.name());
                    continue;
                }
            };
            let (mw, mh) = (image.width() as usize, image.height() as usize);
            let raw = image.as_raw(); // RGBA8
            let (ox, oy) = ((m.x() - min_x) as usize, (m.y() - min_y) as usize);
            for y in 0..mh {
                if oy + y >= height {
                    break;
                }
                let drow = (oy + y) * width;
                let srow = y * mw;
                for x in 0..mw {
                    if ox + x >= width {
                        break;
                    }
                    let i = srow + x;
                    let r = raw[i * 4] as u32;
                    let g = raw[i * 4 + 1] as u32;
                    let b = raw[i * 4 + 2] as u32;
                    let di = drow + ox + x;
                    bright[di] = (r << 16) | (g << 8) | b;
                    dim[di] = ((r / 2) << 16) | ((g / 2) << 8) | (b / 2);
                }
            }
        }

        Ok(Self {
            width,
            height,
            bright,
            dim,
            origin: (min_x, min_y),
            monitors: monitor_rects,
            monitor_dpis,
        })
    }
}

/// Crops rect `r` out of `bright` (a 0x00RRGGBB buffer of width
/// `src_width`) into an RGBA8 `Shot`. An OS-independent pure function
/// (unit-tested).
fn crop_region(bright: &[u32], src_width: usize, r: Rect) -> Shot {
    let (w, h) = (r.width(), r.height());
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in r.y0..r.y1 {
        let row = y * src_width;
        for x in r.x0..r.x1 {
            let px = bright[row + x];
            rgba.push(((px >> 16) & 0xff) as u8);
            rgba.push(((px >> 8) & 0xff) as u8);
            rgba.push((px & 0xff) as u8);
            rgba.push(255);
        }
    }
    Shot {
        width: w as u32,
        height: h as u32,
        rgba,
    }
}

/// The local key bindings used while region-selecting (Selecting mode).
/// Changeable via the settings GUI's Hotkeys tab (Escape is excluded and
/// always fixed). Since an action can have multiple keys, each is kept as
/// a `Vec` (empty = unbound).
struct RegionKeys {
    undo: Vec<LocalKey>,
    redo: Vec<LocalKey>,
    reuse_region: Vec<LocalKey>,
    clear_selection: Vec<LocalKey>,
    save_as: Vec<LocalKey>,
    edit_external: Vec<LocalKey>,
    menu: menu::MenuKeys,
}

/// Converts a list of spec strings into `LocalKey`s, keeping only the
/// ones that parse successfully (unparseable entries are dropped; an
/// empty `Vec` is a valid "unbound" state for this action).
fn parse_local_keys(specs: &[String]) -> Vec<LocalKey> {
    specs
        .iter()
        .filter_map(|s| crate::localkey::parse(s))
        .collect()
}

impl RegionKeys {
    fn from_config(cfg: &crate::store::hotkeys::HotkeyConfig) -> Self {
        Self {
            undo: parse_local_keys(&cfg.hotkey_undo),
            redo: parse_local_keys(&cfg.hotkey_redo),
            reuse_region: parse_local_keys(&cfg.hotkey_reuse_region),
            clear_selection: parse_local_keys(&cfg.hotkey_clear_selection),
            save_as: parse_local_keys(&cfg.hotkey_save_as),
            edit_external: parse_local_keys(&cfg.hotkey_edit_external),
            menu: menu::MenuKeys {
                save: parse_local_keys(&cfg.hotkey_menu_save),
                copy: parse_local_keys(&cfg.hotkey_menu_copy),
                edit: parse_local_keys(&cfg.hotkey_menu_edit),
                upload: parse_local_keys(&cfg.hotkey_menu_upload),
                record: parse_local_keys(&cfg.hotkey_menu_record),
                quit: parse_local_keys(&cfg.hotkey_quit),
            },
        }
    }
}

pub struct Overlay {
    frozen: Frozen,
    /// True once an action is committed or canceled; App watches this to fold the session up.
    finished: bool,
    window: Option<Rc<Window>>,
    context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// The surface's (window's) current size.
    surface_size: (usize, usize),
    /// The real cursor's current position (screen coordinates).
    cursor: PhysicalPosition<f64>,
    /// The src coordinate directly under the cursor. Coincides with the
    /// real cursor position since the magnifier is 1:1 there.
    cursor_src: (f64, f64),
    /// Drag start point (src coordinates). `None` if not dragging.
    drag_start_src: Option<(f64, f64)>,
    dragging: bool,
    /// Magnifier zoom level (1.0 = no zoom).
    zoom: f64,
    /// Whether in cursor-warp mode while zoomed (zoom>1), driving a
    /// slowed-down cursor from raw input.
    fine: bool,
    /// The committed selection rect (src coordinates). `Some` means the menu phase.
    selection: Option<Rect>,
    /// The handle-drag/move operation in progress during the menu phase.
    adjust: Adjust,
    /// The menu's layout (when `selection` is `Some`).
    menu: Option<menu::Menu>,
    /// Index of the hovered / pressed button.
    hovered: Option<usize>,
    pressed: Option<usize>,
    /// Text rendering (`None` if font loading failed).
    text: Option<TextRenderer>,
    /// The chosen result (stays `None` if canceled).
    outcome: Option<Outcome>,
    /// The window list enumerated at capture start (for auto region snapping).
    snapshot: Option<snap::Snapshot>,
    /// The snap-candidate rect directly under the cursor, during the
    /// pre-selection phase (src coordinates).
    snap_rect: Option<Rect>,
    /// Whether Ctrl is held (for hotkey matching).
    ctrl: bool,
    /// Whether Shift is held (for hotkey matching).
    shift: bool,
    /// Whether Alt is held (for hotkey matching).
    alt: bool,
    /// Undo history (last entry = the previous selection state).
    undo_stack: Vec<Option<Rect>>,
    /// Redo history; discarded on a new change.
    redo_stack: Vec<Option<Rect>>,
    /// This session's local key bindings (changeable via the settings GUI's Hotkeys tab).
    keys: RegionKeys,

    // --- Recording (RecordSetup / Recording modes) ---
    mode: Mode,
    /// The region being recorded (adjusted to even dimensions, src coordinates).
    record_region: Option<Rect>,
    /// The border windows around the region (4 edges; excluded from the recording).
    border: Vec<SolidWindow>,
    /// The secondary window for the control bar.
    control_window: Option<Rc<Window>>,
    control_context: Option<softbuffer::Context<Rc<Window>>>,
    control_surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// The control bar's logical (DPI-independent) size, used for layout
    /// and hit-testing — see `control_dpi`.
    control_size: (usize, usize),
    /// The control bar window/surface's actual physical pixel size.
    control_physical_size: (usize, usize),
    /// The control bar window's DPI scale factor (`window.scale_factor()`),
    /// read once at creation (this window is short-lived, so unlike
    /// Settings/Editor it doesn't react to `ScaleFactorChanged`).
    control_dpi: f64,
    /// The button the cursor is over on the control bar.
    control_hover: Option<CtrlBtn>,
    /// Output format (toggle, default MP4).
    record_format: capture::RecordFormat,
    /// Current recording FPS.
    fps: u32,
    /// The presets cycled through each time the FPS button is pressed (read from config).
    fps_presets: Vec<u32>,
    /// Whether to record desktop audio (toggle, default off).
    desktop_audio: bool,
    /// Whether to record the mic (toggle, default off).
    mic: bool,
    /// The recording in progress.
    recorder: Option<capture::Recorder>,
    /// A disposable preview audio session used only to drive the level
    /// meters before Start is pressed. Separate from the actual
    /// recording's session; its output PCM is discarded (drained in
    /// `about_to_wait`). Released once recording begins
    /// (`begin_recording`), when `Recorder` takes over with its own session.
    preview_audio: Option<(capture::AudioSession, std::sync::mpsc::Receiver<Vec<u8>>)>,
    /// The recording's output path.
    record_path: Option<PathBuf>,
    /// Recording start time (for the elapsed-time display).
    record_started: Instant,
    /// If false, ignores Record even if selected (set for a screenshot
    /// session opened in a separate window while already recording, to
    /// prevent double recording).
    allow_record: bool,
}

/// A helper window that just fills itself with a solid color (used for each edge of the region border).
struct SolidWindow {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    color: u32,
}

impl SolidWindow {
    fn redraw(&mut self) {
        if let Ok(mut buf) = self.surface.buffer_mut() {
            buf.fill(self.color);
            // Waits for the vblank before blitting, same ordering and
            // reasoning as `Overlay::draw()`'s own `DwmFlush` call — this
            // window's position/size changes on every cursor move while
            // adjusting the region, so the strip tears without it.
            // SAFETY: a DWM global function call taking no arguments that
            // changes no other state.
            let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
            let _ = buf.present();
        }
    }

    /// Repaints immediately on resize instead of waiting for
    /// `RedrawRequested`, since this decoration-only border window (a few
    /// px wide) never receives a WM_PAINT from Windows and would
    /// otherwise never get drawn.
    fn resize(&mut self, w: u32, h: u32) {
        let _ = self.surface.resize(
            NonZeroU32::new(w.max(1)).unwrap(),
            NonZeroU32::new(h.max(1)).unwrap(),
        );
        self.redraw();
    }
}

impl Overlay {
    fn new(frozen: Frozen) -> Self {
        // Carries over the last recording-setup settings (format, audio toggles).
        let cfg = crate::store::snapshot();
        let record_format = if cfg.record_format == "gif" {
            capture::RecordFormat::Gif
        } else {
            capture::RecordFormat::Mp4
        };
        let keys = RegionKeys::from_config(&crate::store::hotkeys::snapshot());
        Self {
            frozen,
            finished: false,
            window: None,
            context: None,
            surface: None,
            surface_size: (0, 0),
            cursor: PhysicalPosition::new(0.0, 0.0),
            cursor_src: (0.0, 0.0),
            drag_start_src: None,
            dragging: false,
            zoom: 1.0,
            fine: false,
            selection: None,
            adjust: Adjust::Idle,
            menu: None,
            hovered: None,
            pressed: None,
            text: TextRenderer::load(),
            outcome: None,
            snapshot: None,
            snap_rect: None,
            ctrl: false,
            shift: false,
            alt: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            keys,
            mode: Mode::Selecting,
            record_region: None,
            border: Vec::new(),
            control_window: None,
            control_context: None,
            control_surface: None,
            control_size: (0, 0),
            control_physical_size: (0, 0),
            control_dpi: 1.0,
            control_hover: None,
            record_format,
            fps: cfg.record_fps.max(1),
            fps_presets: cfg.record_fps_presets,
            desktop_audio: cfg.record_desktop_audio,
            mic: cfg.record_mic,
            recorder: None,
            preview_audio: None,
            record_path: None,
            record_started: Instant::now(),
            allow_record: true,
        }
    }

    fn img_size(&self) -> (usize, usize) {
        (self.frozen.width, self.frozen.height)
    }

    /// Saves the current recording-setup settings (format, audio toggles)
    /// to the config file. Auto-saved (no GUI action needed) so they carry
    /// over the next time capture mode is entered.
    fn save_record_prefs(&self) {
        let mut cfg = crate::store::snapshot();
        cfg.record_format = match self.record_format {
            capture::RecordFormat::Mp4 => "mp4".into(),
            capture::RecordFormat::Gif => "gif".into(),
        };
        cfg.record_desktop_audio = self.desktop_audio;
        cfg.record_mic = self.mic;
        cfg.record_fps = self.fps;
        crate::store::set_and_save(cfg);
    }

    /// The current view (zoom level + cursor center).
    fn view(&self) -> View {
        View {
            zoom: self.zoom,
            center: self.cursor_src,
        }
    }

    /// Returns the rect normalized to src coordinates from the drag start
    /// point and the cursor.
    ///
    /// `x0..x1` are exclusive bounds; the far side is +1 to include the pixel under the cursor.
    fn current_rect(&self) -> Option<Rect> {
        let start = self.drag_start_src?;
        let (fw, fh) = self.img_size();
        let px = |v: f64, max: usize| (v.round().max(0.0) as usize).min(max - 1);

        let sx = px(start.0, fw);
        let sy = px(start.1, fh);
        let cx = px(self.cursor_src.0, fw);
        let cy = px(self.cursor_src.1, fh);

        Some(Rect {
            x0: sx.min(cx),
            y0: sy.min(cy),
            x1: sx.max(cx) + 1,
            y1: sy.max(cy) + 1,
        })
    }

    /// Distance moved from the drag start point to the current cursor
    /// (src px). Infinite if not dragging.
    fn drag_moved(&self) -> f64 {
        self.drag_start_src.map_or(f64::INFINITY, |s| {
            (self.cursor_src.0 - s.0)
                .abs()
                .max((self.cursor_src.1 - s.1).abs())
        })
    }

    /// Whether the rect currently drawn/committed should be the snap
    /// candidate. True during the pre-selection phase, when either ①
    /// nothing has been pressed yet (hover only), or ② it was pressed but
    /// barely moved (treated as a click, `SNAP_CLICK_TOL`). Judging this
    /// the same way regardless of `dragging` avoids the snap frame
    /// flashing away to a near-zero-size drag rect the instant the button
    /// is pressed. `snap_rect` is kept updated in `device_event` even
    /// while zoomed (`fine`), so no exclusion is needed here — window
    /// auto-selection stays on at all times.
    fn snap_active(&self) -> bool {
        matches!(self.mode, Mode::Selecting)
            && self.selection.is_none()
            && self.snap_rect.is_some()
            && (!self.dragging || self.drag_moved() < SNAP_CLICK_TOL)
    }

    /// Clamps the real cursor position to a pixel index in surface
    /// coordinates (for menu hit-testing).
    fn cursor_px(&self) -> (usize, usize) {
        let (sw, sh) = self.surface_size;
        let x = (self.cursor.x.round().max(0.0) as usize).min(sw.saturating_sub(1));
        let y = (self.cursor.y.round().max(0.0) as usize).min(sh.saturating_sub(1));
        (x, y)
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Rebuilds the menu layout from `selection`. Clamps within the
    /// monitor showing the selection rather than the whole composited
    /// canvas, since with mismatched monitor heights/layouts the latter
    /// could place it somewhere belonging to no monitor.
    fn build_menu(&mut self) {
        let (sw, sh) = self.surface_size;
        let canvas = Rect {
            x0: 0,
            y0: 0,
            x1: sw,
            y1: sh,
        };
        let uploaders_configured = !crate::store::enabled_uploaders().is_empty();
        self.menu = self.selection.map(|sel| {
            let bounds = containing_monitor(&self.frozen.monitors, sel, canvas);
            // Looked up per-monitor (not a single window-wide DPI): this
            // window spans every monitor at once, and a multi-monitor
            // setup can mix DPIs, so the menu has to scale to whichever
            // monitor the selection actually landed on.
            let dpi = containing_monitor_index(&self.frozen.monitors, sel)
                .and_then(|i| self.frozen.monitor_dpis.get(i))
                .copied()
                .unwrap_or(1.0);
            menu::Menu::layout(sel, bounds, &self.keys.menu, uploaders_configured, dpi)
        });
    }

    /// Handle-grab tolerance (src coordinates). Divided by zoom to stay a
    /// constant screen px.
    fn grab_tol(&self) -> f64 {
        HANDLE_GRAB / self.zoom
    }

    /// Returns the handle directly under the cursor (src); only meaningful during the menu phase.
    fn handle_at(&self) -> Option<Handle> {
        let sel = self.selection?;
        hit_handle(sel, self.cursor_src, self.grab_tol())
    }

    /// Whether the cursor (src) is inside the selection rect.
    fn cursor_in_selection(&self) -> bool {
        match self.selection {
            Some(r) => {
                let (x, y) = self.cursor_src;
                x >= r.x0 as f64 && x < r.x1 as f64 && y >= r.y0 as f64 && y < r.y1 as f64
            }
            None => false,
        }
    }

    /// Updates `selection` by applying the in-progress `adjust`.
    fn apply_adjust(&mut self) {
        let Some(sel) = self.selection else {
            return;
        };
        let img = self.img_size();
        let new = match self.adjust {
            Adjust::Resizing(h) => resize_rect(sel, h, self.cursor_src, img),
            Adjust::Moving { anchor, orig } => {
                let delta = (self.cursor_src.0 - anchor.0, self.cursor_src.1 - anchor.1);
                move_rect(orig, delta, img)
            }
            Adjust::Idle => return,
        };
        self.selection = Some(new);
    }

    /// Sets the cursor icon to match handle/inside/normal during the menu
    /// phase (only while not zoomed).
    fn update_adjust_cursor(&self) {
        if self.fine {
            return;
        }
        let Some(w) = self.window.as_ref() else {
            return;
        };
        let icon = if let Some(h) = self.handle_at() {
            handle_cursor(h)
        } else if self.cursor_in_selection() {
            CursorIcon::Move
        } else {
            CursorIcon::Default
        };
        w.set_cursor(icon);
    }

    /// Starts a new drag (redoing the selection).
    ///
    /// Doesn't clear `snap_rect` here: the snap candidate at press time is
    /// still needed on release to decide whether it was a click without
    /// dragging (`SNAP_CLICK_TOL`).
    fn begin_drag(&mut self) {
        self.push_undo();
        self.selection = None;
        self.menu = None;
        self.hovered = None;
        self.pressed = None;
        self.drag_start_src = Some(self.cursor_src);
        self.dragging = true;
        if !self.fine {
            // Coming from the menu phase (redo via an outside click) the
            // real cursor is visible, so hide it again to switch back to
            // the custom crosshair.
            if let Some(w) = self.window.as_ref() {
                w.set_cursor_visible(false);
            }
        }
        self.request_redraw();
    }

    /// Clears the selection, returning to a redrawable (unselected) state (X key; no-op if unselected).
    fn clear_selection(&mut self) {
        self.push_undo();
        self.selection = None;
        self.menu = None;
        self.hovered = None;
        self.pressed = None;
        self.adjust = Adjust::Idle;
        self.dragging = false;
        self.snap_rect = None;
        // Leaving the old drag start point would let current_rect()
        // (used while unconfirmed) rebuild a rect from it, making the
        // top-left corner look already committed.
        self.drag_start_src = None;
        if !self.fine {
            // Coming from the menu phase the real cursor is visible, so
            // hide it again to switch back to the custom crosshair (same
            // handling as begin_drag).
            if let Some(w) = self.window.as_ref() {
                w.set_cursor_visible(false);
            }
        }
        self.request_redraw();
    }

    /// Sets the last-used region as the selection (R key). Replaces the
    /// current selection whether it's already set or mid-drag/adjustment.
    /// Clamped to the current image size so it can't panic if the monitor
    /// layout differs from last time.
    fn use_previous_region(&mut self) {
        let Some(prev) = last_region() else {
            return;
        };
        self.push_undo();
        self.selection = Some(clamp_rect_to_image(prev, self.img_size()));
        self.zoom = 1.0;
        self.dragging = false;
        self.drag_start_src = None;
        self.adjust = Adjust::Idle;
        self.pressed = None;
        self.snap_rect = None;
        self.build_menu();
        self.hovered = None;
        if let Some(w) = self.window.as_ref() {
            w.set_cursor(CursorIcon::Default);
            w.set_cursor_visible(true);
        }
        self.request_redraw();
    }

    /// Selects a whole monitor with number keys 1-9 (1 = first monitor,
    /// etc). No-op if there's no such monitor. Replaces the current
    /// selection whether it's already set or mid-drag (same handling as
    /// `use_previous_region`).
    fn select_monitor(&mut self, idx: usize) {
        let Some(&rect) = self.frozen.monitors.get(idx) else {
            return;
        };
        self.select_rect(rect);
    }

    /// Selects all monitors (the whole composited canvas) with the 0 key.
    fn select_all_monitors(&mut self) {
        let (w, h) = self.img_size();
        self.select_rect(Rect {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        });
    }

    /// Shared by `select_monitor`/`select_all_monitors`: sets the given
    /// rect as the selection and opens the menu (same steps as `use_previous_region`).
    fn select_rect(&mut self, rect: Rect) {
        self.push_undo();
        self.selection = Some(clamp_rect_to_image(rect, self.img_size()));
        self.zoom = 1.0;
        self.dragging = false;
        self.drag_start_src = None;
        self.adjust = Adjust::Idle;
        self.pressed = None;
        self.snap_rect = None;
        self.build_menu();
        self.hovered = None;
        if let Some(w) = self.window.as_ref() {
            w.set_cursor(CursorIcon::Default);
            w.set_cursor_visible(true);
        }
        self.request_redraw();
    }

    /// Called before a selection change to push the current value onto
    /// the undo history (discards the redo history, since this is a new
    /// change). Not called for continuous updates within the same gesture
    /// (e.g. per-pixel movement during a drag) — callers should only call
    /// this at a gesture's start.
    fn push_undo(&mut self) {
        self.undo_stack.push(self.selection);
        self.redo_stack.clear();
    }

    /// Restores a selection state for undo/redo (runs the same follow-up
    /// steps as clearing / reusing the last region).
    fn restore_selection(&mut self, sel: Option<Rect>) {
        self.selection = sel;
        self.menu = None;
        self.hovered = None;
        self.pressed = None;
        self.adjust = Adjust::Idle;
        self.dragging = false;
        self.drag_start_src = None;
        self.snap_rect = None;
        self.zoom = 1.0;
        if self.fine {
            self.exit_fine();
        }
        if sel.is_some() {
            self.build_menu();
        }
        if let Some(w) = self.window.as_ref() {
            w.set_cursor(CursorIcon::Default);
            w.set_cursor_visible(sel.is_some());
        }
        self.request_redraw();
    }

    /// `Ctrl+Z`: reverts to the previous selection state. No-op mid-drag/
    /// resize/move, since the target would be ambiguous (use it after
    /// finishing the gesture).
    fn undo(&mut self) {
        if self.dragging || !matches!(self.adjust, Adjust::Idle) {
            return;
        }
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.selection);
            self.restore_selection(prev);
        }
    }

    /// `Ctrl+Shift+Z`: redoes one undo.
    fn redo(&mut self) {
        if self.dragging || !matches!(self.adjust, Adjust::Idle) {
            return;
        }
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.selection);
            self.restore_selection(next);
        }
    }

    /// Ends the session. `self.finished = true` must always go through
    /// here, restoring the real cursor's visibility first. winit's
    /// Windows implementation backs `set_cursor_visible` with a process-
    /// wide shared flag (`ShowCursor`) rather than a per-window one, so
    /// closing the overlay via Esc etc. while the cursor is hidden
    /// (showing the crosshair or magnifier) used to leave that flag
    /// stuck "hidden," making the cursor disappear even over unrelated
    /// UI like the taskbar's right-click menu.
    fn finish(&mut self) {
        // A safety net for paths that end while still in recording setup
        // (cancel/Quit/error, etc). If already switched to recording,
        // `begin_recording` has already released this, so it isn't
        // stopped twice here (guarded by `Option::take`).
        self.stop_preview_audio();
        if let Some(w) = self.window.as_ref() {
            w.set_cursor_visible(true);
        }
        self.finished = true;
    }

    /// Starts a disposable preview audio session, before Start is
    /// pressed, solely to drive the Desktop/Mic level meters. Separate
    /// from the actual recording's session; its output PCM is discarded
    /// (drained in `about_to_wait`). No-op for gif, which doesn't support audio.
    fn start_preview_audio(&mut self) {
        if self.preview_audio.is_some() || !matches!(self.record_format, capture::RecordFormat::Mp4)
        {
            return;
        }
        let cfg = crate::store::snapshot();
        let (session, _fmt, rx) = capture::AudioSession::start(
            self.desktop_audio,
            self.mic,
            &cfg.record_audio_output_device,
            &cfg.record_audio_input_device,
            cfg.record_audio_sample_rate,
        );
        self.preview_audio = Some((session, rx));
    }

    /// Stops the preview audio session (no-op if there isn't one).
    fn stop_preview_audio(&mut self) {
        if let Some((session, _rx)) = self.preview_audio.take() {
            session.stop();
        }
    }

    /// Commits an action. Record transitions to recording setup, Quit
    /// discards the selection and ends the session, and everything else
    /// (Save/Copy/Edit/Upload) crops the still image and ends the
    /// session, leaving save/copy/editor-launch/upload to the caller (App).
    fn trigger(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::Record => {
                // A screenshot session opened in a separate window while
                // already recording sets `allow_record = false` to avoid
                // double recording, so this is a no-op when pressed.
                if self.allow_record {
                    self.enter_record_setup(event_loop);
                }
                return;
            }
            Action::Quit => {
                self.finish();
                return;
            }
            _ => {}
        }
        if let Some(r) = self.selection {
            save_last_region(r);
            let shot = crop_region(&self.frozen.bright, self.frozen.width, r);
            self.outcome = Some(Outcome::Captured { action, shot });
        }
        self.finish();
    }

    /// Shift+S (default): saves as png to a location chosen via the OS's
    /// "save as" dialog. Hotkey-only, no button. If the dialog is
    /// canceled, stays on the selection screen doing nothing (unlike
    /// Save, this doesn't end the session immediately).
    fn save_as(&mut self) {
        let Some(r) = self.selection else {
            return;
        };
        let shot = crop_region(&self.frozen.bright, self.frozen.width, r);
        let default_name = format!(
            "pashari_{}.png",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        let mut dialog = rfd::FileDialog::new()
            .set_title("名前を付けて保存")
            .add_filter("PNG image", &["png"])
            .set_file_name(&default_name);
        if let Ok(dir) = crate::export::output_dir("png") {
            dialog = dialog.set_directory(dir);
        }
        // This window is full-screen and AlwaysOnTop, so leaving it
        // visible would hide the dialog behind it and make the event
        // loop look frozen while waiting for the dialog. Hide it first.
        if let Some(w) = self.window.as_ref() {
            w.set_visible(false);
        }
        let result = dialog.save_file();
        let Some(path) = result else {
            // Canceled: return to the selection screen.
            if let Some(w) = self.window.as_ref() {
                w.set_visible(true);
            }
            return;
        };
        match crate::export::save_png_to(&shot, &path) {
            Ok(()) => {
                save_last_region(r);
                self.outcome = Some(Outcome::Saved(path));
                self.finish();
            }
            Err(e) => {
                eprintln!("名前を付けて保存に失敗: {e}");
                if let Some(w) = self.window.as_ref() {
                    w.set_visible(true);
                }
            }
        }
    }

    /// Enters recording setup: hides the selection overlay and shows the
    /// region border and control bar (Start). Doesn't start recording yet.
    fn enter_record_setup(&mut self, event_loop: &ActiveEventLoop) {
        let Some(sel) = self.selection else {
            return;
        };
        save_last_region(sel);
        // H.264 requires even width/height, so shrink by 1px if odd.
        let region = Rect {
            x0: sel.x0,
            y0: sel.y0,
            x1: sel.x1 - (sel.width() % 2),
            y1: sel.y1 - (sel.height() % 2),
        };
        if region.width() < 2 || region.height() < 2 {
            eprintln!("録画には最低 2x2 px の領域が必要です");
            self.finish();
            return;
        }

        // Hides the selection overlay so it doesn't show up in the recording.
        if let Some(w) = self.window.as_ref() {
            w.set_visible(false);
        }

        self.record_region = Some(region);
        self.create_border(event_loop, region);
        self.create_control_window(event_loop, region);
        self.mode = Mode::RecordSetup;
        self.start_preview_audio();
    }

    /// Creates the border windows (4 edges) around the region. Excluded from the recording.
    fn create_border(&mut self, event_loop: &ActiveEventLoop, r: Rect) {
        let t = BORDER_STRIP as i32;
        let (ox, oy) = self.frozen.origin;
        // r is in overlay-window-local coordinates. The border is a
        // separate top-level window, so convert to screen absolute
        // coordinates before placing it.
        let (x0, y0) = (r.x0 as i32 + ox, r.y0 as i32 + oy);
        let (x1, y1) = (r.x1 as i32 + ox, r.y1 as i32 + oy);
        let w = r.width() as u32;
        let h = r.height() as u32;
        let tt = BORDER_STRIP as u32;
        // (x, y, w, h). The border sits just outside the region.
        let strips = [
            (x0 - t, y0 - t, w + tt * 2, tt), // top
            (x0 - t, y1, w + tt * 2, tt),     // bottom
            (x0 - t, y0, tt, h),              // left
            (x1, y0, tt, h),                  // right
        ];
        for (x, y, sw, sh) in strips {
            let sw_win = create_solid_window(event_loop, x, y, sw, sh, ACCENT);
            self.border.push(sw_win);
        }
    }

    /// Creates the control bar window and excludes it from the recording.
    fn create_control_window(&mut self, event_loop: &ActiveEventLoop, r: Rect) {
        let (sw, sh) = self.surface_size;
        let canvas = Rect {
            x0: 0,
            y0: 0,
            x1: sw,
            y1: sh,
        };
        // Clamping against the whole composited canvas could place the
        // control bar in a blind spot belonging to no monitor with
        // mismatched monitor heights/layouts, so clamp within the
        // monitor showing the recording region instead.
        let bounds = containing_monitor(&self.frozen.monitors, r, canvas);
        // Below the region, else above, else clamped (still local coordinates here).
        let x =
            r.x0.max(bounds.x0)
                .min(bounds.x1.saturating_sub(CONTROL_W).max(bounds.x0));
        let y = if r.y1 + 12 + CONTROL_H <= bounds.y1 {
            r.y1 + 12
        } else if r.y0 >= bounds.y0 + CONTROL_H + 12 {
            r.y0 - CONTROL_H - 12
        } else {
            bounds.y1.saturating_sub(CONTROL_H).max(bounds.y0)
        };
        // The control bar is a separate top-level window, so convert to screen absolute coordinates.
        let (ox, oy) = self.frozen.origin;

        let attrs = Window::default_attributes()
            .with_title("pashari Recording")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(PhysicalPosition::new(x as i32 + ox, y as i32 + oy))
            .with_inner_size(LogicalSize::new(CONTROL_W as f64, CONTROL_H as f64))
            // Created hidden so the animation-disable flag can be set before it's shown.
            .with_visible(false)
            .with_skip_taskbar(true);
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("コントロールバー生成に失敗"),
        );
        exclude_from_capture(&window);
        disable_window_animations(&window);
        window.set_visible(true);

        let context = softbuffer::Context::new(window.clone()).expect("control context");
        let mut surface =
            softbuffer::Surface::new(&context, window.clone()).expect("control surface");
        let size = window.inner_size();
        let (cw, ch) = (size.width.max(1), size.height.max(1));
        surface
            .resize(NonZeroU32::new(cw).unwrap(), NonZeroU32::new(ch).unwrap())
            .expect("control resize");

        self.control_dpi = window.scale_factor();
        self.control_physical_size = (cw as usize, ch as usize);
        self.control_size = (
            ((cw as f64) / self.control_dpi).round().max(1.0) as usize,
            ((ch as f64) / self.control_dpi).round().max(1.0) as usize,
        );
        window.request_redraw();
        self.control_window = Some(window);
        self.control_context = Some(context);
        self.control_surface = Some(surface);
    }

    /// Actually starts recording when Start is pressed.
    fn begin_recording(&mut self, event_loop: &ActiveEventLoop) {
        let Some(region) = self.record_region else {
            return;
        };
        // The real Recorder gets its own new AudioSession, so release the
        // preview one first (avoids grabbing the mic/desktop audio twice).
        self.stop_preview_audio();
        let is_mp4 = matches!(self.record_format, capture::RecordFormat::Mp4);
        let ext = if is_mp4 { "mp4" } else { "gif" };
        let path = match crate::export::output_path(ext) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("録画パスの用意に失敗: {e}");
                self.finish();
                return;
            }
        };
        // region is in overlay-window-local coordinates. Recording
        // resolves the monitor from screen absolute coordinates, so add
        // the virtual desktop origin before passing it in.
        let (ox, oy) = self.frozen.origin;
        let cfg = crate::store::snapshot();
        let req = capture::RecordRequest {
            x0: region.x0 as i32 + ox,
            y0: region.y0 as i32 + oy,
            x1: region.x1 as i32 + ox,
            y1: region.y1 as i32 + oy,
            path: path.to_string_lossy().into_owned(),
            format: self.record_format,
            // GIF has no audio.
            desktop_audio: is_mp4 && self.desktop_audio,
            mic: is_mp4 && self.mic,
            fps: self.fps,
            show_cursor: cfg.record_show_cursor,
            bitrate_mbps: cfg.record_bitrate_mbps,
            max_width: cfg.record_max_width,
            max_height: cfg.record_max_height,
            show_click_ripple: cfg.record_show_click_ripple,
            click_color_left: cfg.record_click_color_left,
            click_color_right: cfg.record_click_color_right,
            audio_output_device: cfg.record_audio_output_device.clone(),
            audio_input_device: cfg.record_audio_input_device.clone(),
            audio_sample_rate: cfg.record_audio_sample_rate,
            strip_silent_audio: cfg.record_strip_silent_audio,
        };
        match capture::Recorder::start(req) {
            Ok(rec) => self.recorder = Some(rec),
            Err(e) => {
                eprintln!("録画開始に失敗: {e}");
                self.finish();
                return;
            }
        }

        self.record_path = Some(path);
        self.record_started = Instant::now();
        self.mode = Mode::Recording;
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + CONTROL_REDRAW_INTERVAL,
        ));
        if let Some(w) = self.control_window.as_ref() {
            w.request_redraw();
        }
    }

    /// Stops recording, finalizes the mp4, sets the result, and ends the session.
    fn stop_recording(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(rec) = self.recorder.take()
            && let Err(e) = rec.stop()
        {
            eprintln!("録画停止に失敗: {e}");
        }
        if let Some(path) = self.record_path.take() {
            self.outcome = Some(Outcome::Recorded(path));
        }
        self.finish();
    }

    /// Shared handling for Esc / closing: cancels during setup, stops and saves while recording.
    fn cancel_or_stop(&mut self, event_loop: &ActiveEventLoop) {
        match self.mode {
            Mode::Recording => self.stop_recording(event_loop),
            _ => self.finish(),
        }
    }

    /// The control bar's buttons and their rects (mode-dependent). All
    /// the same square size (`ACTION_BTN`), matching the action menu.
    fn control_buttons(&self) -> Vec<(CtrlBtn, Rect)> {
        let ch = self.control_size.1;
        let y0 = ch.saturating_sub(ACTION_BTN) / 2;

        // (button, gap from the previous button). All gaps are 0
        // (packed together) for now; to space out a specific button
        // group later, just change the value here.
        let mut specs: Vec<(CtrlBtn, usize)> = vec![(CtrlBtn::Primary, 0)];
        // Format/FPS toggles only appear during setup (fixed once
        // recording starts, since the encoder settings are locked in).
        if matches!(self.mode, Mode::RecordSetup) {
            specs.push((CtrlBtn::Format, 0));
            specs.push((CtrlBtn::Fps, 0));
        }
        // Audio toggles work during both setup and recording (can be
        // switched on/off mid-recording).
        if matches!(self.mode, Mode::RecordSetup | Mode::Recording) {
            specs.push((CtrlBtn::Desktop, 0));
            specs.push((CtrlBtn::Mic, 0));
        }
        // Quit is always shown, during both setup and recording.
        specs.push((CtrlBtn::Quit, 0));

        let mut v = Vec::with_capacity(specs.len());
        let mut x = CTRL_BTN_X;
        for (i, (btn, gap_before)) in specs.into_iter().enumerate() {
            if i > 0 {
                x += gap_before;
            }
            v.push((
                btn,
                Rect {
                    x0: x,
                    y0,
                    x1: x + ACTION_BTN,
                    y1: y0 + ACTION_BTN,
                },
            ));
            x += ACTION_BTN;
        }
        v
    }

    /// Returns the button under point `(x, y)`.
    fn control_hit(&self, x: usize, y: usize) -> Option<CtrlBtn> {
        self.control_buttons()
            .into_iter()
            .find(|(_, r)| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
            .map(|(b, _)| b)
    }

    /// Draws the control bar (Start + audio toggles during setup, Stop + elapsed time while recording).
    fn draw_control(&mut self) {
        let (sw, sh) = self.control_size;
        if sw == 0 || sh == 0 {
            return;
        }
        let recording = matches!(self.mode, Mode::Recording);
        let secs = self.record_started.elapsed().as_secs();
        let hover = self.control_hover;
        let desktop_on = self.desktop_audio;
        let mic_on = self.mic;
        let is_gif = matches!(self.record_format, capture::RecordFormat::Gif);
        let fps = self.fps;
        // Reads the level meters from Recorder while recording, or the
        // preview session during setup (before Start).
        let (desktop_level, mic_level) = if let Some(rec) = self.recorder.as_ref() {
            rec.levels()
        } else if let Some((session, _)) = self.preview_audio.as_ref() {
            session.levels()
        } else {
            (0.0, 0.0)
        };
        let buttons = self.control_buttons();
        let text = self.text.as_ref();

        let Some(surface) = self.control_surface.as_mut() else {
            return;
        };
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };
        buf.fill(0x001E_1E1E);

        {
            let (pw, ph) = self.control_physical_size;
            let mut canvas = Canvas {
                buf: &mut buf[..],
                w: pw,
                h: ph,
                scale: self.control_dpi,
            };
            for (btn, rect) in &buttons {
                let (base, label, enabled): (u32, String, bool) = match btn {
                    CtrlBtn::Primary if recording => (0x00E0_4040, "Stop".into(), true),
                    CtrlBtn::Primary => (0x0033_A852, "Start".into(), true),
                    CtrlBtn::Format => (
                        0x0044_4444,
                        if is_gif { "GIF".into() } else { "MP4".into() },
                        true,
                    ),
                    CtrlBtn::Fps => (0x0044_4444, format!("{fps} FPS"), true),
                    // GIF can't carry audio, so shown disabled.
                    CtrlBtn::Desktop if is_gif => (0x0033_3333, "Desktop".into(), false),
                    CtrlBtn::Desktop => {
                        let c = if desktop_on { 0x004D_A6FF } else { 0x0044_4444 };
                        (c, "Desktop".into(), true)
                    }
                    CtrlBtn::Mic if is_gif => (0x0033_3333, "Mic".into(), false),
                    CtrlBtn::Mic => {
                        let c = if mic_on { 0x004D_A6FF } else { 0x0044_4444 };
                        (c, "Mic".into(), true)
                    }
                    CtrlBtn::Quit => (0x0044_4444, "Quit".into(), true),
                };
                let bg = if enabled && hover == Some(*btn) {
                    lighten(base)
                } else {
                    base
                };
                if let Some(t) = text {
                    let tcolor = if enabled { 0x00FF_FFFF } else { 0x0088_8888 };
                    crate::ui::draw_icon_button(&mut canvas, *rect, bg, tcolor, &label, tcolor, t);
                } else {
                    canvas.fill(*rect, bg);
                }
                // Overlays a level meter on the inner-right side of the
                // Desktop/Mic buttons, so it's visible at a glance whether
                // audio is actually coming in (skipped when shown
                // disabled for GIF, since there's no audio at all).
                match btn {
                    CtrlBtn::Desktop if enabled => {
                        draw_level_meter(&mut canvas, *rect, desktop_level * METER_GAIN)
                    }
                    CtrlBtn::Mic if enabled => {
                        draw_level_meter(&mut canvas, *rect, mic_level * METER_GAIN)
                    }
                    _ => {}
                }
            }
            // While recording, shows elapsed time to the right of the
            // last button (Mic, since Desktop/Mic stay shown while recording too).
            if recording && let (Some(t), Some((_, last))) = (text, buttons.last()) {
                let elapsed = format!("{:02}:{:02}", secs / 60, secs % 60);
                let ey = t.baseline_for_center(sh as f32 / 2.0, 17.0);
                t.draw(
                    &mut canvas,
                    (last.x1 + 16) as f32,
                    ey,
                    &elapsed,
                    17.0,
                    0x00EA_EAEA,
                );
            }
        }

        let _ = buf.present();
    }

    /// Event handling for `mode != Selecting` (control bar + region border).
    fn recording_family_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // Closing is handled the same regardless of which window it came
        // from. Esc doesn't end the session (error-prone; only the
        // control bar's Quit button does that).
        if let WindowEvent::CloseRequested = &event {
            self.cancel_or_stop(event_loop);
            return;
        }

        if self.control_window.as_ref().is_some_and(|w| w.id() == id) {
            self.control_event(event_loop, event);
        } else if let Some(sw) = self.border.iter_mut().find(|s| s.window.id() == id) {
            match event {
                WindowEvent::RedrawRequested => sw.redraw(),
                WindowEvent::Resized(size) => sw.resize(size.width, size.height),
                _ => {}
            }
        }
    }

    /// Event handling for the control bar (Start / Stop buttons).
    fn control_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::Resized(size) => {
                if let Some(surface) = self.control_surface.as_mut() {
                    let (cw, ch) = (size.width.max(1), size.height.max(1));
                    let _ =
                        surface.resize(NonZeroU32::new(cw).unwrap(), NonZeroU32::new(ch).unwrap());
                    self.control_physical_size = (cw as usize, ch as usize);
                    self.control_size = (
                        ((cw as f64) / self.control_dpi).round().max(1.0) as usize,
                        ((ch as f64) / self.control_dpi).round().max(1.0) as usize,
                    );
                }
                if let Some(w) = self.control_window.as_ref() {
                    w.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left && state == ElementState::Released {
                    match self.control_hover {
                        Some(CtrlBtn::Primary) => match self.mode {
                            Mode::Recording => self.stop_recording(event_loop),
                            _ => self.begin_recording(event_loop),
                        },
                        Some(CtrlBtn::Format) => {
                            self.record_format = match self.record_format {
                                capture::RecordFormat::Mp4 => capture::RecordFormat::Gif,
                                capture::RecordFormat::Gif => capture::RecordFormat::Mp4,
                            };
                            // GIF doesn't support audio, so stop the
                            // preview; it restarts with the current
                            // toggle state if switched back to MP4 (during setup).
                            self.stop_preview_audio();
                            self.start_preview_audio();
                            self.save_record_prefs();
                            if let Some(w) = self.control_window.as_ref() {
                                w.request_redraw();
                            }
                        }
                        Some(CtrlBtn::Fps) => {
                            self.fps = next_fps(self.fps, &self.fps_presets);
                            self.save_record_prefs();
                            if let Some(w) = self.control_window.as_ref() {
                                w.request_redraw();
                            }
                        }
                        // The audio toggles are disabled when GIF is
                        // selected (only flip for mp4). Can be switched
                        // during setup or recording; while recording it
                        // also applies to the actual recording.
                        Some(CtrlBtn::Desktop)
                            if matches!(self.record_format, capture::RecordFormat::Mp4) =>
                        {
                            self.desktop_audio = !self.desktop_audio;
                            if let Some(rec) = self.recorder.as_mut() {
                                rec.set_desktop_audio(self.desktop_audio);
                            }
                            if let Some((session, _)) = self.preview_audio.as_mut() {
                                let cfg = crate::store::snapshot();
                                session.set_desktop(
                                    self.desktop_audio,
                                    &cfg.record_audio_output_device,
                                );
                            }
                            self.save_record_prefs();
                            if let Some(w) = self.control_window.as_ref() {
                                w.request_redraw();
                            }
                        }
                        Some(CtrlBtn::Mic)
                            if matches!(self.record_format, capture::RecordFormat::Mp4) =>
                        {
                            self.mic = !self.mic;
                            if let Some(rec) = self.recorder.as_mut() {
                                rec.set_mic(self.mic);
                            }
                            if let Some((session, _)) = self.preview_audio.as_mut() {
                                let cfg = crate::store::snapshot();
                                session.set_mic(self.mic, &cfg.record_audio_input_device);
                            }
                            self.save_record_prefs();
                            if let Some(w) = self.control_window.as_ref() {
                                w.request_redraw();
                            }
                        }
                        Some(CtrlBtn::Quit) => self.cancel_or_stop(event_loop),
                        _ => {}
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let x = position.x / self.control_dpi;
                let y = position.y / self.control_dpi;
                let hit = self.control_hit(x as usize, y as usize);
                if hit != self.control_hover {
                    self.control_hover = hit;
                    if let Some(w) = self.control_window.as_ref() {
                        w.request_redraw();
                    }
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if self.control_hover.is_some() {
                    self.control_hover = None;
                    if let Some(w) = self.control_window.as_ref() {
                        w.request_redraw();
                    }
                }
            }

            WindowEvent::RedrawRequested => self.draw_control(),

            _ => {}
        }
    }

    /// Warps the real cursor to the window center (to keep receiving raw
    /// input while zoomed). The real cursor stays hidden, independent of
    /// the virtual cursor (`cursor_src`) used for the selection.
    fn warp_to_center(&mut self) {
        if let Some(w) = self.window.as_ref() {
            let (sw, sh) = self.surface_size;
            let pos = PhysicalPosition::new(sw as f64 / 2.0, sh as f64 / 2.0);
            let _ = w.set_cursor_position(pos);
            self.cursor = pos;
        }
    }

    /// Exits fine mode: moves the real cursor back to the virtual
    /// cursor's position. Kept hidden before selecting (so the custom
    /// crosshair keeps showing), shown otherwise (menu phase etc., where
    /// the caller sets the appropriate cursor).
    fn exit_fine(&mut self) {
        self.fine = false;
        let pos = PhysicalPosition::new(self.cursor_src.0, self.cursor_src.1);
        if let Some(w) = self.window.as_ref() {
            let _ = w.set_cursor_position(pos);
            let selecting_pre_drag =
                self.selection.is_none() && matches!(self.mode, Mode::Selecting);
            w.set_cursor_visible(!selecting_pre_drag);
        }
        self.cursor = pos;
    }

    /// Applies continuous wheel zoom (`amount` = notch count; only called
    /// during the selection phase).
    ///
    /// The magnification center is always the cursor position, so only
    /// the zoom level changes. At zoom>1, hides the real cursor and
    /// drives the virtual cursor from raw input (`device_event`) at
    /// 1/zoom speed (reduced sensitivity).
    fn change_zoom(&mut self, amount: f64) {
        if amount == 0.0 {
            return;
        }
        let new = (self.zoom * ZOOM_FACTOR.powf(amount)).clamp(ZOOM_MIN, ZOOM_MAX);
        if (new - self.zoom).abs() < 1e-6 {
            return;
        }
        let was_fine = self.fine;
        self.zoom = new;
        self.fine = new > 1.0;
        if self.fine && !was_fine {
            if let Some(w) = self.window.as_ref() {
                w.set_cursor_visible(false);
            }
            self.warp_to_center();
        } else if !self.fine && was_fine {
            self.exit_fine();
        }
        // The menu only shows at 1x (hidden while zoomed to focus on fine adjustment, restored after).
        if self.selection.is_some() {
            if self.zoom == 1.0 {
                self.build_menu();
            } else {
                self.menu = None;
                self.hovered = None;
            }
        }
        self.request_redraw();
    }

    fn draw(&mut self) {
        let (sw, sh) = self.surface_size;
        if sw == 0 || sh == 0 {
            return;
        }
        let (fw, fh) = self.img_size();
        // The committed rect while the menu is showing; otherwise the
        // snap candidate while snapping is active (hovering, or just
        // pressed and still counted as a click), else the in-progress
        // drag rect (src coordinates).
        let snapping = self.snap_active();
        let rect_src = self.selection.or(if snapping {
            self.snap_rect
        } else {
            self.current_rect()
        });
        // Whether committed (menu phase), for deciding whether to show the custom crosshair.
        let confirmed = self.selection.is_some();
        let menu_geo = self.menu.clone();
        let hovered = self.hovered;
        let pressed = self.pressed;
        let view = self.view();
        let fine = self.fine;
        let selecting = matches!(self.mode, Mode::Selecting);

        let text = self.text.as_ref();
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };

        // --- Background (dimmed freeze image + cutout inside the selection) ---
        if view.zoom == 1.0 {
            // 1x: a fast path that can copy row by row.
            let rows = sh.min(fh);
            buf[..rows * sw].copy_from_slice(&self.frozen.dim[..rows * sw]);
            for p in buf[rows * sw..].iter_mut() {
                *p = 0;
            }
            if let Some(r) = rect_src {
                for y in r.y0..r.y1.min(sh) {
                    let row = y * sw;
                    buf[row + r.x0..row + r.x1]
                        .copy_from_slice(&self.frozen.bright[row + r.x0..row + r.x1]);
                }
            }
        } else {
            // Zoomed: nearest-neighbor sampling centered on the cursor.
            // Each row is laid down dimmed first, then the inside of
            // selected rows is overwritten at full brightness.
            let col_src = axis_map(&view, Axis::X, sw, fw);
            let row_src = axis_map(&view, Axis::Y, sh, fh);
            let bright_cols = rect_src.map(|r| bright_col_range(&view, r, sw));

            // This composites every screen pixel (not just the magnified
            // viewport), and the canvas spans the whole virtual desktop —
            // easily several million pixels on a multi-monitor/4K setup,
            // redone on every redraw while dragging or zooming. Scanlines
            // are independent (each only touches its own row_src entry
            // plus the shared, read-only dim/bright buffers), so split
            // them across CPU cores the same way editor::annotation's
            // paint_image does with rayon's persistent worker pool
            // (spawning fresh OS threads every frame would erase the gains).
            const PAR_PIXEL_THRESHOLD: usize = 200_000;
            let n_threads = rayon::current_num_threads().min(8);
            let parallel = n_threads > 1 && sw * sh >= PAR_PIXEL_THRESHOLD;
            let rows_per_chunk = sh.div_ceil(n_threads.max(1));
            let ctx = ZoomRowCtx {
                col_src: &col_src,
                dim: &self.frozen.dim,
                bright: &self.frozen.bright,
                fw,
                rect_src,
                bright_cols,
            };

            if parallel {
                buf.par_chunks_mut(rows_per_chunk * sw)
                    .enumerate()
                    .for_each(|(chunk_idx, chunk)| {
                        let y0 = chunk_idx * rows_per_chunk;
                        for (i, row) in chunk.chunks_mut(sw).enumerate() {
                            compose_zoomed_row(row, row_src[y0 + i], &ctx);
                        }
                    });
            } else {
                for (sy, row) in buf.chunks_mut(sw).enumerate() {
                    compose_zoomed_row(row, row_src[sy], &ctx);
                }
            }
        }

        // --- Foreground (border, menu) ---
        {
            let mut canvas = Canvas {
                buf: &mut buf[..],
                w: sw,
                h: sh,
                scale: 1.0,
            };

            if let Some(r) = rect_src {
                let (x0, y0) = view.src_to_screen(r.x0 as f64, r.y0 as f64);
                let (x1, y1) = view.src_to_screen(r.x1 as f64, r.y1 as f64);
                if let Some(sr) = screen_rect(x0, y0, x1, y1, sw, sh) {
                    draw_border(&mut canvas, sr);
                }
            }

            // Draws the custom crosshair whenever the real cursor is
            // hidden (pre-drag before selecting / while zoomed). The real
            // cursor stays hidden while zoomed even during a handle
            // adjustment, so the same crosshair shows there too, and also
            // while showing the snap-candidate border instead of the real cursor.
            if fine || (selecting && !confirmed) {
                draw_selection_crosshair(&mut canvas, view.center.0, view.center.1);
            }

            if let Some(m) = menu_geo.as_ref() {
                menu::draw(&mut canvas, m, hovered, pressed, text);
            }
        }

        // softbuffer's Windows backend blits with GDI's BitBlt, which has
        // no vsync control at all — it copies wherever the scanout
        // happens to be. Waiting here, *before* the blit, starts it right
        // after a vblank so it finishes ahead of the beam. Flushing after
        // presenting instead would only pace the loop: the blit would
        // still land however long the repaint took past the vblank, which
        // is exactly where the tear line shows up.
        // DWM is always on from Windows 8 onward, so this can't fail.
        // SAFETY: a DWM global function call taking no arguments that
        // changes no other state.
        let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
        let _ = buf.present();
    }
}

enum Axis {
    X,
    Y,
}

/// Per-frame inputs to [`compose_zoomed_row`] that stay the same for every
/// row (bundled to keep the function's argument count down).
struct ZoomRowCtx<'a> {
    col_src: &'a [usize],
    dim: &'a [u32],
    bright: &'a [u32],
    fw: usize,
    rect_src: Option<Rect>,
    /// Screen-column range covering `rect_src`'s X extent, see [`bright_col_range`].
    bright_cols: Option<(usize, usize)>,
}

/// Composes one screen row of the zoomed view: dim-fill, then overwrite the
/// part inside `ctx.rect_src` (if any) at full brightness. `ctx.bright_cols`
/// lets this only touch the pixels that actually change instead of
/// rescanning the whole row for them. Shared by the serial and
/// rayon-parallel branches in [`Overlay::draw`].
fn compose_zoomed_row(row: &mut [u32], row_srcy: usize, ctx: &ZoomRowCtx) {
    if row_srcy == usize::MAX {
        row.fill(0);
        return;
    }
    let base = row_srcy * ctx.fw;
    for (sx, &srcx) in ctx.col_src.iter().enumerate() {
        row[sx] = if srcx == usize::MAX {
            0
        } else {
            ctx.dim[base + srcx]
        };
    }
    if let Some(r) = ctx.rect_src
        && row_srcy >= r.y0
        && row_srcy < r.y1
        && let Some((bx0, bx1)) = ctx.bright_cols
    {
        for (sx, &srcx) in ctx.col_src[bx0..bx1].iter().enumerate() {
            row[bx0 + sx] = ctx.bright[base + srcx];
        }
    }
}

/// Computes the screen-column range covering src rect `r`'s `x0..x1` extent
/// under `view`. `View::screen_to_src` is monotonic in the screen
/// coordinate (zoom is always > 0), so the columns whose source falls
/// inside `r` form one contiguous range; this inverts the mapping directly
/// via `src_to_screen` instead of rescanning `col_src` to find it.
fn bright_col_range(view: &View, r: Rect, sw: usize) -> (usize, usize) {
    let x0 = view.src_to_screen(r.x0 as f64, view.center.1).0;
    let x1 = view.src_to_screen(r.x1 as f64, view.center.1).0;
    let lo = x0.ceil().clamp(0.0, sw as f64) as usize;
    let hi = x1.ceil().clamp(0.0, sw as f64) as usize;
    (lo, hi.max(lo))
}

/// Computes the src index for each screen-axis pixel (`usize::MAX` if out
/// of range). The mapping matches [`View::screen_to_src`].
fn axis_map(view: &View, axis: Axis, screen_len: usize, img_len: usize) -> Vec<usize> {
    (0..screen_len)
        .map(|s| {
            let src = match axis {
                Axis::X => view.screen_to_src(s as f64, view.center.1).0,
                Axis::Y => view.screen_to_src(view.center.0, s as f64).1,
            };
            if src >= 0.0 && (src as usize) < img_len {
                src as usize
            } else {
                usize::MAX
            }
        })
        .collect()
}

/// Clamps an f64 screen rect within the screen into a `Rect` (`None` if the area is <= 0).
fn screen_rect(x0: f64, y0: f64, x1: f64, y1: f64, sw: usize, sh: usize) -> Option<Rect> {
    let cx0 = x0.max(0.0).min(sw as f64);
    let cy0 = y0.max(0.0).min(sh as f64);
    let cx1 = x1.max(0.0).min(sw as f64);
    let cy1 = y1.max(0.0).min(sh as f64);
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    Some(Rect {
        x0: cx0 as usize,
        y0: cy0 as usize,
        x1: cx1 as usize,
        y1: cy1 as usize,
    })
}

/// Draws a `BORDER`-thick frame around the selection rect.
fn draw_border(canvas: &mut Canvas, r: Rect) {
    for t in 0..BORDER {
        for x in r.x0..r.x1 {
            if r.y0 + t < r.y1 {
                canvas.set(x, r.y0 + t, ACCENT);
            }
            if r.y1 > t && r.y1 - 1 - t >= r.y0 {
                canvas.set(x, r.y1 - 1 - t, ACCENT);
            }
        }
        for y in r.y0..r.y1 {
            if r.x0 + t < r.x1 {
                canvas.set(r.x0 + t, y, ACCENT);
            }
            if r.x1 > t && r.x1 - 1 - t >= r.x0 {
                canvas.set(r.x1 - 1 - t, y, ACCENT);
            }
        }
    }
}

/// Returns which of rect `r`'s edges point `p` (src coordinates) falls
/// within grab tolerance `tol` (src coordinates) of. Rather than 8 fixed
/// grab points, tests against the whole edge (widened into a corner at
/// each end) so grabbing anywhere along an edge resizes just that axis.
/// Near two edges at once -> a corner (both axes); near one edge only -> a single-axis handle.
fn hit_handle(r: Rect, p: (f64, f64), tol: f64) -> Option<Handle> {
    let (x0, y0, x1, y1) = (r.x0 as f64, r.y0 as f64, r.x1 as f64, r.y1 as f64);
    let near_left = (p.0 - x0).abs() <= tol && p.1 >= y0 - tol && p.1 <= y1 + tol;
    let near_right = (p.0 - x1).abs() <= tol && p.1 >= y0 - tol && p.1 <= y1 + tol;
    let near_top = (p.1 - y0).abs() <= tol && p.0 >= x0 - tol && p.0 <= x1 + tol;
    let near_bottom = (p.1 - y1).abs() <= tol && p.0 >= x0 - tol && p.0 <= x1 + tol;
    if near_left && near_top {
        Some(Handle::TopLeft)
    } else if near_right && near_top {
        Some(Handle::TopRight)
    } else if near_right && near_bottom {
        Some(Handle::BottomRight)
    } else if near_left && near_bottom {
        Some(Handle::BottomLeft)
    } else if near_top {
        Some(Handle::Top)
    } else if near_bottom {
        Some(Handle::Bottom)
    } else if near_left {
        Some(Handle::Left)
    } else if near_right {
        Some(Handle::Right)
    } else {
        None
    }
}

/// The new rect after dragging handle `h` to `cursor` (src coordinates).
/// Moves only the corresponding edge, keeping a minimum 1px without
/// flipping. `img` is the image size (for range clamping).
fn resize_rect(r: Rect, h: Handle, cursor: (f64, f64), img: (usize, usize)) -> Rect {
    let (fw, fh) = img;
    let cx = (cursor.0.round().max(0.0) as usize).min(fw - 1);
    let cy = (cursor.1.round().max(0.0) as usize).min(fh - 1);
    let mut x0 = r.x0;
    let mut y0 = r.y0;
    let mut x1 = r.x1;
    let mut y1 = r.y1;
    match h {
        Handle::TopLeft | Handle::Left | Handle::BottomLeft => x0 = cx.min(r.x1 - 1),
        Handle::TopRight | Handle::Right | Handle::BottomRight => x1 = (cx + 1).max(r.x0 + 1),
        Handle::Top | Handle::Bottom => {}
    }
    match h {
        Handle::TopLeft | Handle::Top | Handle::TopRight => y0 = cy.min(r.y1 - 1),
        Handle::BottomLeft | Handle::Bottom | Handle::BottomRight => y1 = (cy + 1).max(r.y0 + 1),
        Handle::Left | Handle::Right => {}
    }
    Rect { x0, y0, x1, y1 }
}

/// Translates `orig` by `delta` (src coordinates) keeping its size, clamped within the image.
fn move_rect(orig: Rect, delta: (f64, f64), img: (usize, usize)) -> Rect {
    let (fw, fh) = img;
    let w = orig.width();
    let h = orig.height();
    let nx0 = (orig.x0 as i64 + delta.0.round() as i64).clamp(0, (fw - w) as i64) as usize;
    let ny0 = (orig.y0 as i64 + delta.1.round() as i64).clamp(0, (fh - h) as i64) as usize;
    Rect {
        x0: nx0,
        y0: ny0,
        x1: nx0 + w,
        y1: ny0 + h,
    }
}

/// Clamps a rect within the image size (unlike `move_rect`, doesn't
/// preserve size — this safely fits the last-used region even if the
/// monitor layout changed and it no longer fits as-is).
fn clamp_rect_to_image(r: Rect, img: (usize, usize)) -> Rect {
    let (fw, fh) = img;
    let x0 = r.x0.min(fw.saturating_sub(1));
    let y0 = r.y0.min(fh.saturating_sub(1));
    let x1 = r.x1.min(fw).max(x0 + 1);
    let y1 = r.y1.min(fh).max(y0 + 1);
    Rect { x0, y0, x1, y1 }
}

/// The resize cursor icon matching a handle.
fn handle_cursor(h: Handle) -> CursorIcon {
    match h {
        Handle::TopLeft | Handle::BottomRight => CursorIcon::NwseResize,
        Handle::TopRight | Handle::BottomLeft => CursorIcon::NeswResize,
        Handle::Top | Handle::Bottom => CursorIcon::NsResize,
        Handle::Left | Handle::Right => CursorIcon::EwResize,
    }
}

/// The custom cursor shown while region-selecting/zoomed. The OS's
/// standard crosshair is thin and gets lost against the background, so
/// this draws a cross-shaped guide stretching to the screen edges. Style
/// varies with distance `d` (px from the center):
/// - Within radius `OUTER` (=100px) of the center, a solid accent color;
///   1px within `INNER` (=50px), 3px between 50-100px, to make the
///   cursor position stand out.
/// - Beyond `OUTER`, a 1px black-and-white stripe pattern, visible
///   against any background and useful for aligning with other UI.
///
/// Drawn the same way while zoomed too (since the magnifier fixes the
/// pixel directly under the cursor, `view.center` can be used as-is).
const SEL_CURSOR_GAP: i64 = 4;
const SEL_CURSOR_INNER: i64 = 50;
const SEL_CURSOR_OUTER: i64 = 100;
const SEL_CURSOR_STRIPE: i64 = 6;

fn draw_selection_crosshair(canvas: &mut Canvas, x: f64, y: f64) {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    // set_i ignores off-screen points, so extending to match the longer
    // side reaches every screen edge in all four directions.
    let max_d = (canvas.w as i64).max(canvas.h as i64);
    for d in SEL_CURSOR_GAP..=max_d {
        // (half-width perpendicular to the radial direction, color).
        let (half, color) = if d <= SEL_CURSOR_INNER {
            (0, ACCENT)
        } else if d <= SEL_CURSOR_OUTER {
            (1, ACCENT)
        } else if (d / SEL_CURSOR_STRIPE) % 2 == 0 {
            (0, 0x00FF_FFFF)
        } else {
            (0, 0x0000_0000)
        };
        for t in -half..=half {
            canvas.set_i(cx - d, cy + t, color);
            canvas.set_i(cx + d, cy + t, color);
            canvas.set_i(cx + t, cy - d, color);
            canvas.set_i(cx + t, cy + d, color);
        }
    }
}

/// Excludes a window from screen capture (Graphics Capture) by setting
/// `WDA_EXCLUDEFROMCAPTURE`, so the control bar doesn't show up in the recording.
fn exclude_from_capture(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };

    let Ok(handle) = window.window_handle() else {
        return;
    };
    if let RawWindowHandle::Win32(h) = handle.as_raw() {
        // SAFETY: a direct Win32 API call that just sets the
        // capture-exclusion flag on a valid HWND created by winit, no
        // other state is touched.
        unsafe {
            let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
    }
}

/// Forces a window to the foreground. Even winit's `focus_window()`
/// (internally faking an Alt key via SendInput + `SetForegroundWindow`)
/// can occasionally fail due to Windows' foreground lock. Temporarily
/// attaching to the current foreground window's thread input queue
/// (`AttachThreadInput`) before calling `SetForegroundWindow` reliably
/// bypasses that restriction.
fn force_foreground(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    // SAFETY: a direct Win32 API call, using only a valid HWND created by
    // winit and a valid foreground window/thread ID obtained from the OS.
    // Every attach is paired with a detach.
    //
    // Extra foreground-forcing via BringWindowToTop/SetActiveWindow/
    // SetFocus/SetWindowPos(HWND_TOPMOST) was tried, but it didn't fix
    // shell surfaces like the Start menu staying on top, and instead
    // caused video in some browsers to pause the instant capture mode
    // started (grabbing too much focus made them look backgrounded), so
    // it was reverted. Kept to the more conservative SetForegroundWindow-only approach.
    unsafe {
        let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return;
        }
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let cur_thread = GetCurrentThreadId();
        let attached = fg_thread != 0
            && fg_thread != cur_thread
            && AttachThreadInput(cur_thread, fg_thread, true).as_bool();

        let _ = SetForegroundWindow(hwnd);

        if attached {
            let _ = AttachThreadInput(cur_thread, fg_thread, false);
        }
    }
}

/// Disables the fade-in/out animations DWM adds when showing/hiding a
/// window, so the overlay switches instantly on capture start/end.
fn disable_window_animations(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{BOOL, HWND};
    use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    if let RawWindowHandle::Win32(h) = handle.as_raw() {
        // SAFETY: a direct Win32 API call that just sets the
        // animation-disable flag on a valid HWND created by winit, no
        // other state is touched.
        unsafe {
            let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &enable as *const BOOL as *const core::ffi::c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
        }
    }
}

/// Creates a solid-color helper window (one edge of the region border). Excluded from the recording.
fn create_solid_window(
    event_loop: &ActiveEventLoop,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) -> SolidWindow {
    let attrs = Window::default_attributes()
        .with_title("pashari Border")
        .with_decorations(false)
        .with_resizable(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_position(PhysicalPosition::new(x, y))
        .with_inner_size(PhysicalSize::new(w.max(1), h.max(1)))
        // Created hidden so the animation-disable flag can be set before it's shown.
        .with_visible(false)
        .with_skip_taskbar(true);
    let window = Rc::new(
        event_loop
            .create_window(attrs)
            .expect("枠ウィンドウ生成に失敗"),
    );
    exclude_from_capture(&window);
    disable_window_animations(&window);
    window.set_visible(true);

    let context = softbuffer::Context::new(window.clone()).expect("border context");
    let mut surface = softbuffer::Surface::new(&context, window.clone()).expect("border surface");
    let size = window.inner_size();
    let _ = surface.resize(
        NonZeroU32::new(size.width.max(1)).unwrap(),
        NonZeroU32::new(size.height.max(1)).unwrap(),
    );
    let mut solid = SolidWindow {
        window,
        _context: context,
        surface,
        color,
    };
    // Not drawn via `request_redraw()` (this border window never gets a
    // WM_PAINT, so `RedrawRequested` never fires). Painted directly at creation instead.
    solid.redraw();
    solid
}

/// Lightens a color slightly (for hover states).
fn lighten(c: u32) -> u32 {
    let ch = |shift: u32| (((c >> shift) & 0xff) + 40).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Draws a vertical level meter (0.0..=1.0) inside the right edge of a
/// button rect (a dark background track, filled from the bottom up to the level).
fn draw_level_meter(canvas: &mut Canvas, rect: Rect, level: f32) {
    const METER_W: usize = 4;
    const METER_MARGIN: usize = 4;
    let level = level.clamp(0.0, 1.0);
    let x1 = rect.x1.saturating_sub(METER_MARGIN);
    let x0 = x1.saturating_sub(METER_W);
    let y0 = rect.y0 + METER_MARGIN;
    let y1 = rect.y1.saturating_sub(METER_MARGIN);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    canvas.fill(Rect { x0, y0, x1, y1 }, 0x0022_2222);
    let h = ((y1 - y0) as f32 * level).round() as usize;
    if h > 0 {
        let fy0 = y1.saturating_sub(h);
        canvas.fill(
            Rect {
                x0,
                y0: fy0,
                x1,
                y1,
            },
            0x0033_A852,
        );
    }
}

/// Returns the value after `current` in `presets` (wraps to the first
/// after the last). Returns the first preset if `current` isn't among
/// them; returns `current` unchanged if `presets` is empty.
fn next_fps(current: u32, presets: &[u32]) -> u32 {
    if presets.is_empty() {
        return current;
    }
    match presets.iter().position(|&f| f == current) {
        Some(i) => presets[(i + 1) % presets.len()],
        None => presets[0],
    }
}

impl Overlay {
    /// Captures the freeze image and starts an overlay session (called by
    /// App on a hotkey). `self.snapshot` starts `None` — App follows up
    /// with `spawn_snapshot_capture` once the window exists, since that
    /// enumeration can't safely run synchronously here (see its doc).
    pub(crate) fn start(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        let frozen = Frozen::capture(event_loop)?;
        let mut overlay = Overlay::new(frozen);
        overlay.create_overlay_window(event_loop);
        Ok(overlay)
    }

    /// Enumerates top-level windows for auto region snapping on a
    /// background thread, applying the result via `set_snapshot` once
    /// ready (dragging a selection works fine without it in the
    /// meantime — this only affects hover-to-snap). Has to run off the
    /// main thread: `snap::Snapshot::capture` queries every visible
    /// top-level window via Win32/DWM calls, and if any one of them
    /// belongs to a process that's slow to respond (a hung or heavily
    /// loaded app — not unusual to have at least one open), a single
    /// query can block for a long time. Since this whole app is
    /// single-threaded/event-loop-driven, doing that synchronously while
    /// starting a capture session would freeze the entire app, not just
    /// the snap feature — including the overlay window itself, which
    /// wouldn't even get to paint until the call returns.
    pub(crate) fn spawn_snapshot_capture(&self, proxy: EventLoopProxy<UserEvent>) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let hwnd = match handle.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => return,
        };
        let origin = self.frozen.origin;
        let size = (self.frozen.width, self.frozen.height);
        std::thread::spawn(move || {
            let snapshot = snap::Snapshot::capture(origin, size, Some(hwnd));
            let _ = proxy.send_event(UserEvent::SnapshotReady(hwnd, snapshot));
        });
    }

    /// Applies a snapshot captured by `spawn_snapshot_capture`.
    pub(crate) fn set_snapshot(&mut self, snapshot: snap::Snapshot) {
        self.snapshot = Some(snapshot);
    }

    /// Whether `hwnd` (from a `UserEvent::SnapshotReady`) is this
    /// overlay's own main window — used to route the result to the right
    /// session when more than one can be active (`App`'s `session`/`shot_session`).
    pub(crate) fn owns_hwnd(&self, hwnd: isize) -> bool {
        let Some(window) = self.window.as_ref() else {
            return false;
        };
        let Ok(handle) = window.window_handle() else {
            return false;
        };
        matches!(handle.as_raw(), raw_window_handle::RawWindowHandle::Win32(h) if h.hwnd.get() == hwnd)
    }

    /// Whether an action has been committed / canceled.
    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// Takes the result (`None` if canceled).
    pub(crate) fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }

    /// Whether in recording setup (`RecordSetup`) or recording
    /// (`Recording`) — i.e. the selection overlay itself is hidden and
    /// only the border + control bar are showing. A screenshot session
    /// can be started in a separate window during this time (used by
    /// App's checks). Doesn't apply while still region-selecting
    /// (`Selecting`), which stays blocked as before.
    pub(crate) fn in_record_flow(&self) -> bool {
        !matches!(self.mode, Mode::Selecting)
    }

    /// Sets whether Record is enabled for this session. Set to false for
    /// a screenshot session opened in a separate window while already
    /// recording, to prevent double recording.
    pub(crate) fn set_allow_record(&mut self, allow: bool) {
        self.allow_record = allow;
    }

    /// Whether `id` matches any window owned by this overlay (main /
    /// border / control bar). Used by App, where multiple sessions can
    /// coexist, to decide which session an incoming `WindowEvent` belongs to.
    pub(crate) fn owns_window(&self, id: WindowId) -> bool {
        self.window.as_ref().is_some_and(|w| w.id() == id)
            || self.control_window.as_ref().is_some_and(|w| w.id() == id)
            || self.border.iter().any(|s| s.window.id() == id)
    }

    fn create_overlay_window(&mut self, event_loop: &ActiveEventLoop) {
        let (fw, fh) = self.img_size();
        let (ox, oy) = self.frozen.origin;
        let attrs = Window::default_attributes()
            .with_title("pashari")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            // Positioned at the virtual desktop origin (can be negative
            // depending on monitor layout) to cover every monitor.
            .with_position(PhysicalPosition::new(ox, oy))
            .with_inner_size(PhysicalSize::new(fw as u32, fh as u32))
            // Created hidden so the animation-disable flag can be set
            // before it's shown (DWM decides its show animation at first
            // show time, so disabling it afterward would be too late).
            .with_visible(false)
            .with_skip_taskbar(true);

        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("ウィンドウ生成に失敗"),
        );
        // If this overlay is opened in a separate window while already
        // recording, exclude it from the recording's screen capture so
        // this window itself (crosshair, freeze image, menu) doesn't show
        // up (same handling as border/control_window). Harmless in the
        // normal flow, which always hides it before recording starts (`enter_record_setup`).
        exclude_from_capture(&window);
        disable_window_animations(&window);
        window.set_visible(true);
        // The OS's standard crosshair is thin and hard to see, so hide it
        // and draw a large striped custom crosshair instead (see the
        // `draw_selection_crosshair` call in `draw`).
        window.set_cursor_visible(false);
        window.focus_window();
        // Right after a hotkey launch, Windows' foreground lock can make
        // focus_window() alone fail to bring the window forward, in which
        // case key input never arrives and Esc stops working. Try a more
        // reliable method as a follow-up.
        force_foreground(&window);

        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let mut surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");

        let size = window.inner_size();
        let (sw, sh) = (size.width.max(1), size.height.max(1));
        surface
            .resize(NonZeroU32::new(sw).unwrap(), NonZeroU32::new(sh).unwrap())
            .expect("surface resize");

        self.surface_size = (sw as usize, sh as usize);
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
    }

    pub(crate) fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _id: DeviceId,
        event: DeviceEvent,
    ) {
        if !self.fine {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event {
            let (fw, fh) = self.img_size();
            // Scales the raw delta down by 1/zoom to move the virtual
            // cursor (reduced sensitivity). The cursor slowly roams the
            // whole screen ([0,img)) with the magnifier following it.
            self.cursor_src.0 =
                (self.cursor_src.0 + delta.0 / self.zoom).clamp(0.0, (fw - 1) as f64);
            self.cursor_src.1 =
                (self.cursor_src.1 + delta.1 / self.zoom).clamp(0.0, (fh - 1) as f64);
            // Handle/move drags while zoomed are also driven by the virtual cursor (menu hidden).
            if !matches!(self.adjust, Adjust::Idle) {
                self.apply_adjust();
            } else if !self.dragging && self.selection.is_none() {
                // While hovering before selecting, updates the window
                // auto-select candidate under the cursor the same way
                // `CursorMoved` does at 1x — this used to be missing
                // here, breaking auto-select while zoomed.
                self.snap_rect = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.element_at(self.cursor_src));
            }
            self.warp_to_center();
            self.request_redraw();
        }
    }

    pub(crate) fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // While recording, periodically updates the elapsed time and
        // level meters; during setup (before Start), just the preview
        // audio's level meter. Otherwise waits event-driven.
        if matches!(self.mode, Mode::Recording | Mode::RecordSetup) {
            // Nothing consumes the preview session's output PCM, so
            // drain it empty every time to avoid it piling up.
            if let Some((_, rx)) = self.preview_audio.as_ref() {
                while rx.try_recv().is_ok() {}
            }
            if let Some(w) = self.control_window.as_ref() {
                w.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + CONTROL_REDRAW_INTERVAL,
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    pub(crate) fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        match self.mode {
            Mode::Selecting => {}
            _ => {
                self.recording_family_event(event_loop, id, event);
                return;
            }
        }
        match event {
            WindowEvent::CloseRequested => self.finish(),

            WindowEvent::ModifiersChanged(mods) => {
                self.ctrl = mods.state().control_key();
                self.shift = mods.state().shift_key();
                self.alt = mods.state().alt_key();
            }

            WindowEvent::Resized(size) => {
                if let Some(surface) = self.surface.as_mut() {
                    let (sw, sh) = (size.width.max(1), size.height.max(1));
                    let _ =
                        surface.resize(NonZeroU32::new(sw).unwrap(), NonZeroU32::new(sh).unwrap());
                    self.surface_size = (sw as usize, sh as usize);
                }
                self.build_menu();
                self.request_redraw();
            }

            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // On gaining focus, winit can send a synthetic
                // KeyboardInput for any key that's still physically held
                // down at that moment. For example, opening this window
                // via the global hotkey Ctrl+Shift+2 while the 2 key is
                // still held delivers a synthetic "2 pressed" event, and
                // if ModifiersChanged hasn't caught up yet it's
                // misread as an unmodified "2" — wrongly triggering the
                // number-key monitor-selection feature. Synthetic events
                // don't reflect a key actually pressed right now, so they're ignored.
                if is_synthetic {
                    return;
                }
                if event.state == ElementState::Pressed {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.finish(),
                        Key::Character(s) => {
                            if let Some(ch) = s.chars().next() {
                                let pressed = LocalKey::new(self.ctrl, self.shift, self.alt, ch);
                                // Any time: reuse the last region as-is
                                // (no button). Replaces the current
                                // selection whether it's already set or mid-drag.
                                if self.keys.undo.contains(&pressed) {
                                    self.undo();
                                } else if self.keys.redo.contains(&pressed) {
                                    self.redo();
                                } else if self.keys.reuse_region.contains(&pressed) {
                                    self.use_previous_region();
                                } else if ch == '0' && !self.ctrl && !self.alt {
                                    // The 0 key (unmodified): selects all monitors (the whole composited canvas).
                                    self.select_all_monitors();
                                } else if ch.is_ascii_digit() && !self.ctrl && !self.alt {
                                    // A digit key (unmodified): selects that whole monitor (1 = first, etc).
                                    self.select_monitor(ch.to_digit(10).unwrap() as usize - 1);
                                } else if self.keys.clear_selection.contains(&pressed)
                                    && self.selection.is_some()
                                {
                                    // Only while selected: clears the selection so it can be redrawn (no button).
                                    self.clear_selection();
                                } else if self.keys.save_as.contains(&pressed)
                                    && self.selection.is_some()
                                {
                                    // Only while selected: saves to a
                                    // location chosen via a dialog (no
                                    // button, hotkey-only, default Shift+S).
                                    self.save_as();
                                } else if self.keys.edit_external.contains(&pressed)
                                    && self.selection.is_some()
                                    && crate::store::external_editor().is_some()
                                {
                                    // Only while selected and an external
                                    // editor is configured (no-op
                                    // otherwise; no button, default Shift+E).
                                    self.trigger(Action::EditExternal, event_loop);
                                } else if self.keys.menu.quit.contains(&pressed) {
                                    // Quit always works regardless of whether the menu is showing (same as Esc).
                                    self.trigger(Action::Quit, event_loop);
                                } else if let Some(m) = self.menu.as_ref()
                                    && let Some(btn) = m
                                        .buttons
                                        .iter()
                                        .find(|b| !b.disabled && b.hotkeys.contains(&pressed))
                                {
                                    // Everything else only accepts a
                                    // button's hotkey while the menu is
                                    // showing (a disabled button doesn't fire via hotkey either).
                                    self.trigger(btn.action, event_loop);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Zoom only applies during initial selection, or while a
                // handle/region is grabbed. Disabled while the menu is
                // idle (selection committed, nothing grabbed).
                if self.selection.is_none() || !matches!(self.adjust, Adjust::Idle) {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f64,
                        MouseScrollDelta::PixelDelta(p) => p.y / 50.0,
                    };
                    self.change_zoom(amount);
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            // Menu buttons take priority.
                            if let Some(m) = self.menu.as_ref() {
                                let (cx, cy) = self.cursor_px();
                                if let Some(i) = m.hit(cx, cy) {
                                    self.pressed = Some(i);
                                    self.request_redraw();
                                    return;
                                }
                            }
                            // Menu phase: handle -> resize / inside -> move / outside -> redo.
                            if let Some(sel) = self.selection {
                                if let Some(h) = self.handle_at() {
                                    self.push_undo();
                                    self.adjust = Adjust::Resizing(h);
                                } else if self.cursor_in_selection() {
                                    self.push_undo();
                                    self.adjust = Adjust::Moving {
                                        anchor: self.cursor_src,
                                        orig: sel,
                                    };
                                } else {
                                    self.begin_drag();
                                }
                            } else {
                                self.begin_drag();
                            }
                        }
                        ElementState::Released => {
                            // Committing an in-progress adjustment (handle/move) takes priority.
                            if !matches!(self.adjust, Adjust::Idle) {
                                self.adjust = Adjust::Idle;
                                // Zoom is a temporary aid only while
                                // grabbed; resets to 1x on release
                                // (restoring the standard cursor and
                                // clearing the magnifier distortion while idle).
                                self.zoom = 1.0;
                                if self.fine {
                                    self.exit_fine();
                                }
                                self.build_menu();
                                self.update_adjust_cursor();
                                self.request_redraw();
                                return;
                            }
                            if let Some(m) = self.menu.as_ref() {
                                if let Some(i) = self.pressed {
                                    let (cx, cy) = self.cursor_px();
                                    // Even when disabled, the menu still
                                    // absorbs the click (so it doesn't
                                    // fall through into a new selection),
                                    // just without firing the action.
                                    if m.hit(cx, cy) == Some(i) {
                                        if !m.buttons[i].disabled {
                                            self.trigger(m.buttons[i].action, event_loop);
                                        }
                                        return;
                                    }
                                }
                                self.pressed = None;
                                self.request_redraw();
                            } else if self.dragging {
                                // Commits the selection -> resets to 1x
                                // and shows the menu. Commits the snap
                                // candidate if released with barely any
                                // movement (a click), or the drag rect
                                // otherwise (evaluated before clearing
                                // `dragging`, since the check reads it).
                                let chosen = if self.snap_active() {
                                    self.snap_rect
                                } else {
                                    self.current_rect()
                                };
                                self.dragging = false;
                                if let Some(r) = chosen {
                                    self.selection = Some(r);
                                    self.snap_rect = None;
                                    self.zoom = 1.0;
                                    if self.fine {
                                        self.exit_fine();
                                    }
                                    self.build_menu();
                                    self.hovered = None;
                                    if let Some(w) = self.window.as_ref() {
                                        w.set_cursor(CursorIcon::Default);
                                        w.set_cursor_visible(true);
                                    }
                                    self.request_redraw();
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // While zoomed, movement is driven by device_event, so a
                // CursorMoved caused by the warp is ignored.
                if self.fine {
                    return;
                }
                self.cursor = position;
                self.cursor_src = (position.x, position.y);
                if self.dragging {
                    // Redraw, since the selection border changes while dragging.
                    self.request_redraw();
                } else if !matches!(self.adjust, Adjust::Idle) {
                    // Mid handle/move drag (not zoomed, i.e. zoom==1); follows the rect and menu.
                    self.apply_adjust();
                    self.build_menu();
                    self.request_redraw();
                } else if let Some(m) = self.menu.as_ref() {
                    let (cx, cy) = self.cursor_px();
                    let h = m.hit(cx, cy);
                    if h != self.hovered {
                        self.hovered = h;
                        self.request_redraw();
                    }
                    self.update_adjust_cursor();
                } else {
                    // Before selecting: updates the snap candidate under
                    // the cursor. The real cursor stays hidden at all
                    // times; the custom crosshair shows the position regardless of a candidate.
                    self.snap_rect = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.element_at(self.cursor_src));
                    self.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.draw(),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bright_col_range_matches_a_brute_force_scan_of_axis_map() {
        // Cross-checks the closed-form range against the same
        // per-pixel column check axis_map itself does, across zoom
        // levels/centers/rects that don't line up on integer boundaries.
        let cases = [
            (4.0, (100.0, 50.0), (90, 110)),
            (3.0, (0.0, 0.0), (1, 4)),
            (2.5, (37.0, 10.0), (5, 200)),
            (1.5, (0.5, 0.5), (0, 3)),
        ];
        let sw = 400;
        for (zoom, center, (x0, x1)) in cases {
            let view = View { zoom, center };
            let col_src = axis_map(&view, Axis::X, sw, 400);
            let r = Rect {
                x0,
                y0: 0,
                x1,
                y1: 1,
            };

            let mut expected_lo = None;
            let mut expected_hi = 0;
            for (sx, &srcx) in col_src.iter().enumerate() {
                if srcx != usize::MAX && srcx >= r.x0 && srcx < r.x1 {
                    expected_lo.get_or_insert(sx);
                    expected_hi = sx + 1;
                }
            }
            let expected = (expected_lo.unwrap_or(0), expected_hi);

            assert_eq!(bright_col_range(&view, r, sw), expected, "zoom={zoom}");
        }
    }

    #[test]
    fn monitors_bounds_covers_all_monitors() {
        // A 1280x1024 secondary monitor with a vertical offset, to the left of the primary (0,0)1920x1080.
        let rects = [(0, 0, 1920, 1080), (-1280, 200, 1280, 1024)];
        assert_eq!(monitors_bounds(&rects), Some((-1280, 0, 1920, 1224)));
        assert_eq!(monitors_bounds(&[]), None);
        // A single monitor becomes its own rect.
        assert_eq!(
            monitors_bounds(&[(100, 50, 800, 600)]),
            Some((100, 50, 900, 650))
        );
    }

    #[test]
    fn containing_monitor_picks_the_monitor_that_fully_contains_the_selection() {
        // A shorter secondary monitor (1920,300)-(3200,700), to the right of the primary (0,0)-(1920,1080).
        let primary = Rect {
            x0: 0,
            y0: 0,
            x1: 1920,
            y1: 1080,
        };
        let secondary = Rect {
            x0: 1920,
            y0: 300,
            x1: 3200,
            y1: 700,
        };
        let monitors = [primary, secondary];
        let canvas = Rect {
            x0: 0,
            y0: 0,
            x1: 3200,
            y1: 1080,
        };
        let sel_in_secondary = Rect {
            x0: 2000,
            y0: 400,
            x1: 2100,
            y1: 500,
        };
        assert_eq!(
            containing_monitor(&monitors, sel_in_secondary, canvas),
            secondary
        );
        let sel_in_primary = Rect {
            x0: 100,
            y0: 100,
            x1: 200,
            y1: 200,
        };
        assert_eq!(
            containing_monitor(&monitors, sel_in_primary, canvas),
            primary
        );
    }

    #[test]
    fn containing_monitor_falls_back_to_largest_overlap_when_spanning_monitors() {
        let left = Rect {
            x0: 0,
            y0: 0,
            x1: 1000,
            y1: 1000,
        };
        let right = Rect {
            x0: 1000,
            y0: 0,
            x1: 2000,
            y1: 1000,
        };
        let monitors = [left, right];
        let canvas = Rect {
            x0: 0,
            y0: 0,
            x1: 2000,
            y1: 1000,
        };
        // The selection spans the boundary, overlapping the right monitor more.
        let sel = Rect {
            x0: 900,
            y0: 0,
            x1: 1500,
            y1: 500,
        };
        assert_eq!(containing_monitor(&monitors, sel, canvas), right);
    }

    #[test]
    fn containing_monitor_falls_back_to_canvas_when_no_monitors() {
        let canvas = Rect {
            x0: 0,
            y0: 0,
            x1: 1920,
            y1: 1080,
        };
        let sel = Rect {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
        };
        assert_eq!(containing_monitor(&[], sel, canvas), canvas);
    }

    #[test]
    fn match_monitor_dpis_pairs_each_monitor_with_its_own_scale_factor() {
        // A 100% primary at (-1920,0) and a 150% secondary at (0,0), so
        // the composited origin is negative (like a left-of-primary layout).
        let origin = (-1920, 0);
        let monitors = [
            Rect {
                x0: 0,
                y0: 0,
                x1: 1920,
                y1: 1080,
            },
            Rect {
                x0: 1920,
                y0: 0,
                x1: 1920 + 2560,
                y1: 1440,
            },
        ];
        let handles = [
            ((-1920, 0), (1920u32, 1080u32), 1.0),
            ((0, 0), (2560u32, 1440u32), 1.5),
        ];
        assert_eq!(
            match_monitor_dpis(origin, &monitors, &handles),
            vec![1.0, 1.5]
        );
    }

    #[test]
    fn match_monitor_dpis_falls_back_to_1_when_no_handle_covers_the_monitor() {
        let monitors = [Rect {
            x0: 0,
            y0: 0,
            x1: 1920,
            y1: 1080,
        }];
        // A handle that sits somewhere else entirely.
        let handles = [((5000, 5000), (800u32, 600u32), 2.0)];
        assert_eq!(match_monitor_dpis((0, 0), &monitors, &handles), vec![1.0]);
        assert_eq!(match_monitor_dpis((0, 0), &monitors, &[]), vec![1.0]);
    }

    /// A minimal `Overlay` for tests (no real capture/window). Just one `w x h` monitor.
    fn test_overlay(w: usize, h: usize) -> Overlay {
        let frozen = Frozen {
            width: w,
            height: h,
            bright: vec![0; w * h],
            dim: vec![0; w * h],
            origin: (0, 0),
            monitors: vec![Rect {
                x0: 0,
                y0: 0,
                x1: w,
                y1: h,
            }],
            monitor_dpis: vec![1.0],
        };
        let mut overlay = Overlay::new(frozen);
        overlay.surface_size = (w, h);
        overlay
    }

    #[test]
    fn undo_redo_steps_back_and_forward_through_selection_history() {
        let mut ov = test_overlay(1920, 1080);
        let rect_a = Rect {
            x0: 10,
            y0: 10,
            x1: 100,
            y1: 100,
        };
        let rect_b = Rect {
            x0: 200,
            y0: 200,
            x1: 300,
            y1: 300,
        };

        // Pushes two changes (none -> rect_a -> rect_b) the same way an
        // actual gesture commit would: "push_undo before the change, then update selection."
        ov.push_undo();
        ov.selection = Some(rect_a);
        ov.push_undo();
        ov.selection = Some(rect_b);

        ov.undo();
        assert_eq!(ov.selection, Some(rect_a));
        ov.undo();
        assert_eq!(ov.selection, None);
        // No-op once the history is exhausted.
        ov.undo();
        assert_eq!(ov.selection, None);

        ov.redo();
        assert_eq!(ov.selection, Some(rect_a));
        ov.redo();
        assert_eq!(ov.selection, Some(rect_b));
        // No-op once the history is exhausted.
        ov.redo();
        assert_eq!(ov.selection, Some(rect_b));
    }

    #[test]
    fn push_undo_discards_redo_history() {
        let mut ov = test_overlay(1920, 1080);
        let rect_a = Rect {
            x0: 10,
            y0: 10,
            x1: 100,
            y1: 100,
        };
        let rect_b = Rect {
            x0: 200,
            y0: 200,
            x1: 300,
            y1: 300,
        };

        ov.push_undo();
        ov.selection = Some(rect_a);
        ov.undo();
        assert_eq!(ov.selection, None);
        assert!(!ov.redo_stack.is_empty());

        // A new change discards the redo history saved by the earlier undo.
        ov.push_undo();
        ov.selection = Some(rect_b);
        assert!(ov.redo_stack.is_empty());
        ov.redo();
        assert_eq!(ov.selection, Some(rect_b));
    }

    #[test]
    fn undo_redo_are_ignored_mid_gesture() {
        let mut ov = test_overlay(1920, 1080);
        let rect_a = Rect {
            x0: 10,
            y0: 10,
            x1: 100,
            y1: 100,
        };
        ov.push_undo();
        ov.selection = Some(rect_a);

        // Undo is ignored mid-drag, since the target would be ambiguous.
        ov.dragging = true;
        ov.undo();
        assert_eq!(ov.selection, Some(rect_a));
        ov.dragging = false;

        // Same during a handle drag/move.
        ov.adjust = Adjust::Resizing(Handle::TopLeft);
        ov.undo();
        assert_eq!(ov.selection, Some(rect_a));
        ov.adjust = Adjust::Idle;

        ov.undo();
        assert_eq!(ov.selection, None);
    }

    #[test]
    fn crop_region_extracts_expected_pixels() {
        // A 3x2 bright buffer (0x00RRGGBB).
        let bright = vec![
            0x00_11_22_33,
            0x00_44_55_66,
            0x00_77_88_99,
            0x00_AA_BB_CC,
            0x00_DD_EE_FF,
            0x00_01_02_03,
        ];
        // Crops the bottom-right 2x1 (x:1..3, y:1..2).
        let r = Rect {
            x0: 1,
            y0: 1,
            x1: 3,
            y1: 2,
        };
        let shot = crop_region(&bright, 3, r);

        assert_eq!(shot.width, 2);
        assert_eq!(shot.height, 1);
        // (1,1)=0xDDEEFF, (2,1)=0x010203, both alpha=255.
        assert_eq!(
            shot.rgba,
            vec![0xDD, 0xEE, 0xFF, 0xFF, 0x01, 0x02, 0x03, 0xFF]
        );
    }

    fn rect(x0: usize, y0: usize, x1: usize, y1: usize) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    #[test]
    fn hit_handle_matches_anywhere_along_an_edge_not_just_its_midpoint() {
        let r = rect(10, 20, 30, 40);
        // The top edge matches Top not just at its midpoint (20.0) but
        // also away from the corner (toward the right), since matching
        // tests the whole edge rather than 8 fixed points.
        assert_eq!(hit_handle(r, (24.0, 19.0), 5.0), Some(Handle::Top));
        // Same for the left edge: Left away from the corner (toward the bottom).
        assert_eq!(hit_handle(r, (9.0, 33.0), 5.0), Some(Handle::Left));
    }

    #[test]
    fn hit_handle_prefers_corner_and_misses_when_far() {
        let r = rect(10, 20, 30, 40);
        // Exactly at a corner.
        assert_eq!(hit_handle(r, (10.0, 20.0), 5.0), Some(Handle::TopLeft));
        // Near the top edge's midpoint.
        assert_eq!(hit_handle(r, (20.0, 21.0), 5.0), Some(Handle::Top));
        // Far from anything (near the center).
        assert_eq!(hit_handle(r, (20.0, 30.0), 5.0), None);
    }

    #[test]
    fn resize_rect_moves_only_matching_edges() {
        let r = rect(10, 20, 30, 40);
        let img = (100, 100);
        // Only the right edge moves (far side is exclusive, so +1).
        assert_eq!(
            resize_rect(r, Handle::Right, (50.0, 5.0), img),
            rect(10, 20, 51, 40)
        );
        // The top-left corner moves both x0 and y0.
        assert_eq!(
            resize_rect(r, Handle::TopLeft, (5.0, 8.0), img),
            rect(5, 8, 30, 40)
        );
        // The top edge moves only y0.
        assert_eq!(
            resize_rect(r, Handle::Top, (99.0, 12.0), img),
            rect(10, 12, 30, 40)
        );
    }

    #[test]
    fn resize_rect_never_inverts_and_clamps_to_image() {
        let r = rect(10, 20, 30, 40);
        let img = (100, 100);
        // Dragging the right edge left of the left edge still keeps a minimum 1px (x1 >= x0+1).
        let out = resize_rect(r, Handle::Right, (0.0, 30.0), img);
        assert_eq!(out, rect(10, 20, 11, 40));
        // Dragging the left edge past the right edge still keeps x0 < x1.
        let out = resize_rect(r, Handle::Left, (99.0, 30.0), img);
        assert_eq!(out, rect(29, 20, 30, 40));
    }

    #[test]
    fn move_rect_translates_and_clamps() {
        let r = rect(10, 20, 30, 40); // 20x20
        let img = (100, 100);
        // A normal move.
        assert_eq!(move_rect(r, (5.0, -4.0), img), rect(15, 16, 35, 36));
        // A move past the bottom-right edge clamps within the image (keeping size).
        let out = move_rect(r, (1000.0, 1000.0), img);
        assert_eq!(out, rect(80, 80, 100, 100));
        // Same for the top-left edge.
        let out = move_rect(r, (-1000.0, -1000.0), img);
        assert_eq!(out, rect(0, 0, 20, 20));
    }

    #[test]
    fn clamp_rect_to_image_keeps_rect_that_already_fits() {
        let r = rect(10, 20, 30, 40);
        assert_eq!(clamp_rect_to_image(r, (100, 100)), r);
    }

    #[test]
    fn clamp_rect_to_image_shrinks_when_image_is_smaller() {
        // Fits within the image without panicking even if the last-used region is larger/overflows.
        let r = rect(10, 20, 300, 400);
        assert_eq!(clamp_rect_to_image(r, (100, 50)), rect(10, 20, 100, 50));
    }

    #[test]
    fn clamp_rect_to_image_origin_outside_bounds_still_yields_nonempty_rect() {
        // Leaves at least 1px even if x0/y0 themselves are outside the
        // image (e.g. the monitor layout shrank).
        let r = rect(500, 500, 600, 600);
        let out = clamp_rect_to_image(r, (100, 80));
        assert_eq!(out, rect(99, 79, 100, 80));
    }

    #[test]
    fn next_fps_cycles_through_presets_and_wraps() {
        let presets = [15, 24, 30, 60];
        assert_eq!(next_fps(15, &presets), 24);
        assert_eq!(next_fps(24, &presets), 30);
        assert_eq!(next_fps(30, &presets), 60);
        // Wraps from the last back to the first.
        assert_eq!(next_fps(60, &presets), 15);
    }

    #[test]
    fn next_fps_jumps_to_first_when_current_not_in_presets() {
        // Snaps to the first preset even if hand-edited to a value not among them.
        assert_eq!(next_fps(45, &[15, 24, 30, 60]), 15);
    }

    #[test]
    fn next_fps_keeps_current_when_presets_empty() {
        assert_eq!(next_fps(30, &[]), 30);
    }
}
