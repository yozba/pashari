//! The action menu shown after committing a selection (Save/Copy/Edit/
//! Upload/Record/Quit).
//!
//! Layout and hit-testing are separated out as OS-independent pure logic,
//! unit-tested directly. Drawing writes straight to the softbuffer buffer.

use super::{ACTION_BTN, Action, RegionKeys};
use crate::localkey::LocalKey;
use crate::store::MenuButton;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect, draw_icon_button};

/// Key bindings for the menu's 6 original buttons (changeable via the
/// settings GUI's Hotkeys tab). Since an action can have multiple keys,
/// each is kept as a `Vec` (empty = unbound).
#[derive(Clone)]
pub(super) struct MenuKeys {
    pub save: Vec<LocalKey>,
    pub copy: Vec<LocalKey>,
    pub edit: Vec<LocalKey>,
    pub upload: Vec<LocalKey>,
    pub record: Vec<LocalKey>,
    pub quit: Vec<LocalKey>,
}

/// Which of the menu's action buttons are currently usable — computed by
/// the caller (`Overlay::build_menu`) from state `Menu::layout` doesn't
/// otherwise have access to (uploader config, undo/redo history, external
/// editor config, last region). An action with no applicable condition
/// (e.g. `Save`) is just never disabled.
pub(super) struct MenuAvailability {
    pub uploaders: bool,
    pub external_editor: bool,
    pub undo: bool,
    pub redo: bool,
    pub reuse_region: bool,
}

/// Whether `action`'s button should be disabled, given `avail`.
fn action_disabled(action: Action, avail: &MenuAvailability) -> bool {
    match action {
        Action::Upload => !avail.uploaders,
        Action::EditExternal => !avail.external_editor,
        Action::Undo => !avail.undo,
        Action::Redo => !avail.redo,
        Action::ReuseRegion => !avail.reuse_region,
        _ => false,
    }
}

/// Gap between the selection outline and the menu.
const MARGIN: usize = 10;
/// Font size for the Size button's three stacked lines, the aspect-ratio
/// button's single line, and the dropdown's option list — all the same size.
const ASPECT_BTN_FONT: f32 = 15.0;
/// Height of each row in the open aspect-ratio option list.
const ASPECT_ROW_H: usize = 22;
/// Divider slot width, as a fraction of `ACTION_BTN` — just enough room
/// for a line with some breathing space either side, not a full button.
const DIVIDER_W_DIVISOR: usize = 6;
/// Divider slot width never shrinks below this, so it stays visible even
/// at a tiny `ACTION_BTN * dpi`.
const DIVIDER_W_MIN: usize = 6;
/// Vertical inset of the divider line from the top/bottom of its slot.
const DIVIDER_INSET_FRAC: f64 = 0.2;
const DIVIDER_COLOR: u32 = 0x0055_5555;

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

/// One `button_order` entry, resolved to what it actually takes to lay
/// out and draw: the size display or the aspect-ratio dropdown (each its
/// own single-square slot with its own draw code, neither a clickable
/// `Action`), a regular single-width `Action` button, or a layout-only
/// divider/spacer (same narrow width, neither clickable — a divider draws
/// a line on a button-colored background, a spacer draws nothing at all).
enum MenuSlot {
    Size,
    Aspect,
    Action(Action, &'static str, Vec<LocalKey>),
    Divider,
    Spacer,
}

/// The label is always `b.label()` — one short label per `MenuButton`,
/// shared with the settings GUI's chips (`store::MenuButton::label`'s doc).
fn menu_slot(b: MenuButton, keys: &RegionKeys) -> MenuSlot {
    match b {
        MenuButton::Size => MenuSlot::Size,
        MenuButton::AspectRatio => MenuSlot::Aspect,
        MenuButton::Save => MenuSlot::Action(Action::Save, b.label(), keys.menu.save.clone()),
        MenuButton::Copy => MenuSlot::Action(Action::Copy, b.label(), keys.menu.copy.clone()),
        MenuButton::Edit => MenuSlot::Action(Action::Edit, b.label(), keys.menu.edit.clone()),
        MenuButton::Upload => MenuSlot::Action(Action::Upload, b.label(), keys.menu.upload.clone()),
        MenuButton::Video => MenuSlot::Action(Action::Record, b.label(), keys.menu.record.clone()),
        MenuButton::Quit => MenuSlot::Action(Action::Quit, b.label(), keys.menu.quit.clone()),
        MenuButton::Undo => MenuSlot::Action(Action::Undo, b.label(), keys.undo.clone()),
        MenuButton::Redo => MenuSlot::Action(Action::Redo, b.label(), keys.redo.clone()),
        MenuButton::ReuseRegion => {
            MenuSlot::Action(Action::ReuseRegion, b.label(), keys.reuse_region.clone())
        }
        MenuButton::ClearSelection => MenuSlot::Action(
            Action::ClearSelection,
            b.label(),
            keys.clear_selection.clone(),
        ),
        MenuButton::SaveAs => MenuSlot::Action(Action::SaveAs, b.label(), keys.save_as.clone()),
        MenuButton::EditExternal => {
            MenuSlot::Action(Action::EditExternal, b.label(), keys.edit_external.clone())
        }
        MenuButton::Divider => MenuSlot::Divider,
        MenuButton::Spacer => MenuSlot::Spacer,
    }
}

/// A menu built from a caller-supplied, possibly-customized
/// `button_order` (settings GUI's General tab): a variable number of
/// single-width slots — action buttons, plus, if included, the size
/// display and the aspect-ratio dropdown — wherever each falls in that
/// order. No frame is drawn around any of them (each stands alone).
#[derive(Clone)]
pub struct Menu {
    pub buttons: Vec<Button>,
    /// The selection's width in pixels, precomputed so `draw` doesn't
    /// need the selection rect too. Shown as the Size button's top line.
    pub size_w_label: String,
    /// The selection's height in pixels. Shown as the Size button's
    /// bottom line.
    pub size_h_label: String,
    /// The aspect-ratio dropdown's current state ("Free", "1:1", ...).
    pub aspect_label: String,
    /// The Size button's rect, if `button_order` included it (`None`
    /// means it's hidden).
    pub size_rect: Option<Rect>,
    /// The aspect-ratio dropdown button's rect, if `button_order`
    /// included it (`None` means it's hidden — no button, no dropdown).
    pub aspect_rect: Option<Rect>,
    /// One rect per divider `button_order` included (any number, including
    /// zero). Not clickable — see `draw` for how the lines themselves are drawn.
    pub divider_rects: Vec<Rect>,
    /// The whole panel's bounding box (all slots, including dividers).
    /// Used to absorb a click anywhere within the menu — e.g. right on a
    /// divider — rather than letting it fall through to the selection
    /// underneath.
    pub rect: Rect,
}

impl Menu {
    /// Places the menu near selection rect `sel` (surface coordinates),
    /// laying out `button_order` left to right (customizable via the
    /// settings GUI; see `MenuSlot`/`menu_slot`). Below if there's room,
    /// else above, else clamped within `bounds` (the monitor showing the
    /// selection). The caller passes the actual monitor's rect rather
    /// than clamping to the whole composited canvas, since with
    /// mismatched monitor heights/layouts that could place it somewhere
    /// belonging to no monitor. Disables buttons whose action isn't
    /// currently usable per `avail` (e.g. Upload with no uploader
    /// configured), so pressing one doesn't just silently do nothing.
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
        keys: &RegionKeys,
        button_order: &[MenuButton],
        avail: &MenuAvailability,
        aspect_lock: Option<(u32, u32)>,
        dpi: f64,
    ) -> Self {
        let action_btn = ((ACTION_BTN as f64) * dpi).round() as usize;
        let margin = ((MARGIN as f64) * dpi).round() as usize;
        let divider_w = (action_btn / DIVIDER_W_DIVISOR).max(DIVIDER_W_MIN);
        // A spacer is exactly as wide as a divider — same footprint,
        // transparent instead of drawing a line.
        let spacer_w = divider_w;

        let slots: Vec<MenuSlot> = button_order.iter().map(|b| menu_slot(*b, keys)).collect();
        let panel_w: usize = slots
            .iter()
            .map(|s| match s {
                MenuSlot::Size | MenuSlot::Aspect | MenuSlot::Action(..) => action_btn,
                MenuSlot::Divider => divider_w,
                MenuSlot::Spacer => spacer_w,
            })
            .sum();
        let panel_h = action_btn;

        // Horizontal position: centered on the selection, clamped within bounds.
        let sel_center_x = (sel.x0 + sel.x1) / 2;
        let px = sel_center_x
            .saturating_sub(panel_w / 2)
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

        let mut size_rect = None;
        let mut aspect_rect = None;
        let mut divider_rects = Vec::new();
        let mut buttons = Vec::with_capacity(slots.len());
        let mut bx = px;
        for slot in slots {
            match slot {
                MenuSlot::Size => {
                    size_rect = Some(Rect {
                        x0: bx,
                        y0: py,
                        x1: bx + action_btn,
                        y1: py + action_btn,
                    });
                    bx += action_btn;
                }
                MenuSlot::Aspect => {
                    aspect_rect = Some(Rect {
                        x0: bx,
                        y0: py,
                        x1: bx + action_btn,
                        y1: py + action_btn,
                    });
                    bx += action_btn;
                }
                MenuSlot::Action(action, label, hotkeys) => {
                    buttons.push(Button {
                        rect: Rect {
                            x0: bx,
                            y0: py,
                            x1: bx + action_btn,
                            y1: py + action_btn,
                        },
                        disabled: action_disabled(action, avail),
                        action,
                        label,
                        hotkeys,
                    });
                    bx += action_btn;
                }
                MenuSlot::Divider => {
                    divider_rects.push(Rect {
                        x0: bx,
                        y0: py,
                        x1: bx + divider_w,
                        y1: py + action_btn,
                    });
                    bx += divider_w;
                }
                MenuSlot::Spacer => {
                    bx += spacer_w;
                }
            }
        }

        Self {
            buttons,
            size_w_label: sel.width().to_string(),
            size_h_label: sel.height().to_string(),
            aspect_label: aspect_option_label(aspect_lock),
            size_rect,
            aspect_rect,
            divider_rects,
            rect: Rect {
                x0: px,
                y0: py,
                x1: px + panel_w,
                y1: py + panel_h,
            },
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
    // The Size button: same square as the other buttons, with the
    // selection's width and height stacked on three centered lines
    // ("1920" / "x" / "1080") instead of an icon+label — the numbers
    // are self-explanatory, so no icon is needed. `None` means it's
    // hidden (`button_order` didn't include it) — nothing to draw.
    if let Some(r) = menu.size_rect {
        canvas.fill(r, BTN_BG);
        if let Some(t) = text {
            let lines = [menu.size_w_label.as_str(), "x", menu.size_h_label.as_str()];
            let line_h = r.height() as f32 / 3.0;
            for (i, line) in lines.into_iter().enumerate() {
                let cy = r.y0 as f32 + line_h * (i as f32 + 0.5);
                let tw = t.text_width(line, ASPECT_BTN_FONT);
                let lx = r.x0 as f32 + (r.width() as f32 - tw) / 2.0;
                let baseline = t.baseline_for_center(cy, ASPECT_BTN_FONT);
                t.draw(canvas, lx, baseline, line, ASPECT_BTN_FONT, TEXT_COLOR);
            }
        }
    }

    // The aspect-ratio dropdown button: same square, a single centered
    // line showing the current lock state ("Free", "16:9", ...).
    if let Some(r) = menu.aspect_rect {
        canvas.fill(r, BTN_BG);
        if let Some(t) = text {
            let cy = r.y0 as f32 + r.height() as f32 / 2.0;
            let tw = t.text_width(&menu.aspect_label, ASPECT_BTN_FONT);
            let lx = r.x0 as f32 + (r.width() as f32 - tw) / 2.0;
            let baseline = t.baseline_for_center(cy, ASPECT_BTN_FONT);
            t.draw(
                canvas,
                lx,
                baseline,
                &menu.aspect_label,
                ASPECT_BTN_FONT,
                TEXT_COLOR,
            );
        }
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

    // Each divider: a button-colored background (unlike a spacer, which
    // stays fully transparent) with a thin vertical line centered in it,
    // inset from the top/bottom so it doesn't touch the panel's edges.
    for r in &menu.divider_rects {
        canvas.fill(*r, BTN_BG);
        let inset = (r.height() as f64 * DIVIDER_INSET_FRAC) as i64;
        let cx = ((r.x0 + r.x1) / 2) as i64;
        canvas.line(
            cx,
            r.y0 as i64 + inset,
            cx,
            r.y1 as i64 - inset,
            2,
            DIVIDER_COLOR,
        );
    }

    // Drawn last so the open option list sits on top of the size label
    // and buttons it overlaps, instead of being painted over by them.
    if let Some(r) = menu.aspect_rect
        && aspect_dropdown_open
    {
        for (rect, preset) in aspect_option_rects(r).into_iter().zip(ASPECT_PRESETS) {
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

    /// `RegionKeys` wrapping `test_keys()`, with the non-menu hotkeys
    /// unbound (empty) — good enough for layout tests, which don't assert
    /// on those.
    fn test_region_keys() -> RegionKeys {
        RegionKeys {
            undo: vec![],
            redo: vec![],
            reuse_region: vec![],
            clear_selection: vec![],
            save_as: vec![],
            edit_external: vec![],
            menu: test_keys(),
        }
    }

    /// Nothing disabled — the common case for layout tests that aren't
    /// specifically exercising `disabled`.
    fn test_availability() -> MenuAvailability {
        MenuAvailability {
            uploaders: true,
            external_editor: true,
            undo: true,
            redo: true,
            reuse_region: true,
        }
    }

    /// The menu's original fixed layout (before button customization),
    /// used by tests that assert on a specific 6-action-button shape.
    fn default_visible_order() -> [MenuButton; 8] {
        [
            MenuButton::Size,
            MenuButton::AspectRatio,
            MenuButton::Save,
            MenuButton::Copy,
            MenuButton::Edit,
            MenuButton::Upload,
            MenuButton::Video,
            MenuButton::Quit,
        ]
    }

    #[test]
    fn menu_shows_separate_size_and_aspect_buttons_left_of_the_action_buttons() {
        let m = Menu::layout(
            sel(100, 100, 100 + 1280, 100 + 720),
            full_hd(),
            &test_region_keys(),
            &MenuButton::ALL,
            &test_availability(),
            Some((16, 9)),
            1.0,
        );
        assert_eq!(m.size_w_label, "1280");
        assert_eq!(m.size_h_label, "720");
        assert_eq!(m.aspect_label, "16:9");
        // Size and aspect are each exactly one action-button square, sitting
        // adjacent, directly left of Save.
        let sr = m.size_rect.expect("MenuButton::ALL includes Size");
        let ar = m.aspect_rect.expect("MenuButton::ALL includes AspectRatio");
        assert_eq!(sr.width(), m.buttons[0].rect.width());
        assert_eq!(sr.height(), m.buttons[0].rect.height());
        assert_eq!(ar.width(), m.buttons[0].rect.width());
        assert_eq!(ar.height(), m.buttons[0].rect.height());
        assert_eq!(sr.x1, ar.x0);
        assert_eq!(ar.x1, m.buttons[0].rect.x0);
        assert_eq!(sr.y0, m.buttons[0].rect.y0);
        assert_eq!(ar.y0, m.buttons[0].rect.y0);
    }

    #[test]
    fn menu_is_horizontally_centered_on_the_selection() {
        let m = Menu::layout(
            sel(1000, 100, 1200, 200), // 200px wide, centered at x=1100.
            full_hd(),
            &test_region_keys(),
            &MenuButton::ALL,
            &test_availability(),
            None,
            1.0,
        );
        let panel_center = (m.rect.x0 + m.rect.x1) / 2;
        assert_eq!(panel_center, 1100);
    }

    #[test]
    fn menu_layout_excludes_hidden_buttons_and_uses_the_given_order() {
        // Only Quit, Save, then the aspect-ratio button — in that order,
        // and skipping Copy/Edit/Upload/Video/Size entirely.
        let order = [MenuButton::Quit, MenuButton::Save, MenuButton::AspectRatio];
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &order,
            &test_availability(),
            None,
            1.0,
        );
        assert_eq!(m.buttons.len(), 2);
        assert!(matches!(m.buttons[0].action, Action::Quit));
        assert!(matches!(m.buttons[1].action, Action::Save));
        assert!(m.size_rect.is_none());
        // The aspect button comes after both action buttons in this order,
        // not pinned to the left.
        let ar = m.aspect_rect.expect("order includes AspectRatio");
        assert_eq!(ar.x0, m.buttons[1].rect.x1);
    }

    #[test]
    fn menu_layout_has_no_size_or_aspect_rect_when_neither_is_in_the_order() {
        let order = [MenuButton::Save, MenuButton::Quit];
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &order,
            &test_availability(),
            None,
            1.0,
        );
        assert!(m.size_rect.is_none());
        assert!(m.aspect_rect.is_none());
        // The panel is exactly 2 action-button squares wide (no room
        // reserved for either).
        assert_eq!(
            m.buttons[1].rect.x1 - m.buttons[0].rect.x0,
            m.buttons[0].rect.width() * 2
        );
    }

    #[test]
    fn menu_layout_size_and_aspect_can_appear_independently() {
        let size_only = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &[MenuButton::Size, MenuButton::Save],
            &test_availability(),
            None,
            1.0,
        );
        assert!(size_only.size_rect.is_some());
        assert!(size_only.aspect_rect.is_none());

        let aspect_only = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &[MenuButton::AspectRatio, MenuButton::Save],
            &test_availability(),
            None,
            1.0,
        );
        assert!(aspect_only.size_rect.is_none());
        assert!(aspect_only.aspect_rect.is_some());
    }

    #[test]
    fn menu_layout_places_a_divider_between_buttons_without_making_it_a_button() {
        let order = [MenuButton::Save, MenuButton::Divider, MenuButton::Quit];
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &order,
            &test_availability(),
            None,
            1.0,
        );
        // Only Save and Quit are real (clickable) buttons.
        assert_eq!(m.buttons.len(), 2);
        assert!(matches!(m.buttons[0].action, Action::Save));
        assert!(matches!(m.buttons[1].action, Action::Quit));
        // The divider sits between them, narrower than a full button, and
        // isn't reachable via `hit`.
        assert_eq!(m.divider_rects.len(), 1);
        let d = m.divider_rects[0];
        assert_eq!(d.x0, m.buttons[0].rect.x1);
        assert_eq!(d.x1, m.buttons[1].rect.x0);
        assert!(d.width() < m.buttons[0].rect.width());
        let (dcx, dcy) = ((d.x0 + d.x1) / 2, (d.y0 + d.y1) / 2);
        assert_eq!(m.hit(dcx, dcy), None);
        // The panel's overall bounds still span from Save's left edge to Quit's right edge.
        assert_eq!(m.rect.x0, m.buttons[0].rect.x0);
        assert_eq!(m.rect.x1, m.buttons[1].rect.x1);
    }

    #[test]
    fn menu_layout_supports_multiple_dividers() {
        let order = [
            MenuButton::Divider,
            MenuButton::Save,
            MenuButton::Divider,
            MenuButton::Quit,
            MenuButton::Divider,
        ];
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &order,
            &test_availability(),
            None,
            1.0,
        );
        assert_eq!(m.buttons.len(), 2);
        assert_eq!(m.divider_rects.len(), 3);
        // Left to right: divider, Save, divider, Quit, divider — none overlap.
        assert_eq!(m.divider_rects[0].x1, m.buttons[0].rect.x0);
        assert_eq!(m.buttons[0].rect.x1, m.divider_rects[1].x0);
        assert_eq!(m.divider_rects[1].x1, m.buttons[1].rect.x0);
        assert_eq!(m.buttons[1].rect.x1, m.divider_rects[2].x0);
        assert_eq!(m.rect.x1, m.divider_rects[2].x1);
    }

    #[test]
    fn menu_layout_spacer_is_the_same_width_as_a_divider_but_transparent() {
        let divider_order = [MenuButton::Save, MenuButton::Divider, MenuButton::Quit];
        let spacer_order = [MenuButton::Save, MenuButton::Spacer, MenuButton::Quit];
        let with_divider = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &divider_order,
            &test_availability(),
            None,
            1.0,
        );
        let with_spacer = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &spacer_order,
            &test_availability(),
            None,
            1.0,
        );
        // Same overall width (spacer reserves exactly a divider's worth of
        // space) and Quit ends up in the same spot either way.
        assert_eq!(with_divider.rect.x1, with_spacer.rect.x1);
        assert_eq!(with_divider.buttons[1].rect, with_spacer.buttons[1].rect);
        // But a spacer has no rect to draw a line in (transparent).
        assert!(with_spacer.divider_rects.is_empty());
    }

    #[test]
    fn menu_sits_below_selection_when_space_allows() {
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &default_visible_order(),
            &test_availability(),
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
            &test_region_keys(),
            &MenuButton::ALL,
            &test_availability(),
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
            &test_region_keys(),
            &MenuButton::ALL,
            &test_availability(),
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
            &test_region_keys(),
            &MenuButton::ALL,
            &test_availability(),
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
        let avail = MenuAvailability {
            uploaders: false,
            ..test_availability()
        };
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &MenuButton::ALL,
            &avail,
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

    #[test]
    fn edit_external_button_is_disabled_without_a_configured_external_editor() {
        let avail = MenuAvailability {
            external_editor: false,
            ..test_availability()
        };
        let order = [MenuButton::Save, MenuButton::EditExternal];
        let m = Menu::layout(
            sel(100, 100, 300, 200),
            full_hd(),
            &test_region_keys(),
            &order,
            &avail,
            None,
            1.0,
        );
        assert!(!m.buttons[0].disabled);
        assert!(m.buttons[1].disabled);
    }
}
