//! General tab: filename template, launch at startup.

use winit::keyboard::{Key, NamedKey};

use super::{
    ACCENT, Btn, CONTENT_X, Settings, SettingsResult, TextCursor, apply_common_edit_key,
    char_index_for_x, field, hover_tint_for, next_row_y_with_extra_gap, theme_colors,
    x_for_char_index,
};
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect};

const FILENAME_FORMAT_ROW_Y: usize = 88;
const FILENAME_FORMAT_ROW_H: usize = 26;

/// Filename template field: right of the label, follows window width.
fn filename_format_row_layout(sw: usize) -> Rect {
    const LABEL_W: usize = 140;
    field(
        CONTENT_X + LABEL_W,
        FILENAME_FORMAT_ROW_Y,
        sw.saturating_sub(CONTENT_X + LABEL_W + 20),
        FILENAME_FORMAT_ROW_H,
    )
}

/// Below the filename field + 2 lines of help text (hence the extra gap).
const STARTUP_ROW_Y: usize =
    next_row_y_with_extra_gap(FILENAME_FORMAT_ROW_Y, FILENAME_FORMAT_ROW_H, 50);

/// Commit the buffer: trims and keeps it, or falls back to `current` if
/// empty.
fn commit_filename_format_value(buf: &str, current: &str) -> String {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        current.to_string()
    } else {
        trimmed.to_string()
    }
}

impl Settings {
    pub(super) fn buttons_general(&self, sw: usize) -> Vec<(Btn, Rect)> {
        vec![
            (Btn::FilenameFormatField, filename_format_row_layout(sw)),
            (
                Btn::LaunchAtStartup,
                field(CONTENT_X, STARTUP_ROW_Y, 260, 30),
            ),
        ]
    }

    pub(super) fn activate_general(&mut self, btn: Btn) -> Option<SettingsResult> {
        match btn {
            Btn::FilenameFormatField => {
                self.focus_filename_format();
                None
            }
            Btn::LaunchAtStartup => {
                self.launch_at_startup = !self.launch_at_startup;
                self.request_redraw();
                None
            }
            _ => None,
        }
    }

    pub(super) fn on_filename_format_key(&mut self, event: &winit::event::KeyEvent) {
        if apply_common_edit_key(
            &mut self.filename_format_cursor,
            &mut self.filename_format_buf,
            event,
            self.mods,
        ) {
            self.request_redraw();
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => self.commit_filename_format(),
            Key::Named(NamedKey::Escape) => {
                self.filename_format_focus = false;
                self.filename_format_buf.clear();
                self.filename_format_cursor = TextCursor::default();
                self.request_redraw();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("c") => {
                self.copy_filename_format_selection();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("x") => {
                self.copy_filename_format_selection();
                self.filename_format_cursor
                    .delete_selection(&mut self.filename_format_buf);
                self.request_redraw();
            }
            Key::Character(s) if self.mods.control_key() && s.eq_ignore_ascii_case("v") => {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    self.filename_format_cursor
                        .insert(&mut self.filename_format_buf, text.trim());
                    self.request_redraw();
                }
            }
            // Other Ctrl shortcuts aren't typed as text.
            Key::Character(_) if self.mods.control_key() => {}
            Key::Character(s) => {
                self.filename_format_cursor
                    .insert(&mut self.filename_format_buf, s);
                self.request_redraw();
            }
            Key::Named(NamedKey::Space) => {
                self.filename_format_cursor
                    .insert(&mut self.filename_format_buf, " ");
                self.request_redraw();
            }
            _ => {}
        }
    }

    /// Focus the field and load the current value into the buffer.
    fn focus_filename_format(&mut self) {
        self.filename_format_buf = self.filename_format.clone();
        self.filename_format_cursor = TextCursor::at_end(&self.filename_format_buf);
        self.filename_format_focus = true;
        self.request_redraw();
    }

    /// Left-aligned text draw-start x (matches the layout math in `draw`).
    pub(super) fn filename_format_text_x0(&self) -> f32 {
        filename_format_row_layout(self.size.0).x0 as f32 + 8.0
    }

    /// Mouse press: focus if needed, place caret at click, start drag-select.
    pub(super) fn begin_filename_format_press(&mut self, click_x: f64) {
        if !self.filename_format_focus {
            self.focus_filename_format();
        }
        let rel_x = click_x as f32 - self.filename_format_text_x0();
        let idx = char_index_for_x(self.text.as_ref(), &self.filename_format_buf, 15.0, rel_x);
        self.filename_format_cursor.set_from_click(idx, false);
        self.text_drag = true;
        self.request_redraw();
    }

    /// Commit the focused field. No-op if unfocused, so it's safe to call
    /// unconditionally.
    pub(super) fn commit_filename_format(&mut self) {
        if !self.filename_format_focus {
            return;
        }
        self.filename_format_focus = false;
        self.filename_format =
            commit_filename_format_value(&self.filename_format_buf, &self.filename_format);
        self.filename_format_buf.clear();
        self.filename_format_cursor = TextCursor::default();
        self.request_redraw();
    }

    /// Copy the current selection to the clipboard, if any.
    fn copy_filename_format_selection(&self) {
        if let Some((lo, hi)) = self.filename_format_cursor.selection()
            && let Ok(mut clip) = arboard::Clipboard::new()
        {
            let _ = clip.set_text(self.filename_format_buf[lo..hi].to_string());
        }
    }
}

#[allow(non_snake_case, unused_variables, clippy::too_many_arguments)]
pub(super) fn draw_general(
    canvas: &mut Canvas,
    t: &TextRenderer,
    dark: bool,
    hover: Option<Btn>,
    buttons: &[(Btn, Rect)],
    sw: usize,
    filename_format: &str,
    filename_format_focus: bool,
    filename_format_buf: &str,
    filename_format_cursor: TextCursor,
    launch_at_startup: bool,
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

    let format_rect = filename_format_row_layout(sw);
    let format_label_baseline =
        t.baseline_for_center((format_rect.y0 + format_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        format_label_baseline,
        "Filename format:",
        15.0,
        DIM,
    );
    canvas.fill(format_rect, FIELD_BG);
    canvas.stroke(
        format_rect,
        if filename_format_focus {
            ACCENT
        } else {
            0x0080_8080
        },
    );
    let format_shown = if filename_format_focus {
        filename_format_buf.to_string()
    } else {
        filename_format.to_string()
    };
    let format_text_x0 = format_rect.x0 as f32 + 8.0;
    let format_baseline =
        t.baseline_for_center((format_rect.y0 + format_rect.y1) as f32 / 2.0, 15.0);
    let (format_ascent, format_descent) = t.glyph_vextent(15.0);
    let format_caret_y0 = (format_baseline - format_ascent) as usize;
    let format_caret_y1 = (format_baseline - format_descent) as usize;
    if filename_format_focus && let Some((lo, hi)) = filename_format_cursor.selection() {
        let x0 = format_text_x0 + x_for_char_index(Some(t), &format_shown, 15.0, lo);
        let x1 = format_text_x0 + x_for_char_index(Some(t), &format_shown, 15.0, hi);
        canvas.fill(
            Rect {
                x0: x0 as usize,
                y0: format_caret_y0,
                x1: x1 as usize,
                y1: format_caret_y1,
            },
            TEXT_SELECTION_BG,
        );
    }
    t.draw_clipped(
        canvas,
        format_text_x0,
        format_baseline,
        &format_shown,
        15.0,
        TEXT,
        format_rect,
    );
    if filename_format_focus {
        let cx = (format_text_x0
            + x_for_char_index(Some(t), &format_shown, 15.0, filename_format_cursor.cursor))
            as usize;
        canvas.fill(
            Rect {
                x0: cx,
                y0: format_caret_y0,
                x1: cx + 1,
                y1: format_caret_y1,
            },
            TEXT,
        );
    }
    // Format help text (2 lines to fit the width).
    t.draw(
        canvas,
        CONTENT_X as f32,
        (format_rect.y1 + 16) as f32,
        "Date/time: any chrono format, e.g. %Y %m %d %H %M %S",
        13.0,
        DIM,
    );
    t.draw(
        canvas,
        CONTENT_X as f32,
        (format_rect.y1 + 34) as f32,
        "Counter: %n (skip existing files) / %#n (persistent), e.g. %04n",
        13.0,
        DIM,
    );

    let (_, cb_rect) = buttons
        .iter()
        .find(|(b, _)| *b == Btn::LaunchAtStartup)
        .expect("LaunchAtStartup は General タブに常に存在する");
    let box_size = 18;
    let box_y = cb_rect.y0 + (cb_rect.height() - box_size) / 2;
    let box_rect = Rect {
        x0: cb_rect.x0,
        y0: box_y,
        x1: cb_rect.x0 + box_size,
        y1: box_y + box_size,
    };
    canvas.fill(box_rect, if launch_at_startup { ACCENT } else { FIELD_BG });
    canvas.stroke(
        box_rect,
        if hover == Some(Btn::LaunchAtStartup) {
            ACCENT
        } else {
            0x0080_8080
        },
    );
    if launch_at_startup {
        // Checkmark (two short lines).
        let (x0, y0, x1, y1) = (
            box_rect.x0 as i64,
            box_rect.y0 as i64,
            box_rect.x1 as i64,
            box_rect.y1 as i64,
        );
        canvas.line(x0 + 3, y0 + 9, x0 + 7, y1 - 4, 2, 0x00FF_FFFF);
        canvas.line(x0 + 7, y1 - 4, x1 - 3, y0 + 3, 2, 0x00FF_FFFF);
    }
    let label_x = (cb_rect.x0 + box_size + 8) as f32;
    let baseline = t.baseline_for_center((cb_rect.y0 + cb_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        label_x,
        baseline,
        "Launch at Windows startup",
        15.0,
        TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::commit_filename_format_value;

    #[test]
    fn commit_filename_format_value_trims_and_keeps_current_when_empty() {
        assert_eq!(
            commit_filename_format_value("pashari_%Y%m%d", "old"),
            "pashari_%Y%m%d"
        );
        assert_eq!(commit_filename_format_value("  spaced  ", "old"), "spaced");
        assert_eq!(commit_filename_format_value("", "old"), "old");
        assert_eq!(commit_filename_format_value("   ", "old"), "old");
    }
}
