//! General tab: filename template, launch at startup.

use winit::keyboard::{Key, NamedKey};

use super::{
    ACCENT, Btn, CONTENT_X, Settings, SettingsResult, TextCursor, apply_common_edit_key,
    char_index_for_x, field, hover_tint_for, inside, next_row_y_with_extra_gap, theme_colors,
    wrap_slots, x_for_char_index,
};
use crate::store::MenuButton;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect, draw_icon_button};

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

/// "Menu buttons" section: drag-reorder/show-hide the region-selection
/// menu's buttons, laid out left-to-right (matching the real menu) as two
/// rows of chips — "Shown" (in menu order) and "Hidden" (available but not
/// shown). Dragging a chip within Shown reorders it; dragging between rows
/// shows/hides it. No checkbox — which row a chip is in *is* its
/// visibility. Each chip is drawn as a square, same as the real menu's
/// buttons (`draw_icon_button`), with its label below.
const MENU_BUTTONS_HEADER_Y: usize = next_row_y_with_extra_gap(STARTUP_ROW_Y, 30, 20);
/// Mirrors `overlay::ACTION_BTN` (not importable here — `pub(super)` to
/// `overlay`, and DPI-scaled there besides) so a chip is the same size as
/// the real menu's square buttons.
const CHIP_SIZE: usize = 56;
const CHIP_GAP: i64 = 8;
const CHIP_LINE_GAP: i64 = 8;
/// Gap between a row's subheader ("Shown"/"Hidden") and its chips.
const SUBHEADER_GAP: usize = 22;
/// Gap between the Shown block's last chip row and the Hidden subheader.
const BLOCK_GAP: usize = 16;
/// Width of the drop-position indicator drawn between chips while dragging.
const INSERT_BAR_W: usize = 3;

/// The row's usable width (content area minus the right margin).
fn chip_row_max_w(sw: usize) -> i64 {
    sw.saturating_sub(CONTENT_X + 16) as i64
}

/// `items` laid out left to right as `CHIP_SIZE` squares starting at `y0`,
/// wrapping to further lines if they don't fit `sw`. Order matches `items`.
fn chip_rects(items: &[MenuButton], sw: usize, y0: usize) -> Vec<(MenuButton, Rect)> {
    let widths = vec![CHIP_SIZE as i64; items.len()];
    let slots = wrap_slots(&widths, CHIP_GAP, chip_row_max_w(sw));
    items
        .iter()
        .copied()
        .zip(slots)
        .map(|(b, (line, x))| {
            let ry = y0 as i64 + line as i64 * (CHIP_SIZE as i64 + CHIP_LINE_GAP);
            (
                b,
                Rect {
                    x0: (CONTENT_X as i64 + x) as usize,
                    y0: ry as usize,
                    x1: (CONTENT_X as i64 + x + CHIP_SIZE as i64) as usize,
                    y1: (ry + CHIP_SIZE as i64) as usize,
                },
            )
        })
        .collect()
}

/// Total height of `items`'s wrapped chip rows (at least one line).
fn chip_block_h(items: &[MenuButton], sw: usize) -> usize {
    let widths = vec![CHIP_SIZE as i64; items.len()];
    let slots = wrap_slots(&widths, CHIP_GAP, chip_row_max_w(sw));
    let lines = slots.last().map(|(l, _)| l + 1).unwrap_or(1) as i64;
    (lines * CHIP_SIZE as i64 + (lines - 1).max(0) * CHIP_LINE_GAP) as usize
}

fn shown_label_y() -> usize {
    MENU_BUTTONS_HEADER_Y + 26
}

fn shown_chips_y0() -> usize {
    shown_label_y() + SUBHEADER_GAP
}

/// The Hidden subheader's Y — depends on how many lines the Shown block
/// wrapped to, hence the `shown`/`sw` parameters.
fn hidden_label_y(shown: &[MenuButton], sw: usize) -> usize {
    shown_chips_y0() + chip_block_h(shown, sw) + BLOCK_GAP
}

fn hidden_chips_y0(shown: &[MenuButton], sw: usize) -> usize {
    hidden_label_y(shown, sw) + SUBHEADER_GAP
}

fn shown_chip_rects(shown: &[MenuButton], sw: usize) -> Vec<(MenuButton, Rect)> {
    chip_rects(shown, sw, shown_chips_y0())
}

fn hidden_chip_rects(
    shown: &[MenuButton],
    hidden: &[MenuButton],
    sw: usize,
) -> Vec<(MenuButton, Rect)> {
    chip_rects(hidden, sw, hidden_chips_y0(shown, sw))
}

/// The chip under `(x, y)`, if any (checks both rows). Used to start a drag.
pub(super) fn menu_chip_at(
    shown: &[MenuButton],
    hidden: &[MenuButton],
    sw: usize,
    x: f64,
    y: f64,
) -> Option<MenuButton> {
    shown_chip_rects(shown, sw)
        .into_iter()
        .chain(hidden_chip_rects(shown, hidden, sw))
        .find(|(_, r)| inside(*r, x, y))
        .map(|(b, _)| b)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuRow {
    Shown,
    Hidden,
}

/// Which row `y` is over: below the Hidden subheader is Hidden, everything
/// else (including the Shown block and its own subheader) is Shown. A
/// generous, forgiving split rather than exact per-row bounds.
pub(super) fn menu_row_at(shown: &[MenuButton], sw: usize, y: f64) -> MenuRow {
    if y >= hidden_label_y(shown, sw) as f64 {
        MenuRow::Hidden
    } else {
        MenuRow::Shown
    }
}

/// Where `dragged` should land in `target` (with `dragged` already removed
/// from it) given a drop at `(x, y)`: the index of the first chip that's on
/// an earlier line, or on the same line but to the right of `x` —
/// otherwise the end (append).
pub(super) fn menu_chip_drop_index(
    target: &[MenuButton],
    sw: usize,
    y0: usize,
    x: f64,
    y: f64,
) -> usize {
    let rects = chip_rects(target, sw, y0);
    for (i, (_, r)) in rects.iter().enumerate() {
        let mid_x = (r.x0 + r.x1) as f64 / 2.0;
        if (y as usize) < r.y0 || ((y as usize) < r.y1 && x < mid_x) {
            return i;
        }
    }
    rects.len()
}

/// The drop-position indicator's rect for a drag currently over `target`
/// (with `dragged` already excluded from it) at `(x, y)`: a thin vertical
/// bar just before the chip it would land at, or after the last chip (or
/// at the row's start, if `target` is empty) when it would append.
///
/// The dragged chip itself keeps rendering at its original spot for the
/// whole drag (see `draw_menu_button_chip`) rather than being pulled out of
/// the row — so `full_list` (still including it) is what's actually on
/// screen, and this looks up positions there rather than from `target`'s
/// own (gap-closed) layout, which would drift out of sync with the other
/// chips' real positions once the dragged chip isn't at the very end.
fn menu_chip_insert_bar(
    full_list: &[MenuButton],
    target: &[MenuButton],
    sw: usize,
    y0: usize,
    x: f64,
    y: f64,
) -> Rect {
    let idx = menu_chip_drop_index(target, sw, y0, x, y);
    let full_rects = chip_rects(full_list, sw, y0);
    let rect_of = |b: MenuButton| full_rects.iter().find(|(fb, _)| *fb == b).map(|(_, r)| *r);
    let half_gap = (CHIP_GAP / 2) as usize;

    if let Some(r) = target.get(idx).copied().and_then(rect_of) {
        let cx = r.x0.saturating_sub(half_gap);
        return Rect {
            x0: cx.saturating_sub(INSERT_BAR_W / 2),
            y0: r.y0,
            x1: cx.saturating_sub(INSERT_BAR_W / 2) + INSERT_BAR_W,
            y1: r.y1,
        };
    }
    if idx > 0
        && let Some(r) = target.get(idx - 1).copied().and_then(rect_of)
    {
        let cx = r.x1 + half_gap;
        return Rect {
            x0: cx.saturating_sub(INSERT_BAR_W / 2),
            y0: r.y0,
            x1: cx.saturating_sub(INSERT_BAR_W / 2) + INSERT_BAR_W,
            y1: r.y1,
        };
    }
    // `target` is empty (only the dragged chip was in this row).
    Rect {
        x0: CONTENT_X,
        y0,
        x1: CONTENT_X + INSERT_BAR_W,
        y1: y0 + CHIP_SIZE,
    }
}

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

    /// Drops the chip being dragged (`self.menu_drag`, already taken by the
    /// caller) at the current cursor position: moves it between/within the
    /// shown/hidden lists. No-op (drag simply cancelled) if the cursor
    /// isn't over either row.
    pub(super) fn drop_menu_chip(&mut self, dragged: MenuButton) {
        let sw = self.size.0;
        let (x, y) = self.cursor;
        let row = menu_row_at(&self.menu_buttons_shown, sw, y);

        self.menu_buttons_shown.retain(|b| *b != dragged);
        self.menu_buttons_hidden.retain(|b| *b != dragged);

        match row {
            MenuRow::Shown => {
                let i = menu_chip_drop_index(&self.menu_buttons_shown, sw, shown_chips_y0(), x, y);
                self.menu_buttons_shown.insert(i, dragged);
            }
            MenuRow::Hidden => {
                let y0 = hidden_chips_y0(&self.menu_buttons_shown, sw);
                let i = menu_chip_drop_index(&self.menu_buttons_hidden, sw, y0, x, y);
                self.menu_buttons_hidden.insert(i, dragged);
            }
        }
        self.request_redraw();
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
    menu_buttons_shown: &[MenuButton],
    menu_buttons_hidden: &[MenuButton],
    menu_drag: Option<MenuButton>,
    chip_hover: Option<MenuButton>,
    cursor: (f64, f64),
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

    // "Menu buttons": drag chips between "Shown" (in menu order) and
    // "Hidden" to show/hide them; drag within "Shown" to reorder. Each chip
    // is a square drawn exactly like the real menu's buttons
    // (`draw_icon_button`), so it's obvious what it'll look like. Chips
    // aren't `Btn`s, so hover here is a direct cursor/rect check rather
    // than the generic `hover: Option<Btn>` (unused for this section).
    let _ = hover;
    t.draw(
        canvas,
        CONTENT_X as f32,
        (MENU_BUTTONS_HEADER_Y + 14) as f32,
        "Menu buttons",
        15.0,
        TEXT,
    );
    t.draw(
        canvas,
        CONTENT_X as f32,
        (shown_label_y() + 14) as f32,
        "Shown",
        13.0,
        DIM,
    );
    for (menu_btn, r) in shown_chip_rects(menu_buttons_shown, sw) {
        draw_menu_button_chip(
            canvas, t, r, menu_btn, menu_drag, chip_hover, BTN_BG, TEXT, hover_tint,
        );
    }
    let hidden_y = hidden_label_y(menu_buttons_shown, sw);
    t.draw(
        canvas,
        CONTENT_X as f32,
        (hidden_y + 14) as f32,
        "Hidden",
        13.0,
        DIM,
    );
    for (menu_btn, r) in hidden_chip_rects(menu_buttons_shown, menu_buttons_hidden, sw) {
        draw_menu_button_chip(
            canvas, t, r, menu_btn, menu_drag, chip_hover, FIELD_BG, DIM, hover_tint,
        );
    }

    // While dragging, a thin bar shows exactly where the chip would land
    // if dropped right now — recomputed live from the cursor position, but
    // otherwise nothing about the layout changes mid-drag.
    if let Some(dragged) = menu_drag {
        let row = menu_row_at(menu_buttons_shown, sw, cursor.1);
        let bar = match row {
            MenuRow::Shown => {
                let without_dragged: Vec<MenuButton> = menu_buttons_shown
                    .iter()
                    .copied()
                    .filter(|b| *b != dragged)
                    .collect();
                menu_chip_insert_bar(
                    menu_buttons_shown,
                    &without_dragged,
                    sw,
                    shown_chips_y0(),
                    cursor.0,
                    cursor.1,
                )
            }
            MenuRow::Hidden => {
                let without_dragged: Vec<MenuButton> = menu_buttons_hidden
                    .iter()
                    .copied()
                    .filter(|b| *b != dragged)
                    .collect();
                let y0 = hidden_chips_y0(menu_buttons_shown, sw);
                menu_chip_insert_bar(
                    menu_buttons_hidden,
                    &without_dragged,
                    sw,
                    y0,
                    cursor.0,
                    cursor.1,
                )
            }
        };
        canvas.fill(bar, ACCENT);
    }
}

/// Draws one menu-button chip: the real menu's square-button look
/// (`draw_icon_button`). The dragged chip keeps rendering right where it
/// started (see `menu_chip_insert_bar`), just with the same subtle
/// `hover_tint` treatment a plain hover gets — no separate "selected"
/// color, so dragging it back over its own spot looks like nothing special
/// is happening.
#[allow(clippy::too_many_arguments)]
fn draw_menu_button_chip(
    canvas: &mut Canvas,
    t: &TextRenderer,
    r: Rect,
    menu_btn: MenuButton,
    menu_drag: Option<MenuButton>,
    chip_hover: Option<MenuButton>,
    base: u32,
    fg: u32,
    hover_tint: impl Fn(u32) -> u32,
) {
    let dragging = menu_drag == Some(menu_btn);
    // No hover highlight on other chips while any drag is in progress —
    // sweeping the cursor across the row while dragging shouldn't light up
    // everything it passes over. `chip_hover` only reflects an actual
    // `CursorMoved`, not raw cursor-vs-rect at draw time, so a chip
    // doesn't light up just because the layout shifted under a
    // stationary cursor (e.g. right after a drop reorders things).
    let hovered = menu_drag.is_none() && chip_hover == Some(menu_btn);
    let bg = if dragging || hovered {
        hover_tint(base)
    } else {
        base
    };
    draw_icon_button(canvas, r, bg, fg, menu_btn.label(), fg, t);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn menu_chip_drop_index_orders_by_x_within_one_line() {
        let items = [MenuButton::Save, MenuButton::Copy, MenuButton::Quit];
        let rects = chip_rects(&items, 720, 0);

        // Just left of the first chip -> insert before it.
        let (_, r0) = rects[0];
        assert_eq!(
            menu_chip_drop_index(&items, 720, 0, r0.x0 as f64, r0.y0 as f64 + 1.0),
            0
        );

        // Well past the last chip -> append at the end.
        let (_, r_last) = *rects.last().unwrap();
        assert_eq!(
            menu_chip_drop_index(
                &items,
                720,
                0,
                (r_last.x1 + 50) as f64,
                r_last.y0 as f64 + 1.0
            ),
            items.len()
        );

        // The right half of the middle chip -> insert after it.
        let (_, r1) = rects[1];
        let past_mid_x = (r1.x0 + r1.x1) as f64 / 2.0 + 1.0;
        assert_eq!(
            menu_chip_drop_index(&items, 720, 0, past_mid_x, r1.y0 as f64 + 1.0),
            2
        );
    }

    #[test]
    fn menu_chip_insert_bar_appends_at_the_full_lists_true_right_edge() {
        // Dragging the FIRST of 3 chips toward the far right: `without_dragged`
        // (Copy, Quit) would compact leftward if laid out on its own, but
        // since the dragged chip keeps rendering in place, Copy/Quit are
        // still exactly where the *full* list puts them — the bar must
        // align with Quit's true on-screen right edge, not with a
        // recomputed "2 items only" layout (which would put it noticeably
        // further left, one chip-width + gap short).
        let full = [MenuButton::Save, MenuButton::Copy, MenuButton::Quit];
        let without_dragged = [MenuButton::Copy, MenuButton::Quit];
        let full_rects = chip_rects(&full, 720, 0);
        let quit_rect = full_rects
            .iter()
            .find(|(b, _)| *b == MenuButton::Quit)
            .unwrap()
            .1;

        let bar = menu_chip_insert_bar(
            &full,
            &without_dragged,
            720,
            0,
            (quit_rect.x1 + 100) as f64,
            quit_rect.y0 as f64 + 1.0,
        );

        assert!(bar.x0 >= quit_rect.x1);
        assert!(bar.x0 < quit_rect.x1 + CHIP_GAP as usize * 2);
    }

    #[test]
    fn chip_rects_are_uniform_squares_in_order_without_overlap() {
        let items = [MenuButton::SizeAspect, MenuButton::Save, MenuButton::Quit];
        let rects = chip_rects(&items, 720, 0);
        for (_, r) in &rects {
            assert_eq!(r.width(), CHIP_SIZE);
            assert_eq!(r.height(), CHIP_SIZE);
        }
        for pair in rects.windows(2) {
            assert!(pair[0].1.x1 <= pair[1].1.x0);
        }
    }

    #[test]
    fn menu_row_at_splits_exactly_on_the_hidden_subheader() {
        let shown = [MenuButton::Save];
        let boundary = hidden_label_y(&shown, 720);
        assert_eq!(
            menu_row_at(&shown, 720, (boundary - 1) as f64),
            MenuRow::Shown
        );
        assert_eq!(menu_row_at(&shown, 720, boundary as f64), MenuRow::Hidden);
    }
}
