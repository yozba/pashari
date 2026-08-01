//! The action menu shown after committing a selection (Save/Copy/Edit/
//! Upload/Record/Quit).
//!
//! Layout and hit-testing are separated out as OS-independent pure logic,
//! unit-tested directly. Drawing writes straight to the softbuffer buffer.

use super::{ACTION_BTN, Action};
use crate::localkey::LocalKey;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect, draw_icon_button};

/// Key bindings for the menu's 6 buttons (changeable via the settings
/// GUI's Hotkeys tab). Since an action can have multiple keys, each is
/// kept as a `Vec` (empty = unbound).
#[derive(Clone)]
pub(super) struct MenuKeys {
    pub save: Vec<LocalKey>,
    pub copy: Vec<LocalKey>,
    pub edit: Vec<LocalKey>,
    pub upload: Vec<LocalKey>,
    pub record: Vec<LocalKey>,
    pub quit: Vec<LocalKey>,
}

/// Number of action buttons (Save/Copy/Edit/Upload/Video/Quit — not
/// counting the combined size/aspect-ratio button to their left).
const N: usize = 6;
/// Gap between the selection outline and the menu.
const MARGIN: usize = 10;
/// Width of the combined size/aspect-ratio button, in `ACTION_BTN`
/// squares — wider than the other (single-square) buttons so its two
/// text lines have room to breathe.
const ASPECT_BTN_SQUARES: usize = 2;
/// Font size for the combined button's two stacked lines (size on top,
/// aspect-ratio state on bottom) and the dropdown's option list — all
/// the same size.
const ASPECT_BTN_FONT: f32 = 15.0;
/// Height of each row in the open aspect-ratio option list.
const ASPECT_ROW_H: usize = 22;

/// Aspect-ratio presets the dropdown offers (`None` = free/unlocked),
/// in display order.
pub(super) const ASPECT_PRESETS: [Option<(u32, u32)>; 4] =
    [None, Some((1, 1)), Some((16, 9)), Some((4, 3))];

/// The label shown for a preset ("Free", "1:1", ...).
pub(super) fn aspect_option_label(preset: Option<(u32, u32)>) -> String {
    match preset {
        None => "Free".to_string(),
        Some((w, h)) => format!("{w}:{h}"),
    }
}

/// The 4 option rows shown while the aspect dropdown is open, stacked
/// directly below the closed button (`closed`), in `ASPECT_PRESETS` order.
pub(super) fn aspect_option_rects(closed: Rect) -> [Rect; 4] {
    std::array::from_fn(|i| {
        let y0 = closed.y1 + i * ASPECT_ROW_H;
        Rect {
            x0: closed.x0,
            y0,
            x1: closed.x1,
            y1: y0 + ASPECT_ROW_H,
        }
    })
}

/// Index into `ASPECT_PRESETS` of the option row containing `(x, y)`, if any.
pub(super) fn aspect_option_hit(closed: Rect, x: usize, y: usize) -> Option<usize> {
    aspect_option_rects(closed)
        .iter()
        .position(|r| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
}

const BTN_BG: u32 = 0x0033_3333;
const BTN_HOVER: u32 = 0x0045_4545;
const BTN_PRESSED: u32 = 0x004D_A6FF;
const TEXT_COLOR: u32 = 0x00EA_EAEA;
const ICON_COLOR: u32 = 0x0099_9999;
const BTN_DISABLED_BG: u32 = 0x0026_2626;
const TEXT_DISABLED: u32 = 0x0066_6666;
const ICON_DISABLED: u32 = 0x0055_5555;

#[derive(Clone)]
pub struct Button {
    pub rect: Rect,
    pub action: Action,
    pub label: &'static str,
    /// Keys that can trigger this button (multiple allowed; empty
    /// shouldn't normally happen, but kept for exhaustiveness).
    pub hotkeys: Vec<LocalKey>,
    /// If true, unclickable (neither click nor hotkey triggers it).
    /// Currently only set for Upload, when no uploader is configured.
    pub disabled: bool,
}

/// A menu with a fixed `N` action buttons, plus one more square button to
/// their left combining the selection's pixel size and the aspect-ratio
/// dropdown (same size as the rest, so the row reads as one uniform set
/// of squares). No frame is drawn around any of them (each stands alone).
#[derive(Clone)]
pub struct Menu {
    pub buttons: [Button; N],
    /// The selection's size as "W x H", precomputed so `draw` doesn't
    /// need the selection rect too.
    pub size_label: String,
    /// The aspect-ratio dropdown's current state ("Free", "1:1", ...).
    pub aspect_label: String,
    /// The combined size/aspect-ratio button, left of `buttons[0]`.
    pub aspect_rect: Rect,
}

impl Menu {
    /// Places the menu near selection rect `sel` (surface coordinates).
    /// Below if there's room, else above, else clamped within `bounds`
    /// (the monitor showing the selection). The caller passes the actual
    /// monitor's rect rather than clamping to the whole composited
    /// canvas, since with mismatched monitor heights/layouts that could
    /// place it somewhere belonging to no monitor. Disables the Upload
    /// button if `uploaders_configured` is false, so pressing it with no
    /// uploader configured doesn't just silently do nothing.
    ///
    /// `dpi` scales the button/gap size (`ACTION_BTN`/`MARGIN`) directly,
    /// unlike the rest of this window's drawing — everything else here
    /// (`sel`/`bounds`) is real physical screen coordinates that must stay
    /// exact, but the menu itself isn't tied to a specific pixel position,
    /// so growing it at high DPI is just a sizing choice. Since the
    /// resulting `Button::rect`s are used for both drawing and hit-testing
    /// (`hit`), scaling them here keeps the two consistent without
    /// touching the window's shared `Canvas`.
    pub(super) fn layout(
        sel: Rect,
        bounds: Rect,
        keys: &MenuKeys,
        uploaders_configured: bool,
        aspect_lock: Option<(u32, u32)>,
        dpi: f64,
    ) -> Self {
        let action_btn = ((ACTION_BTN as f64) * dpi).round() as usize;
        let margin = ((MARGIN as f64) * dpi).round() as usize;

        // (Action, label, keys, gap from the previous button). All gaps
        // are currently 0 (packed together); to space out a specific
        // button later, just change its value here.
        let specs = [
            (Action::Save, "Save", keys.save.clone(), 0usize),
            (Action::Copy, "Copy", keys.copy.clone(), 0),
            (Action::Edit, "Edit", keys.edit.clone(), 0),
            (Action::Upload, "Upload", keys.upload.clone(), 0),
            (Action::Record, "Video", keys.record.clone(), 0),
            (Action::Quit, "Quit", keys.quit.clone(), 0),
        ];
        let total_gap: usize = specs.iter().map(|(.., g)| g).sum();
        let aspect_btn_w = action_btn * ASPECT_BTN_SQUARES;
        let panel_w = action_btn * N + aspect_btn_w + total_gap;
        let panel_h = action_btn;

        // Horizontal position: aligned to the selection's left edge, clamped within bounds.
        let px = sel
            .x0
            .max(bounds.x0)
            .min(bounds.x1.saturating_sub(panel_w).max(bounds.x0));
        // Vertical position: below the selection first, else above, else clamped to bounds' bottom edge.
        let below = sel.y1 + margin;
        let py = if below + panel_h <= bounds.y1 {
            below
        } else if sel.y0 >= bounds.y0 + panel_h + margin {
            sel.y0 - panel_h - margin
        } else {
            bounds.y1.saturating_sub(panel_h).max(bounds.y0)
        };

        let panel = Rect {
            x0: px,
            y0: py,
            x1: px + panel_w,
            y1: py + panel_h,
        };
        let aspect_rect = Rect {
            x0: px,
            y0: py,
            x1: px + aspect_btn_w,
            y1: py + action_btn,
        };

        let mut buttons: [Button; N] = std::array::from_fn(|_| Button {
            rect: panel,
            action: Action::Save,
            label: "",
            hotkeys: Vec::new(),
            disabled: false,
        });
        let mut bx = px + aspect_btn_w;
        for (i, (action, label, hotkeys, gap_before)) in specs.into_iter().enumerate() {
            if i > 0 {
                bx += gap_before;
            }
            buttons[i] = Button {
                rect: Rect {
                    x0: bx,
                    y0: py,
                    x1: bx + action_btn,
                    y1: py + action_btn,
                },
                disabled: matches!(action, Action::Upload) && !uploaders_configured,
                action,
                label,
                hotkeys,
            };
            bx += action_btn;
        }

        Self {
            buttons,
            size_label: format!("{} x {}", sel.width(), sel.height()),
            aspect_label: aspect_option_label(aspect_lock),
            aspect_rect,
        }
    }

    /// Index of the button containing point `(x, y)`, regardless of
    /// `disabled`. Hitting a disabled button here too prevents the click
    /// from falling through and starting a new region selection. Whether
    /// it's actually allowed to trigger is decided by the caller, via
    /// `buttons[i].disabled`.
    pub fn hit(&self, x: usize, y: usize) -> Option<usize> {
        self.buttons
            .iter()
            .position(|b| x >= b.rect.x0 && x < b.rect.x1 && y >= b.rect.y0 && y < b.rect.y1)
    }
}

/// Draws the menu. `hovered`/`pressed` are button indices.
/// `aspect_dropdown_open` draws the option list below the aspect-ratio
/// row instead of just its closed label.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Canvas,
    menu: &Menu,
    hovered: Option<usize>,
    pressed: Option<usize>,
    text: Option<&TextRenderer>,
    aspect_lock: Option<(u32, u32)>,
    aspect_dropdown_open: bool,
) {
    // The combined size/aspect-ratio button: same square as the other
    // buttons, with two centered lines (size on top, ratio state below)
    // instead of an icon+label.
    canvas.fill(menu.aspect_rect, BTN_BG);
    if let Some(t) = text {
        let r = menu.aspect_rect;
        let top_cy = r.y0 as f32 + r.height() as f32 * 0.3;
        let bottom_cy = r.y0 as f32 + r.height() as f32 * 0.72;
        let tw = t.text_width(&menu.size_label, ASPECT_BTN_FONT);
        let lx = r.x0 as f32 + (r.width() as f32 - tw) / 2.0;
        let baseline = t.baseline_for_center(top_cy, ASPECT_BTN_FONT);
        t.draw(
            canvas,
            lx,
            baseline,
            &menu.size_label,
            ASPECT_BTN_FONT,
            TEXT_COLOR,
        );
        let tw = t.text_width(&menu.aspect_label, ASPECT_BTN_FONT);
        let lx = r.x0 as f32 + (r.width() as f32 - tw) / 2.0;
        let baseline = t.baseline_for_center(bottom_cy, ASPECT_BTN_FONT);
        t.draw(
            canvas,
            lx,
            baseline,
            &menu.aspect_label,
            ASPECT_BTN_FONT,
            TEXT_COLOR,
        );
    }

    for (i, btn) in menu.buttons.iter().enumerate() {
        let (bg, icon_color, text_color) = if btn.disabled {
            (BTN_DISABLED_BG, ICON_DISABLED, TEXT_DISABLED)
        } else if pressed == Some(i) {
            (BTN_PRESSED, ICON_COLOR, TEXT_COLOR)
        } else if hovered == Some(i) {
            (BTN_HOVER, ICON_COLOR, TEXT_COLOR)
        } else {
            (BTN_BG, ICON_COLOR, TEXT_COLOR)
        };
        if let Some(t) = text {
            draw_icon_button(canvas, btn.rect, bg, icon_color, btn.label, text_color, t);
        } else {
            canvas.fill(btn.rect, bg);
        }
    }

    // Drawn last so the open option list sits on top of the size label
    // and buttons it overlaps, instead of being painted over by them.
    if aspect_dropdown_open {
        for (rect, preset) in aspect_option_rects(menu.aspect_rect)
            .into_iter()
            .zip(ASPECT_PRESETS)
        {
            let selected = preset == aspect_lock;
            canvas.fill(rect, if selected { BTN_PRESSED } else { BTN_HOVER });
            if let Some(t) = text {
                let label = aspect_option_label(preset);
                let tw = t.text_width(&label, ASPECT_BTN_FONT);
                let lx = rect.x0 as f32 + (rect.width() as f32 - tw) / 2.0;
                let baseline =
                    t.baseline_for_center((rect.y0 + rect.y1) as f32 / 2.0, ASPECT_BTN_FONT);
                t.draw(canvas, lx, baseline, &label, ASPECT_BTN_FONT, TEXT_COLOR);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(x0: usize, y0: usize, x1: usize, y1: usize) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn full_hd() -> Rect {
        Rect {
            x0: 0,
            y0: 0,
            x1: 1920,
            y1: 1080,
        }
    }

    /// Default key bindings for tests (matches the app's actual defaults).
    fn test_keys() -> MenuKeys {
        MenuKeys {
            save: vec![LocalKey::new(false, false, false, 's')],
            copy: vec![LocalKey::new(false, false, false, 'c')],
            edit: vec![LocalKey::new(false, false, false, 'e')],
            upload: vec![LocalKey::new(false, false, false, 'u')],
            record: vec![LocalKey::new(false, false, false, 'v')],
            quit: vec![LocalKey::new(false, false, false, 'q')],
        }
    }

    #[test]
    fn menu_shows_a_combined_size_and_aspect_button_left_of_the_action_buttons() {
        let m = Menu::layout(
            sel(100, 100, 100 + 1280, 100 + 720),
            full_hd(),
            &test_keys(),
            true,
            Some((16, 9)),
            1.0,
        );
        assert_eq!(m.size_label, "1280 x 720");
        assert_eq!(m.aspect_label, "16:9");
        // Twice as wide as one action-button square, same height, sitting
        // directly left of Save.
        assert_eq!(m.aspect_rect.width(), m.buttons[0].rect.width() * 2);
        assert_eq!(m.aspect_rect.height(), m.buttons[0].rect.height());
        assert_eq!(m.aspect_rect.x1, m.buttons[0].rect.x0);
        assert_eq!(m.aspect_rect.y0, m.buttons[0].rect.y0);
    }

    #[test]
    fn menu_sits_below_selection_when_space_allows() {
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_keys(),
            true,
            None,
            1.0,
        );
        // Placed below the selection (y1=200 plus margin).
        assert!(m.buttons[0].rect.y0 >= 200);
        assert_eq!(m.buttons.len(), 6);
        // Left to right: Save / Copy / Edit / Upload / Record(Video) / Quit.
        assert!(matches!(m.buttons[0].action, Action::Save));
        assert!(matches!(m.buttons[2].action, Action::Edit));
        assert!(matches!(m.buttons[3].action, Action::Upload));
        assert!(matches!(m.buttons[4].action, Action::Record));
        assert_eq!(m.buttons[4].label, "Video");
        assert_eq!(
            m.buttons[4].hotkeys,
            vec![LocalKey::new(false, false, false, 'v')]
        );
        // Quit is furthest right.
        assert!(matches!(m.buttons[5].action, Action::Quit));
        assert_eq!(m.buttons[5].label, "Quit");
        assert_eq!(
            m.buttons[5].hotkeys,
            vec![LocalKey::new(false, false, false, 'q')]
        );
    }

    #[test]
    fn aspect_option_rects_stack_below_the_closed_button_without_overlap() {
        let closed = Rect {
            x0: 10,
            y0: 20,
            x1: 110,
            y1: 42,
        };
        let rows = aspect_option_rects(closed);
        assert_eq!(rows[0].y0, closed.y1);
        for pair in rows.windows(2) {
            assert_eq!(
                pair[0].y1, pair[1].y0,
                "rows must be contiguous, no gaps/overlap"
            );
            assert_eq!(pair[0].x0, closed.x0);
            assert_eq!(pair[0].x1, closed.x1);
        }
        // Matches ASPECT_PRESETS order.
        assert_eq!(aspect_option_label(ASPECT_PRESETS[0]), "Free");
        assert_eq!(aspect_option_label(ASPECT_PRESETS[1]), "1:1");
    }

    #[test]
    fn menu_flips_above_when_no_space_below() {
        // The selection touches the bottom edge -> no room below, so it flips above.
        let m = Menu::layout(
            sel(100, 1000, 300, 1080),
            full_hd(),
            &test_keys(),
            true,
            None,
            1.0,
        );
        assert!(m.buttons[0].rect.y1 <= 1000);
    }

    #[test]
    fn menu_clamps_within_offset_monitor_bounds() {
        // A case where the secondary monitor is right of the primary and
        // shorter: even with bounds offset like (1920,200)-(3840,1000),
        // it should stay clamped within that range and never spill into a
        // blind spot of the origin-0 composited canvas (e.g. past y=1000).
        let bounds = Rect {
            x0: 1920,
            y0: 200,
            x1: 3840,
            y1: 1000,
        };
        let m = Menu::layout(
            sel(3700, 900, 3800, 990),
            bounds,
            &test_keys(),
            true,
            None,
            1.0,
        );
        for b in &m.buttons {
            assert!(b.rect.x0 >= bounds.x0 && b.rect.x1 <= bounds.x1);
            assert!(b.rect.y0 >= bounds.y0 && b.rect.y1 <= bounds.y1);
        }
    }

    #[test]
    fn hit_returns_button_index_or_none() {
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_keys(),
            true,
            None,
            1.0,
        );
        let b0 = m.buttons[0].rect;
        // The button's center hits.
        let cx = (b0.x0 + b0.x1) / 2;
        let cy = (b0.y0 + b0.y1) / 2;
        assert_eq!(m.hit(cx, cy), Some(0));
        // Outside the panel misses.
        assert_eq!(m.hit(0, 0), None);
    }

    #[test]
    fn upload_button_is_disabled_without_configured_uploaders_but_still_absorbs_clicks() {
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_keys(),
            false,
            None,
            1.0,
        );
        assert!(m.buttons[3].disabled);
        let b3 = m.buttons[3].rect;
        let cx = (b3.x0 + b3.x1) / 2;
        let cy = (b3.y0 + b3.y1) / 2;
        // Still hits even when disabled (prevents the click from falling
        // through and starting a new region selection). Whether it's
        // allowed to trigger is decided separately by the caller, via `buttons[i].disabled`.
        assert_eq!(m.hit(cx, cy), Some(3));
        // Other buttons stay enabled as usual.
        assert!(!m.buttons[0].disabled);
    }
}
