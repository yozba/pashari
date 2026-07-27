//! Video tab: MP4/GIF save location, resolution cap, bitrate, audio
//! devices, sample rate, checkboxes, click-ripple color picker.

use winit::keyboard::{Key, NamedKey};

use super::{
    ACCENT, Btn, CONTENT_X, SAVE_KINDS, SaveKind, Settings, SettingsResult, TextCursor, WIN_H,
    apply_common_edit_key, char_index_for_x, field, hover_tint_for, inset, next_row_y,
    save_row_layout, theme_colors, x_for_char_index,
};
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, PickerPart, Rect, hsv_to_rgb, marker, rgb_to_hsv};

/// Shared width and right-edge margin for Video tab value controls
/// (dropdowns/fields/checkboxes); labels vary in length but controls all
/// align to the same width and right edge.
const VIDEO_VALUE_W: usize = 220;
const VIDEO_VALUE_MARGIN: usize = 20;

/// MP4/GIF save rows; PNG stays on the General/Capture tab.
const VIDEO_SAVE_MP4_ROW_Y: usize = 88;
const VIDEO_SAVE_ROW_H: usize = 26;
const VIDEO_SAVE_GIF_ROW_Y: usize = next_row_y(VIDEO_SAVE_MP4_ROW_Y, VIDEO_SAVE_ROW_H);

/// Fixed-width value rect, right-aligned.
fn video_value_rect(sw: usize, y: usize, h: usize) -> Rect {
    let x1 = sw.saturating_sub(VIDEO_VALUE_MARGIN);
    field(x1.saturating_sub(VIDEO_VALUE_W), y, VIDEO_VALUE_W, h)
}

/// Checkbox row hit target: spans the full row so clicking the label also
/// toggles it; the label draws at CONTENT_X and the box at the right edge.
fn video_row_rect(sw: usize, y: usize, h: usize) -> Rect {
    field(
        CONTENT_X,
        y,
        sw.saturating_sub(VIDEO_VALUE_MARGIN + CONTENT_X),
        h,
    )
}

/// Shared height for the checkbox rows (Show cursor / Show ripple / Strip
/// silent audio).
const CHECKBOX_ROW_H: usize = 30;

const SHOW_CURSOR_ROW_Y: usize = next_row_y(STRIP_SILENT_AUDIO_ROW_Y, CHECKBOX_ROW_H);
const SHOW_RIPPLE_ROW_Y: usize = next_row_y(SHOW_CURSOR_ROW_Y, CHECKBOX_ROW_H);

/// Left/right click-ripple color swatches; the whole row only shows while
/// ripples are enabled.
const CLICK_COLOR_SWATCH_SIZE: usize = 24;
const CLICK_COLOR_LABEL_GAP: usize = 8;
const CLICK_COLOR_LABEL_W_FALLBACK: usize = 40;
const CLICK_COLOR_GROUP_GAP: usize = 20;
const RIPPLE_COLOR_LABEL: &str = "Ripple color:";
const LEFT_CLICK_COLOR_LABEL: &str = "Left";
const RIGHT_CLICK_COLOR_LABEL: &str = "Right";
const CLICK_COLOR_ROW_Y: usize = next_row_y(SHOW_RIPPLE_ROW_Y, CHECKBOX_ROW_H);

/// Color picker popup dimensions (SV square + hue bar).
const PICK_SV: usize = 128;
const PICK_HUE_H: usize = 14;
const PICK_PAD: usize = 8;

/// Which swatch the color picker is currently editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ClickColorTarget {
    Left,
    Right,
}

/// Measured label width + gap (fixed fallback if no font yet).
fn click_color_label_w(text: Option<&TextRenderer>, label: &str) -> usize {
    text.map(|tr| tr.text_width(label, 15.0).ceil() as usize + CLICK_COLOR_LABEL_GAP)
        .unwrap_or(CLICK_COLOR_LABEL_W_FALLBACK)
}

/// Start x of the Left/Right label+swatch row, aligned with other value
/// columns.
fn click_color_group_x0(sw: usize) -> usize {
    video_value_rect(sw, CLICK_COLOR_ROW_Y, CLICK_COLOR_SWATCH_SIZE).x0
}

fn left_click_color_swatch_rect(sw: usize, text: Option<&TextRenderer>) -> Rect {
    let x0 = click_color_group_x0(sw) + click_color_label_w(text, LEFT_CLICK_COLOR_LABEL);
    field(
        x0,
        CLICK_COLOR_ROW_Y,
        CLICK_COLOR_SWATCH_SIZE,
        CLICK_COLOR_SWATCH_SIZE,
    )
}

fn right_click_color_swatch_rect(sw: usize, text: Option<&TextRenderer>) -> Rect {
    let x0 = left_click_color_swatch_rect(sw, text).x1
        + CLICK_COLOR_GROUP_GAP
        + click_color_label_w(text, RIGHT_CLICK_COLOR_LABEL);
    field(
        x0,
        CLICK_COLOR_ROW_Y,
        CLICK_COLOR_SWATCH_SIZE,
        CLICK_COLOR_SWATCH_SIZE,
    )
}

/// Popup/SV-square/hue-bar rects, positioned just below the target swatch.
pub(super) fn click_color_picker_geom(
    sw: usize,
    text: Option<&TextRenderer>,
    target: ClickColorTarget,
) -> (Rect, Rect, Rect) {
    let swatch = match target {
        ClickColorTarget::Left => left_click_color_swatch_rect(sw, text),
        ClickColorTarget::Right => right_click_color_swatch_rect(sw, text),
    };
    let px = swatch.x0;
    let py = swatch.y1 + 4;
    let popup = field(
        px,
        py,
        PICK_SV + 2 * PICK_PAD,
        PICK_SV + PICK_HUE_H + 3 * PICK_PAD,
    );
    let sv = field(px + PICK_PAD, py + PICK_PAD, PICK_SV, PICK_SV);
    let hue = field(px + PICK_PAD, sv.y1 + PICK_PAD, PICK_SV, PICK_HUE_H);
    (popup, sv, hue)
}

/// The rect for option `i` in a stacked list of `count` items of height
/// `item_h`, normally stacked below `closed`. Flips to sit above `closed`
/// instead if the full list wouldn't fit within the window (mirrors the
/// action menu's below/above fallback in `overlay::menu`) — otherwise a
/// dropdown near the bottom of the tab would draw (and hit-test) past the
/// window edge.
///
/// Left-aligned to `closed`'s left edge, like a normal dropdown, unless
/// that would run the list past the window's current right edge (`sw`) —
/// the audio-device lists can be wider than the closed button to fit a
/// long device name (`audio_dropdown_option_w`). In that case it grows
/// left from `closed`'s right edge instead, staying on-screen. A no-op
/// for the fixed-width bitrate/sample-rate lists, where `w` always equals
/// the closed button's own width and both anchors agree.
fn stacked_option_rect(
    closed: Rect,
    item_h: usize,
    count: usize,
    i: usize,
    w: usize,
    sw: usize,
) -> Rect {
    let total_h = count * item_h;
    let y0 = if closed.y1 + total_h <= WIN_H {
        closed.y1 + i * item_h
    } else {
        closed.y0.saturating_sub(total_h) + i * item_h
    };
    let x0 = if closed.x0 + w <= sw {
        closed.x0
    } else {
        closed.x1.saturating_sub(w)
    };
    field(x0, y0, w, item_h)
}

const BITRATE_PRESETS: [(u32, &str); 3] = [(8, "Low"), (15, "Medium"), (30, "High")];

const BITRATE_LABEL: &str = "Video bitrate:";
const BITRATE_ROW_Y: usize = next_row_y(MAX_HEIGHT_ROW_Y, MAX_RESOLUTION_FIELD_H);
const BITRATE_DROPDOWN_H: usize = 28;

fn bitrate_dropdown_rect(sw: usize) -> Rect {
    video_value_rect(sw, BITRATE_ROW_Y, BITRATE_DROPDOWN_H)
}

/// Option `i` rect when open (see `stacked_option_rect`).
fn bitrate_option_rect(sw: usize, i: usize) -> Rect {
    let closed = bitrate_dropdown_rect(sw);
    stacked_option_rect(
        closed,
        BITRATE_DROPDOWN_H,
        BITRATE_PRESETS.len(),
        i,
        VIDEO_VALUE_W,
        sw,
    )
}

const AUDIO_SAMPLE_RATE_PRESETS: [(u32, &str); 2] = [(44_100, "44.1 kHz"), (48_000, "48 kHz")];

const SAMPLE_RATE_LABEL: &str = "Audio sample rate:";
const SAMPLE_RATE_ROW_Y: usize = next_row_y(AUDIO_INPUT_ROW_Y, AUDIO_DROPDOWN_H);
const SAMPLE_RATE_DROPDOWN_H: usize = 28;

fn sample_rate_dropdown_rect(sw: usize) -> Rect {
    video_value_rect(sw, SAMPLE_RATE_ROW_Y, SAMPLE_RATE_DROPDOWN_H)
}

/// Option `i` rect when open (see `stacked_option_rect`).
fn sample_rate_option_rect(sw: usize, i: usize) -> Rect {
    let closed = sample_rate_dropdown_rect(sw);
    stacked_option_rect(
        closed,
        SAMPLE_RATE_DROPDOWN_H,
        AUDIO_SAMPLE_RATE_PRESETS.len(),
        i,
        VIDEO_VALUE_W,
        sw,
    )
}

const STRIP_SILENT_AUDIO_ROW_Y: usize = next_row_y(SAMPLE_RATE_ROW_Y, SAMPLE_RATE_DROPDOWN_H);

/// Audio device dropdown layout: the closed button is fixed-width (clipped
/// if text overflows); the open option list fits its content instead.
const AUDIO_DROPDOWN_OPTION_PAD: usize = 16;
const AUDIO_DROPDOWN_H: usize = 28;

const AUDIO_OUTPUT_LABEL: &str = "Desktop audio device:";
const AUDIO_OUTPUT_ROW_Y: usize = next_row_y(BITRATE_ROW_Y, BITRATE_DROPDOWN_H);
const AUDIO_INPUT_LABEL: &str = "Microphone device:";
const AUDIO_INPUT_ROW_Y: usize = next_row_y(AUDIO_OUTPUT_ROW_Y, AUDIO_DROPDOWN_H);

/// Empty name (the system-default sentinel) displays as "System default".
fn audio_device_display_name(name: &str) -> &str {
    if name.is_empty() {
        "System default"
    } else {
        name
    }
}

/// Width fits the longest device name (never narrower than the closed
/// button); always measured against the full list so it doesn't jitter as
/// the selection changes.
fn audio_dropdown_option_w(text: Option<&TextRenderer>, devices: &[String]) -> usize {
    let Some(tr) = text else {
        return VIDEO_VALUE_W;
    };
    let max_text_w = devices
        .iter()
        .map(|d| tr.text_width(audio_device_display_name(d), 15.0).ceil() as usize)
        .max()
        .unwrap_or(0);
    (max_text_w + AUDIO_DROPDOWN_OPTION_PAD).max(VIDEO_VALUE_W)
}

fn audio_output_dropdown_rect(sw: usize) -> Rect {
    video_value_rect(sw, AUDIO_OUTPUT_ROW_Y, AUDIO_DROPDOWN_H)
}

/// Option `i` rect when open (see `stacked_option_rect`); width fits its content.
fn audio_output_option_rect(
    sw: usize,
    text: Option<&TextRenderer>,
    devices: &[String],
    i: usize,
) -> Rect {
    let closed = audio_output_dropdown_rect(sw);
    let w = audio_dropdown_option_w(text, devices);
    stacked_option_rect(closed, AUDIO_DROPDOWN_H, devices.len(), i, w, sw)
}

fn audio_input_dropdown_rect(sw: usize) -> Rect {
    video_value_rect(sw, AUDIO_INPUT_ROW_Y, AUDIO_DROPDOWN_H)
}

fn audio_input_option_rect(
    sw: usize,
    text: Option<&TextRenderer>,
    devices: &[String],
    i: usize,
) -> Rect {
    let closed = audio_input_dropdown_rect(sw);
    let w = audio_dropdown_option_w(text, devices);
    stacked_option_rect(closed, AUDIO_DROPDOWN_H, devices.len(), i, w, sw)
}

/// Max width/height rows: label + numeric field, no ± stepper (not useful
/// at 1px granularity).
const MAX_RESOLUTION_FIELD_H: usize = 26;
const MAX_WIDTH_LABEL: &str = "Max width (px, 0=unlimited):";
const MAX_WIDTH_ROW_Y: usize = next_row_y(VIDEO_SAVE_GIF_ROW_Y, VIDEO_SAVE_ROW_H);
const MAX_HEIGHT_LABEL: &str = "Max height (px, 0=unlimited):";
const MAX_HEIGHT_ROW_Y: usize = next_row_y(MAX_WIDTH_ROW_Y, MAX_RESOLUTION_FIELD_H);

/// Which resolution-cap field is focused; shares one edit buffer between
/// the two (same pattern as `UploadField`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MaxResDim {
    Width,
    Height,
}

impl MaxResDim {
    fn row_y(self) -> usize {
        match self {
            MaxResDim::Width => MAX_WIDTH_ROW_Y,
            MaxResDim::Height => MAX_HEIGHT_ROW_Y,
        }
    }
}

fn max_resolution_row_layout(sw: usize, dim: MaxResDim) -> Rect {
    video_value_rect(sw, dim.row_y(), MAX_RESOLUTION_FIELD_H)
}

/// Parse and clamp the buffer; falls back to `current` if empty/invalid.
/// Unlike the session limit, 0 (disabled) is allowed, so there's no lower
/// clamp.
fn parse_max_resolution(buf: &str, current: u32) -> u32 {
    match buf.trim().parse::<u32>() {
        Ok(v) => v.min(7680),
        Err(_) => current,
    }
}

impl Settings {
    pub(super) fn buttons_video(&self, sw: usize) -> Vec<(Btn, Rect)> {
        let mut v = Vec::new();
        // Open dropdown options are drawn on top of everything else, so
        // they must be registered first here too (`button_at` picks the
        // first match). Otherwise a lower row could win the hit test
        // despite being drawn underneath.
        if self.bitrate_dropdown_open {
            for (i, (val, _)) in BITRATE_PRESETS.into_iter().enumerate() {
                v.push((Btn::BitrateOption(val), bitrate_option_rect(sw, i)));
            }
        }
        if self.audio_output_dropdown_open {
            for i in 0..self.audio_output_devices.len() {
                v.push((
                    Btn::AudioOutputOption(i),
                    audio_output_option_rect(sw, self.text.as_ref(), &self.audio_output_devices, i),
                ));
            }
        }
        if self.audio_input_dropdown_open {
            for i in 0..self.audio_input_devices.len() {
                v.push((
                    Btn::AudioInputOption(i),
                    audio_input_option_rect(sw, self.text.as_ref(), &self.audio_input_devices, i),
                ));
            }
        }
        if self.sample_rate_dropdown_open {
            for (i, (val, _)) in AUDIO_SAMPLE_RATE_PRESETS.into_iter().enumerate() {
                v.push((Btn::SampleRateOption(val), sample_rate_option_rect(sw, i)));
            }
        }

        for (kind, y) in [
            (SaveKind::Mp4, VIDEO_SAVE_MP4_ROW_Y),
            (SaveKind::Gif, VIDEO_SAVE_GIF_ROW_Y),
        ] {
            let (_, browse_rect, default_rect) = save_row_layout(sw, y);
            v.push((Btn::Browse(kind), browse_rect));
            v.push((Btn::DefaultDir(kind), default_rect));
        }
        v.push((
            Btn::ShowCursorInRecording,
            video_row_rect(sw, SHOW_CURSOR_ROW_Y, CHECKBOX_ROW_H),
        ));
        v.push((
            Btn::ShowClickRipple,
            video_row_rect(sw, SHOW_RIPPLE_ROW_Y, CHECKBOX_ROW_H),
        ));
        if self.record_show_click_ripple {
            v.push((
                Btn::LeftClickColorSwatch,
                left_click_color_swatch_rect(sw, self.text.as_ref()),
            ));
            v.push((
                Btn::RightClickColorSwatch,
                right_click_color_swatch_rect(sw, self.text.as_ref()),
            ));
        }
        v.push((Btn::BitrateDropdown, bitrate_dropdown_rect(sw)));
        v.push((Btn::AudioOutputDropdown, audio_output_dropdown_rect(sw)));
        v.push((Btn::AudioInputDropdown, audio_input_dropdown_rect(sw)));
        v.push((
            Btn::MaxResolutionField(MaxResDim::Width),
            max_resolution_row_layout(sw, MaxResDim::Width),
        ));
        v.push((
            Btn::MaxResolutionField(MaxResDim::Height),
            max_resolution_row_layout(sw, MaxResDim::Height),
        ));
        v.push((
            Btn::StripSilentAudio,
            video_row_rect(sw, STRIP_SILENT_AUDIO_ROW_Y, CHECKBOX_ROW_H),
        ));
        v.push((Btn::SampleRateDropdown, sample_rate_dropdown_rect(sw)));
        v
    }

    pub(super) fn activate_video(&mut self, btn: Btn) -> Option<SettingsResult> {
        match btn {
            Btn::MaxResolutionField(dim) => {
                self.focus_max_resolution(dim);
                None
            }
            Btn::BitrateDropdown => {
                self.bitrate_dropdown_open = !self.bitrate_dropdown_open;
                self.request_redraw();
                None
            }
            Btn::BitrateOption(v) => {
                self.record_bitrate_mbps = v;
                self.bitrate_dropdown_open = false;
                self.request_redraw();
                None
            }
            Btn::AudioOutputDropdown => {
                self.audio_output_dropdown_open = !self.audio_output_dropdown_open;
                self.request_redraw();
                None
            }
            Btn::AudioOutputOption(i) => {
                if let Some(name) = self.audio_output_devices.get(i) {
                    self.record_audio_output_device = name.clone();
                }
                self.audio_output_dropdown_open = false;
                self.request_redraw();
                None
            }
            Btn::AudioInputDropdown => {
                self.audio_input_dropdown_open = !self.audio_input_dropdown_open;
                self.request_redraw();
                None
            }
            Btn::AudioInputOption(i) => {
                if let Some(name) = self.audio_input_devices.get(i) {
                    self.record_audio_input_device = name.clone();
                }
                self.audio_input_dropdown_open = false;
                self.request_redraw();
                None
            }
            Btn::SampleRateDropdown => {
                self.sample_rate_dropdown_open = !self.sample_rate_dropdown_open;
                self.request_redraw();
                None
            }
            Btn::SampleRateOption(v) => {
                self.record_audio_sample_rate = v;
                self.sample_rate_dropdown_open = false;
                self.request_redraw();
                None
            }
            Btn::StripSilentAudio => {
                self.record_strip_silent_audio = !self.record_strip_silent_audio;
                self.request_redraw();
                None
            }
            Btn::ShowCursorInRecording => {
                self.record_show_cursor = !self.record_show_cursor;
                self.request_redraw();
                None
            }
            Btn::ShowClickRipple => {
                self.record_show_click_ripple = !self.record_show_click_ripple;
                if !self.record_show_click_ripple {
                    // The swatch row disappears immediately, so close any
                    // open picker before it's orphaned.
                    self.picker = None;
                    self.picker_target = None;
                }
                self.request_redraw();
                None
            }
            Btn::LeftClickColorSwatch => {
                self.toggle_click_color_picker(ClickColorTarget::Left);
                None
            }
            Btn::RightClickColorSwatch => {
                self.toggle_click_color_picker(ClickColorTarget::Right);
                None
            }
            _ => None,
        }
    }

    pub(super) fn on_max_resolution_key(&mut self, event: &winit::event::KeyEvent) {
        if apply_common_edit_key(
            &mut self.max_resolution_cursor,
            &mut self.max_resolution_buf,
            event,
            self.mods,
        ) {
            self.request_redraw();
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => self.commit_max_resolution(),
            Key::Named(NamedKey::Escape) => {
                self.max_resolution_focus = None;
                self.max_resolution_buf.clear();
                self.max_resolution_cursor = TextCursor::default();
                self.request_redraw();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("c") => {
                self.copy_max_resolution_selection();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("x") => {
                self.copy_max_resolution_selection();
                self.max_resolution_cursor
                    .delete_selection(&mut self.max_resolution_buf);
                self.request_redraw();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("v") => {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                    if !digits.is_empty() {
                        self.max_resolution_cursor
                            .insert(&mut self.max_resolution_buf, &digits);
                        self.request_redraw();
                    }
                }
            }
            // Other Ctrl shortcuts aren't typed as text.
            Key::Character(_) if self.mods.control_key() => {}
            Key::Character(s) if s.chars().all(|c| c.is_ascii_digit()) => {
                self.max_resolution_cursor
                    .insert(&mut self.max_resolution_buf, s);
                self.request_redraw();
            }
            _ => {}
        }
    }

    /// Focus the field for `dim` and load its current value into the buffer.
    fn focus_max_resolution(&mut self, dim: MaxResDim) {
        let current = match dim {
            MaxResDim::Width => self.record_max_width,
            MaxResDim::Height => self.record_max_height,
        };
        self.max_resolution_buf = current.to_string();
        self.max_resolution_cursor = TextCursor::at_end(&self.max_resolution_buf);
        self.max_resolution_focus = Some(dim);
        self.request_redraw();
    }

    /// Centered text draw-start x for the currently focused field (matches
    /// `draw`'s centering math).
    pub(super) fn max_resolution_text_x0(&self) -> f32 {
        let dim = self.max_resolution_focus.unwrap_or(MaxResDim::Width);
        let field_rect = max_resolution_row_layout(self.size.0, dim);
        let tw = self
            .text
            .as_ref()
            .map(|tr| tr.text_width(&self.max_resolution_buf, 15.0))
            .unwrap_or(0.0);
        field_rect.x0 as f32 + (field_rect.width() as f32 - tw) / 2.0
    }

    /// Mouse press: focus this field if needed, place caret at click, start
    /// drag-select.
    pub(super) fn begin_max_resolution_press(&mut self, dim: MaxResDim, click_x: f64) {
        if self.max_resolution_focus != Some(dim) {
            self.focus_max_resolution(dim);
        }
        let rel_x = click_x as f32 - self.max_resolution_text_x0();
        let idx = char_index_for_x(self.text.as_ref(), &self.max_resolution_buf, 15.0, rel_x);
        self.max_resolution_cursor.set_from_click(idx, false);
        self.text_drag = true;
        self.request_redraw();
    }

    /// Commit the focused field (parse, clamp, apply). No-op if unfocused,
    /// so it's safe to call unconditionally.
    pub(super) fn commit_max_resolution(&mut self) {
        let Some(dim) = self.max_resolution_focus.take() else {
            return;
        };
        match dim {
            MaxResDim::Width => {
                self.record_max_width =
                    parse_max_resolution(&self.max_resolution_buf, self.record_max_width);
            }
            MaxResDim::Height => {
                self.record_max_height =
                    parse_max_resolution(&self.max_resolution_buf, self.record_max_height);
            }
        }
        self.max_resolution_buf.clear();
        self.max_resolution_cursor = TextCursor::default();
        self.request_redraw();
    }

    /// Copy the current selection to the clipboard, if any.
    fn copy_max_resolution_selection(&self) {
        if let Some((lo, hi)) = self.max_resolution_cursor.selection()
            && let Ok(mut clip) = arboard::Clipboard::new()
        {
            let _ = clip.set_text(self.max_resolution_buf[lo..hi].to_string());
        }
    }

    /// Toggle the picker for `target`; closes if already open for it,
    /// otherwise opens from the current color's HSV.
    fn toggle_click_color_picker(&mut self, target: ClickColorTarget) {
        if self.picker_target == Some(target) {
            self.picker = None;
            self.picker_target = None;
        } else {
            let current = match target {
                ClickColorTarget::Left => self.record_click_color_left,
                ClickColorTarget::Right => self.record_click_color_right,
            };
            self.picker = Some(rgb_to_hsv(current));
            self.picker_target = Some(target);
        }
        self.request_redraw();
    }

    /// Update HSV from the drag position and apply it to the target color.
    pub(super) fn apply_click_color_picker(&mut self, wx: f64, wy: f64) {
        let (Some((h, s, v)), Some(part), Some(target)) =
            (self.picker, self.picker_drag, self.picker_target)
        else {
            return;
        };
        let (_, sv_rect, hue_rect) =
            click_color_picker_geom(self.size.0, self.text.as_ref(), target);
        let (nh, ns, nv) = match part {
            PickerPart::Sv => {
                let ns = ((wx - sv_rect.x0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32;
                let nv = 1.0 - ((wy - sv_rect.y0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32;
                (h, ns, nv)
            }
            PickerPart::Hue => {
                let nh =
                    ((wx - hue_rect.x0 as f64) / PICK_SV as f64).clamp(0.0, 1.0) as f32 * 360.0;
                (nh, s, v)
            }
        };
        self.picker = Some((nh, ns, nv));
        let c = hsv_to_rgb(nh, ns, nv);
        match target {
            ClickColorTarget::Left => self.record_click_color_left = c,
            ClickColorTarget::Right => self.record_click_color_right = c,
        }
        self.request_redraw();
    }
}

#[allow(non_snake_case, unused_variables, clippy::too_many_arguments)]
pub(super) fn draw_video(
    canvas: &mut Canvas,
    t: &TextRenderer,
    dark: bool,
    hover: Option<Btn>,
    buttons: &[(Btn, Rect)],
    sw: usize,
    text: Option<&TextRenderer>,
    save_dirs: &[String; 3],
    record_show_cursor: bool,
    record_show_click_ripple: bool,
    record_click_color_left: u32,
    record_click_color_right: u32,
    picker: Option<(f32, f32, f32)>,
    picker_target: Option<ClickColorTarget>,
    record_bitrate_mbps: u32,
    bitrate_dropdown_open: bool,
    record_max_width: u32,
    record_max_height: u32,
    max_resolution_focus: Option<MaxResDim>,
    max_resolution_buf: &str,
    max_resolution_cursor: TextCursor,
    record_audio_output_device: &str,
    record_audio_input_device: &str,
    audio_output_devices: &[String],
    audio_input_devices: &[String],
    audio_output_dropdown_open: bool,
    audio_input_dropdown_open: bool,
    record_audio_sample_rate: u32,
    sample_rate_dropdown_open: bool,
    record_strip_silent_audio: bool,
) {
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
    let hover_tint = |c: u32| hover_tint_for(c, dark);

    t.draw(canvas, CONTENT_X as f32, 72.0, "Video:", 15.0, DIM);

    for (kind_idx, y) in [(1, VIDEO_SAVE_MP4_ROW_Y), (2, VIDEO_SAVE_GIF_ROW_Y)] {
        let (_, label) = SAVE_KINDS[kind_idx];
        let (path_rect, _, _) = save_row_layout(sw, y);
        let baseline = t.baseline_for_center((y + 13) as f32, 15.0);
        t.draw(canvas, CONTENT_X as f32, baseline, label, 15.0, DIM);

        let save_dir = &save_dirs[kind_idx];
        let path_label = if save_dir.is_empty() {
            "(default: Pictures/pashari)".to_string()
        } else {
            save_dir.clone()
        };
        canvas.fill(path_rect, FIELD_BG);
        let path_color = if save_dir.is_empty() { DIM } else { TEXT };
        let path_baseline = t.baseline_for_center((path_rect.y0 + path_rect.y1) as f32 / 2.0, 15.0);
        t.draw(
            canvas,
            path_rect.x0 as f32 + 8.0,
            path_baseline,
            &path_label,
            15.0,
            path_color,
        );
    }

    // Show-cursor checkbox; box aligns with the dropdown column, label at
    // CONTENT_X.
    let (_, cb_rect) = buttons
        .iter()
        .find(|(b, _)| *b == Btn::ShowCursorInRecording)
        .expect("ShowCursorInRecording は Video タブに常に存在する");
    let box_size = 18;
    let box_y = cb_rect.y0 + (cb_rect.height() - box_size) / 2;
    let value_x0 = video_value_rect(sw, SHOW_CURSOR_ROW_Y, CHECKBOX_ROW_H).x0;
    let box_rect = Rect {
        x0: value_x0,
        y0: box_y,
        x1: value_x0 + box_size,
        y1: box_y + box_size,
    };
    canvas.fill(box_rect, if record_show_cursor { ACCENT } else { FIELD_BG });
    canvas.stroke(
        box_rect,
        if hover == Some(Btn::ShowCursorInRecording) {
            ACCENT
        } else {
            0x0080_8080
        },
    );
    if record_show_cursor {
        // Checkmark (two short strokes).
        let (x0, y0, x1, y1) = (
            box_rect.x0 as i64,
            box_rect.y0 as i64,
            box_rect.x1 as i64,
            box_rect.y1 as i64,
        );
        canvas.line(x0 + 3, y0 + 9, x0 + 7, y1 - 4, 2, 0x00FF_FFFF);
        canvas.line(x0 + 7, y1 - 4, x1 - 3, y0 + 3, 2, 0x00FF_FFFF);
    }
    let baseline = t.baseline_for_center((cb_rect.y0 + cb_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        baseline,
        "Show mouse cursor in recordings",
        15.0,
        DIM,
    );

    // Show-click-ripple checkbox (same layout as above).
    let (_, ripple_rect) = buttons
        .iter()
        .find(|(b, _)| *b == Btn::ShowClickRipple)
        .expect("ShowClickRipple は Video タブに常に存在する");
    let ripple_box_y = ripple_rect.y0 + (ripple_rect.height() - box_size) / 2;
    let ripple_value_x0 = video_value_rect(sw, SHOW_RIPPLE_ROW_Y, CHECKBOX_ROW_H).x0;
    let ripple_box_rect = Rect {
        x0: ripple_value_x0,
        y0: ripple_box_y,
        x1: ripple_value_x0 + box_size,
        y1: ripple_box_y + box_size,
    };
    canvas.fill(
        ripple_box_rect,
        if record_show_click_ripple {
            ACCENT
        } else {
            FIELD_BG
        },
    );
    canvas.stroke(
        ripple_box_rect,
        if hover == Some(Btn::ShowClickRipple) {
            ACCENT
        } else {
            0x0080_8080
        },
    );
    if record_show_click_ripple {
        // Checkmark (two short strokes).
        let (x0, y0, x1, y1) = (
            ripple_box_rect.x0 as i64,
            ripple_box_rect.y0 as i64,
            ripple_box_rect.x1 as i64,
            ripple_box_rect.y1 as i64,
        );
        canvas.line(x0 + 3, y0 + 9, x0 + 7, y1 - 4, 2, 0x00FF_FFFF);
        canvas.line(x0 + 7, y1 - 4, x1 - 3, y0 + 3, 2, 0x00FF_FFFF);
    }
    let ripple_baseline =
        t.baseline_for_center((ripple_rect.y0 + ripple_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        ripple_baseline,
        "Show click ripples",
        15.0,
        DIM,
    );

    // Left/right click ripple color swatches (click to open a color picker).
    // Row only shows while "Show click ripples" is on.
    if record_show_click_ripple {
        let row_baseline = t.baseline_for_center(
            (CLICK_COLOR_ROW_Y + CLICK_COLOR_SWATCH_SIZE / 2) as f32,
            15.0,
        );
        t.draw(
            canvas,
            CONTENT_X as f32,
            row_baseline,
            RIPPLE_COLOR_LABEL,
            15.0,
            DIM,
        );
        for (label, current, target, btn) in [
            (
                LEFT_CLICK_COLOR_LABEL,
                record_click_color_left,
                ClickColorTarget::Left,
                Btn::LeftClickColorSwatch,
            ),
            (
                RIGHT_CLICK_COLOR_LABEL,
                record_click_color_right,
                ClickColorTarget::Right,
                Btn::RightClickColorSwatch,
            ),
        ] {
            let r = match target {
                ClickColorTarget::Left => left_click_color_swatch_rect(sw, text),
                ClickColorTarget::Right => right_click_color_swatch_rect(sw, text),
            };
            let label_x0 = r.x0 as f32 - click_color_label_w(text, label) as f32;
            let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
            t.draw(canvas, label_x0, baseline, label, 15.0, DIM);
            canvas.fill(r, current);
            let open = picker_target == Some(target);
            canvas.stroke(
                r,
                if open {
                    ACCENT
                } else if hover == Some(btn) {
                    SWATCH_HOVER
                } else {
                    0x0080_8080
                },
            );
            if open {
                canvas.stroke(inset(r, 1), ACCENT);
            }
        }
    }

    // Bitrate dropdown (Low/Medium/High).
    let closed_rect = bitrate_dropdown_rect(sw);
    let row_baseline = t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        row_baseline,
        BITRATE_LABEL,
        15.0,
        DIM,
    );

    canvas.fill(closed_rect, FIELD_BG);
    canvas.stroke(
        closed_rect,
        if hover == Some(Btn::BitrateDropdown) || bitrate_dropdown_open {
            ACCENT
        } else {
            0x0080_8080
        },
    );
    let current_label = BITRATE_PRESETS
        .iter()
        .find(|(v, _)| *v == record_bitrate_mbps)
        .map(|(_, label)| *label)
        .unwrap_or("Custom");
    let label_baseline =
        t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        closed_rect.x0 as f32 + 8.0,
        label_baseline,
        current_label,
        15.0,
        TEXT,
    );
    // Down chevron indicating open/closed state (two unaliased lines).
    let (cx, cy) = (
        closed_rect.x1 as i64 - 14,
        (closed_rect.y0 + closed_rect.y1) as i64 / 2,
    );
    canvas.line(cx - 4, cy - 2, cx, cy + 2, 2, TEXT);
    canvas.line(cx, cy + 2, cx + 4, cy - 2, 2, TEXT);

    // Desktop-audio and microphone device dropdowns (drawn identically to
    // the bitrate dropdown).
    type ClosedRectFn = fn(usize) -> Rect;
    for (label, dropdown_btn, closed_rect_fn, current) in [
        (
            AUDIO_OUTPUT_LABEL,
            Btn::AudioOutputDropdown,
            audio_output_dropdown_rect as ClosedRectFn,
            record_audio_output_device,
        ),
        (
            AUDIO_INPUT_LABEL,
            Btn::AudioInputDropdown,
            audio_input_dropdown_rect as ClosedRectFn,
            record_audio_input_device,
        ),
    ] {
        let open = match dropdown_btn {
            Btn::AudioOutputDropdown => audio_output_dropdown_open,
            _ => audio_input_dropdown_open,
        };
        let closed_rect = closed_rect_fn(sw);
        let row_baseline =
            t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
        t.draw(canvas, CONTENT_X as f32, row_baseline, label, 15.0, DIM);

        canvas.fill(closed_rect, FIELD_BG);
        canvas.stroke(
            closed_rect,
            if hover == Some(dropdown_btn) || open {
                ACCENT
            } else {
                0x0080_8080
            },
        );
        let current_label = audio_device_display_name(current);
        let label_baseline =
            t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
        // Clip before the chevron so long device names don't overflow the
        // box.
        let label_clip = Rect {
            x1: closed_rect.x1.saturating_sub(20),
            ..closed_rect
        };
        t.draw_clipped(
            canvas,
            closed_rect.x0 as f32 + 8.0,
            label_baseline,
            current_label,
            15.0,
            TEXT,
            label_clip,
        );
        let (cx, cy) = (
            closed_rect.x1 as i64 - 14,
            (closed_rect.y0 + closed_rect.y1) as i64 / 2,
        );
        canvas.line(cx - 4, cy - 2, cx, cy + 2, 2, TEXT);
        canvas.line(cx, cy + 2, cx + 4, cy - 2, 2, TEXT);
    }

    // Max resolution width/height: plain numeric fields with no +/- step,
    // same layout as `session_limit`.
    for (dim, label, current) in [
        (MaxResDim::Width, MAX_WIDTH_LABEL, record_max_width),
        (MaxResDim::Height, MAX_HEIGHT_LABEL, record_max_height),
    ] {
        let focused = max_resolution_focus == Some(dim);
        let field_rect = max_resolution_row_layout(sw, dim);
        let row_baseline =
            t.baseline_for_center((field_rect.y0 + field_rect.y1) as f32 / 2.0, 15.0);
        t.draw(canvas, CONTENT_X as f32, row_baseline, label, 15.0, DIM);

        canvas.fill(field_rect, FIELD_BG);
        canvas.stroke(field_rect, if focused { ACCENT } else { 0x0080_8080 });
        let shown = if focused {
            max_resolution_buf.to_string()
        } else if current == 0 {
            "Unlimited".to_string()
        } else {
            current.to_string()
        };
        let tw = t.text_width(&shown, 15.0);
        let lx = field_rect.x0 as f32 + (field_rect.width() as f32 - tw) / 2.0;
        let baseline = t.baseline_for_center((field_rect.y0 + field_rect.y1) as f32 / 2.0, 15.0);
        let (ascent, descent) = t.glyph_vextent(15.0);
        let caret_y0 = (baseline - ascent) as usize;
        let caret_y1 = (baseline - descent) as usize;
        if focused && let Some((lo, hi)) = max_resolution_cursor.selection() {
            let x0 = lx + x_for_char_index(Some(t), &shown, 15.0, lo);
            let x1 = lx + x_for_char_index(Some(t), &shown, 15.0, hi);
            canvas.fill(
                Rect {
                    x0: x0 as usize,
                    y0: caret_y0,
                    x1: x1 as usize,
                    y1: caret_y1,
                },
                TEXT_SELECTION_BG,
            );
        }
        t.draw(canvas, lx, baseline, &shown, 15.0, TEXT);
        if focused {
            let cx = (lx + x_for_char_index(Some(t), &shown, 15.0, max_resolution_cursor.cursor))
                as usize;
            canvas.fill(
                Rect {
                    x0: cx,
                    y0: caret_y0,
                    x1: cx + 1,
                    y1: caret_y1,
                },
                TEXT,
            );
        }
    }

    // Sample rate dropdown (44.1kHz/48kHz), drawn identically to the
    // bitrate dropdown.
    {
        let closed_rect = sample_rate_dropdown_rect(sw);
        let row_baseline =
            t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
        t.draw(
            canvas,
            CONTENT_X as f32,
            row_baseline,
            SAMPLE_RATE_LABEL,
            15.0,
            DIM,
        );

        canvas.fill(closed_rect, FIELD_BG);
        canvas.stroke(
            closed_rect,
            if hover == Some(Btn::SampleRateDropdown) || sample_rate_dropdown_open {
                ACCENT
            } else {
                0x0080_8080
            },
        );
        let current_label = AUDIO_SAMPLE_RATE_PRESETS
            .iter()
            .find(|(v, _)| *v == record_audio_sample_rate)
            .map(|(_, label)| *label)
            .unwrap_or("Custom");
        let label_baseline =
            t.baseline_for_center((closed_rect.y0 + closed_rect.y1) as f32 / 2.0, 15.0);
        t.draw(
            canvas,
            closed_rect.x0 as f32 + 8.0,
            label_baseline,
            current_label,
            15.0,
            TEXT,
        );
        let (cx, cy) = (
            closed_rect.x1 as i64 - 14,
            (closed_rect.y0 + closed_rect.y1) as i64 / 2,
        );
        canvas.line(cx - 4, cy - 2, cx, cy + 2, 2, TEXT);
        canvas.line(cx, cy + 2, cx + 4, cy - 2, 2, TEXT);
    }

    // Strip-silent-audio checkbox; box at the dropdown column, label at
    // CONTENT_X.
    {
        let (_, silent_rect) = buttons
            .iter()
            .find(|(b, _)| *b == Btn::StripSilentAudio)
            .expect("StripSilentAudio は Video タブに常に存在する");
        let silent_box_y = silent_rect.y0 + (silent_rect.height() - box_size) / 2;
        let silent_value_x0 = video_value_rect(sw, STRIP_SILENT_AUDIO_ROW_Y, CHECKBOX_ROW_H).x0;
        let silent_box_rect = Rect {
            x0: silent_value_x0,
            y0: silent_box_y,
            x1: silent_value_x0 + box_size,
            y1: silent_box_y + box_size,
        };
        canvas.fill(
            silent_box_rect,
            if record_strip_silent_audio {
                ACCENT
            } else {
                FIELD_BG
            },
        );
        canvas.stroke(
            silent_box_rect,
            if hover == Some(Btn::StripSilentAudio) {
                ACCENT
            } else {
                0x0080_8080
            },
        );
        if record_strip_silent_audio {
            // Checkmark (two short strokes).
            let (x0, y0, x1, y1) = (
                silent_box_rect.x0 as i64,
                silent_box_rect.y0 as i64,
                silent_box_rect.x1 as i64,
                silent_box_rect.y1 as i64,
            );
            canvas.line(x0 + 3, y0 + 9, x0 + 7, y1 - 4, 2, 0x00FF_FFFF);
            canvas.line(x0 + 7, y1 - 4, x1 - 3, y0 + 3, 2, 0x00FF_FFFF);
        }
        let silent_baseline =
            t.baseline_for_center((silent_rect.y0 + silent_rect.y1) as f32 / 2.0, 15.0);
        t.draw(
            canvas,
            CONTENT_X as f32,
            silent_baseline,
            "Strip audio track if the recording is silent",
            15.0,
            DIM,
        );
    }

    // Draw an open dropdown's option list last, after every other row, so
    // later rows don't paint over it (they're mutually exclusive, so at
    // most one is open at a time).
    if bitrate_dropdown_open {
        for (i, (val, label)) in BITRATE_PRESETS.into_iter().enumerate() {
            let r = bitrate_option_rect(sw, i);
            let btn = Btn::BitrateOption(val);
            let selected = val == record_bitrate_mbps;
            let base = if selected { ACCENT } else { FIELD_BG };
            let color = if hover == Some(btn) && !selected {
                hover_tint(base)
            } else {
                base
            };
            canvas.fill(r, color);
            let tcolor = if selected { 0x0011_1111 } else { TEXT };
            let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
            t.draw(canvas, r.x0 as f32 + 8.0, baseline, label, 15.0, tcolor);
        }
        // One stroke around the whole list instead of per-option, so
        // shared edges aren't drawn twice (and look thicker).
        let first = bitrate_option_rect(sw, 0);
        let last = bitrate_option_rect(sw, BITRATE_PRESETS.len() - 1);
        let list_rect = Rect {
            x0: first.x0,
            y0: first.y0,
            x1: first.x1,
            y1: last.y1,
        };
        canvas.stroke(list_rect, 0x0080_8080);
    } else if audio_output_dropdown_open || audio_input_dropdown_open {
        type OptionRectFn = fn(usize, Option<&TextRenderer>, &[String], usize) -> Rect;
        let (option_rect_fn, devices, current): (OptionRectFn, &[String], &str) =
            if audio_output_dropdown_open {
                (
                    audio_output_option_rect,
                    audio_output_devices,
                    record_audio_output_device,
                )
            } else {
                (
                    audio_input_option_rect,
                    audio_input_devices,
                    record_audio_input_device,
                )
            };
        for (i, name) in devices.iter().enumerate() {
            let r = option_rect_fn(sw, text, devices, i);
            let btn = if audio_output_dropdown_open {
                Btn::AudioOutputOption(i)
            } else {
                Btn::AudioInputOption(i)
            };
            let selected = name == current;
            let base = if selected { ACCENT } else { FIELD_BG };
            let color = if hover == Some(btn) && !selected {
                hover_tint(base)
            } else {
                base
            };
            canvas.fill(r, color);
            let tcolor = if selected { 0x0011_1111 } else { TEXT };
            let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
            // Clip long device names to the option row.
            t.draw_clipped(
                canvas,
                r.x0 as f32 + 8.0,
                baseline,
                audio_device_display_name(name),
                15.0,
                tcolor,
                r,
            );
        }
        let first = option_rect_fn(sw, text, devices, 0);
        let last = option_rect_fn(sw, text, devices, devices.len() - 1);
        let list_rect = Rect {
            x0: first.x0,
            y0: first.y0,
            x1: first.x1,
            y1: last.y1,
        };
        canvas.stroke(list_rect, 0x0080_8080);
    } else if sample_rate_dropdown_open {
        for (i, (val, label)) in AUDIO_SAMPLE_RATE_PRESETS.into_iter().enumerate() {
            let r = sample_rate_option_rect(sw, i);
            let btn = Btn::SampleRateOption(val);
            let selected = val == record_audio_sample_rate;
            let base = if selected { ACCENT } else { FIELD_BG };
            let color = if hover == Some(btn) && !selected {
                hover_tint(base)
            } else {
                base
            };
            canvas.fill(r, color);
            let tcolor = if selected { 0x0011_1111 } else { TEXT };
            let baseline = t.baseline_for_center((r.y0 + r.y1) as f32 / 2.0, 15.0);
            t.draw(canvas, r.x0 as f32 + 8.0, baseline, label, 15.0, tcolor);
        }
        let first = sample_rate_option_rect(sw, 0);
        let last = sample_rate_option_rect(sw, AUDIO_SAMPLE_RATE_PRESETS.len() - 1);
        let list_rect = Rect {
            x0: first.x0,
            y0: first.y0,
            x1: first.x1,
            y1: last.y1,
        };
        canvas.stroke(list_rect, 0x0080_8080);
    }

    // Color picker popup, drawn last so it overlays everything else in
    // the tab when open.
    if let (Some((h, s, v)), Some(target)) = (picker, picker_target) {
        let (popup, sv_rect, hue_rect) = click_color_picker_geom(sw, text, target);
        canvas.fill(popup, PICK_BG);
        canvas.stroke(popup, 0x0080_8080);

        // These per-pixel gradients bypass `Canvas`'s auto-scaling shape
        // methods, so scale `sv_rect`/`hue_rect` to physical pixels here
        // (matching `Canvas::fill`/`stroke`'s own rounding) and resample
        // the gradient once per physical pixel — sharper at high DPI
        // rather than nearest-neighbor blocky.
        let scaled = |r: Rect| {
            let s = |v: usize| ((v as f64) * canvas.scale).round() as usize;
            Rect {
                x0: s(r.x0),
                y0: s(r.y0),
                x1: s(r.x1),
                y1: s(r.y1),
            }
        };
        let sv_p = scaled(sv_rect);
        let hue_p = scaled(hue_rect);

        for yy in 0..sv_p.height() {
            let vv = 1.0 - yy as f32 / sv_p.height().max(1) as f32;
            for xx in 0..sv_p.width() {
                let ss = xx as f32 / sv_p.width().max(1) as f32;
                canvas.set(sv_p.x0 + xx, sv_p.y0 + yy, hsv_to_rgb(h, ss, vv));
            }
        }
        marker(
            canvas,
            sv_rect.x0 as i64 + (s * PICK_SV as f32) as i64,
            sv_rect.y0 as i64 + ((1.0 - v) * PICK_SV as f32) as i64,
        );
        for xx in 0..hue_p.width() {
            let col = hsv_to_rgb(xx as f32 / hue_p.width().max(1) as f32 * 360.0, 1.0, 1.0);
            for yy in 0..hue_p.height() {
                canvas.set(hue_p.x0 + xx, hue_p.y0 + yy, col);
            }
        }
        let hx = hue_p.x0 as i64 + (h / 360.0 * hue_p.width() as f32) as i64;
        for yy in 0..hue_p.height() as i64 {
            canvas.set_i(hx, hue_p.y0 as i64 + yy, 0x00FF_FFFF);
            canvas.set_i(hx - 1, hue_p.y0 as i64 + yy, 0x0000_0000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        VIDEO_VALUE_W, audio_device_display_name, audio_dropdown_option_w,
        audio_input_dropdown_rect, audio_output_dropdown_rect, bitrate_dropdown_rect,
        parse_max_resolution, stacked_option_rect,
    };
    use crate::settings::{WIN_H, WIN_W};
    use crate::ui::Rect;

    #[test]
    fn parse_max_resolution_allows_zero_and_keeps_current_on_invalid() {
        assert_eq!(parse_max_resolution("1920", 0), 1920);
        // Unlike session_history_limit, 0 (disabled/unlimited) is allowed; no lower clamp.
        assert_eq!(parse_max_resolution("0", 1920), 0);
        assert_eq!(parse_max_resolution("999999", 0), 7680); // upper clamp
        assert_eq!(parse_max_resolution("", 1920), 1920); // empty keeps current
        assert_eq!(parse_max_resolution("abc", 1920), 1920); // invalid keeps current
    }

    #[test]
    fn audio_device_dropdown_rows_do_not_overlap_bitrate_row_or_each_other() {
        let bitrate = bitrate_dropdown_rect(WIN_W);
        let output = audio_output_dropdown_rect(WIN_W);
        let input = audio_input_dropdown_rect(WIN_W);
        assert!(bitrate.y1 <= output.y0, "音声出力の行はビットレートの下");
        assert!(output.y1 <= input.y0, "マイクの行は音声出力の下");
    }

    #[test]
    fn audio_dropdown_option_w_fits_the_longest_device_name_and_has_a_minimum() {
        // No device names: same fixed width as the closed button.
        assert_eq!(audio_dropdown_option_w(None, &[]), VIDEO_VALUE_W);
        // No font available: falls back to the fixed width even with device names.
        assert_eq!(
            audio_dropdown_option_w(None, &["Very Long Device Name Indeed".to_string()]),
            VIDEO_VALUE_W
        );
    }

    #[test]
    fn audio_device_display_name_shows_system_default_for_empty_string() {
        assert_eq!(audio_device_display_name(""), "System default");
        assert_eq!(audio_device_display_name("Speakers"), "Speakers");
    }

    #[test]
    fn stacked_option_rect_stacks_below_when_it_fits() {
        let closed = Rect {
            x0: 10,
            y0: 50,
            x1: 210,
            y1: 78,
        };
        let r0 = stacked_option_rect(closed, 28, 3, 0, 200, WIN_W);
        let r2 = stacked_option_rect(closed, 28, 3, 2, 200, WIN_W);
        assert_eq!(r0.y0, closed.y1);
        assert_eq!(r2.y0, closed.y1 + 2 * 28);
    }

    #[test]
    fn stacked_option_rect_left_aligns_to_the_closed_button_when_it_fits() {
        // Wider than the closed button (e.g. a longer device name), but
        // still fits within the window when left-aligned.
        let closed = Rect {
            x0: 50,
            y0: 300,
            x1: 250,
            y1: 328,
        };
        let w = 300;
        let r = stacked_option_rect(closed, 28, 1, 0, w, WIN_W);
        assert_eq!(r.x0, closed.x0);
        assert_eq!(r.x1, closed.x0 + w);
    }

    #[test]
    fn stacked_option_rect_grows_left_from_the_shared_right_edge_when_left_aligning_would_overflow()
    {
        // Left-aligning this wide a list to the closed button would run
        // past the window's right edge, so it grows left from the shared
        // right edge instead, staying on-screen.
        let closed = Rect {
            x0: WIN_W - 240,
            y0: 300,
            x1: WIN_W - 20,
            y1: 328,
        };
        let wide = 400;
        let r = stacked_option_rect(closed, 28, 1, 0, wide, WIN_W);
        assert_eq!(r.x1, closed.x1);
        assert_eq!(r.x0, closed.x1 - wide);
        assert!(r.x1 <= WIN_W);
    }

    #[test]
    fn stacked_option_rect_flips_above_when_the_list_would_overflow_the_window() {
        // The closed button sits near the bottom of the window, leaving no
        // room for a 3*28=84px list below it.
        let closed = Rect {
            x0: 10,
            y0: WIN_H - 40,
            x1: 210,
            y1: WIN_H - 12,
        };
        let r0 = stacked_option_rect(closed, 28, 3, 0, 200, WIN_W);
        let r2 = stacked_option_rect(closed, 28, 3, 2, 200, WIN_W);
        // The whole list sits above the closed button; the last option
        // (bottommost) ends exactly where the button begins.
        assert!(r0.y0 < closed.y0);
        assert_eq!(r2.y1, closed.y0);
    }
}
