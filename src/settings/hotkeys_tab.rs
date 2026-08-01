//! Hotkeys tab: global hotkeys and in-app shortcuts — listing, binding,
//! conflict detection, and chip layout.

use winit::keyboard::{KeyCode, ModifiersState};

use super::{
    ACCENT, Btn, CONTENT_X, CaptureTarget, SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, Settings,
    SettingsResult, field, hover_tint_for, scrollbar_thumb_rect, stroke_top_bottom_aware,
    theme_colors, wrap_slots,
};
use crate::localkey::LocalKey;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect, draw_refresh_icon_clipped};

/// In-app shortcut editable from the Hotkeys tab (Escape and Delete/Backspace
/// are excluded).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LocalAction {
    Undo,
    Redo,
    ReuseRegion,
    ClearSelection,
    SaveAs,
    EditExternal,
    Quit,
    MenuSave,
    MenuCopy,
    MenuEdit,
    MenuUpload,
    MenuRecord,
    EditorResetZoom,
    EditorToolSelect,
    EditorToolArrow,
    EditorToolPolyline,
    EditorToolDraw,
    EditorToolRect,
    EditorToolEllipse,
    EditorToolText,
    EditorToolNumberMarker,
}

/// Section heading group used for display and layout in the Hotkeys tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionGroup {
    General,
    Region,
    Editor,
}

pub(super) struct HotkeyRow {
    pub(super) action: LocalAction,
    label: &'static str,
    group: ActionGroup,
}

/// The 21 rows shown in the Hotkeys tab, in display order within each group.
pub(super) const HOTKEY_ROWS: [HotkeyRow; 21] = [
    HotkeyRow {
        action: LocalAction::Undo,
        label: "Undo",
        group: ActionGroup::General,
    },
    HotkeyRow {
        action: LocalAction::Redo,
        label: "Redo",
        group: ActionGroup::General,
    },
    HotkeyRow {
        action: LocalAction::ReuseRegion,
        label: "Reuse previous region",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::ClearSelection,
        label: "Clear selection",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::SaveAs,
        label: "Save as...",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::EditExternal,
        label: "Edit with external editor",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::MenuSave,
        label: "Save",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::MenuCopy,
        label: "Copy",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::MenuEdit,
        label: "Edit",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::MenuUpload,
        label: "Upload",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::MenuRecord,
        label: "Record video",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::Quit,
        label: "Quit",
        group: ActionGroup::Region,
    },
    HotkeyRow {
        action: LocalAction::EditorToolSelect,
        label: "Select",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolArrow,
        label: "Arrow",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolPolyline,
        label: "Polyline",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolDraw,
        label: "Draw",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolRect,
        label: "Rect",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolEllipse,
        label: "Ellipse",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolText,
        label: "Text",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorToolNumberMarker,
        label: "Number marker",
        group: ActionGroup::Editor,
    },
    HotkeyRow {
        action: LocalAction::EditorResetZoom,
        label: "Reset zoom",
        group: ActionGroup::Editor,
    },
];

const GROUPS: [(ActionGroup, &str); 3] = [
    (ActionGroup::General, "General"),
    (ActionGroup::Region, "Region Selection"),
    (ActionGroup::Editor, "Editor"),
];

/// Active-key sets per context, used for conflict detection (Undo/Redo
/// belong to both).
const REGION_GROUP: [LocalAction; 12] = [
    LocalAction::Undo,
    LocalAction::Redo,
    LocalAction::ReuseRegion,
    LocalAction::ClearSelection,
    LocalAction::SaveAs,
    LocalAction::EditExternal,
    LocalAction::MenuSave,
    LocalAction::MenuCopy,
    LocalAction::MenuEdit,
    LocalAction::MenuUpload,
    LocalAction::MenuRecord,
    LocalAction::Quit,
];
const EDITOR_GROUP: [LocalAction; 11] = [
    LocalAction::Undo,
    LocalAction::Redo,
    LocalAction::EditorResetZoom,
    LocalAction::EditorToolSelect,
    LocalAction::EditorToolArrow,
    LocalAction::EditorToolPolyline,
    LocalAction::EditorToolDraw,
    LocalAction::EditorToolRect,
    LocalAction::EditorToolEllipse,
    LocalAction::EditorToolText,
    LocalAction::EditorToolNumberMarker,
];

/// The groups to check for conflicts when rebinding `action`. Undo/Redo are
/// used in both contexts, so both are checked.
fn conflict_groups(action: LocalAction) -> &'static [&'static [LocalAction]] {
    use LocalAction::*;
    match action {
        Undo | Redo => &[&REGION_GROUP, &EDITOR_GROUP],
        ReuseRegion | ClearSelection | SaveAs | EditExternal | Quit | MenuSave | MenuCopy
        | MenuEdit | MenuUpload | MenuRecord => &[&REGION_GROUP],
        EditorResetZoom
        | EditorToolSelect
        | EditorToolArrow
        | EditorToolPolyline
        | EditorToolDraw
        | EditorToolRect
        | EditorToolEllipse
        | EditorToolText
        | EditorToolNumberMarker => &[&EDITOR_GROUP],
    }
}

/// Returns the other action already using `candidate`, if any, when trying to
/// bind it to `action` (`current` is the full set of existing bindings).
fn find_conflict(
    current: &[(LocalAction, LocalKey)],
    action: LocalAction,
    candidate: LocalKey,
) -> Option<LocalAction> {
    let groups = conflict_groups(action);
    current
        .iter()
        .find(|(a, k)| *a != action && groups.iter().any(|g| g.contains(a)) && *k == candidate)
        .map(|(a, _)| *a)
}

/// Whether `action`'s own current bindings already include `candidate` — used
/// to silently no-op re-recording the same key from the "+" chip.
fn is_duplicate_within_action(
    current: &[(LocalAction, LocalKey)],
    action: LocalAction,
    candidate: LocalKey,
) -> bool {
    current.iter().any(|(a, k)| *a == action && *k == candidate)
}

fn label_for(action: LocalAction) -> &'static str {
    HOTKEY_ROWS
        .iter()
        .find(|r| r.action == action)
        .map(|r| r.label)
        .unwrap_or("?")
}

/// The default bindings for `action` from `cfg` (multiple allowed). The
/// `Config`-based counterpart of `Settings::local_keys`; always reads from
/// `Config::default()` as the single source of truth for defaults.
fn default_local_keys(cfg: &crate::store::hotkeys::HotkeyConfig, action: LocalAction) -> &[String] {
    use LocalAction::*;
    match action {
        Undo => &cfg.hotkey_undo,
        Redo => &cfg.hotkey_redo,
        ReuseRegion => &cfg.hotkey_reuse_region,
        ClearSelection => &cfg.hotkey_clear_selection,
        SaveAs => &cfg.hotkey_save_as,
        EditExternal => &cfg.hotkey_edit_external,
        Quit => &cfg.hotkey_quit,
        MenuSave => &cfg.hotkey_menu_save,
        MenuCopy => &cfg.hotkey_menu_copy,
        MenuEdit => &cfg.hotkey_menu_edit,
        MenuUpload => &cfg.hotkey_menu_upload,
        MenuRecord => &cfg.hotkey_menu_record,
        EditorResetZoom => &cfg.hotkey_editor_reset_zoom,
        EditorToolSelect => &cfg.hotkey_editor_tool_select,
        EditorToolArrow => &cfg.hotkey_editor_tool_arrow,
        EditorToolPolyline => &cfg.hotkey_editor_tool_polyline,
        EditorToolDraw => &cfg.hotkey_editor_tool_draw,
        EditorToolRect => &cfg.hotkey_editor_tool_rect,
        EditorToolEllipse => &cfg.hotkey_editor_tool_ellipse,
        EditorToolText => &cfg.hotkey_editor_tool_text,
        EditorToolNumberMarker => &cfg.hotkey_editor_tool_number_marker,
    }
}

/// Draw/hit-test info for one Hotkeys tab row. The global hotkey
/// (`Btn::Capture`) has no `LocalAction` (it's OS-registered, keyed by
/// physical `KeyCode`) but is listed alongside the local ones. Since an
/// action can have multiple bindings, the right side of a row is a run of
/// existing chips (each with a delete button) plus a trailing "+" chip,
/// which can wrap onto multiple lines.
struct HotkeyRowGeom {
    /// Click target for the "+"/recording chip (`Btn::Capture` or
    /// `Btn::CaptureLocal`); starts recording a new binding for this action.
    add_btn: Btn,
    /// Resets this row to its default (`Btn::ResetLocal`/`Btn::ResetGlobal`).
    reset_btn: Btn,
    label: &'static str,
    /// Existing binding chips: the delete `Btn`, the chip's viewport-clamped
    /// rect (background/border/text), the X icon's true (unclamped) center —
    /// computed this way so it doesn't drift as the chip is scrolled past the
    /// viewport edge, and drawn via `line_clipped` — the X icon's own hit
    /// rect (kept small so clicking elsewhere in the chip doesn't delete it),
    /// and whether the top/bottom edge is actually clipped by the viewport
    /// (passed to `stroke_top_bottom_aware` so a clipped edge doesn't get a
    /// stray border line that would look like a height change).
    chips: Vec<ChipGeom>,
    /// Viewport-clamped rect of the "+" chip (shows "Press key..." while
    /// recording).
    add_visible: Rect,
    /// True (unclamped) center Y of the "+" icon — same rationale as
    /// `icon_center` on chips (X is unaffected by clamping, so it's read
    /// straight from `add_visible`).
    add_center_y: i64,
    /// Reset-icon rect (viewport-clamped).
    reset_visible: Rect,
    /// The row's true (unclamped, scroll-following) rect, used for label
    /// baseline math — using the clamped rect would drift as the row is
    /// clipped. Rows have no background/border of their own (only chips do),
    /// so there's no clamped `visible` counterpart.
    raw: Rect,
    /// Unclamped rect of the reset icon; its center/radius are derived from
    /// this so the icon doesn't appear to shrink as it's clipped.
    reset_raw: Rect,
}

/// A Hotkeys tab group heading (label, scroll-adjusted raw Y). Not clamped;
/// the drawing side clips it to the viewport with `TextRenderer::draw_clipped`
/// while its position keeps following scroll.
struct HotkeyHeaderGeom {
    label: &'static str,
    raw_y: f32,
}

type HotkeyRowLayout = Vec<HotkeyRowGeom>;
type HotkeyHeaderLayout = Vec<HotkeyHeaderGeom>;
/// Geometry for one chip: delete `Btn`, clamped rect, X icon's true center,
/// X icon's hit rect, top/bottom actually-clipped flags. See
/// `HotkeyRowGeom::chips`.
type ChipGeom = (Btn, Rect, (i64, i64), Rect, bool, bool);
/// A row from `hotkey_raw_layout`: key-area `Btn`, reset `Btn`, label, Y from
/// content top, height.
type HotkeyRawRowLayout = Vec<(Btn, Btn, &'static str, i64, usize)>;
/// A header from `hotkey_raw_layout`: label, Y from content top.
type HotkeyRawHeaderLayout = Vec<(&'static str, i64)>;

const HOTKEY_ROW_GAP: usize = 4;
const HOTKEY_HEADER_H: usize = 22;
const HOTKEY_GROUP_GAP: usize = 10;
const HOTKEY_KEY_X_OFFSET: usize = 260;
/// Side length of the (square) reset-icon area, and its gap from the key area.
const HOTKEY_RESET_W: usize = 24;
const HOTKEY_RESET_GAP: usize = 8;

/// Chip height, horizontal/vertical gaps, and text padding (all fixed).
/// Chip width itself is variable, sized to its text (see `chip_width`).
const CHIP_H: i64 = 22;
const CHIP_GAP: i64 = 6;
const CHIP_LINE_GAP: i64 = 6;
const CHIP_TEXT_SIZE: f32 = 13.0;
/// Left padding before the text.
const CHIP_PAD_X: i64 = 8;
/// Reserved width for the X icon plus right padding.
const CHIP_X_RESERVED: i64 = 20;
const CHIP_MIN_W: i64 = 50;
/// Hit-test radius around the X icon's center. Kept only slightly larger
/// than the icon itself (4px radius) so clicking elsewhere in the chip
/// doesn't delete it.
const CHIP_DELETE_HIT_HALF: i64 = 8;
/// Fixed fallback width used when no `TextRenderer` is available. Same
/// pattern as `session_limit_label_w`/`bitrate_label_w`: text-dependent
/// layout takes `Option<&TextRenderer>` and falls back to a fixed value.
const CHIP_TEXT_FALLBACK_W: i64 = 70;
/// Width of the "+" (start recording) button. Unlike a chip, it's a square
/// holding just a small icon, not text — same side length as `CHIP_H`.
const ADD_BTN_W: i64 = CHIP_H;

/// Width of one chip: measured text plus padding, or a fixed fallback if
/// `text` is unavailable.
fn chip_width(text: Option<&TextRenderer>, spec: &str) -> i64 {
    let text_w = text
        .map(|tr| tr.text_width(spec, CHIP_TEXT_SIZE).ceil() as i64)
        .unwrap_or(CHIP_TEXT_FALLBACK_W);
    (text_w + CHIP_PAD_X + CHIP_X_RESERVED).max(CHIP_MIN_W)
}

/// Wrapped (line, x_offset, width) for `specs` (existing chips) plus a
/// trailing "+" button (`specs.len() + 1` entries, "+" last).
fn chip_slots(text: Option<&TextRenderer>, specs: &[String], max_w: i64) -> Vec<(usize, i64, i64)> {
    let mut widths: Vec<i64> = specs.iter().map(|s| chip_width(text, s)).collect();
    widths.push(ADD_BTN_W);
    wrap_slots(&widths, CHIP_GAP, max_w)
        .into_iter()
        .zip(widths)
        .map(|((line, x), w)| (line, x, w))
        .collect()
}

/// Row height from `chip_slots`'s result (at least one line).
fn chip_row_h(slots: &[(usize, i64, i64)]) -> usize {
    let lines = slots.last().map(|(l, _, _)| l + 1).unwrap_or(1) as i64;
    (lines * CHIP_H + (lines - 1).max(0) * CHIP_LINE_GAP) as usize
}

/// Width of the chip area (key column), shared with the calculation from
/// `HOTKEY_KEY_X_OFFSET`/`HOTKEY_RESET_W`/`HOTKEY_RESET_GAP`.
fn chip_area_width(sw: usize) -> i64 {
    let (key_x0, key_x1) = chip_area_x(sw);
    (key_x1 - key_x0) as i64
}

/// The chip area's (left x, right x), used by both `hotkey_raw_layout` and
/// `hotkey_layout`.
fn chip_area_x(sw: usize) -> (usize, usize) {
    let viewport_x1 = sw.saturating_sub(16);
    let key_x0 = CONTENT_X + HOTKEY_KEY_X_OFFSET;
    let reset_x0 = viewport_x1.saturating_sub(HOTKEY_RESET_W);
    let key_x1 = reset_x0.saturating_sub(HOTKEY_RESET_GAP).max(key_x0);
    (key_x0, key_x1)
}

/// The Hotkeys tab's visible content area (outside it is scrolled out of
/// view). Takes the current window size `(sw, sh)` rather than a fixed value
/// so it follows window resizes.
pub(super) fn hotkey_viewport(sw: usize, sh: usize) -> Rect {
    Rect {
        x0: CONTENT_X,
        y0: 68,
        x1: sw.saturating_sub(16),
        y1: sh.saturating_sub(64),
    }
}

/// The current bindings for `action` (empty slice if absent from `local_specs`).
fn specs_for(local_specs: &[(LocalAction, Vec<String>)], action: LocalAction) -> &[String] {
    local_specs
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, v)| v.as_slice())
        .unwrap_or(&[])
}

/// Raw row/header positions relative to content top (0), ignoring scroll —
/// a pure layout function. Like most layout functions here it takes
/// `Option<&TextRenderer>` for measuring chip widths, falling back to a
/// fixed width if unavailable. Row height depends on chip wrapping (i.e. the
/// row's actual bound text), so it also takes `sw` to compute available width.
fn hotkey_raw_layout(
    text: Option<&TextRenderer>,
    sw: usize,
    global_specs: &[String],
    local_specs: &[(LocalAction, Vec<String>)],
) -> (HotkeyRawRowLayout, HotkeyRawHeaderLayout) {
    let max_w = chip_area_width(sw);
    let mut rows = Vec::with_capacity(HOTKEY_ROWS.len() + 1);
    let mut headers = Vec::with_capacity(GROUPS.len());
    let mut y: i64 = 0;
    for (group, glabel) in GROUPS {
        headers.push((glabel, y));
        y += HOTKEY_HEADER_H as i64;
        if group == ActionGroup::General {
            // The global hotkey (launch capture) has no LocalAction, so it's
            // added manually at the head of the General group.
            let h = chip_row_h(&chip_slots(text, global_specs, max_w));
            rows.push((Btn::Capture, Btn::ResetGlobal, "Launch capture", y, h));
            y += (h + HOTKEY_ROW_GAP) as i64;
        }
        for row in HOTKEY_ROWS.iter().filter(|r| r.group == group) {
            let specs = specs_for(local_specs, row.action);
            let h = chip_row_h(&chip_slots(text, specs, max_w));
            rows.push((
                Btn::CaptureLocal(row.action),
                Btn::ResetLocal(row.action),
                row.label,
                y,
                h,
            ));
            y += (h + HOTKEY_ROW_GAP) as i64;
        }
        y += HOTKEY_GROUP_GAP as i64;
    }
    (rows, headers)
}

/// Total content height, used to compute the scroll range.
pub(super) fn hotkey_content_height(
    text: Option<&TextRenderer>,
    sw: usize,
    global_specs: &[String],
    local_specs: &[(LocalAction, Vec<String>)],
) -> i64 {
    let (rows, _) = hotkey_raw_layout(text, sw, global_specs, local_specs);
    rows.last()
        .map(|(_, _, _, y, h)| y + *h as i64)
        .unwrap_or(0)
}

/// Applies `scroll` (px) and returns only the rows/headers that (at least
/// partially) overlap the viewport. Rows carry both a viewport-clamped rect
/// (`visible`, for hit-testing/fill) and the unclamped rect (`raw`, for
/// baseline math). Actual draw clipping happens on the caller's side via
/// `TextRenderer::draw_clipped` with the viewport rect; this function only
/// computes overlap and clamping. `global_specs`/`local_specs` drive chip
/// wrapping and text-width measurement.
fn hotkey_layout(
    scroll: i32,
    sw: usize,
    sh: usize,
    text: Option<&TextRenderer>,
    global_specs: &[String],
    local_specs: &[(LocalAction, Vec<String>)],
) -> (HotkeyRowLayout, HotkeyHeaderLayout) {
    let viewport = hotkey_viewport(sw, sh);
    let (raw_rows, raw_headers) = hotkey_raw_layout(text, sw, global_specs, local_specs);
    let (key_x0, key_x1) = chip_area_x(sw);
    let max_w = chip_area_width(sw);
    let reset_x0 = viewport.x1.saturating_sub(HOTKEY_RESET_W);

    let rows = raw_rows
        .into_iter()
        .filter_map(|(btn, reset_btn, label, y, h)| {
            let raw_y0 = viewport.y0 as i64 + y - scroll as i64;
            let raw_y1 = raw_y0 + h as i64;
            if raw_y1 <= viewport.y0 as i64 || raw_y0 >= viewport.y1 as i64 {
                return None; // no overlap with the viewport at all
            }
            let clamped_y0 = raw_y0.max(viewport.y0 as i64) as usize;
            let clamped_y1 = raw_y1.min(viewport.y1 as i64) as usize;

            let specs: &[String] = match btn {
                Btn::Capture => global_specs,
                Btn::CaptureLocal(a) => specs_for(local_specs, a),
                _ => &[],
            };
            let slots = chip_slots(text, specs, max_w);

            // One chip's clamped rect (Y clamped to `viewport`, None if
            // entirely outside), plus its true (unclamped) center Y and
            // whether the top/bottom edge is actually clipped. The caller
            // only strokes unclipped edges (`stroke_top_bottom_aware`) to
            // avoid a stray border line at the clip boundary that would look
            // like a height change. The X icon is positioned from this true
            // center Y rather than the clamped rect (same idea as
            // `draw_refresh_icon_clipped`) and drawn with `line_clipped`, so
            // it doesn't appear to drift as the chip is scrolled past the edge.
            let clamp_chip = |line: usize, x_off: i64, w: i64| -> Option<(Rect, i64, bool, bool)> {
                let cy0 = raw_y0 + line as i64 * (CHIP_H + CHIP_LINE_GAP);
                let cy1 = cy0 + CHIP_H;
                if cy1 <= viewport.y0 as i64 || cy0 >= viewport.y1 as i64 {
                    return None;
                }
                let cx0 = key_x0 + x_off as usize;
                let top_clipped = cy0 < viewport.y0 as i64;
                let bottom_clipped = cy1 > viewport.y1 as i64;
                let true_cy = (cy0 + cy1) / 2;
                Some((
                    Rect {
                        x0: cx0,
                        y0: cy0.max(viewport.y0 as i64) as usize,
                        x1: cx0 + w as usize,
                        y1: cy1.min(viewport.y1 as i64) as usize,
                    },
                    true_cy,
                    top_clipped,
                    bottom_clipped,
                ))
            };

            let chips = slots[..specs.len()]
                .iter()
                .enumerate()
                .filter_map(|(idx, &(line, x_off, w))| {
                    let (rect, true_cy, top_clipped, bottom_clipped) = clamp_chip(line, x_off, w)?;
                    let remove_btn = match btn {
                        Btn::Capture => Btn::RemoveGlobalBinding(idx),
                        Btn::CaptureLocal(a) => Btn::RemoveLocalBinding(a, idx),
                        _ => return None, // unreachable: btn is always one of the two above
                    };
                    // X icon center: x is unaffected by clamping (`rect.x1 - 10`),
                    // y uses the true (unclamped) center `true_cy`.
                    let cx = rect.x1 as i64 - 10;
                    let icon_center = (cx, true_cy);
                    // Hit area is centered on the icon but stays within the
                    // visible (clamped) chip bounds.
                    let (rx0, rx1) = (rect.x0 as i64, rect.x1 as i64);
                    let (ry0, ry1) = (rect.y0 as i64, rect.y1 as i64);
                    let delete_rect = Rect {
                        x0: (cx - CHIP_DELETE_HIT_HALF).clamp(rx0, rx1) as usize,
                        y0: (true_cy - CHIP_DELETE_HIT_HALF).clamp(ry0, ry1) as usize,
                        x1: (cx + CHIP_DELETE_HIT_HALF).clamp(rx0, rx1) as usize,
                        y1: (true_cy + CHIP_DELETE_HIT_HALF).clamp(ry0, ry1) as usize,
                    };
                    Some((
                        remove_btn,
                        rect,
                        icon_center,
                        delete_rect,
                        top_clipped,
                        bottom_clipped,
                    ))
                })
                .collect();

            let &(add_line, add_x_off, add_w) = slots.last().expect("chip_slots は必ず1件以上返す");
            // Same rationale as the chip X icons: the "+" icon's center Y
            // uses the true (unclamped) position (`add_center_y`) so it
            // doesn't appear to drift as it's scrolled past the viewport edge.
            let (add_visible, add_center_y) = match clamp_chip(add_line, add_x_off, add_w) {
                Some((rect, true_cy, _, _)) => (rect, true_cy),
                None => (
                    Rect {
                        x0: key_x0,
                        y0: clamped_y0,
                        x1: key_x0,
                        y1: clamped_y0,
                    },
                    clamped_y0 as i64,
                ),
            };

            Some(HotkeyRowGeom {
                add_btn: btn,
                reset_btn,
                label,
                chips,
                add_visible,
                add_center_y,
                reset_visible: Rect {
                    x0: reset_x0,
                    y0: clamped_y0,
                    x1: reset_x0 + HOTKEY_RESET_W,
                    y1: clamped_y1,
                },
                raw: Rect {
                    x0: key_x0,
                    y0: raw_y0.max(0) as usize,
                    x1: key_x1,
                    y1: raw_y1.max(0) as usize,
                },
                reset_raw: Rect {
                    x0: reset_x0,
                    y0: raw_y0.max(0) as usize,
                    x1: reset_x0 + HOTKEY_RESET_W,
                    y1: raw_y1.max(0) as usize,
                },
            })
        })
        .collect();

    let headers = raw_headers
        .into_iter()
        .filter_map(|(label, y)| {
            let raw_y0 = viewport.y0 as i64 + y - scroll as i64;
            let raw_y1 = raw_y0 + HOTKEY_HEADER_H as i64;
            if raw_y1 <= viewport.y0 as i64 || raw_y0 >= viewport.y1 as i64 {
                return None;
            }
            Some(HotkeyHeaderGeom {
                label,
                raw_y: raw_y0 as f32,
            })
        })
        .collect();

    (rows, headers)
}

/// Builds a hotkey spec string from a keyboard event (a bare modifier key is
/// not accepted as the main key; the result is only used if `crate::hotkey`
/// can parse it back).
pub(super) fn build_hotkey(mods: ModifiersState, code: KeyCode) -> Option<String> {
    // A bare modifier key is not accepted as the main key.
    let name = match code {
        KeyCode::ControlLeft
        | KeyCode::ControlRight
        | KeyCode::ShiftLeft
        | KeyCode::ShiftRight
        | KeyCode::AltLeft
        | KeyCode::AltRight
        | KeyCode::SuperLeft
        | KeyCode::SuperRight => return None,
        other => format!("{other:?}"),
    };
    let mut spec = String::new();
    if mods.control_key() {
        spec.push_str("Ctrl+");
    }
    if mods.shift_key() {
        spec.push_str("Shift+");
    }
    if mods.alt_key() {
        spec.push_str("Alt+");
    }
    if mods.super_key() {
        spec.push_str("Super+");
    }
    spec.push_str(&name);
    // Only accept it if the code name resolves.
    crate::hotkey::parse(&spec).map(|_| spec)
}

impl Settings {
    /// Current bindings for every local action, as input to
    /// `hotkey_raw_layout`/`hotkey_layout`'s chip-wrapping calculation.
    pub(super) fn local_bindings_snapshot(&self) -> Vec<(LocalAction, Vec<String>)> {
        HOTKEY_ROWS
            .iter()
            .map(|r| (r.action, self.local_keys(r.action).clone()))
            .collect()
    }

    pub(super) fn local_keys(&self, a: LocalAction) -> &Vec<String> {
        use LocalAction::*;
        match a {
            Undo => &self.hotkey_undo,
            Redo => &self.hotkey_redo,
            ReuseRegion => &self.hotkey_reuse_region,
            ClearSelection => &self.hotkey_clear_selection,
            SaveAs => &self.hotkey_save_as,
            EditExternal => &self.hotkey_edit_external,
            Quit => &self.hotkey_quit,
            MenuSave => &self.hotkey_menu_save,
            MenuCopy => &self.hotkey_menu_copy,
            MenuEdit => &self.hotkey_menu_edit,
            MenuUpload => &self.hotkey_menu_upload,
            MenuRecord => &self.hotkey_menu_record,
            EditorResetZoom => &self.hotkey_editor_reset_zoom,
            EditorToolSelect => &self.hotkey_editor_tool_select,
            EditorToolArrow => &self.hotkey_editor_tool_arrow,
            EditorToolPolyline => &self.hotkey_editor_tool_polyline,
            EditorToolDraw => &self.hotkey_editor_tool_draw,
            EditorToolRect => &self.hotkey_editor_tool_rect,
            EditorToolEllipse => &self.hotkey_editor_tool_ellipse,
            EditorToolText => &self.hotkey_editor_tool_text,
            EditorToolNumberMarker => &self.hotkey_editor_tool_number_marker,
        }
    }

    fn local_keys_mut(&mut self, a: LocalAction) -> &mut Vec<String> {
        use LocalAction::*;
        match a {
            Undo => &mut self.hotkey_undo,
            Redo => &mut self.hotkey_redo,
            ReuseRegion => &mut self.hotkey_reuse_region,
            ClearSelection => &mut self.hotkey_clear_selection,
            SaveAs => &mut self.hotkey_save_as,
            EditExternal => &mut self.hotkey_edit_external,
            Quit => &mut self.hotkey_quit,
            MenuSave => &mut self.hotkey_menu_save,
            MenuCopy => &mut self.hotkey_menu_copy,
            MenuEdit => &mut self.hotkey_menu_edit,
            MenuUpload => &mut self.hotkey_menu_upload,
            MenuRecord => &mut self.hotkey_menu_record,
            EditorResetZoom => &mut self.hotkey_editor_reset_zoom,
            EditorToolSelect => &mut self.hotkey_editor_tool_select,
            EditorToolArrow => &mut self.hotkey_editor_tool_arrow,
            EditorToolPolyline => &mut self.hotkey_editor_tool_polyline,
            EditorToolDraw => &mut self.hotkey_editor_tool_draw,
            EditorToolRect => &mut self.hotkey_editor_tool_rect,
            EditorToolEllipse => &mut self.hotkey_editor_tool_ellipse,
            EditorToolText => &mut self.hotkey_editor_tool_text,
            EditorToolNumberMarker => &mut self.hotkey_editor_tool_number_marker,
        }
    }

    /// Every action's current bindings, flattened for `find_conflict`.
    fn all_local_bindings(&self) -> Vec<(LocalAction, LocalKey)> {
        HOTKEY_ROWS
            .iter()
            .flat_map(|r| {
                self.local_keys(r.action)
                    .iter()
                    .filter_map(move |s| crate::localkey::parse(s).map(|k| (r.action, k)))
            })
            .collect()
    }

    /// Adds `candidate` as a new binding for `action` (from the Hotkeys tab's
    /// "+" chip). A no-op if `action` already has this key. If another action
    /// already uses it, rejects and leaves a message in `hotkey_error`
    /// (recording stays open for another attempt). On success, appends it and
    /// closes recording.
    pub(super) fn try_add_local_key(&mut self, action: LocalAction, candidate: LocalKey) {
        let current = self.all_local_bindings();
        if is_duplicate_within_action(&current, action, candidate) {
            self.capturing = None;
            self.request_redraw();
            return;
        }
        match find_conflict(&current, action, candidate) {
            Some(other) => {
                self.hotkey_error = Some(format!(
                    "'{candidate}' is already used by {}",
                    label_for(other)
                ));
            }
            None => {
                self.local_keys_mut(action).push(candidate.to_string());
                self.hotkey_error = None;
                self.capturing = None;
            }
        }
        self.request_redraw();
    }

    /// Whether `action`'s current bindings differ from its defaults (drives
    /// the Hotkeys tab's reset-icon enabled/disabled look; treated as
    /// unmodified if the sets match regardless of order).
    pub(super) fn is_modified(&self, action: LocalAction) -> bool {
        let default_cfg = crate::store::hotkeys::HotkeyConfig::default();
        let cur: Vec<LocalKey> = self
            .local_keys(action)
            .iter()
            .filter_map(|s| crate::localkey::parse(s))
            .collect();
        let def: Vec<LocalKey> = default_local_keys(&default_cfg, action)
            .iter()
            .filter_map(|s| crate::localkey::parse(s))
            .collect();
        cur.len() != def.len() || !cur.iter().all(|k| def.contains(k))
    }

    /// Whether the global hotkey's current bindings differ from its defaults.
    pub(super) fn is_global_modified(&self) -> bool {
        let cur: Vec<global_hotkey::hotkey::HotKey> = self
            .hotkey
            .iter()
            .filter_map(|s| crate::hotkey::parse(s))
            .collect();
        let def: Vec<global_hotkey::hotkey::HotKey> =
            crate::store::hotkeys::HotkeyConfig::default()
                .hotkey
                .iter()
                .filter_map(|s| crate::hotkey::parse(s))
                .collect();
        cur.len() != def.len() || !cur.iter().all(|k| def.contains(k))
    }

    /// Resets `action` to its single default binding (the Hotkeys tab's
    /// per-row reset icon). A no-op if already at default. If the default key
    /// conflicts with another action, only shows the error and leaves the
    /// current bindings untouched.
    fn reset_local(&mut self, action: LocalAction) {
        if !self.is_modified(action) {
            return;
        }
        let default_cfg = crate::store::hotkeys::HotkeyConfig::default();
        let Some(key) = default_local_keys(&default_cfg, action)
            .first()
            .and_then(|s| crate::localkey::parse(s))
        else {
            return;
        };
        let others: Vec<(LocalAction, LocalKey)> = self
            .all_local_bindings()
            .into_iter()
            .filter(|(a, _)| *a != action)
            .collect();
        match find_conflict(&others, action, key) {
            Some(other) => {
                self.hotkey_error =
                    Some(format!("'{key}' is already used by {}", label_for(other)));
            }
            None => {
                *self.local_keys_mut(action) = vec![key.to_string()];
                self.hotkey_error = None;
            }
        }
        self.request_redraw();
    }

    /// Resets the global hotkey to its single default binding (a no-op if
    /// already at default).
    fn reset_global(&mut self) {
        if !self.is_global_modified() {
            return;
        }
        self.hotkey = crate::store::hotkeys::HotkeyConfig::default().hotkey;
        self.request_redraw();
    }

    pub(super) fn buttons_hotkeys(&self, sw: usize, sh: usize) -> Vec<(Btn, Rect)> {
        let mut v = Vec::new();
        let local_specs = self.local_bindings_snapshot();
        let (rows, _) = hotkey_layout(
            self.hotkey_scroll,
            sw,
            sh,
            self.text.as_ref(),
            &self.hotkey,
            &local_specs,
        );
        for row in rows {
            for (btn, _, _, delete_rect, _, _) in row.chips {
                v.push((btn, delete_rect));
            }
            v.push((row.add_btn, row.add_visible));
            v.push((row.reset_btn, row.reset_visible));
        }
        v
    }

    pub(super) fn activate_hotkeys(&mut self, btn: Btn) -> Option<SettingsResult> {
        match btn {
            Btn::Capture => {
                self.capturing = Some(CaptureTarget::Global);
                self.request_redraw();
                None
            }
            Btn::CaptureLocal(action) => {
                self.capturing = Some(CaptureTarget::Local(action));
                self.hotkey_error = None;
                self.request_redraw();
                None
            }
            Btn::ResetLocal(action) => {
                self.reset_local(action);
                None
            }
            Btn::ResetGlobal => {
                self.reset_global();
                None
            }
            Btn::RemoveLocalBinding(action, idx) => {
                let v = self.local_keys_mut(action);
                if idx < v.len() {
                    v.remove(idx);
                }
                self.hotkey_error = None;
                self.request_redraw();
                None
            }
            Btn::RemoveGlobalBinding(idx) => {
                if idx < self.hotkey.len() {
                    self.hotkey.remove(idx);
                }
                self.request_redraw();
                None
            }
            _ => None,
        }
    }
}

#[allow(non_snake_case, unused_variables, clippy::too_many_arguments)]
pub(super) fn draw_hotkeys(
    canvas: &mut Canvas,
    t: &TextRenderer,
    dark: bool,
    hover: Option<Btn>,
    sw: usize,
    sh: usize,
    hotkey: &[String],
    hotkey_values: &[(LocalAction, Vec<String>)],
    hotkey_modified: &[(LocalAction, bool)],
    global_modified: bool,
    hotkey_scroll: i32,
    hotkey_error: &Option<String>,
    capturing: Option<CaptureTarget>,
    scrollbar_active: bool,
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

    let viewport = hotkey_viewport(sw, sh);
    let (rows, headers) = hotkey_layout(hotkey_scroll, sw, sh, Some(t), hotkey, hotkey_values);
    for header in &headers {
        // Baseline is computed from the unclamped position; only the part
        // outside the viewport is clipped by `draw_clipped`, so it slides
        // smoothly with scroll instead of jumping.
        t.draw_clipped(
            canvas,
            CONTENT_X as f32,
            header.raw_y + 14.0,
            header.label,
            13.0,
            DIM,
            viewport,
        );
    }
    for row in &rows {
        let label_baseline = t.baseline_for_center((row.raw.y0 + row.raw.y1) as f32 / 2.0, 15.0);
        t.draw_clipped(
            canvas,
            CONTENT_X as f32,
            label_baseline,
            row.label,
            15.0,
            TEXT,
            viewport,
        );

        let is_capturing_this = match row.add_btn {
            Btn::Capture => capturing == Some(CaptureTarget::Global),
            Btn::CaptureLocal(a) => capturing == Some(CaptureTarget::Local(a)),
            _ => false,
        };

        // The text of this action's current bindings, in the same order as
        // `row.chips`.
        let specs: &[String] = match row.add_btn {
            Btn::Capture => hotkey,
            Btn::CaptureLocal(a) => hotkey_values
                .iter()
                .find(|(x, _)| *x == a)
                .map(|(_, v)| v.as_slice())
                .unwrap_or(&[]),
            _ => &[],
        };

        // Existing chips (text plus a small X at the right edge).
        for (idx, (remove_btn, rect, icon_center, _delete_rect, top_clipped, bottom_clipped)) in
            row.chips.iter().enumerate()
        {
            canvas.fill(*rect, FIELD_BG);
            // Skip strokes on edges that are actually clipped, so a stray
            // border line doesn't appear at the boundary as a fake height change.
            stroke_top_bottom_aware(
                canvas,
                *rect,
                !top_clipped,
                !bottom_clipped,
                if hover == Some(*remove_btn) {
                    hover_tint(0x0080_8080)
                } else {
                    0x0080_8080
                },
            );
            if let Some(spec) = specs.get(idx) {
                // Same rationale as the X icon: baseline is derived from
                // `icon_center`'s true center Y, not the clamped rect's
                // center, so the text doesn't drift as the chip is scrolled
                // past the viewport edge.
                let baseline = t.baseline_for_center(icon_center.1 as f32, 13.0);
                t.draw_clipped(
                    canvas,
                    rect.x0 as f32 + 6.0,
                    baseline,
                    spec,
                    13.0,
                    TEXT,
                    viewport,
                );
            }
            // X icon (chip's right edge): same two-line pattern used for
            // delete icons elsewhere in Settings. Centered on the true
            // (unclamped) position (`icon_center`) and drawn with
            // `line_clipped` so it doesn't drift as the chip is scrolled
            // past the viewport edge (same idea as `draw_refresh_icon_clipped`).
            let (cx, cy) = *icon_center;
            let r = 4i64;
            let x_color = if hover == Some(*remove_btn) {
                0x00FF_6B6B
            } else {
                DIM
            };
            canvas.line_clipped(cx - r, cy - r, cx + r, cy + r, 2, x_color, viewport);
            canvas.line_clipped(cx - r, cy + r, cx + r, cy - r, 2, x_color, viewport);
        }

        // "+" button (start recording). No chip-like border; same look as
        // the delete X icons and row reset icons — a bare icon with a
        // subtle background only on hover.
        if hover == Some(row.add_btn) {
            canvas.fill(row.add_visible, FIELD_BG);
        }
        if is_capturing_this {
            // "Press key..." doesn't fit the small square button, so it's
            // drawn from that position outward — there's nothing else beside
            // it to overlap, and it's clipped only at the key area's right edge.
            let clip = Rect {
                x0: viewport.x0,
                y0: viewport.y0,
                x1: row.raw.x1,
                y1: viewport.y1,
            };
            let baseline = t.baseline_for_center(row.add_center_y as f32, 13.0);
            t.draw_clipped(
                canvas,
                row.add_visible.x0 as f32,
                baseline,
                "Press key...",
                13.0,
                ACCENT,
                clip,
            );
        } else {
            // cy uses the true (unclamped) center (`add_center_y`), same
            // rationale as the X icons; cx is unaffected by clamping, so
            // it's read straight from `add_visible`.
            let cx = ((row.add_visible.x0 + row.add_visible.x1) / 2) as i64;
            let cy = row.add_center_y;
            let r = 5i64;
            let plus_color = if hover == Some(row.add_btn) {
                TEXT
            } else {
                DIM
            };
            canvas.line_clipped(cx - r, cy, cx + r, cy, 2, plus_color, viewport);
            canvas.line_clipped(cx, cy - r, cx, cy + r, 2, plus_color, viewport);
        }

        // Reset icon. Rows still at their default are shown dimmed and
        // effectively unclickable (visual only — reset_local/reset_global
        // already no-op when already at default).
        let is_modified_this = match row.add_btn {
            Btn::Capture => global_modified,
            Btn::CaptureLocal(a) => hotkey_modified
                .iter()
                .find(|(x, _)| *x == a)
                .map(|(_, m)| *m)
                .unwrap_or(false),
            _ => false,
        };
        let icon_color = if is_modified_this {
            if hover == Some(row.reset_btn) {
                TEXT
            } else {
                DIM
            }
        } else {
            VERY_DIM
        };
        if is_modified_this && hover == Some(row.reset_btn) {
            canvas.fill(row.reset_visible, FIELD_BG);
        }
        // Center/radius are derived from the unclamped rect (`reset_raw`,
        // always constant) so the icon doesn't appear to shrink as it's
        // clipped; `draw_refresh_icon_clipped` clips only the part outside
        // the viewport, leaving the icon's size unchanged.
        let cx = ((row.reset_raw.x0 + row.reset_raw.x1) / 2) as i64;
        let cy = ((row.reset_raw.y0 + row.reset_raw.y1) / 2) as i64;
        let icon_r = (row.reset_raw.width().min(row.reset_raw.height()) as i64 / 2 - 3).max(4);
        draw_refresh_icon_clipped(canvas, (cx, cy), icon_r, 2, icon_color, viewport);
    }

    // Scrollbar (also drag-scrollable; see `Settings::scrollbar_drag`).
    let content_h = hotkey_content_height(Some(t), sw, hotkey, hotkey_values);
    let track_x0 = sw.saturating_sub(10);
    if let Some(thumb) = scrollbar_thumb_rect(track_x0, viewport, content_h, hotkey_scroll) {
        let track = field(track_x0, viewport.y0, 4, (viewport.y1 - viewport.y0).max(1));
        canvas.fill(track, FIELD_BG);
        let thumb_color = if scrollbar_active {
            SCROLLBAR_THUMB_HOVER
        } else {
            SCROLLBAR_THUMB
        };
        canvas.fill(thumb, thumb_color);
    }

    if let Some(msg) = hotkey_error {
        t.draw(
            canvas,
            CONTENT_X as f32,
            sh.saturating_sub(74) as f32,
            msg,
            14.0,
            0x00FF_8080,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{WIN_H, WIN_W};

    fn key(spec: &str) -> LocalKey {
        crate::localkey::parse(spec).unwrap()
    }

    /// For layout tests: a typical (default) state with exactly one default
    /// key per local action, as `(global_specs, local_specs)`.
    fn default_specs() -> (Vec<String>, Vec<(LocalAction, Vec<String>)>) {
        let cfg = crate::store::hotkeys::HotkeyConfig::default();
        let global = cfg.hotkey.clone();
        let local = HOTKEY_ROWS
            .iter()
            .map(|r| (r.action, default_local_keys(&cfg, r.action).to_vec()))
            .collect();
        (global, local)
    }

    #[test]
    fn default_local_key_parses_for_every_hotkey_row() {
        // Guards against typos in defaults (e.g. an invalid string like "Ctrl+").
        let cfg = crate::store::hotkeys::HotkeyConfig::default();
        for row in &HOTKEY_ROWS {
            for spec in default_local_keys(&cfg, row.action) {
                assert!(
                    crate::localkey::parse(spec).is_some(),
                    "{:?} の既定値 '{spec}' がパースできない",
                    row.action
                );
            }
        }
    }

    #[test]
    fn hotkey_layout_reset_icon_sits_right_of_key_box_without_overlap() {
        let (gs, ls) = default_specs();
        let (rows, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        for row in &rows {
            for (_, rect, _, _, _, _) in &row.chips {
                assert!(
                    rect.x1 <= row.reset_visible.x0,
                    "チップがリセットアイコンと重なっている"
                );
            }
            assert!(
                row.add_visible.x1 <= row.reset_visible.x0,
                "「+」ボタンがリセットアイコンと重なっている"
            );
        }
        // The reset icon follows the window's right edge.
        let (rows_wide, _) = hotkey_layout(0, WIN_W + 300, WIN_H, None, &gs, &ls);
        assert!(rows_wide[0].reset_visible.x0 > rows[0].reset_visible.x0);
    }

    #[test]
    fn find_conflict_detects_collision_within_region_group() {
        // MenuSave already has 'S'; binding 'S' to ReuseRegion too should conflict.
        let current = [
            (LocalAction::MenuSave, key("S")),
            (LocalAction::ReuseRegion, key("R")),
        ];
        assert_eq!(
            find_conflict(&current, LocalAction::ReuseRegion, key("S")),
            Some(LocalAction::MenuSave)
        );
        // No conflict with itself (re-setting its own current value is fine).
        assert_eq!(
            find_conflict(&current, LocalAction::MenuSave, key("S")),
            None
        );
    }

    #[test]
    fn find_conflict_ignores_other_context_group() {
        // EditorToolRect using Ctrl+S doesn't conflict with the Region-only
        // ReuseRegion, since they're different contexts.
        let current = [
            (LocalAction::EditorToolRect, key("Ctrl+S")),
            (LocalAction::ReuseRegion, key("R")),
        ];
        assert_eq!(
            find_conflict(&current, LocalAction::ReuseRegion, key("Ctrl+S")),
            None
        );
    }

    #[test]
    fn find_conflict_checks_both_groups_for_undo_redo() {
        // Undo/Redo are used in both the Region and Editor contexts, so they
        // also conflict with Editor-only keys.
        let current = [
            (LocalAction::EditorToolSelect, key("Ctrl+V")),
            (LocalAction::Undo, key("Ctrl+Z")),
        ];
        assert_eq!(
            find_conflict(&current, LocalAction::Undo, key("Ctrl+V")),
            Some(LocalAction::EditorToolSelect)
        );
    }

    #[test]
    fn is_duplicate_within_action_ignores_other_actions_same_key() {
        let current = [
            (LocalAction::Undo, key("Ctrl+Z")),
            (LocalAction::ReuseRegion, key("R")),
        ];
        // Same action, same key: a duplicate.
        assert!(is_duplicate_within_action(
            &current,
            LocalAction::Undo,
            key("Ctrl+Z")
        ));
        // Same key but a different action isn't a duplicate (that's
        // find_conflict's job).
        assert!(!is_duplicate_within_action(
            &current,
            LocalAction::Redo,
            key("Ctrl+Z")
        ));
        // Same action but a different key isn't a duplicate.
        assert!(!is_duplicate_within_action(
            &current,
            LocalAction::Undo,
            key("Ctrl+Shift+Z")
        ));
    }

    #[test]
    fn hotkey_raw_layout_covers_all_rows_and_groups_without_overlap() {
        let (gs, ls) = default_specs();
        let (rows, headers) = hotkey_raw_layout(None, WIN_W, &gs, &ls);
        // 14 LocalAction entries plus the global hotkey (Launch capture) = 15.
        assert_eq!(rows.len(), HOTKEY_ROWS.len() + 1);
        assert_eq!(headers.len(), GROUPS.len());
        assert!(
            matches!(rows[0].0, Btn::Capture),
            "先頭はグローバルホットキー"
        );
        assert!(matches!(rows[0].1, Btn::ResetGlobal));
        // Rows never overlap vertically (y is monotonically increasing).
        for w in rows.windows(2) {
            assert!(
                w[1].3 >= w[0].3 + w[0].4 as i64,
                "Hotkeys の行が重なっている"
            );
        }
    }

    #[test]
    fn hotkey_layout_clamps_visible_rows_to_viewport_bounds() {
        let viewport = hotkey_viewport(WIN_W, WIN_H);
        let (gs, ls) = default_specs();
        let (rows, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        // Something is shown, and it all fits within the viewport (not
        // everything fits without scrolling).
        assert!(!rows.is_empty());
        assert!(rows.len() < HOTKEY_ROWS.len() + 1);
        for row in &rows {
            for (_, rect, _, _, _, _) in &row.chips {
                assert!(rect.y0 >= viewport.y0 && rect.y1 <= viewport.y1);
            }
            assert!(row.add_visible.y0 >= viewport.y0 && row.add_visible.y1 <= viewport.y1);
            assert!(row.reset_visible.y0 >= viewport.y0 && row.reset_visible.y1 <= viewport.y1);
        }
    }

    #[test]
    fn hotkey_layout_scrolling_reveals_later_rows() {
        let (gs, ls) = default_specs();
        let (rows_top, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        let viewport = hotkey_viewport(WIN_W, WIN_H);
        let max_scroll = (hotkey_content_height(None, WIN_W, &gs, &ls)
            - (viewport.y1 - viewport.y0) as i64)
            .max(0);
        let (rows_bottom, _) = hotkey_layout(max_scroll as i32, WIN_W, WIN_H, None, &gs, &ls);
        // Scrolled all the way down, the first row (global hotkey) is no
        // longer visible.
        assert!(rows_top.iter().any(|r| matches!(r.add_btn, Btn::Capture)));
        assert!(
            !rows_bottom
                .iter()
                .any(|r| matches!(r.add_btn, Btn::Capture))
        );
        // The last row (Reset zoom) is visible once scrolled all the way down.
        assert!(
            rows_bottom
                .iter()
                .any(|r| matches!(r.add_btn, Btn::CaptureLocal(LocalAction::EditorResetZoom)))
        );
    }

    #[test]
    fn hotkey_layout_marks_only_actually_clipped_edges() {
        let (gs, ls) = default_specs();
        // No scroll (top): the first row's "+" chip is fully visible.
        let (rows_top, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        assert_eq!(rows_top[0].add_visible.height(), CHIP_H as usize);
        // Nothing is clipped yet, so the existing chip's top/bottom flags are both false.
        let (_, _, icon_center0, _, top0, bottom0) = rows_top[0].chips[0];
        assert!(!top0 && !bottom0);

        // A small scroll clips the first row's "+" chip at the top, making
        // it shorter (clamped). Nothing clips until scrolling past the
        // header height (HOTKEY_HEADER_H).
        let (rows_scrolled, _) =
            hotkey_layout(HOTKEY_HEADER_H as i32 + 10, WIN_W, WIN_H, None, &gs, &ls);
        assert!(rows_scrolled[0].add_visible.height() < CHIP_H as usize);
        // The existing chip is likewise clipped only at the top (not the bottom).
        let (_, _, icon_center1, _, top1, bottom1) = rows_scrolled[0].chips[0];
        assert!(top1 && !bottom1);
        // The X icon's center is derived from the unclamped position, so it
        // doesn't drift vertically once clipping starts (moves exactly with scroll).
        assert_eq!(icon_center0.1 - icon_center1.1, HOTKEY_HEADER_H as i64 + 10);
        assert_eq!(icon_center0.0, icon_center1.0);
        // Same for the "+" icon's center: it moves exactly with scroll once
        // clipping starts, without jumping.
        assert_eq!(
            rows_top[0].add_center_y - rows_scrolled[0].add_center_y,
            HOTKEY_HEADER_H as i64 + 10
        );
    }

    #[test]
    fn chip_delete_hit_rect_is_confined_to_the_x_icon_not_the_whole_chip() {
        // The X's hit area is confined to near the icon, not the whole chip
        // (including its text) — regression guard against an oversized hit area.
        let (gs, ls) = default_specs();
        let (rows, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        let (_, chip_rect, _, delete_rect, _, _) = rows[0]
            .chips
            .first()
            .expect("先頭行（Launch capture）には既定のキーが割り当たっている");
        assert!(
            delete_rect.width() < chip_rect.width(),
            "当たり判定がチップ全体と同じ幅になっている"
        );
        assert!(delete_rect.x0 >= chip_rect.x0 && delete_rect.x1 <= chip_rect.x1);
        assert_eq!(delete_rect.width(), (CHIP_DELETE_HIT_HALF * 2) as usize);
    }

    #[test]
    fn hotkey_layout_shows_more_rows_when_window_is_taller() {
        // Enlarging the window shows more rows without scrolling (confirms
        // the viewport follows window size).
        let (gs, ls) = default_specs();
        let (small, _) = hotkey_layout(0, WIN_W, WIN_H, None, &gs, &ls);
        let (tall, _) = hotkey_layout(0, WIN_W, WIN_H + 400, None, &gs, &ls);
        assert!(tall.len() > small.len());
        assert!(tall.len() == HOTKEY_ROWS.len() + 1);
    }

    #[test]
    fn hotkey_layout_widens_key_box_when_window_is_wider() {
        // Widening the window also widens the chip area itself (confirms it
        // follows the viewport's right edge instead of being a fixed width).
        assert!(chip_area_width(WIN_W + 300) > chip_area_width(WIN_W));
    }

    #[test]
    fn chip_width_falls_back_to_fixed_size_without_a_text_renderer() {
        // With no font available (text: None), width is a fixed fallback
        // regardless of actual text length (same idea as
        // `session_limit_label_w` and similar).
        let w_short = chip_width(None, "R");
        let w_long = chip_width(None, "Ctrl+Shift+NumpadSubtract");
        assert_eq!(w_short, w_long);
        assert!(w_short >= CHIP_MIN_W);
    }

    #[test]
    fn wrap_slots_places_items_left_to_right_then_wraps() {
        let widths = [50i64, 50, 50];
        // gap=6, max_w=106: exactly enough width for two items (50+6+50).
        let positions = wrap_slots(&widths, 6, 106);
        assert_eq!(positions, vec![(0, 0), (0, 56), (1, 0)]);
    }

    #[test]
    fn wrap_slots_never_stalls_when_a_single_item_exceeds_max_w() {
        // A single element wider than max_w still fits on one line without looping forever.
        let positions = wrap_slots(&[500i64], 6, 100);
        assert_eq!(positions, vec![(0, 0)]);
    }

    #[test]
    fn chip_row_h_grows_when_more_bindings_force_wrapping() {
        // With a narrow width, more bindings wrap onto more lines, growing the row height.
        let narrow_w = 200i64;
        let one = vec!["Ctrl+Z".to_string()];
        let three = vec![
            "Ctrl+Z".to_string(),
            "Ctrl+Shift+Z".to_string(),
            "R".to_string(),
        ];
        let h1 = chip_row_h(&chip_slots(None, &one, narrow_w));
        let h3 = chip_row_h(&chip_slots(None, &three, narrow_w));
        assert!(h3 > h1, "割当が増えれば折り返して行が高くなるはず");
    }
}
