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
/// rows of chips — "Shown" (in menu order) and "Available" (things you can
/// drag in). Dragging a chip within Shown reorders it; dragging it to
/// Available removes it. No checkbox — which row a chip is in *is* its
/// visibility. Each chip is drawn as a square, same as the real menu's
/// buttons (`draw_icon_button`), with its label below — except `Divider`,
/// which draws as the same thin line it is in the real menu.
///
/// `Divider` is the one repeatable item (`MenuButton::repeatable`): its
/// Available chip is a permanent template, not consumed when dragged in,
/// and `Settings::menu_buttons_shown` can hold any number of independent
/// copies of it. Every other item is a singleton, so a plain `MenuButton`
/// value is enough to identify it — but with duplicates possible, a
/// specific *chip* (for drag/hover) has to be identified by more than its
/// value; see `MenuChipRef`.
const MENU_BUTTONS_HEADER_Y: usize = next_row_y_with_extra_gap(STARTUP_ROW_Y, 30, 20);
/// Mirrors `overlay::ACTION_BTN` (not importable here — `pub(super)` to
/// `overlay`, and DPI-scaled there besides) so a chip is the same size as
/// the real menu's square buttons.
const CHIP_SIZE: usize = 56;
/// Divider chip width — mirrors `overlay::menu`'s halved-relative-to-a-
/// button divider width, so this layout UI looks like what capture mode
/// actually shows instead of a generic square.
const DIVIDER_CHIP_W: i64 = CHIP_SIZE as i64 / 6;
const CHIP_GAP: i64 = 8;
const CHIP_LINE_GAP: i64 = 8;
/// Gap between a row's subheader ("Shown"/"Available") and its chips.
const SUBHEADER_GAP: usize = 22;
/// Gap between the Shown block's last chip row and the Available subheader.
const BLOCK_GAP: usize = 16;
/// Width of the drop-position indicator drawn between chips while dragging.
const INSERT_BAR_W: usize = 3;

/// A chip's width: `CHIP_SIZE` for everything except `Divider`, which is
/// narrower (see `DIVIDER_CHIP_W`).
fn chip_w(b: MenuButton) -> i64 {
    if b == MenuButton::Divider {
        DIVIDER_CHIP_W
    } else {
        CHIP_SIZE as i64
    }
}

/// The row's usable width (content area minus the right margin).
fn chip_row_max_w(sw: usize) -> i64 {
    sw.saturating_sub(CONTENT_X + 16) as i64
}

/// `items` laid out left to right starting at `y0`, wrapping to further
/// lines if they don't fit `sw`. Order matches `items`. Every chip is
/// `CHIP_SIZE` tall regardless of width (see `chip_w`).
fn chip_rects(items: &[MenuButton], sw: usize, y0: usize) -> Vec<(MenuButton, Rect)> {
    let widths: Vec<i64> = items.iter().map(|&b| chip_w(b)).collect();
    let slots = wrap_slots(&widths, CHIP_GAP, chip_row_max_w(sw));
    items
        .iter()
        .copied()
        .zip(slots)
        .zip(widths)
        .map(|((b, (line, x)), w)| {
            let ry = y0 as i64 + line as i64 * (CHIP_SIZE as i64 + CHIP_LINE_GAP);
            (
                b,
                Rect {
                    x0: (CONTENT_X as i64 + x) as usize,
                    y0: ry as usize,
                    x1: (CONTENT_X as i64 + x + w) as usize,
                    y1: (ry + CHIP_SIZE as i64) as usize,
                },
            )
        })
        .collect()
}

/// Total height of `items`'s wrapped chip rows (at least one line).
fn chip_block_h(items: &[MenuButton], sw: usize) -> usize {
    let widths: Vec<i64> = items.iter().map(|&b| chip_w(b)).collect();
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

/// The Available subheader's Y — depends on how many lines the Shown block
/// wrapped to, hence the `shown`/`sw` parameters.
fn available_label_y(shown: &[MenuButton], sw: usize) -> usize {
    shown_chips_y0() + chip_block_h(shown, sw) + BLOCK_GAP
}

fn available_chips_y0(shown: &[MenuButton], sw: usize) -> usize {
    available_label_y(shown, sw) + SUBHEADER_GAP
}

/// What the "Available" row shows: every singleton not already in `shown`,
/// plus `Divider` always (it's repeatable — see the module doc). In
/// `MenuButton::ALL`'s canonical order.
fn available_menu_buttons(shown: &[MenuButton]) -> Vec<MenuButton> {
    MenuButton::ALL
        .into_iter()
        .filter(|b| b.repeatable() || !shown.contains(b))
        .collect()
}

/// Identifies one on-screen chip — needed (rather than a bare `MenuButton`)
/// because `Divider` can appear more than once in the Shown row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuChipRef {
    /// The chip at this index in `Settings::menu_buttons_shown`.
    Shown(usize),
    /// The Available row's entry for this button (see `available_menu_buttons`).
    Available(MenuButton),
}

fn shown_chip_rects(shown: &[MenuButton], sw: usize) -> Vec<(MenuChipRef, MenuButton, Rect)> {
    chip_rects(shown, sw, shown_chips_y0())
        .into_iter()
        .enumerate()
        .map(|(i, (b, r))| (MenuChipRef::Shown(i), b, r))
        .collect()
}

fn available_chip_rects(shown: &[MenuButton], sw: usize) -> Vec<(MenuChipRef, MenuButton, Rect)> {
    let available = available_menu_buttons(shown);
    chip_rects(&available, sw, available_chips_y0(shown, sw))
        .into_iter()
        .map(|(b, r)| (MenuChipRef::Available(b), b, r))
        .collect()
}

/// The chip under `(x, y)`, if any (checks both rows). Used to start a drag.
pub(super) fn menu_chip_at(shown: &[MenuButton], sw: usize, x: f64, y: f64) -> Option<MenuChipRef> {
    shown_chip_rects(shown, sw)
        .into_iter()
        .chain(available_chip_rects(shown, sw))
        .find(|(_, _, r)| inside(*r, x, y))
        .map(|(cref, _, _)| cref)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuRow {
    Shown,
    Available,
}

/// Which row `y` is over: below the Available subheader is Available,
/// everything else (including the Shown block and its own subheader) is
/// Shown. A generous, forgiving split rather than exact per-row bounds.
pub(super) fn menu_row_at(shown: &[MenuButton], sw: usize, y: f64) -> MenuRow {
    if y >= available_label_y(shown, sw) as f64 {
        MenuRow::Available
    } else {
        MenuRow::Shown
    }
}

/// Where a drop at `(x, y)` should land in `target` (already reflecting any
/// removal — see `menu_chip_insert_bar`): the index of the first chip
/// that's on an earlier line, or on the same line but to the right of `x`
/// — otherwise the end (append).
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

/// The drop-position indicator's rect for a drag currently over the Shown
/// row at `(x, y)`. `full_list` is `Settings::menu_buttons_shown`
/// unmodified — the dragged chip itself keeps rendering at its original
/// spot for the whole drag (see `draw_menu_button_chip`) rather than being
/// pulled out of the row, so `full_list` is what's actually on screen.
/// `removed_at` is the dragged chip's own index in `full_list` if it came
/// from Shown (`None` if it came from Available, i.e. nothing to remove).
///
/// Looked up via `full_list`'s positions rather than `target`'s own
/// (gap-closed) layout, which would drift out of sync with the other
/// chips' real positions once the dragged chip isn't at the very end.
/// Index-mapped rather than matched by value, since `Divider` can repeat
/// (a value-based lookup couldn't tell two dividers apart).
fn menu_chip_insert_bar(
    full_list: &[MenuButton],
    removed_at: Option<usize>,
    target: &[MenuButton],
    sw: usize,
    y0: usize,
    x: f64,
    y: f64,
) -> Rect {
    let idx = menu_chip_drop_index(target, sw, y0, x, y);
    let full_rects = chip_rects(full_list, sw, y0);
    // `target`'s index `j` is `full_list`'s index `j`, or `j + 1` past the
    // point something was removed.
    let to_full_idx = |j: usize| match removed_at {
        Some(removed) if j >= removed => j + 1,
        _ => j,
    };
    let half_gap = (CHIP_GAP / 2) as usize;

    if idx < target.len()
        && let Some((_, r)) = full_rects.get(to_full_idx(idx))
    {
        let cx = r.x0.saturating_sub(half_gap);
        return Rect {
            x0: cx.saturating_sub(INSERT_BAR_W / 2),
            y0: r.y0,
            x1: cx.saturating_sub(INSERT_BAR_W / 2) + INSERT_BAR_W,
            y1: r.y1,
        };
    }
    if idx > 0
        && let Some((_, r)) = full_rects.get(to_full_idx(idx - 1))
    {
        let cx = r.x1 + half_gap;
        return Rect {
            x0: cx.saturating_sub(INSERT_BAR_W / 2),
            y0: r.y0,
            x1: cx.saturating_sub(INSERT_BAR_W / 2) + INSERT_BAR_W,
            y1: r.y1,
        };
    }
    // `target` is empty (Shown only ever held the dragged chip).
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
    /// caller) at the current cursor position.
    ///
    /// Dropped on Shown: reorders (if it came from Shown) or adds a new
    /// instance (if it came from Available — for a singleton this is the
    /// only copy there ever was; for `Divider` it's always a fresh one).
    /// Dropped on Available: if it came from Shown, that instance is
    /// simply removed (a singleton becomes available again automatically,
    /// since Available is computed from what's *not* in Shown); if it
    /// came from Available, nothing happened to begin with. No-op (drag
    /// simply cancelled) if the cursor isn't over either row.
    pub(super) fn drop_menu_chip(&mut self, dragged: MenuChipRef) {
        let sw = self.size.0;
        let (x, y) = self.cursor;
        let row = menu_row_at(&self.menu_buttons_shown, sw, y);

        let Some(value) = (match dragged {
            MenuChipRef::Shown(i) => self.menu_buttons_shown.get(i).copied(),
            MenuChipRef::Available(b) => Some(b),
        }) else {
            return;
        };
        if let MenuChipRef::Shown(i) = dragged
            && i < self.menu_buttons_shown.len()
        {
            self.menu_buttons_shown.remove(i);
        }

        if row == MenuRow::Shown {
            let i = menu_chip_drop_index(&self.menu_buttons_shown, sw, shown_chips_y0(), x, y);
            self.menu_buttons_shown
                .insert(i.min(self.menu_buttons_shown.len()), value);
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
    menu_drag: Option<MenuChipRef>,
    chip_hover: Option<MenuChipRef>,
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
    // "Available" (things you can drag in); drag within "Shown" to
    // reorder. Each chip is a square drawn exactly like the real menu's
    // buttons (`draw_icon_button`), so it's obvious what it'll look like —
    // except `Divider`, drawn as the thin line it actually is. Chips
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
    for (cref, menu_btn, r) in shown_chip_rects(menu_buttons_shown, sw) {
        draw_menu_button_chip(
            canvas, t, r, cref, menu_btn, menu_drag, chip_hover, BTN_BG, TEXT, hover_tint,
        );
    }
    let available_y = available_label_y(menu_buttons_shown, sw);
    t.draw(
        canvas,
        CONTENT_X as f32,
        (available_y + 14) as f32,
        "Available",
        13.0,
        DIM,
    );
    for (cref, menu_btn, r) in available_chip_rects(menu_buttons_shown, sw) {
        draw_menu_button_chip(
            canvas, t, r, cref, menu_btn, menu_drag, chip_hover, FIELD_BG, DIM, hover_tint,
        );
    }

    // While dragging onto Shown, a thin bar shows exactly where the chip
    // would land if dropped right now — recomputed live from the cursor
    // position, but otherwise nothing about the layout changes mid-drag.
    // Dropping onto Available just removes/no-ops (see `drop_menu_chip`),
    // so there's nothing to preview there.
    if let Some(dragged) = menu_drag
        && menu_row_at(menu_buttons_shown, sw, cursor.1) == MenuRow::Shown
    {
        let (removed_at, target) = match dragged {
            MenuChipRef::Shown(i) => {
                let mut t = menu_buttons_shown.to_vec();
                if i < t.len() {
                    t.remove(i);
                }
                (Some(i), t)
            }
            MenuChipRef::Available(_) => (None, menu_buttons_shown.to_vec()),
        };
        let bar = menu_chip_insert_bar(
            menu_buttons_shown,
            removed_at,
            &target,
            sw,
            shown_chips_y0(),
            cursor.0,
            cursor.1,
        );
        canvas.fill(bar, ACCENT);
    }
}

/// Draws one menu-button chip: the real menu's square-button look
/// (`draw_icon_button`) for anything except `Divider`, which draws as the
/// same thin line it is in the real menu. The dragged chip keeps rendering
/// right where it started (see `menu_chip_insert_bar`), just with the same
/// subtle `hover_tint` treatment a plain hover gets — no separate
/// "selected" color, so dragging it back over its own spot looks like
/// nothing special is happening.
#[allow(clippy::too_many_arguments)]
fn draw_menu_button_chip(
    canvas: &mut Canvas,
    t: &TextRenderer,
    r: Rect,
    cref: MenuChipRef,
    menu_btn: MenuButton,
    menu_drag: Option<MenuChipRef>,
    chip_hover: Option<MenuChipRef>,
    base: u32,
    fg: u32,
    hover_tint: impl Fn(u32) -> u32,
) {
    let dragging = menu_drag == Some(cref);
    // No hover highlight on other chips while any drag is in progress —
    // sweeping the cursor across the row while dragging shouldn't light up
    // everything it passes over. `chip_hover` only reflects an actual
    // `CursorMoved`, not raw cursor-vs-rect at draw time, so a chip
    // doesn't light up just because the layout shifted under a
    // stationary cursor (e.g. right after a drop reorders things).
    let hovered = menu_drag.is_none() && chip_hover == Some(cref);
    let bg = if dragging || hovered {
        hover_tint(base)
    } else {
        base
    };
    if menu_btn == MenuButton::Divider {
        canvas.fill(r, bg);
        let inset = (r.height() as f64 * 0.2) as i64;
        let cx = ((r.x0 + r.x1) / 2) as i64;
        canvas.line(cx, r.y0 as i64 + inset, cx, r.y1 as i64 - inset, 2, fg);
    } else {
        draw_icon_button(canvas, r, bg, fg, menu_btn.label(), fg, t);
    }
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
        // Dragging the FIRST of 3 chips (index 0) toward the far right:
        // `target` (Copy, Quit) would compact leftward if laid out on its
        // own, but since the dragged chip keeps rendering in place,
        // Copy/Quit are still exactly where the *full* list puts them —
        // the bar must align with Quit's true on-screen right edge, not
        // with a recomputed "2 items only" layout (which would put it
        // noticeably further left, one chip-width + gap short).
        let full = [MenuButton::Save, MenuButton::Copy, MenuButton::Quit];
        let target = [MenuButton::Copy, MenuButton::Quit];
        let full_rects = chip_rects(&full, 720, 0);
        let quit_rect = full_rects
            .iter()
            .find(|(b, _)| *b == MenuButton::Quit)
            .unwrap()
            .1;

        let bar = menu_chip_insert_bar(
            &full,
            Some(0),
            &target,
            720,
            0,
            (quit_rect.x1 + 100) as f64,
            quit_rect.y0 as f64 + 1.0,
        );

        assert!(bar.x0 >= quit_rect.x1);
        assert!(bar.x0 < quit_rect.x1 + CHIP_GAP as usize * 2);
    }

    #[test]
    fn menu_chip_insert_bar_tells_apart_duplicate_dividers_by_index() {
        // Two dividers around Save; dragging the FIRST one (index 0) far to
        // the right should land the bar after Save/the second divider —
        // not confuse the two dividers via a value-based lookup.
        let full = [MenuButton::Divider, MenuButton::Save, MenuButton::Divider];
        let target = [MenuButton::Save, MenuButton::Divider];
        let full_rects = chip_rects(&full, 720, 0);
        let last_rect = full_rects.last().unwrap().1;

        let bar = menu_chip_insert_bar(
            &full,
            Some(0),
            &target,
            720,
            0,
            (last_rect.x1 + 100) as f64,
            last_rect.y0 as f64 + 1.0,
        );

        assert!(bar.x0 >= last_rect.x1);
    }

    #[test]
    fn chip_rects_are_same_height_in_order_without_overlap() {
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
    fn chip_rects_make_a_divider_narrower_than_a_regular_chip() {
        let items = [MenuButton::Save, MenuButton::Divider];
        let rects = chip_rects(&items, 720, 0);
        assert!(rects[1].1.width() < rects[0].1.width());
        assert_eq!(rects[1].1.height(), CHIP_SIZE);
    }

    #[test]
    fn menu_row_at_splits_exactly_on_the_available_subheader() {
        let shown = [MenuButton::Save];
        let boundary = available_label_y(&shown, 720);
        assert_eq!(
            menu_row_at(&shown, 720, (boundary - 1) as f64),
            MenuRow::Shown
        );
        assert_eq!(
            menu_row_at(&shown, 720, boundary as f64),
            MenuRow::Available
        );
    }

    #[test]
    fn available_menu_buttons_excludes_shown_singletons_but_always_includes_divider() {
        let shown = [MenuButton::Save, MenuButton::Divider, MenuButton::Divider];
        let available = available_menu_buttons(&shown);
        assert!(!available.contains(&MenuButton::Save));
        assert!(available.contains(&MenuButton::Copy));
        // Divider stays available even though it's already shown twice.
        assert_eq!(
            available
                .iter()
                .filter(|b| **b == MenuButton::Divider)
                .count(),
            1
        );
    }

    #[test]
    fn menu_chip_at_identifies_shown_chips_by_index_not_just_value() {
        // Two Save chips can't both exist (singleton), but two dividers can
        // — and each must resolve to its own index, not the same ref.
        let shown = [MenuButton::Divider, MenuButton::Save, MenuButton::Divider];
        let rects = shown_chip_rects(&shown, 720);
        let (first_divider_ref, _, first_divider_rect) = rects[0];
        let (second_divider_ref, _, second_divider_rect) = rects[2];
        assert_ne!(first_divider_ref, second_divider_ref);

        let hit_first = menu_chip_at(
            &shown,
            720,
            ((first_divider_rect.x0 + first_divider_rect.x1) / 2) as f64,
            ((first_divider_rect.y0 + first_divider_rect.y1) / 2) as f64,
        );
        let hit_second = menu_chip_at(
            &shown,
            720,
            ((second_divider_rect.x0 + second_divider_rect.x1) / 2) as f64,
            ((second_divider_rect.y0 + second_divider_rect.y1) / 2) as f64,
        );
        assert_eq!(hit_first, Some(first_divider_ref));
        assert_eq!(hit_second, Some(second_divider_ref));
    }
}
