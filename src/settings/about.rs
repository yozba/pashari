//! About tab: app name/version/license/repo, and the list of directly
//! depended-on crates (name, version, license — generated at build time
//! from `Cargo.toml`/`cargo metadata`, see `build.rs`, so it can't drift
//! out of sync with a hand-maintained copy).

use super::{
    ACCENT, Btn, CONTENT_X, SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, Settings, SettingsResult,
    field, hover_tint_for, scrollbar_thumb_rect, theme_colors,
};
use crate::app::UserEvent;
use crate::ui::text::TextRenderer;
use crate::ui::{Canvas, Rect};
use crate::update::{self, ReleaseInfo};

include!(concat!(env!("OUT_DIR"), "/used_crates.rs"));

const LICENSE: &str = "CC0-1.0";
const REPO_URL: &str = "https://github.com/yozba/pashari";

const HEADER_ROW_Y: usize = 72;
const HEADER_ROW_H: usize = 22;

/// "Check for updates" row, below the name/license/repo rows.
const CHECK_ROW_Y: usize = HEADER_ROW_Y + HEADER_ROW_H * 3;
const CHECK_BTN_LABEL: &str = "Check for updates";
const UPDATE_BTN_PAD: usize = 24;
const UPDATE_BTN_W_FALLBACK: usize = 200;

/// A second row below the check button for the check's outcome (a status
/// message, and — once a newer version is found — a button to go get it).
/// Always reserved, even before anything's been checked.
const STATUS_ROW_Y: usize = HEADER_ROW_Y + HEADER_ROW_H * 4;
const DOWNLOAD_BTN_LABEL: &str = "Update";
const DOWNLOAD_BTN_W_FALLBACK: usize = 100;
const UPDATE_STATUS_GAP: usize = 16;

const REPORT_BUG_ROW_Y: usize = HEADER_ROW_Y + HEADER_ROW_H * 5;
const REPORT_BUG_BTN_LABEL: &str = "Report a Bug";
const REPORT_BUG_BTN_PAD: usize = 24;
const REPORT_BUG_BTN_W_FALLBACK: usize = 160;
const DEPS_HEADING_Y: usize = HEADER_ROW_Y + HEADER_ROW_H * 6 + 16;
const DEPS_LIST_Y: usize = DEPS_HEADING_Y + 26;
const DEPS_ROW_H: usize = 24;
const DEPS_VERSION_X: usize = 260;
const DEPS_LICENSE_X: usize = 340;

/// Outcome of the last manual check made from this session, shown in the
/// status row below the check button. `update_available` (a persistent,
/// app-wide value) covers the "found a newer version" case on its own;
/// this only needs to distinguish the other two.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateCheckStatus {
    UpToDate,
    Failed,
}

/// Content viewport; scrolling hides anything outside it (same idea as
/// `recent::session_viewport`).
pub(super) fn about_viewport(sw: usize, sh: usize) -> Rect {
    field(
        CONTENT_X,
        DEPS_LIST_Y,
        sw.saturating_sub(CONTENT_X + 16),
        sh.saturating_sub(DEPS_LIST_Y + 64),
    )
}

pub(super) fn about_content_height(count: usize) -> i64 {
    (count * DEPS_ROW_H) as i64
}

/// Sized to fit the label text, like the other buttons.
fn report_bug_button_rect(text: Option<&TextRenderer>) -> Rect {
    let w = text
        .map(|tr| tr.text_width(REPORT_BUG_BTN_LABEL, 15.0).ceil() as usize + REPORT_BUG_BTN_PAD)
        .unwrap_or(REPORT_BUG_BTN_W_FALLBACK);
    field(CONTENT_X, REPORT_BUG_ROW_Y, w, HEADER_ROW_H)
}

/// Opens the GitHub bug-report template with the app version pre-filled
/// (the template's `version` field id matches this query param name).
pub(super) fn bug_report_url() -> String {
    format!(
        "https://github.com/yozba/pashari/issues/new?template=bug_report.yml&version={}",
        update::CURRENT_VERSION
    )
}

/// Sized to fit the label text, like the other buttons. The label is
/// always "Check for updates" now — the outcome shows in the status row
/// below instead of replacing this button's own label.
fn update_button_rect(text: Option<&TextRenderer>) -> Rect {
    let w = text
        .map(|tr| tr.text_width(CHECK_BTN_LABEL, 15.0).ceil() as usize + UPDATE_BTN_PAD)
        .unwrap_or(UPDATE_BTN_W_FALLBACK);
    field(CONTENT_X, CHECK_ROW_Y, w, HEADER_ROW_H)
}

/// The status row's message, if there's anything to show yet (`None`
/// before the first check this session). An install error (from clicking
/// "Update") takes priority, then the in-progress state, then whether a
/// newer version is known, then the plain check outcome.
fn update_status_text(
    update_available: Option<&ReleaseInfo>,
    status: Option<UpdateCheckStatus>,
    updating: bool,
    install_error: Option<&str>,
) -> Option<String> {
    if let Some(msg) = install_error {
        Some(format!("Update failed: {msg}"))
    } else if updating {
        Some("Downloading update...".to_string())
    } else if let Some(info) = update_available {
        Some(format!("A new version is available: v{}", info.version))
    } else {
        match status {
            Some(UpdateCheckStatus::UpToDate) => Some("You're up to date.".to_string()),
            Some(UpdateCheckStatus::Failed) => {
                Some("Update check failed. Try again later.".to_string())
            }
            None => None,
        }
    }
}

/// The "Update" button, shown to the right of `status_label` once a newer
/// version is known (`status_label` is whatever `update_status_text`
/// returned, so the button starts right after that text).
fn download_button_rect(text: Option<&TextRenderer>, status_label: &str) -> Rect {
    let status_w = text
        .map(|tr| tr.text_width(status_label, 15.0).ceil() as usize)
        .unwrap_or(0);
    let w = text
        .map(|tr| tr.text_width(DOWNLOAD_BTN_LABEL, 15.0).ceil() as usize + UPDATE_BTN_PAD)
        .unwrap_or(DOWNLOAD_BTN_W_FALLBACK);
    let x0 = CONTENT_X + status_w + UPDATE_STATUS_GAP;
    field(x0, STATUS_ROW_Y, w, HEADER_ROW_H)
}

impl Settings {
    pub(super) fn buttons_about(&self) -> Vec<(Btn, Rect)> {
        let mut v = vec![
            (Btn::ReportBug, report_bug_button_rect(self.text.as_ref())),
            (Btn::CheckForUpdates, update_button_rect(self.text.as_ref())),
        ];
        if let Some(info) = self.update_available.as_ref() {
            let status_label = update_status_text(
                Some(info),
                self.update_check_status,
                self.updating,
                self.update_install_error.as_deref(),
            )
            .expect("update_available is Some, so update_status_text always returns Some");
            v.push((
                Btn::DownloadUpdate,
                download_button_rect(self.text.as_ref(), &status_label),
            ));
        }
        v
    }

    pub(super) fn activate_about(&mut self, btn: Btn) -> Option<SettingsResult> {
        match btn {
            Btn::ReportBug => {
                crate::shell::open_url(&bug_report_url());
                None
            }
            Btn::CheckForUpdates => {
                // A fresh check always starts over, clearing any stale
                // up-to-date/failed status from a previous click this session.
                self.update_check_status = None;
                let proxy = self.update_proxy.clone();
                std::thread::spawn(move || {
                    let result = update::check_latest();
                    let _ = proxy.send_event(UserEvent::UpdateCheckResult(result));
                });
                self.request_redraw();
                None
            }
            Btn::DownloadUpdate => {
                if self.updating {
                    return None;
                }
                if let Some(info) = self.update_available.clone() {
                    match update::update_target(&info) {
                        Some(target) => {
                            self.updating = true;
                            self.update_install_error = None;
                            let proxy = self.update_proxy.clone();
                            std::thread::spawn(move || {
                                let result = update::download_update(target);
                                let _ = proxy.send_event(UserEvent::UpdateReady(result));
                            });
                            self.request_redraw();
                        }
                        // No matching asset (an old release predating this
                        // feature) — fall back to the manual-download page.
                        None => crate::shell::open_url(&info.url),
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[allow(non_snake_case, unused_variables, clippy::too_many_arguments)]
pub(super) fn draw_about(
    canvas: &mut Canvas,
    t: &TextRenderer,
    dark: bool,
    hover: Option<Btn>,
    sw: usize,
    sh: usize,
    scroll: i32,
    scrollbar_active: bool,
    update_available: &Option<ReleaseInfo>,
    update_check_status: Option<UpdateCheckStatus>,
    updating: bool,
    update_install_error: &Option<String>,
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

    let name_baseline = t.baseline_for_center((HEADER_ROW_Y + HEADER_ROW_H / 2) as f32, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        name_baseline,
        &format!("pashari v{}", update::CURRENT_VERSION),
        15.0,
        TEXT,
    );

    let license_y = HEADER_ROW_Y + HEADER_ROW_H;
    let license_baseline = t.baseline_for_center((license_y + HEADER_ROW_H / 2) as f32, 13.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        license_baseline,
        &format!("License: {LICENSE}"),
        13.0,
        DIM,
    );

    let repo_y = license_y + HEADER_ROW_H;
    let repo_baseline = t.baseline_for_center((repo_y + HEADER_ROW_H / 2) as f32, 13.0);
    t.draw(canvas, CONTENT_X as f32, repo_baseline, REPO_URL, 13.0, DIM);

    let hover_tint = |c: u32| hover_tint_for(c, dark);

    // "Check for updates" button: same fill+centered-text look as other
    // buttons, always the same label now.
    let check_rect = update_button_rect(Some(t));
    let check_color = if hover == Some(Btn::CheckForUpdates) {
        hover_tint(BTN_BG)
    } else {
        BTN_BG
    };
    canvas.fill(check_rect, check_color);
    let check_tw = t.text_width(CHECK_BTN_LABEL, 15.0);
    let check_lx = check_rect.x0 as f32 + ((check_rect.x1 - check_rect.x0) as f32 - check_tw) / 2.0;
    let check_baseline = t.baseline_for_center((check_rect.y0 + check_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        check_lx,
        check_baseline,
        CHECK_BTN_LABEL,
        15.0,
        TEXT,
    );

    // Status row: the last check's outcome, plus an "Update" button once a
    // newer version is known.
    if let Some(status_label) = update_status_text(
        update_available.as_ref(),
        update_check_status,
        updating,
        update_install_error.as_deref(),
    ) {
        let status_baseline = t.baseline_for_center((STATUS_ROW_Y + HEADER_ROW_H / 2) as f32, 15.0);
        t.draw(
            canvas,
            CONTENT_X as f32,
            status_baseline,
            &status_label,
            15.0,
            DIM,
        );

        if update_available.is_some() {
            let dl_rect = download_button_rect(Some(t), &status_label);
            let dl_color = if hover == Some(Btn::DownloadUpdate) {
                hover_tint(ACCENT)
            } else {
                ACCENT
            };
            canvas.fill(dl_rect, dl_color);
            let dl_tw = t.text_width(DOWNLOAD_BTN_LABEL, 15.0);
            let dl_lx = dl_rect.x0 as f32 + ((dl_rect.x1 - dl_rect.x0) as f32 - dl_tw) / 2.0;
            let dl_baseline = t.baseline_for_center((dl_rect.y0 + dl_rect.y1) as f32 / 2.0, 15.0);
            t.draw(
                canvas,
                dl_lx,
                dl_baseline,
                DOWNLOAD_BTN_LABEL,
                15.0,
                0x0011_1111,
            );
        }
    }

    let btn_rect = report_bug_button_rect(Some(t));
    let btn_color = if hover == Some(Btn::ReportBug) {
        hover_tint(BTN_BG)
    } else {
        BTN_BG
    };
    canvas.fill(btn_rect, btn_color);
    let btn_tw = t.text_width(REPORT_BUG_BTN_LABEL, 15.0);
    let btn_lx = btn_rect.x0 as f32 + ((btn_rect.x1 - btn_rect.x0) as f32 - btn_tw) / 2.0;
    let btn_baseline = t.baseline_for_center((btn_rect.y0 + btn_rect.y1) as f32 / 2.0, 15.0);
    t.draw(
        canvas,
        btn_lx,
        btn_baseline,
        REPORT_BUG_BTN_LABEL,
        15.0,
        TEXT,
    );

    let heading_baseline = t.baseline_for_center((DEPS_HEADING_Y + HEADER_ROW_H / 2) as f32, 15.0);
    t.draw(
        canvas,
        CONTENT_X as f32,
        heading_baseline,
        "Dependencies",
        15.0,
        TEXT,
    );

    let viewport = about_viewport(sw, sh);
    for (i, (name, version, license)) in USED_CRATES.iter().enumerate() {
        let raw_y0 = viewport.y0 as i64 + (i * DEPS_ROW_H) as i64 - scroll as i64;
        let raw_y1 = raw_y0 + DEPS_ROW_H as i64;
        if raw_y1 <= viewport.y0 as i64 || raw_y0 >= viewport.y1 as i64 {
            continue; // No overlap with viewport.
        }
        let baseline = t.baseline_for_center((raw_y0 + DEPS_ROW_H as i64 / 2) as f32, 13.0);
        t.draw_clipped(
            canvas,
            CONTENT_X as f32,
            baseline,
            name,
            13.0,
            TEXT,
            viewport,
        );
        t.draw_clipped(
            canvas,
            (CONTENT_X + DEPS_VERSION_X) as f32,
            baseline,
            version,
            13.0,
            DIM,
            viewport,
        );
        t.draw_clipped(
            canvas,
            (CONTENT_X + DEPS_LICENSE_X) as f32,
            baseline,
            license,
            13.0,
            DIM,
            viewport,
        );
    }

    // Scrollbar (also drag-scrollable; see `Settings::scrollbar_drag`).
    let content_h = about_content_height(USED_CRATES.len());
    let track_x0 = sw.saturating_sub(10);
    if let Some(thumb) = scrollbar_thumb_rect(track_x0, viewport, content_h, scroll) {
        let track = field(track_x0, viewport.y0, 4, (viewport.y1 - viewport.y0).max(1));
        canvas.fill(track, FIELD_BG);
        canvas.fill(
            thumb,
            if scrollbar_active {
                SCROLLBAR_THUMB_HOVER
            } else {
                SCROLLBAR_THUMB
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bug_report_url_points_at_the_template_with_the_current_version() {
        let url = bug_report_url();
        assert!(url.starts_with("https://github.com/yozba/pashari/issues/new?"));
        assert!(url.contains("template=bug_report.yml"));
        assert!(url.contains(&format!("version={}", update::CURRENT_VERSION)));
    }

    fn test_release_info() -> ReleaseInfo {
        ReleaseInfo {
            version: "9.9.9".to_string(),
            url: "https://example.test/release".to_string(),
            exe_url: None,
            setup_url: None,
        }
    }

    #[test]
    fn update_status_text_is_none_before_any_check() {
        assert_eq!(update_status_text(None, None, false, None), None);
    }

    #[test]
    fn update_status_text_reports_a_newer_version_regardless_of_status() {
        let info = test_release_info();
        let text = update_status_text(Some(&info), None, false, None).unwrap();
        assert!(text.contains("9.9.9"));
        // Takes priority even if a stale status is still set (shouldn't
        // normally happen together, but `update_available` wins either way).
        let text_with_status =
            update_status_text(Some(&info), Some(UpdateCheckStatus::Failed), false, None).unwrap();
        assert!(text_with_status.contains("9.9.9"));
    }

    #[test]
    fn update_status_text_reports_up_to_date_and_failed_when_no_newer_version() {
        assert!(
            update_status_text(None, Some(UpdateCheckStatus::UpToDate), false, None)
                .unwrap()
                .contains("up to date")
        );
        assert!(
            update_status_text(None, Some(UpdateCheckStatus::Failed), false, None)
                .unwrap()
                .contains("failed")
        );
    }

    #[test]
    fn update_status_text_shows_downloading_while_updating() {
        let info = test_release_info();
        let text = update_status_text(Some(&info), None, true, None).unwrap();
        assert!(text.contains("Downloading"));
    }

    #[test]
    fn update_status_text_install_error_takes_priority_over_everything_else() {
        let info = test_release_info();
        let text = update_status_text(
            Some(&info),
            Some(UpdateCheckStatus::Failed),
            true,
            Some("boom"),
        )
        .unwrap();
        assert!(text.contains("boom"));
    }
}
