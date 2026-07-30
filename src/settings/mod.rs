//! GUI settings window, self-drawn via softbuffer.
//!
//! Opened from the tray icon. Left-side vertical tabs (General/Editor/
//! Upload/Hotkeys/...) switch sections. Key bindings are set by clicking a
//! row, then pressing the next key combination (global hotkeys and in-app
//! shortcuts are both listed in the Hotkeys tab). Save locations use a
//! native folder picker (`rfd`) via "Browse...". Save applies immediately;
//! Cancel discards. The Hotkeys tab scrolls with the mouse wheel since it
//! has many rows.
//!
//! Every layout constant across this module and its per-tab submodules is
//! a fixed logical-pixel count, unscaled by the monitor's DPI — otherwise,
//! since winit makes this a per-monitor-DPI-aware process, the window
//! would stay pixel-for-pixel identical at any DPI and look shrunk
//! relative to the rest of a scaled-up desktop. Rather than threading a
//! scale factor through all of them, `draw()` builds one `Canvas` wrapping
//! the window's real physical-pixel surface directly (see
//! `physical_size`/`scale`), and `Canvas`'s own shape-drawing methods
//! (`fill`/`line`/`fill_circle`/`blit_scaled`/... — see `src/ui/mod.rs`)
//! scale each shape's logical coordinates to physical ones before drawing
//! it, so every existing call site in this module and its submodules
//! keeps passing the same fixed logical values unchanged. Text goes
//! through the same scaling inside `TextRenderer::draw` (`src/ui/text.rs`)
//! — since it rasterizes an actual vector font, this keeps glyphs crisp at
//! any DPI rather than nearest-neighbor magnified.

mod about;
mod capture_tab;
mod editor_tab;
mod general;
mod hotkeys_tab;
mod recent;
mod text_field;
mod upload_tab;
mod video;

use std::num::NonZeroU32;
use std::rc::Rc;

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Theme, Window};

use crate::app::UserEvent;
use crate::localkey::LocalKey;
use crate::store::UploaderProfile;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, PickerPart, Rect};
use crate::update::ReleaseInfo;
use about::UpdateCheckStatus;
use hotkeys_tab::{HOTKEY_ROWS, LocalAction, build_hotkey, hotkey_content_height, hotkey_viewport};
use text_field::{
    TextCursor, apply_common_edit_key, byte_index_for_char_count, char_index_for_x,
    x_for_char_index,
};
use video::{ClickColorTarget, MaxResDim, click_color_picker_geom};

const WIN_W: usize = 720;
/// Minimum window height, sized to the Video tab's row count (grows as
/// recording settings are added).
const WIN_H: usize = 600;

/// Width of the left vertical tab bar, and the content area's left edge x.
const SIDEBAR_W: usize = 140;
const CONTENT_X: usize = SIDEBAR_W + 16;
/// Tab button height and spacing.
const TAB_BTN_H: usize = 36;
const TAB_BTN_GAP: usize = 6;

// Only colors that look the same in both light and dark stay as top-level
// constants; the rest are shadowed per-theme at the top of `draw()`. Only
// `Settings` follows the system light/dark setting automatically — the
// editor doesn't.
const ACCENT: u32 = 0x004D_A6FF;
const SAVE_BG: u32 = 0x0033_A852;
/// Scrollbar thumb colors: subtle by default, brightening while
/// hovered/dragged (see `Settings::scrollbar_hover`/`scrollbar_drag`).
const SCROLLBAR_THUMB: u32 = 0x00A8_A8A8;
const SCROLLBAR_THUMB_HOVER: u32 = 0x0080_8080;

/// A vertical tab. `About` is deliberately not in `TABS` — it's pinned to
/// the bottom of the sidebar instead of stacking with the rest (see
/// `about_tab_rect`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Recent,
    General,
    Capture,
    Video,
    Editor,
    Upload,
    Hotkeys,
    About,
}

const TABS: [(Tab, &str); 7] = [
    (Tab::Recent, "Recent"),
    (Tab::General, "General"),
    (Tab::Capture, "Capture"),
    (Tab::Video, "Video"),
    (Tab::Editor, "Editor"),
    (Tab::Upload, "Upload"),
    (Tab::Hotkeys, "Hotkeys"),
];

const ABOUT_LABEL: &str = "About";

/// The About tab's button rect, pinned to the bottom of the sidebar
/// regardless of window height (unlike the rest, which stack from the top
/// via `tab_buttons`).
fn about_tab_rect(sh: usize) -> Rect {
    field(
        12,
        sh.saturating_sub(TAB_BTN_H + 12),
        SIDEBAR_W - 24,
        TAB_BTN_H,
    )
}

/// Distinguishes save locations by output kind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveKind {
    Png,
    Mp4,
    Gif,
}

const SAVE_KINDS: [(SaveKind, &str); 3] = [
    (SaveKind::Png, "Screenshot"),
    (SaveKind::Mp4, "MP4"),
    (SaveKind::Gif, "GIF"),
];

/// An editable field of the selected profile in the Upload tab (same idea
/// as `editor::Field`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UploadField {
    Name,
    Url,
    FileField,
    TokenField,
    Token,
    ResponseField,
}

const UPLOAD_FIELDS: [UploadField; 6] = [
    UploadField::Name,
    UploadField::Url,
    UploadField::FileField,
    UploadField::TokenField,
    UploadField::Token,
    UploadField::ResponseField,
];

impl UploadField {
    fn label(self) -> &'static str {
        match self {
            UploadField::Name => "Name",
            UploadField::Url => "URL",
            UploadField::FileField => "File field",
            UploadField::TokenField => "Token field",
            UploadField::Token => "Token",
            UploadField::ResponseField => "Response field",
        }
    }

    /// The token value itself is masked with `*` to prevent shoulder-surfing.
    fn is_secret(self) -> bool {
        matches!(self, UploadField::Token)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Tab(Tab),
    Capture,
    /// The General tab's "Check for updates" button; always triggers a
    /// manual check (see `Btn::DownloadUpdate` for opening the release page).
    CheckForUpdates,
    /// Shown next to the status text once a newer version is known; opens its release page.
    DownloadUpdate,
    Browse(SaveKind),
    DefaultDir(SaveKind),
    BrowseEditor,
    /// "Launch at Windows startup" checkbox (General tab).
    LaunchAtStartup,
    /// The About tab's bug-report button; opens the GitHub issue template
    /// with the app version pre-filled.
    ReportBug,
    /// "Show mouse cursor in recordings" checkbox (Video tab).
    ShowCursorInRecording,
    /// "Show click ripples" checkbox (Video tab).
    ShowClickRipple,
    /// Left-click ripple color swatch (click toggles the color picker).
    LeftClickColorSwatch,
    /// Right-click ripple color swatch.
    RightClickColorSwatch,
    /// The Video tab's bitrate dropdown (closed-state button; click toggles it open).
    BitrateDropdown,
    /// An option shown while the dropdown is open (a Mbps value from `BITRATE_PRESETS`).
    BitrateOption(u32),
    /// The Video tab's desktop-audio-device dropdown (closed-state button;
    /// click toggles it open).
    AudioOutputDropdown,
    /// An option shown while open (index into `audio_output_devices`; index
    /// 0 is the "system default" sentinel).
    AudioOutputOption(usize),
    /// The Video tab's microphone-device dropdown.
    AudioInputDropdown,
    /// An option shown while open (index into `audio_input_devices`).
    AudioInputOption(usize),
    /// "Strip audio track if the recording is silent" checkbox (Video tab).
    StripSilentAudio,
    /// The Video tab's sample-rate dropdown (closed-state button; click
    /// toggles it open).
    SampleRateDropdown,
    /// An option shown while open (a Hz value from `AUDIO_SAMPLE_RATE_PRESETS`).
    SampleRateOption(u32),
    /// Clicking an Upload tab list row shows that profile in the edit panel
    /// below (switches which profile is being viewed/edited; whether it's
    /// actually used is set independently via the `ToggleUploaderEnabled`
    /// checkbox).
    SelectUploader(usize),
    /// Deletes an Upload tab row (the right-aligned X icon).
    DeleteUploader(usize),
    /// Toggles whether a profile is used on upload (multiple can be enabled
    /// at once).
    ToggleUploaderEnabled(usize),
    /// Adds a new empty profile.
    AddUploader,
    /// An Upload tab edit field (click to focus).
    UploadField(UploadField),
    CaptureLocal(LocalAction),
    /// Resets one action to its default (the icon to the right of its key field).
    ResetLocal(LocalAction),
    /// Resets only the global hotkey (Launch capture) to its default.
    ResetGlobal,
    /// Removes one local-shortcut chip (index into `action`'s binding Vec).
    RemoveLocalBinding(LocalAction, usize),
    /// Removes one global-hotkey chip (index into the binding Vec).
    RemoveGlobalBinding(usize),
    /// Opens the session at this index (into `self.sessions`) from the Recent tab.
    OpenSession(usize),
    /// Deletes a Recent tab row (the right-aligned X icon).
    DeleteSession(usize),
    /// The Editor tab's history-limit numeric field (click to focus).
    SessionLimitField,
    /// The history-limit's +/-1 stepper (`true` = increase).
    SessionLimitStep(bool),
    /// The General tab's filename template field (click to focus).
    FilenameFormatField,
    /// The Video tab's max-resolution numeric field (click to focus).
    MaxResolutionField(MaxResDim),
    Save,
    Cancel,
}

/// What's being recorded during key capture: either the global hotkey
/// (General tab) or a local shortcut (Hotkeys tab).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureTarget {
    Global,
    Local(LocalAction),
}

/// The outcome of the settings window.
pub enum SettingsResult {
    Saved(Box<SavedSettings>),
    /// A session was clicked in the Recent tab; open its folder in the editor.
    OpenSession(std::path::PathBuf),
    Cancelled,
}

/// The payload of `SettingsResult::Saved`. Boxed because it has enough
/// fields that it would otherwise bloat `SettingsResult` itself.
pub struct SavedSettings {
    pub hotkey: Vec<String>,
    pub save_dir_png: String,
    pub save_dir_mp4: String,
    pub save_dir_gif: String,
    pub external_editor: String,
    pub record_show_cursor: bool,
    pub record_bitrate_mbps: u32,
    pub record_max_width: u32,
    pub record_max_height: u32,
    pub record_show_click_ripple: bool,
    pub record_click_color_left: u32,
    pub record_click_color_right: u32,
    pub record_audio_output_device: String,
    pub record_audio_input_device: String,
    pub record_audio_sample_rate: u32,
    pub record_strip_silent_audio: bool,
    pub uploaders: Vec<UploaderProfile>,
    pub hotkey_undo: Vec<String>,
    pub hotkey_redo: Vec<String>,
    pub hotkey_reuse_region: Vec<String>,
    pub hotkey_clear_selection: Vec<String>,
    pub hotkey_save_as: Vec<String>,
    pub hotkey_edit_external: Vec<String>,
    pub hotkey_quit: Vec<String>,
    pub hotkey_menu_save: Vec<String>,
    pub hotkey_menu_copy: Vec<String>,
    pub hotkey_menu_edit: Vec<String>,
    pub hotkey_menu_upload: Vec<String>,
    pub hotkey_menu_record: Vec<String>,
    pub hotkey_editor_reset_zoom: Vec<String>,
    pub hotkey_editor_tool_select: Vec<String>,
    pub hotkey_editor_tool_arrow: Vec<String>,
    pub hotkey_editor_tool_polyline: Vec<String>,
    pub hotkey_editor_tool_draw: Vec<String>,
    pub hotkey_editor_tool_rect: Vec<String>,
    pub hotkey_editor_tool_ellipse: Vec<String>,
    pub hotkey_editor_tool_text: Vec<String>,
    pub hotkey_editor_tool_number_marker: Vec<String>,
    pub session_history_limit: usize,
    pub launch_at_startup: bool,
    pub filename_format: String,
}

pub struct Settings {
    window: Option<Rc<Window>>,
    _context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// The logical (DPI-independent) window size, used for all layout and
    /// hit-testing — unchanged in meaning from before DPI support was
    /// added, so existing call sites don't need to care about scale.
    size: (usize, usize),
    /// The window's actual physical pixel size (matches the softbuffer
    /// surface); only used for the final upscale blit in `draw()`.
    physical_size: (usize, usize),
    /// The window's current DPI scale factor (`window.scale_factor()`),
    /// used to convert between logical (`size`) and physical
    /// (`physical_size`) — see the module doc for why this window scales
    /// the whole rendered frame rather than every layout constant.
    scale: f64,
    /// Follows the system light/dark setting automatically (no manual toggle).
    dark: bool,

    /// The currently displayed tab.
    tab: Tab,

    hotkey: Vec<String>,
    save_dir_png: String,
    save_dir_mp4: String,
    save_dir_gif: String,
    /// "Launch at Windows startup" checkbox (General tab).
    launch_at_startup: bool,
    /// Path to the external editor executable; used for Shift+E if set.
    external_editor: String,
    /// "Show mouse cursor in recordings" checkbox (Video tab).
    record_show_cursor: bool,
    /// MP4 recording bitrate in Mbps — one of the values from
    /// `BITRATE_PRESETS`, chosen via the Video tab dropdown and written back on Save.
    record_bitrate_mbps: u32,
    /// Whether the bitrate dropdown is open.
    bitrate_dropdown_open: bool,
    /// Recording width cap in px (0 = unlimited), edited via the Video tab
    /// numeric field.
    record_max_width: u32,
    /// Recording height cap in px (0 = unlimited).
    record_max_height: u32,
    /// Whether a max-resolution field (width or height) is focused.
    max_resolution_focus: Option<MaxResDim>,
    /// Edit buffer for the focused max-resolution field (same idea as
    /// `session_limit_buf`; like `upload_field_buf`, shared between width and
    /// height, holding whichever `max_resolution_focus` points to).
    max_resolution_buf: String,
    max_resolution_cursor: TextCursor,
    /// Desktop-audio loopback device name (empty = system default), chosen
    /// via the Video tab dropdown and written back on Save.
    record_audio_output_device: String,
    /// Microphone device name (empty = system default).
    record_audio_input_device: String,
    /// Output (render) device names, enumerated once in `Settings::open`.
    /// The first entry is always an empty string, the "system default" sentinel.
    audio_output_devices: Vec<String>,
    /// Input device names (same layout as above).
    audio_input_devices: Vec<String>,
    /// Whether the desktop-audio-device dropdown is open.
    audio_output_dropdown_open: bool,
    /// Whether the microphone-device dropdown is open.
    audio_input_dropdown_open: bool,
    /// Recording audio sample rate in Hz — one of the values from
    /// `AUDIO_SAMPLE_RATE_PRESETS`, chosen via the Video tab dropdown and
    /// written back on Save.
    record_audio_sample_rate: u32,
    /// Whether the sample-rate dropdown is open.
    sample_rate_dropdown_open: bool,
    /// "Strip audio track if the recording is silent" checkbox (Video tab).
    record_strip_silent_audio: bool,
    /// "Show click ripples" checkbox (Video tab).
    record_show_click_ripple: bool,
    /// Left-click ripple color (`0x00RRGGBB`).
    record_click_color_left: u32,
    /// Right-click ripple color (`0x00RRGGBB`).
    record_click_color_right: u32,
    /// Click-ripple color picker (Some = open, holding HSV) plus its drag
    /// and edit targets.
    picker: Option<(f32, f32, f32)>,
    picker_drag: Option<PickerPart>,
    picker_target: Option<ClickColorTarget>,
    /// Latest cursor position (window-relative), used to test against the
    /// picker's SV/Hue rects (`hover` only tracks per-`Btn` hit-testing and
    /// has no raw coordinates).
    cursor: (f64, f64),
    /// The scrollbar thumb being drag-scrolled for whichever tab is active
    /// (Hotkeys/Recent/Upload), holding the vertical offset between the
    /// initial click and the thumb's top (so dragging doesn't snap the
    /// thumb to be centered on the cursor). `None` when not dragging.
    scrollbar_drag: Option<i32>,
    /// Whether the cursor is over the active tab's scrollbar thumb
    /// (independent of `scrollbar_drag`, so hovering shows the same
    /// brighter color before a drag actually starts).
    scrollbar_hover: bool,

    // --- Upload tab (custom uploaders) ---
    /// Registered profiles; edited in place here and written back on Save.
    /// Whether a profile is actually used is its own `enabled` field
    /// (multiple can be enabled at once).
    uploaders: Vec<UploaderProfile>,
    /// Index into `uploaders` of the profile shown/edited in the panel below
    /// (independent of which are enabled for upload; UI-only state, not
    /// persisted to Config).
    editing_uploader: usize,
    /// Upload tab scroll position in px, changed by the mouse wheel.
    uploader_scroll: i32,
    /// Which edit field is focused (`None` = none).
    upload_field_focus: Option<UploadField>,
    /// Edit buffer for the focused field.
    upload_field_buf: String,
    /// Caret/selection state for the focused field.
    upload_field_cursor: TextCursor,

    /// Whether a text field is being drag-selected (which field —
    /// `upload_field_focus` or `session_limit_focus` — is determined by
    /// which one is set; never both at once).
    text_drag: bool,

    /// The key-capture target (`None` = not currently capturing).
    capturing: Option<CaptureTarget>,
    mods: ModifiersState,

    // --- Hotkeys tab (in-app local shortcuts) ---
    hotkey_undo: Vec<String>,
    hotkey_redo: Vec<String>,
    hotkey_reuse_region: Vec<String>,
    hotkey_clear_selection: Vec<String>,
    hotkey_save_as: Vec<String>,
    hotkey_edit_external: Vec<String>,
    hotkey_quit: Vec<String>,
    hotkey_menu_save: Vec<String>,
    hotkey_menu_copy: Vec<String>,
    hotkey_menu_edit: Vec<String>,
    hotkey_menu_upload: Vec<String>,
    hotkey_menu_record: Vec<String>,
    hotkey_editor_reset_zoom: Vec<String>,
    hotkey_editor_tool_select: Vec<String>,
    hotkey_editor_tool_arrow: Vec<String>,
    hotkey_editor_tool_polyline: Vec<String>,
    hotkey_editor_tool_draw: Vec<String>,
    hotkey_editor_tool_rect: Vec<String>,
    hotkey_editor_tool_ellipse: Vec<String>,
    hotkey_editor_tool_text: Vec<String>,
    hotkey_editor_tool_number_marker: Vec<String>,
    /// Short error message shown at the top of the Hotkeys tab when a
    /// binding conflicts.
    hotkey_error: Option<String>,
    /// Hotkeys tab scroll position in px, changed by the mouse wheel.
    hotkey_scroll: i32,

    // --- About tab ---
    /// About tab scroll position in px, changed by the mouse wheel.
    about_scroll: i32,

    // --- Recent tab (editor session history) ---
    /// Scanned once when opened (newest first); unchanged until Save/Cancel.
    sessions: Vec<crate::session::SessionSummary>,
    /// Recent tab scroll position in px, changed by the mouse wheel.
    recent_scroll: i32,
    /// History limit, edited via the General tab numeric field and written
    /// back on Save.
    session_history_limit: usize,
    /// Whether the history-limit field is focused.
    session_limit_focus: bool,
    /// Edit buffer for the focused history-limit field.
    session_limit_buf: String,
    /// Caret/selection state for the focused history-limit field.
    session_limit_cursor: TextCursor,

    // --- General tab (save filename template) ---
    /// Committed value, edited via the General tab field and written back on Save.
    filename_format: String,
    /// Whether the filename template field is focused.
    filename_format_focus: bool,
    /// Edit buffer for the focused filename template field.
    filename_format_buf: String,
    /// Caret/selection state for the focused filename template field.
    filename_format_cursor: TextCursor,

    hover: Option<Btn>,
    pressed: Option<Btn>,
    text: Option<TextRenderer>,

    // --- General tab (update check) ---
    /// The newer version found, if any (`None` = not checked yet, or
    /// checked and already up to date). Synced from the App's state via
    /// `set_update_check_result` whenever it changes.
    update_available: Option<ReleaseInfo>,
    /// The outcome of the last manual check made from this session, for
    /// the cases `update_available` can't represent on its own (already up
    /// to date, or the check failed). Cleared when a new check starts, and
    /// whenever a newer version is found (`update_available` covers that case instead).
    update_check_status: Option<UpdateCheckStatus>,
    /// Handle for sending the manual-check background thread's result back
    /// (a clone of the App's own).
    update_proxy: EventLoopProxy<UserEvent>,
    /// True while a download/install is in flight (from clicking "Update"),
    /// to ignore a repeat click rather than starting a second one.
    updating: bool,
    /// The error from the last download/install attempt, if it failed
    /// (shown in place of the "a new version is available" status text —
    /// success has no visible state here since the app exits to relaunch).
    update_install_error: Option<String>,
}

impl Settings {
    /// Opens the window with the current settings. `update_available`/
    /// `update_proxy` are the App's current update-check state (a display
    /// snapshot, and a proxy for triggering a manual check).
    pub fn open(
        event_loop: &ActiveEventLoop,
        update_available: Option<ReleaseInfo>,
        update_proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let cfg = crate::store::snapshot();
        let hk = crate::store::hotkeys::snapshot();
        let up = crate::store::uploaders::snapshot();

        // "System default" sentinel (empty string) first, then real device names.
        let mut audio_output_devices = vec![String::new()];
        audio_output_devices.extend(crate::capture::audio_output_device_names());
        let mut audio_input_devices = vec![String::new()];
        audio_input_devices.extend(crate::capture::audio_input_device_names());

        // Centers using the primary monitor's own scale factor, since
        // `with_inner_size` below requests a *logical* size — the window
        // will actually occupy roughly `WIN_W/WIN_H * scale` physical
        // pixels, not `WIN_W`/`WIN_H` directly.
        let (mx, my, primary_scale) = event_loop
            .primary_monitor()
            .map(|m| {
                let s = m.size();
                (s.width as i32, s.height as i32, m.scale_factor())
            })
            .unwrap_or((1280, 720, 1.0));
        let win_w_phys = (WIN_W as f64 * primary_scale).round() as i32;
        let win_h_phys = (WIN_H as f64 * primary_scale).round() as i32;
        let pos = PhysicalPosition::new((mx - win_w_phys) / 2, (my - win_h_phys) / 2);

        let attrs = Window::default_attributes()
            .with_title("pashari Settings")
            .with_resizable(true)
            .with_min_inner_size(LogicalSize::new(WIN_W as f64, WIN_H as f64))
            .with_position(pos)
            .with_inner_size(LogicalSize::new(WIN_W as f64, WIN_H as f64));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("設定ウィンドウ生成に失敗"),
        );

        let context = softbuffer::Context::new(window.clone()).expect("settings context");
        let mut surface =
            softbuffer::Surface::new(&context, window.clone()).expect("settings surface");
        let s = window.inner_size();
        let (pw, ph) = (s.width.max(1), s.height.max(1));
        surface
            .resize(NonZeroU32::new(pw).unwrap(), NonZeroU32::new(ph).unwrap())
            .expect("settings resize");
        window.request_redraw();

        let scale = window.scale_factor();
        let size = (
            ((pw as f64) / scale).round().max(1.0) as usize,
            ((ph as f64) / scale).round().max(1.0) as usize,
        );

        // Uses the OS theme setting as the initial value; falls back to dark
        // (the previous default look) where it can't be read.
        let dark = window.theme().map(|t| t == Theme::Dark).unwrap_or(true);

        let mut this = Self {
            window: Some(window),
            _context: Some(context),
            surface: Some(surface),
            size,
            physical_size: (pw as usize, ph as usize),
            scale,
            dark,
            tab: Tab::General,
            hotkey: hk.hotkey,
            save_dir_png: cfg.save_dir_png,
            save_dir_mp4: cfg.save_dir_mp4,
            save_dir_gif: cfg.save_dir_gif,
            launch_at_startup: cfg.launch_at_startup,
            external_editor: cfg.external_editor,
            record_show_cursor: cfg.record_show_cursor,
            record_bitrate_mbps: cfg.record_bitrate_mbps,
            bitrate_dropdown_open: false,
            record_max_width: cfg.record_max_width,
            record_max_height: cfg.record_max_height,
            max_resolution_focus: None,
            max_resolution_buf: String::new(),
            max_resolution_cursor: TextCursor::default(),
            record_audio_output_device: cfg.record_audio_output_device,
            record_audio_input_device: cfg.record_audio_input_device,
            audio_output_devices,
            audio_input_devices,
            audio_output_dropdown_open: false,
            audio_input_dropdown_open: false,
            record_audio_sample_rate: cfg.record_audio_sample_rate,
            sample_rate_dropdown_open: false,
            record_strip_silent_audio: cfg.record_strip_silent_audio,
            record_show_click_ripple: cfg.record_show_click_ripple,
            record_click_color_left: cfg.record_click_color_left,
            record_click_color_right: cfg.record_click_color_right,
            picker: None,
            picker_drag: None,
            picker_target: None,
            cursor: (0.0, 0.0),
            scrollbar_drag: None,
            scrollbar_hover: false,
            uploaders: up.uploaders,
            editing_uploader: 0,
            uploader_scroll: 0,
            upload_field_focus: None,
            upload_field_buf: String::new(),
            upload_field_cursor: TextCursor::default(),
            text_drag: false,
            capturing: None,
            mods: ModifiersState::empty(),
            hotkey_undo: hk.hotkey_undo,
            hotkey_redo: hk.hotkey_redo,
            hotkey_reuse_region: hk.hotkey_reuse_region,
            hotkey_clear_selection: hk.hotkey_clear_selection,
            hotkey_save_as: hk.hotkey_save_as,
            hotkey_edit_external: hk.hotkey_edit_external,
            hotkey_quit: hk.hotkey_quit,
            hotkey_menu_save: hk.hotkey_menu_save,
            hotkey_menu_copy: hk.hotkey_menu_copy,
            hotkey_menu_edit: hk.hotkey_menu_edit,
            hotkey_menu_upload: hk.hotkey_menu_upload,
            hotkey_menu_record: hk.hotkey_menu_record,
            hotkey_editor_reset_zoom: hk.hotkey_editor_reset_zoom,
            hotkey_editor_tool_select: hk.hotkey_editor_tool_select,
            hotkey_editor_tool_arrow: hk.hotkey_editor_tool_arrow,
            hotkey_editor_tool_polyline: hk.hotkey_editor_tool_polyline,
            hotkey_editor_tool_draw: hk.hotkey_editor_tool_draw,
            hotkey_editor_tool_rect: hk.hotkey_editor_tool_rect,
            hotkey_editor_tool_ellipse: hk.hotkey_editor_tool_ellipse,
            hotkey_editor_tool_text: hk.hotkey_editor_tool_text,
            hotkey_editor_tool_number_marker: hk.hotkey_editor_tool_number_marker,
            hotkey_error: None,
            hotkey_scroll: 0,
            about_scroll: 0,
            sessions: crate::session::list(),
            recent_scroll: 0,
            session_history_limit: cfg.session_history_limit,
            session_limit_focus: false,
            session_limit_buf: String::new(),
            session_limit_cursor: TextCursor::default(),
            filename_format: cfg.filename_format,
            filename_format_focus: false,
            filename_format_buf: String::new(),
            filename_format_cursor: TextCursor::default(),
            hover: None,
            pressed: None,
            text: TextRenderer::load(),
            update_available,
            update_check_status: None,
            update_proxy,
            updating: false,
            update_install_error: None,
        };
        // Relying solely on request_redraw() can leave the window blank on
        // Windows: the OS's initial paint can run before the first
        // RedrawRequested arrives, and nothing repaints it until the next
        // one. Drawing synchronously here ensures correct content from the
        // moment the window appears.
        this.draw();
        this
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    fn save_dir_mut(&mut self, k: SaveKind) -> &mut String {
        match k {
            SaveKind::Png => &mut self.save_dir_png,
            SaveKind::Mp4 => &mut self.save_dir_mp4,
            SaveKind::Gif => &mut self.save_dir_gif,
        }
    }

    /// Brings the window to the front (e.g. to redirect focus to an already
    /// open settings window on a duplicate launch).
    pub fn focus(&self) {
        if let Some(w) = self.window.as_ref() {
            w.focus_window();
        }
    }

    /// Syncs the open settings window's display with an update-check
    /// result (from either the automatic startup check or a manual one
    /// triggered from this window).
    pub fn set_update_check_result(&mut self, result: &Result<Option<ReleaseInfo>, String>) {
        match result {
            Ok(Some(info)) => {
                self.update_available = Some(info.clone());
                self.update_check_status = None;
            }
            Ok(None) => self.update_check_status = Some(UpdateCheckStatus::UpToDate),
            Err(_) => self.update_check_status = Some(UpdateCheckStatus::Failed),
        }
        self.request_redraw();
    }

    /// Reports a failed download/install attempt (from `Btn::DownloadUpdate`),
    /// clearing the in-progress flag so the button can be retried.
    pub fn set_update_install_error(&mut self, msg: String) {
        self.updating = false;
        self.update_install_error = Some(msg);
        self.request_redraw();
    }

    /// Rects of the vertical tab buttons (always the same positions).
    fn tab_buttons() -> [(Tab, Rect); TABS.len()] {
        let mut out = [(Tab::General, field(0, 0, 0, 0)); TABS.len()];
        for (i, (tab, _)) in TABS.into_iter().enumerate() {
            let y = 60 + i * (TAB_BTN_H + TAB_BTN_GAP);
            out[i] = (tab, field(12, y, SIDEBAR_W - 24, TAB_BTN_H));
        }
        out
    }

    /// Buttons for the current tab (tab buttons plus tab content plus Save/Cancel).
    fn buttons(&self) -> Vec<(Btn, Rect)> {
        let mut v: Vec<(Btn, Rect)> = Self::tab_buttons()
            .into_iter()
            .map(|(t, r)| (Btn::Tab(t), r))
            .collect();

        let (sw, sh) = self.size;
        v.push((Btn::Tab(Tab::About), about_tab_rect(sh)));
        match self.tab {
            Tab::Recent => v.extend(self.buttons_recent(sw, sh)),
            Tab::General => v.extend(self.buttons_general(sw)),
            Tab::Capture => v.extend(self.buttons_capture(sw)),
            Tab::Video => v.extend(self.buttons_video(sw)),
            Tab::Editor => v.extend(self.buttons_editor()),
            Tab::Upload => v.extend(self.buttons_upload(sw)),
            Tab::Hotkeys => v.extend(self.buttons_hotkeys(sw, sh)),
            Tab::About => v.extend(self.buttons_about()),
        }

        v.push((
            Btn::Cancel,
            field(sw.saturating_sub(232), sh.saturating_sub(52), 100, 32),
        ));
        v.push((
            Btn::Save,
            field(sw.saturating_sub(120), sh.saturating_sub(52), 100, 32),
        ));
        v
    }

    fn button_at(&self, x: usize, y: usize) -> Option<Btn> {
        self.buttons()
            .into_iter()
            .find(|(_, r)| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
            .map(|(b, _)| b)
    }

    /// Handles an event; returns a result when the window closes.
    pub fn handle_event(&mut self, event: WindowEvent) -> Option<SettingsResult> {
        match event {
            WindowEvent::CloseRequested => return Some(SettingsResult::Cancelled),

            WindowEvent::Resized(size) => {
                if let Some(surface) = self.surface.as_mut() {
                    let (pw, ph) = (size.width.max(1), size.height.max(1));
                    let _ =
                        surface.resize(NonZeroU32::new(pw).unwrap(), NonZeroU32::new(ph).unwrap());
                    self.physical_size = (pw as usize, ph as usize);
                    self.size = (
                        ((pw as f64) / self.scale).round().max(1.0) as usize,
                        ((ph as f64) / self.scale).round().max(1.0) as usize,
                    );
                }
                self.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    return self.on_key(&event);
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    return self.on_mouse(state);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // `position` is always physical; the rest of this module works in
                // logical coordinates (see the module doc), so convert once here.
                let x = position.x / self.scale;
                let y = position.y / self.scale;
                self.cursor = (x, y);
                if self.picker_drag.is_some() {
                    // Follow the color picker drag.
                    self.apply_click_color_picker(x, y);
                } else if self.text_drag {
                    // Follow the text field drag-selection.
                    self.update_text_drag(x);
                } else if let Some(grab_offset) = self.scrollbar_drag {
                    // Follow the scrollbar-thumb drag.
                    self.update_scrollbar_drag(y, grab_offset);
                } else {
                    let h = self.button_at(x as usize, y as usize);
                    if h != self.hover {
                        self.hover = h;
                        self.request_redraw();
                    }
                    let scrollbar_hover = self.scrollbar_hit(x, y).is_some();
                    if scrollbar_hover != self.scrollbar_hover {
                        self.scrollbar_hover = scrollbar_hover;
                        self.request_redraw();
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 * 24.0,
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                let (sw, sh) = self.size;
                if self.tab == Tab::Hotkeys {
                    let viewport = hotkey_viewport(sw, sh);
                    let local_specs = self.local_bindings_snapshot();
                    let max_scroll =
                        (hotkey_content_height(self.text.as_ref(), sw, &self.hotkey, &local_specs)
                            - (viewport.y1 - viewport.y0) as i64)
                            .max(0);
                    self.hotkey_scroll =
                        (self.hotkey_scroll - dy as i32).clamp(0, max_scroll as i32);
                    self.request_redraw();
                } else if self.tab == Tab::Recent {
                    let viewport = recent::session_viewport(sw, sh);
                    let max_scroll = (recent::session_content_height(self.sessions.len())
                        - (viewport.y1 - viewport.y0) as i64)
                        .max(0);
                    self.recent_scroll =
                        (self.recent_scroll - dy as i32).clamp(0, max_scroll as i32);
                    self.request_redraw();
                } else if self.tab == Tab::Upload {
                    let viewport = upload_tab::uploader_viewport(sw);
                    let max_scroll = (upload_tab::uploader_content_height(self.uploaders.len())
                        - (viewport.y1 - viewport.y0) as i64)
                        .max(0);
                    self.uploader_scroll =
                        (self.uploader_scroll - dy as i32).clamp(0, max_scroll as i32);
                    self.request_redraw();
                } else if self.tab == Tab::About {
                    let viewport = about::about_viewport(sw, sh);
                    let max_scroll = (about::about_content_height(about::USED_CRATES.len())
                        - (viewport.y1 - viewport.y0) as i64)
                        .max(0);
                    self.about_scroll = (self.about_scroll - dy as i32).clamp(0, max_scroll as i32);
                    self.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.draw(),

            WindowEvent::ThemeChanged(theme) => {
                self.dark = theme == Theme::Dark;
                self.request_redraw();
            }

            _ => {}
        }
        None
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) -> Option<SettingsResult> {
        if self.picker.is_some()
            && event.logical_key == winit::keyboard::Key::Named(NamedKey::Escape)
        {
            self.picker = None;
            self.picker_target = None;
            self.request_redraw();
            return None;
        }
        if let Some(target) = self.capturing {
            // Esc cancels capture.
            if event.logical_key == winit::keyboard::Key::Named(NamedKey::Escape) {
                self.capturing = None;
                self.hotkey_error = None;
                self.request_redraw();
                return None;
            }
            match target {
                CaptureTarget::Global => {
                    if let PhysicalKey::Code(code) = event.physical_key
                        && let Some(spec) = build_hotkey(self.mods, code)
                    {
                        // If this combination is already registered (parsed
                        // once for normalized comparison), close capture
                        // without adding a duplicate.
                        let candidate = crate::hotkey::parse(&spec);
                        let duplicate = candidate.is_some_and(|c| {
                            self.hotkey
                                .iter()
                                .filter_map(|s| crate::hotkey::parse(s))
                                .any(|k| k == c)
                        });
                        if !duplicate {
                            self.hotkey.push(spec);
                        }
                        self.capturing = None;
                        self.request_redraw();
                    }
                }
                CaptureTarget::Local(action) => {
                    if let winit::keyboard::Key::Character(s) = &event.logical_key
                        && let Some(ch) = s.chars().next()
                    {
                        let candidate = LocalKey::new(
                            self.mods.control_key(),
                            self.mods.shift_key(),
                            self.mods.alt_key(),
                            ch,
                        );
                        self.try_add_local_key(action, candidate);
                    }
                }
            }
            return None;
        }
        if self.upload_field_focus.is_some() {
            self.on_upload_field_key(event);
            return None;
        }
        if self.session_limit_focus {
            self.on_session_limit_key(event);
            return None;
        }
        if self.filename_format_focus {
            self.on_filename_format_key(event);
            return None;
        }
        if self.max_resolution_focus.is_some() {
            self.on_max_resolution_key(event);
            return None;
        }
        if self.bitrate_dropdown_open
            && event.logical_key == winit::keyboard::Key::Named(NamedKey::Escape)
        {
            self.bitrate_dropdown_open = false;
            self.request_redraw();
            return None;
        }
        if (self.audio_output_dropdown_open || self.audio_input_dropdown_open)
            && event.logical_key == winit::keyboard::Key::Named(NamedKey::Escape)
        {
            self.audio_output_dropdown_open = false;
            self.audio_input_dropdown_open = false;
            self.request_redraw();
            return None;
        }
        if self.sample_rate_dropdown_open
            && event.logical_key == winit::keyboard::Key::Named(NamedKey::Escape)
        {
            self.sample_rate_dropdown_open = false;
            self.request_redraw();
            return None;
        }
        // Esc doesn't close the window (only the ✕ or Cancel/Save do).
        None
    }

    /// Follows a text field's drag-selection (called on every `CursorMoved`
    /// while `text_drag` is true). Which field is determined by which of
    /// `upload_field_focus`/`session_limit_focus` is set.
    fn update_text_drag(&mut self, x: f64) {
        if let Some(field) = self.upload_field_focus {
            let rel_x = x as f32 - self.upload_field_text_x0(field);
            let idx = self.upload_field_click_idx(field, rel_x);
            self.upload_field_cursor.set_from_click(idx, true);
            self.request_redraw();
        } else if self.session_limit_focus {
            let rel_x = x as f32 - self.session_limit_text_x0();
            let idx = char_index_for_x(self.text.as_ref(), &self.session_limit_buf, 15.0, rel_x);
            self.session_limit_cursor.set_from_click(idx, true);
            self.request_redraw();
        } else if self.filename_format_focus {
            let rel_x = x as f32 - self.filename_format_text_x0();
            let idx = char_index_for_x(self.text.as_ref(), &self.filename_format_buf, 15.0, rel_x);
            self.filename_format_cursor.set_from_click(idx, true);
            self.request_redraw();
        } else if self.max_resolution_focus.is_some() {
            let rel_x = x as f32 - self.max_resolution_text_x0();
            let idx = char_index_for_x(self.text.as_ref(), &self.max_resolution_buf, 15.0, rel_x);
            self.max_resolution_cursor.set_from_click(idx, true);
            self.request_redraw();
        }
    }

    /// The active tab's scroll viewport, content height, and current
    /// scroll offset — `None` if the current tab isn't one of the
    /// scrollable ones (Hotkeys/Recent/Upload).
    fn active_scroll_state(&self) -> Option<(Rect, i64, i32)> {
        let (sw, sh) = self.size;
        match self.tab {
            Tab::Hotkeys => {
                let viewport = hotkey_viewport(sw, sh);
                let local_specs = self.local_bindings_snapshot();
                let content_h =
                    hotkey_content_height(self.text.as_ref(), sw, &self.hotkey, &local_specs);
                Some((viewport, content_h, self.hotkey_scroll))
            }
            Tab::Recent => {
                let viewport = recent::session_viewport(sw, sh);
                let content_h = recent::session_content_height(self.sessions.len());
                Some((viewport, content_h, self.recent_scroll))
            }
            Tab::Upload => {
                let viewport = upload_tab::uploader_viewport(sw);
                let content_h = upload_tab::uploader_content_height(self.uploaders.len());
                Some((viewport, content_h, self.uploader_scroll))
            }
            Tab::About => {
                let viewport = about::about_viewport(sw, sh);
                let content_h = about::about_content_height(about::USED_CRATES.len());
                Some((viewport, content_h, self.about_scroll))
            }
            _ => None,
        }
    }

    /// If `(x, y)` is on the active tab's scrollbar thumb, returns the
    /// grab offset to start a drag with (the vertical distance from `y` to
    /// the thumb's top).
    fn scrollbar_hit(&self, x: f64, y: f64) -> Option<i32> {
        let (viewport, content_h, scroll) = self.active_scroll_state()?;
        let track_x0 = self.size.0.saturating_sub(10);
        let thumb = scrollbar_thumb_rect(track_x0, viewport, content_h, scroll)?;
        if inside(thumb, x, y) {
            Some((y as i64 - thumb.y0 as i64) as i32)
        } else {
            None
        }
    }

    /// Follows a scrollbar-thumb drag (called on every `CursorMoved` while
    /// `scrollbar_drag` is `Some`), keeping `grab_offset` fixed under the cursor.
    fn update_scrollbar_drag(&mut self, y: f64, grab_offset: i32) {
        let Some((viewport, content_h, _)) = self.active_scroll_state() else {
            return;
        };
        let viewport_h = (viewport.y1 - viewport.y0) as i64;
        let max_scroll = (content_h - viewport_h).max(1);
        let thumb_h = (viewport_h * viewport_h / content_h.max(1)).max(20);
        let track_range = (viewport_h - thumb_h).max(1);
        let thumb_y = y as i64 - grab_offset as i64 - viewport.y0 as i64;
        let new_scroll = (thumb_y * max_scroll / track_range).clamp(0, max_scroll) as i32;
        match self.tab {
            Tab::Hotkeys => self.hotkey_scroll = new_scroll,
            Tab::Recent => self.recent_scroll = new_scroll,
            Tab::Upload => self.uploader_scroll = new_scroll,
            Tab::About => self.about_scroll = new_scroll,
            _ => {}
        }
        self.request_redraw();
    }

    fn on_mouse(&mut self, state: ElementState) -> Option<SettingsResult> {
        match state {
            ElementState::Pressed => {
                let (cx, cy) = self.cursor;
                // Picker open: click inside starts a drag, click outside
                // just closes it without falling through to normal button
                // handling (same "outside click just closes" behavior as
                // the editor's color picker).
                if let Some(target) = self.picker_target {
                    let (_popup, sv_rect, hue_rect) = click_color_picker_geom(
                        self.size.0,
                        self.size.1,
                        self.text.as_ref(),
                        target,
                    );
                    if inside(sv_rect, cx, cy) {
                        self.picker_drag = Some(PickerPart::Sv);
                        self.apply_click_color_picker(cx, cy);
                    } else if inside(hue_rect, cx, cy) {
                        self.picker_drag = Some(PickerPart::Hue);
                        self.apply_click_color_picker(cx, cy);
                    } else {
                        self.picker = None;
                        self.picker_target = None;
                        self.request_redraw();
                    }
                    return None;
                }
                // Scrollbar thumb: start a drag, same "handled directly,
                // bypassing `self.pressed`" treatment as the picker/text-field drags.
                if let Some(grab_offset) = self.scrollbar_hit(cx, cy) {
                    self.scrollbar_drag = Some(grab_offset);
                    return None;
                }
                // Text edit fields: place the caret at the click position.
                // Handled directly here (bypassing `self.pressed`) so a drag
                // can extend it into a selection, same as starting a color
                // picker drag.
                match self.hover {
                    Some(Btn::UploadField(f)) => {
                        self.begin_upload_field_press(f, cx);
                        return None;
                    }
                    Some(Btn::SessionLimitField) => {
                        self.begin_session_limit_press(cx);
                        return None;
                    }
                    Some(Btn::FilenameFormatField) => {
                        self.begin_filename_format_press(cx);
                        return None;
                    }
                    Some(Btn::MaxResolutionField(dim)) => {
                        self.begin_max_resolution_press(dim, cx);
                        return None;
                    }
                    _ => {}
                }
                self.pressed = self.hover;
                // Clicking empty space commits/unfocuses the focused field.
                if self.hover.is_none() {
                    self.commit_upload_field();
                    self.commit_session_limit();
                    self.commit_filename_format();
                    self.commit_max_resolution();
                    self.bitrate_dropdown_open = false;
                    self.audio_output_dropdown_open = false;
                    self.audio_input_dropdown_open = false;
                    self.sample_rate_dropdown_open = false;
                }
                None
            }
            ElementState::Released => {
                if self.picker_drag.take().is_some() {
                    return None;
                }
                if self.text_drag {
                    self.text_drag = false;
                    return None;
                }
                if self.scrollbar_drag.take().is_some() {
                    return None;
                }
                if let Some(btn) = self.pressed.take()
                    && self.hover == Some(btn)
                {
                    return self.activate(btn);
                }
                None
            }
        }
    }

    fn activate(&mut self, btn: Btn) -> Option<SettingsResult> {
        // Commit the Upload tab field's input when anything else is clicked.
        if !matches!(btn, Btn::UploadField(_)) {
            self.commit_upload_field();
        }
        // Commit the history-limit field's input when anything else is clicked.
        if btn != Btn::SessionLimitField {
            self.commit_session_limit();
        }
        // Commit the filename template field's input when anything else is clicked.
        if btn != Btn::FilenameFormatField {
            self.commit_filename_format();
        }
        // Commit the max-resolution field's input when anything else is clicked.
        if !matches!(btn, Btn::MaxResolutionField(_)) {
            self.commit_max_resolution();
        }
        // Close a dropdown on anything other than its own open/select
        // action (the 4 dropdowns are mutually exclusive: opening one closes
        // the others).
        if !matches!(btn, Btn::BitrateDropdown | Btn::BitrateOption(_)) {
            self.bitrate_dropdown_open = false;
        }
        if !matches!(btn, Btn::AudioOutputDropdown | Btn::AudioOutputOption(_)) {
            self.audio_output_dropdown_open = false;
        }
        if !matches!(btn, Btn::AudioInputDropdown | Btn::AudioInputOption(_)) {
            self.audio_input_dropdown_open = false;
        }
        if !matches!(btn, Btn::SampleRateDropdown | Btn::SampleRateOption(_)) {
            self.sample_rate_dropdown_open = false;
        }
        // Close an open color picker on anything other than the swatches
        // (the main close path is in on_mouse; this is a fallback).
        if !matches!(btn, Btn::LeftClickColorSwatch | Btn::RightClickColorSwatch) {
            self.picker = None;
            self.picker_target = None;
        }
        match btn {
            Btn::Tab(t) => {
                self.tab = t;
                // Switching tabs makes an in-progress hotkey capture
                // meaningless, so stop it.
                self.capturing = None;
                self.hotkey_error = None;
                self.request_redraw();
                None
            }
            Btn::Capture
            | Btn::CaptureLocal(_)
            | Btn::ResetLocal(_)
            | Btn::ResetGlobal
            | Btn::RemoveLocalBinding(_, _)
            | Btn::RemoveGlobalBinding(_) => self.activate_hotkeys(btn),
            Btn::SessionLimitField | Btn::SessionLimitStep(_) | Btn::BrowseEditor => {
                self.activate_editor(btn)
            }
            Btn::FilenameFormatField | Btn::LaunchAtStartup => self.activate_general(btn),
            Btn::MaxResolutionField(_)
            | Btn::BitrateDropdown
            | Btn::BitrateOption(_)
            | Btn::AudioOutputDropdown
            | Btn::AudioOutputOption(_)
            | Btn::AudioInputDropdown
            | Btn::AudioInputOption(_)
            | Btn::SampleRateDropdown
            | Btn::SampleRateOption(_)
            | Btn::StripSilentAudio
            | Btn::ShowCursorInRecording
            | Btn::ShowClickRipple
            | Btn::LeftClickColorSwatch
            | Btn::RightClickColorSwatch => self.activate_video(btn),
            Btn::OpenSession(_) | Btn::DeleteSession(_) => self.activate_recent(btn),
            Btn::Browse(k) => {
                if let Some(folder) = rfd::FileDialog::new()
                    .set_title("保存先フォルダを選択")
                    .pick_folder()
                {
                    *self.save_dir_mut(k) = folder.to_string_lossy().into_owned();
                    self.request_redraw();
                }
                None
            }
            Btn::DefaultDir(k) => {
                self.save_dir_mut(k).clear();
                self.request_redraw();
                None
            }
            Btn::ReportBug | Btn::CheckForUpdates | Btn::DownloadUpdate => self.activate_about(btn),
            Btn::SelectUploader(_)
            | Btn::DeleteUploader(_)
            | Btn::ToggleUploaderEnabled(_)
            | Btn::AddUploader
            | Btn::UploadField(_) => self.activate_upload(btn),
            Btn::Save => Some(SettingsResult::Saved(Box::new(SavedSettings {
                hotkey: self.hotkey.clone(),
                save_dir_png: self.save_dir_png.clone(),
                save_dir_mp4: self.save_dir_mp4.clone(),
                save_dir_gif: self.save_dir_gif.clone(),
                launch_at_startup: self.launch_at_startup,
                external_editor: self.external_editor.clone(),
                record_show_cursor: self.record_show_cursor,
                record_bitrate_mbps: self.record_bitrate_mbps,
                record_max_width: self.record_max_width,
                record_max_height: self.record_max_height,
                record_show_click_ripple: self.record_show_click_ripple,
                record_click_color_left: self.record_click_color_left,
                record_click_color_right: self.record_click_color_right,
                record_audio_output_device: self.record_audio_output_device.clone(),
                record_audio_input_device: self.record_audio_input_device.clone(),
                record_audio_sample_rate: self.record_audio_sample_rate,
                record_strip_silent_audio: self.record_strip_silent_audio,
                uploaders: self.uploaders.clone(),
                hotkey_undo: self.hotkey_undo.clone(),
                hotkey_redo: self.hotkey_redo.clone(),
                hotkey_reuse_region: self.hotkey_reuse_region.clone(),
                hotkey_clear_selection: self.hotkey_clear_selection.clone(),
                hotkey_save_as: self.hotkey_save_as.clone(),
                hotkey_edit_external: self.hotkey_edit_external.clone(),
                hotkey_quit: self.hotkey_quit.clone(),
                hotkey_menu_save: self.hotkey_menu_save.clone(),
                hotkey_menu_copy: self.hotkey_menu_copy.clone(),
                hotkey_menu_edit: self.hotkey_menu_edit.clone(),
                hotkey_menu_upload: self.hotkey_menu_upload.clone(),
                hotkey_menu_record: self.hotkey_menu_record.clone(),
                hotkey_editor_reset_zoom: self.hotkey_editor_reset_zoom.clone(),
                hotkey_editor_tool_select: self.hotkey_editor_tool_select.clone(),
                hotkey_editor_tool_arrow: self.hotkey_editor_tool_arrow.clone(),
                hotkey_editor_tool_polyline: self.hotkey_editor_tool_polyline.clone(),
                hotkey_editor_tool_draw: self.hotkey_editor_tool_draw.clone(),
                hotkey_editor_tool_rect: self.hotkey_editor_tool_rect.clone(),
                hotkey_editor_tool_ellipse: self.hotkey_editor_tool_ellipse.clone(),
                hotkey_editor_tool_text: self.hotkey_editor_tool_text.clone(),
                hotkey_editor_tool_number_marker: self.hotkey_editor_tool_number_marker.clone(),
                session_history_limit: self.session_history_limit,
                filename_format: self.filename_format.clone(),
            }))),
            Btn::Cancel => Some(SettingsResult::Cancelled),
        }
    }

    fn draw(&mut self) {
        let (sw, sh) = self.size;
        if sw == 0 || sh == 0 {
            return;
        }
        let tab = self.tab;
        let capturing = self.capturing;
        let hover = self.hover;
        let hotkey = self.hotkey.clone();
        let save_dirs = [
            self.save_dir_png.clone(),
            self.save_dir_mp4.clone(),
            self.save_dir_gif.clone(),
        ];
        let external_editor = self.external_editor.clone();
        let launch_at_startup = self.launch_at_startup;
        let record_show_cursor = self.record_show_cursor;
        let record_show_click_ripple = self.record_show_click_ripple;
        let record_click_color_left = self.record_click_color_left;
        let record_click_color_right = self.record_click_color_right;
        let picker = self.picker;
        let picker_target = self.picker_target;
        let uploaders = self.uploaders.clone();
        let editing_uploader = self.editing_uploader;
        let uploader_scroll = self.uploader_scroll;
        let upload_field_focus = self.upload_field_focus;
        let upload_field_buf = self.upload_field_buf.clone();
        let upload_field_cursor = self.upload_field_cursor;
        let hotkey_values: Vec<(LocalAction, Vec<String>)> = HOTKEY_ROWS
            .iter()
            .map(|r| (r.action, self.local_keys(r.action).clone()))
            .collect();
        let hotkey_modified: Vec<(LocalAction, bool)> = HOTKEY_ROWS
            .iter()
            .map(|r| (r.action, self.is_modified(r.action)))
            .collect();
        let global_modified = self.is_global_modified();
        let hotkey_error = self.hotkey_error.clone();
        let hotkey_scroll = self.hotkey_scroll;
        let recent_scroll = self.recent_scroll;
        let about_scroll = self.about_scroll;
        // Brighten the scrollbar thumb while hovered or actively dragged.
        let scrollbar_active = self.scrollbar_hover || self.scrollbar_drag.is_some();
        let session_history_limit = self.session_history_limit;
        let session_limit_focus = self.session_limit_focus;
        let session_limit_buf = self.session_limit_buf.clone();
        let session_limit_cursor = self.session_limit_cursor;
        let filename_format = self.filename_format.clone();
        let filename_format_focus = self.filename_format_focus;
        let filename_format_buf = self.filename_format_buf.clone();
        let filename_format_cursor = self.filename_format_cursor;
        let record_bitrate_mbps = self.record_bitrate_mbps;
        let bitrate_dropdown_open = self.bitrate_dropdown_open;
        let record_max_width = self.record_max_width;
        let record_max_height = self.record_max_height;
        let max_resolution_focus = self.max_resolution_focus;
        let max_resolution_buf = self.max_resolution_buf.clone();
        let max_resolution_cursor = self.max_resolution_cursor;
        let record_audio_output_device = self.record_audio_output_device.clone();
        let record_audio_input_device = self.record_audio_input_device.clone();
        let audio_output_devices = self.audio_output_devices.clone();
        let audio_input_devices = self.audio_input_devices.clone();
        let audio_output_dropdown_open = self.audio_output_dropdown_open;
        let audio_input_dropdown_open = self.audio_input_dropdown_open;
        let record_audio_sample_rate = self.record_audio_sample_rate;
        let sample_rate_dropdown_open = self.sample_rate_dropdown_open;
        let record_strip_silent_audio = self.record_strip_silent_audio;
        let update_available = self.update_available.clone();
        let update_check_status = self.update_check_status;
        let updating = self.updating;
        let update_install_error = self.update_install_error.clone();
        let session_rows: Vec<(String, usize, usize, Rc<Vec<u32>>)> = self
            .sessions
            .iter()
            .map(|s| (s.label.clone(), s.thumb_w, s.thumb_h, Rc::clone(&s.thumb)))
            .collect();
        let buttons = self.buttons();
        let text = self.text.as_ref();
        let dark = self.dark;

        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let mut buf = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };

        // Shadow the module constants with theme-appropriate values (so
        // existing BG/TEXT/... references in the function body keep working).
        #[allow(non_snake_case, unused_variables)]
        let (
            BG,
            SIDEBAR_BG,
            FIELD_BG,
            BTN_BG,
            TEXT,
            DIM,
            UPLOADER_ACTIVE_BG,
            TEXT_SELECTION_BG,
            PICK_BG,
            VERY_DIM,
            SWATCH_HOVER,
        ) = theme_colors(dark);
        // The hover-emphasis direction (lighten in dark theme, darken in light).
        let hover_tint = |c: u32| hover_tint_for(c, dark);

        let Some(t) = text else {
            buf.fill(BG);
            let _ = buf.present();
            return;
        };

        // Every layout constant across this module and the per-tab
        // submodules is a fixed logical-pixel count, unaffected by DPI —
        // `Canvas`'s own drawing methods scale each shape's coordinates by
        // `self.scale` before touching the (physical-sized) surface
        // buffer, so this call site itself needs no scaling logic at all.
        // See the module doc for why.
        let (pw, ph) = self.physical_size;

        {
            let mut canvas = Canvas {
                buf: &mut buf[..],
                w: pw,
                h: ph,
                scale: self.scale,
            };
            canvas.fill(
                Rect {
                    x0: 0,
                    y0: 0,
                    x1: sw,
                    y1: sh,
                },
                BG,
            );

            t.draw(&mut canvas, 20.0, 30.0, "pashari Settings", 20.0, TEXT);

            // The left vertical tab strip, and its divider from the content area.
            canvas.fill(field(0, 50, SIDEBAR_W, sh.saturating_sub(50)), SIDEBAR_BG);
            canvas.fill(field(SIDEBAR_W, 50, 1, sh.saturating_sub(50)), BTN_BG);

            for (t_kind, label) in TABS {
                let (_, r) = Self::tab_buttons()
                    .into_iter()
                    .find(|(k, _)| *k == t_kind)
                    .expect("TABS と tab_buttons は同じ順序");
                let active = tab == t_kind;
                let base = if active { ACCENT } else { SIDEBAR_BG };
                let color = if !active && hover == Some(Btn::Tab(t_kind)) {
                    hover_tint(base)
                } else {
                    base
                };
                canvas.fill(r, color);
                let tcolor = if active { 0x00FF_FFFF } else { TEXT };
                let tw = t.text_width(label, 15.0);
                let lx = r.x0 as f32 + ((r.x1 - r.x0) as f32 - tw) / 2.0;
                let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
                t.draw(&mut canvas, lx, baseline, label, 15.0, tcolor);
            }

            // The About tab: same look as the rest, but pinned to the
            // sidebar's bottom instead of stacking with them (see `about_tab_rect`).
            {
                let r = about_tab_rect(sh);
                let active = tab == Tab::About;
                let base = if active { ACCENT } else { SIDEBAR_BG };
                let color = if !active && hover == Some(Btn::Tab(Tab::About)) {
                    hover_tint(base)
                } else {
                    base
                };
                canvas.fill(r, color);
                let tcolor = if active { 0x00FF_FFFF } else { TEXT };
                let tw = t.text_width(ABOUT_LABEL, 15.0);
                let lx = r.x0 as f32 + ((r.x1 - r.x0) as f32 - tw) / 2.0;
                let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
                t.draw(&mut canvas, lx, baseline, ABOUT_LABEL, 15.0, tcolor);
            }

            // Per-tab content.
            match tab {
                Tab::Recent => recent::draw_recent(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    sw,
                    sh,
                    &session_rows,
                    recent_scroll,
                    scrollbar_active,
                ),
                Tab::General => general::draw_general(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    &buttons,
                    sw,
                    &filename_format,
                    filename_format_focus,
                    &filename_format_buf,
                    filename_format_cursor,
                    launch_at_startup,
                ),
                Tab::Capture => capture_tab::draw_capture(&mut canvas, t, dark, sw, &save_dirs),
                Tab::Video => video::draw_video(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    &buttons,
                    sw,
                    text,
                    &save_dirs,
                    record_show_cursor,
                    record_show_click_ripple,
                    record_click_color_left,
                    record_click_color_right,
                    picker_target,
                    record_bitrate_mbps,
                    bitrate_dropdown_open,
                    record_max_width,
                    record_max_height,
                    max_resolution_focus,
                    &max_resolution_buf,
                    max_resolution_cursor,
                    &record_audio_output_device,
                    &record_audio_input_device,
                    &audio_output_devices,
                    &audio_input_devices,
                    audio_output_dropdown_open,
                    audio_input_dropdown_open,
                    record_audio_sample_rate,
                    sample_rate_dropdown_open,
                    record_strip_silent_audio,
                ),
                Tab::Editor => editor_tab::draw_editor(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    sw,
                    &external_editor,
                    session_limit_focus,
                    &session_limit_buf,
                    session_limit_cursor,
                    session_history_limit,
                    text,
                ),
                Tab::Upload => upload_tab::draw_upload(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    sw,
                    &uploaders,
                    editing_uploader,
                    uploader_scroll,
                    upload_field_focus,
                    &upload_field_buf,
                    upload_field_cursor,
                    text,
                    scrollbar_active,
                ),
                Tab::Hotkeys => hotkeys_tab::draw_hotkeys(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    sw,
                    sh,
                    &hotkey,
                    &hotkey_values,
                    &hotkey_modified,
                    global_modified,
                    hotkey_scroll,
                    &hotkey_error,
                    capturing,
                    scrollbar_active,
                ),
                Tab::About => about::draw_about(
                    &mut canvas,
                    t,
                    dark,
                    hover,
                    sw,
                    sh,
                    about_scroll,
                    scrollbar_active,
                    &update_available,
                    update_check_status,
                    updating,
                    &update_install_error,
                ),
            }

            // Buttons (tabs, checkboxes, token fields, and Hotkeys rows are
            // drawn individually above, so excluded here).
            for (btn, rect) in &buttons {
                if matches!(
                    btn,
                    Btn::Tab(_)
                        | Btn::LaunchAtStartup
                        | Btn::ShowCursorInRecording
                        | Btn::ShowClickRipple
                        | Btn::LeftClickColorSwatch
                        | Btn::RightClickColorSwatch
                        | Btn::BitrateDropdown
                        | Btn::BitrateOption(_)
                        | Btn::AudioOutputDropdown
                        | Btn::AudioOutputOption(_)
                        | Btn::AudioInputDropdown
                        | Btn::AudioInputOption(_)
                        | Btn::SampleRateDropdown
                        | Btn::SampleRateOption(_)
                        | Btn::StripSilentAudio
                        | Btn::CheckForUpdates
                        | Btn::DownloadUpdate
                        | Btn::ReportBug
                        | Btn::CaptureLocal(_)
                        | Btn::Capture
                        | Btn::ResetLocal(_)
                        | Btn::ResetGlobal
                        | Btn::RemoveLocalBinding(_, _)
                        | Btn::RemoveGlobalBinding(_)
                        | Btn::OpenSession(_)
                        | Btn::DeleteSession(_)
                        | Btn::SessionLimitField
                        | Btn::SessionLimitStep(_)
                        | Btn::SelectUploader(_)
                        | Btn::DeleteUploader(_)
                        | Btn::ToggleUploaderEnabled(_)
                        | Btn::UploadField(_)
                        | Btn::FilenameFormatField
                        | Btn::MaxResolutionField(_)
                ) {
                    continue;
                }
                let base = if *btn == Btn::Save { SAVE_BG } else { BTN_BG };
                let color = if hover == Some(*btn) {
                    hover_tint(base)
                } else {
                    base
                };
                canvas.fill(*rect, color);
                let label = match btn {
                    Btn::Browse(_) => "Browse...",
                    Btn::DefaultDir(_) => "Default",
                    Btn::BrowseEditor => "Browse...",
                    Btn::AddUploader => "+ Add uploader",
                    Btn::Save => "Save",
                    Btn::Cancel => "Cancel",
                    Btn::Tab(_)
                    | Btn::LaunchAtStartup
                    | Btn::ShowCursorInRecording
                    | Btn::ShowClickRipple
                    | Btn::LeftClickColorSwatch
                    | Btn::RightClickColorSwatch
                    | Btn::BitrateDropdown
                    | Btn::BitrateOption(_)
                    | Btn::AudioOutputDropdown
                    | Btn::AudioOutputOption(_)
                    | Btn::AudioInputDropdown
                    | Btn::AudioInputOption(_)
                    | Btn::SampleRateDropdown
                    | Btn::SampleRateOption(_)
                    | Btn::StripSilentAudio
                    | Btn::CheckForUpdates
                    | Btn::DownloadUpdate
                    | Btn::ReportBug
                    | Btn::CaptureLocal(_)
                    | Btn::Capture
                    | Btn::ResetLocal(_)
                    | Btn::ResetGlobal
                    | Btn::RemoveLocalBinding(_, _)
                    | Btn::RemoveGlobalBinding(_)
                    | Btn::OpenSession(_)
                    | Btn::DeleteSession(_)
                    | Btn::SessionLimitField
                    | Btn::SessionLimitStep(_)
                    | Btn::SelectUploader(_)
                    | Btn::DeleteUploader(_)
                    | Btn::ToggleUploaderEnabled(_)
                    | Btn::UploadField(_)
                    | Btn::FilenameFormatField
                    | Btn::MaxResolutionField(_) => {
                        unreachable!()
                    }
                };
                let tw = t.text_width(label, 15.0);
                let lx = rect.x0 as f32 + ((rect.x1 - rect.x0) as f32 - tw) / 2.0;
                let baseline = t.baseline_for_center((rect.y0 + rect.y1) as f32 / 2.0, 15.0);
                // The Save button's background (SAVE_BG) is a fixed green
                // regardless of theme, so its label color is fixed white too
                // (in the light theme, TEXT is dark and unreadable on green).
                let label_color = if *btn == Btn::Save { 0x00FF_FFFF } else { TEXT };
                t.draw(&mut canvas, lx, baseline, label, 15.0, label_color);
            }

            // Drawn last, after the Save/Cancel bar, so this modal popup
            // renders on top of it instead of being covered (it's a no-op
            // when no picker is open).
            video::draw_click_color_picker(
                &mut canvas,
                sw,
                sh,
                text,
                picker,
                picker_target,
                PICK_BG,
            );
        }

        let _ = buf.present();
    }
}

/// Layout for one General tab "Save to:" row: path field, Browse, Default,
/// in that order. Browse/Default are pinned to the window's right edge; the
/// path field stretches to fill the gap, following window width `sw`.
fn save_row_layout(sw: usize, y: usize) -> (Rect, Rect, Rect) {
    const LABEL_W: usize = 84;
    const BTN_W: usize = 90;
    const DEFAULT_W: usize = 80;
    const GAP: usize = 6;
    const ROW_H: usize = 26;

    let default_rect = field(sw.saturating_sub(20 + DEFAULT_W), y, DEFAULT_W, ROW_H);
    let browse_rect = field(default_rect.x0.saturating_sub(GAP + BTN_W), y, BTN_W, ROW_H);
    let path_x0 = CONTENT_X + LABEL_W;
    let path_rect = field(
        path_x0,
        y,
        browse_rect.x0.saturating_sub(path_x0 + GAP),
        ROW_H,
    );
    (path_rect, browse_rect, default_rect)
}

/// Default vertical margin between stacked rows within a tab (from one
/// row's bottom to the next row's top). Each row-Y constant is computed
/// from the previous row's Y and height via this function, so adding,
/// removing, or resizing a row doesn't require manually recomputing the
/// constants that follow it.
const ROW_MARGIN: usize = 8;

/// The next row's Y, from the previous row's Y and height, using the default margin.
pub(super) const fn next_row_y(prev_y: usize, prev_h: usize) -> usize {
    prev_y + prev_h + ROW_MARGIN
}

/// Like `next_row_y`, plus an extra gap — for rows that need more space than
/// the default margin, e.g. ones with a help line squeezed in.
pub(super) const fn next_row_y_with_extra_gap(prev_y: usize, prev_h: usize, extra: usize) -> usize {
    prev_y + prev_h + ROW_MARGIN + extra
}

fn field(x: usize, y: usize, w: usize, h: usize) -> Rect {
    Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    }
}

/// Shrinks a rect by `n` px on each side (used for the color picker
/// swatch's double border).
fn inset(r: Rect, n: usize) -> Rect {
    Rect {
        x0: r.x0 + n,
        y0: r.y0 + n,
        x1: r.x1.saturating_sub(n),
        y1: r.y1.saturating_sub(n),
    }
}

/// Whether `(x, y)` is inside rect `r` (used for the color picker's SV/Hue
/// hit-testing).
fn inside(r: Rect, x: f64, y: f64) -> bool {
    x >= r.x0 as f64 && x < r.x1 as f64 && y >= r.y0 as f64 && y < r.y1 as f64
}

/// The scrollbar thumb's rect for a scrollable tab (`None` if the content
/// fits within `viewport` without scrolling, i.e. no scrollbar shown).
/// Shared by each tab's draw code and by the drag hit-testing/update logic
/// below, so the two can never drift apart.
pub(super) fn scrollbar_thumb_rect(
    track_x0: usize,
    viewport: Rect,
    content_h: i64,
    scroll: i32,
) -> Option<Rect> {
    let viewport_h = (viewport.y1 - viewport.y0) as i64;
    if content_h <= viewport_h {
        return None;
    }
    let thumb_h = ((viewport_h * viewport_h / content_h).max(20)) as usize;
    let max_scroll = (content_h - viewport_h).max(1);
    let thumb_y = viewport.y0
        + ((scroll as i64 * (viewport_h - thumb_h as i64).max(0)) / max_scroll) as usize;
    Some(field(track_x0, thumb_y, 4, thumb_h))
}

/// Like `Canvas::stroke`, but the top/bottom edges are only drawn when
/// `draw_top`/`draw_bottom` is true (left/right are always drawn) — used to
/// avoid a stray line at the clip boundary when a Hotkeys tab row is
/// clamped by scrolling.
fn stroke_top_bottom_aware(
    canvas: &mut Canvas,
    r: Rect,
    draw_top: bool,
    draw_bottom: bool,
    color: u32,
) {
    // Bypasses `Canvas::stroke` (to support the draw_top/draw_bottom
    // split), so it has to scale `r` to physical pixels itself, the same
    // way `Canvas`'s own shape-drawing methods do.
    let s = |v: usize| ((v as f64) * canvas.scale).round() as usize;
    let r = Rect {
        x0: s(r.x0),
        y0: s(r.y0),
        x1: s(r.x1),
        y1: s(r.y1),
    };
    if draw_top {
        for x in r.x0..r.x1 {
            canvas.set(x, r.y0, color);
        }
    }
    if draw_bottom && r.y1 > 0 {
        for x in r.x0..r.x1 {
            canvas.set(x, r.y1 - 1, color);
        }
    }
    for y in r.y0..r.y1 {
        canvas.set(r.x0, y, color);
        if r.x1 > 0 {
            canvas.set(r.x1 - 1, y, color);
        }
    }
}

fn lighten(c: u32) -> u32 {
    let ch = |sh: u32| (((c >> sh) & 0xff) + 40).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The inverse of `lighten` (for hover emphasis in the light theme). Darkens
/// each channel equally (`saturating_sub` keeps it from going below 0).
fn darken(c: u32) -> u32 {
    let ch = |sh: u32| -> u32 { ((c >> sh) & 0xff).saturating_sub(40) };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The dark/light theme color set, in order: BG, SIDEBAR_BG, FIELD_BG,
/// BTN_BG, TEXT, DIM, UPLOADER_ACTIVE_BG, TEXT_SELECTION_BG, PICK_BG,
/// VERY_DIM, SWATCH_HOVER. Used by both `draw()` and the per-tab drawing
/// functions — the latter take only `dark` and rebuild the same tuple here,
/// so the color-constant references don't need renaming.
#[allow(clippy::type_complexity)]
pub(super) fn theme_colors(dark: bool) -> (u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, u32) {
    if dark {
        (
            0x001E_1E1Eu32,
            0x0018_1818,
            0x0029_2929,
            0x0033_3333,
            0x00EA_EAEA,
            0x0099_9999,
            0x0025_3B52,
            0x0026_4F73,
            0x0022_2222,
            0x0044_4444,
            0x00C0_C0C0,
        )
    } else {
        (
            0x00F5_F5F5u32,
            0x00E8_E8E8,
            0x00FF_FFFF,
            0x00E0_E0E0,
            0x0020_2020,
            0x0070_7070,
            0x00D6_E8F7,
            0x00BF_DFF5,
            0x00F0_F0F0,
            0x00B0_B0B0,
            0x0060_6060,
        )
    }
}

/// The hover-emphasis direction (lighten in dark theme, darken in light).
pub(super) fn hover_tint_for(c: u32, dark: bool) -> u32 {
    if dark { lighten(c) } else { darken(c) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_row_layout_stretches_path_field_and_anchors_buttons_to_right_edge() {
        let (path_a, browse_a, default_a) = save_row_layout(WIN_W, 88);
        let (path_b, browse_b, default_b) = save_row_layout(WIN_W + 300, 88);
        // A wider window gives the path field more room.
        assert!(path_b.width() > path_a.width());
        // Browse/Default follow the window's right edge (their absolute x moves right).
        assert!(browse_b.x0 > browse_a.x0);
        assert!(default_b.x0 > default_a.x0);
        // The three rects don't overlap.
        assert!(path_a.x1 <= browse_a.x0);
        assert!(browse_a.x1 <= default_a.x0);
    }

    #[test]
    fn scrollbar_thumb_rect_is_none_when_content_fits_the_viewport() {
        let viewport = Rect {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 200,
        };
        assert!(scrollbar_thumb_rect(90, viewport, 150, 0).is_none());
    }

    #[test]
    fn scrollbar_thumb_rect_spans_the_track_proportionally_at_min_and_max_scroll() {
        let viewport = Rect {
            x0: 0,
            y0: 10,
            x1: 100,
            y1: 210,
        };
        let content_h = 400; // Double the 200px viewport.
        let at_top = scrollbar_thumb_rect(90, viewport, content_h, 0).unwrap();
        assert_eq!(at_top.y0, viewport.y0);
        let max_scroll = (content_h - 200) as i32;
        let at_bottom = scrollbar_thumb_rect(90, viewport, content_h, max_scroll).unwrap();
        assert_eq!(at_bottom.y1, viewport.y1);
    }
}
