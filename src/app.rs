//! The resident controller.
//!
//! Waits in the background and starts the region-selection flow on a
//! global hotkey or the tray icon's Capture. Returns to idle once the
//! flow completes; the tray icon's Settings opens the settings screen,
//! Quit exits. Since there's only one winit event loop, this is the sole
//! `ApplicationHandler`, delegating events to [`overlay::Overlay`] while
//! capturing and to [`settings::Settings`] while the settings screen is shown.

use std::path::Path;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoopProxy};
use winit::window::WindowId;

use crate::editor;
use crate::export;
use crate::hotkey;
use crate::overlay::{self, Action, Outcome, Overlay};
use crate::settings::{SavedSettings, Settings, SettingsResult};
use crate::shell;
use crate::startup;
use crate::store::hotkeys::HotkeyConfig;
use crate::store::uploaders::UploaderConfig;
use crate::store::{self, Config};
use crate::update::{self, ReleaseInfo};
use crate::upload;

/// The polling interval for checking hotkeys/tray while idle.
const IDLE_POLL: Duration = Duration::from_millis(50);
/// The interval (seconds) throttling the automatic startup update check.
const UPDATE_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// A custom event sent to the winit event loop from outside (another thread).
pub enum UserEvent {
    /// Another process detected a duplicate launch and requested Settings be shown.
    ShowSettings,
    /// The update-check background thread's result (`Ok(None)` = checked
    /// and already latest, `Err` = check failed).
    UpdateCheckResult(Result<Option<ReleaseInfo>, String>),
    /// The update-download background thread's result, ready to install.
    UpdateReady(Result<update::UpdateArtifact, String>),
    /// The background window-snapshot capture's result (see
    /// `Overlay::spawn_snapshot_capture`), tagged with the owning
    /// overlay's HWND so it's applied to the right session.
    SnapshotReady(isize, overlay::snap::Snapshot),
}

pub struct App {
    hotkey_manager: Option<GlobalHotKeyManager>,
    /// The currently registered hotkeys (used for unregistering on
    /// re-registration and matching against incoming events).
    current_hotkeys: Vec<HotKey>,
    /// A handle for sending `UserEvent`s from other threads/processes.
    proxy: EventLoopProxy<UserEvent>,
    tray: Option<TrayIcon>,
    quit_id: Option<MenuId>,
    settings_id: Option<MenuId>,
    capture_id: Option<MenuId>,
    editor_id: Option<MenuId>,
    /// A newer version found (`None` = not checked, or checked and already latest).
    update_available: Option<ReleaseInfo>,
    /// The in-progress capture session (`None` = idle).
    session: Option<Overlay>,
    /// A screenshot session opened in a separate window while recording
    /// setup/recording is in progress (`None` = none). This can only be
    /// started via the hotkey/tray Capture while `session` is in
    /// recording setup/recording (`Overlay::in_record_flow`) — it doesn't
    /// double up on recording (Record is disabled for it).
    shot_session: Option<Overlay>,
    /// The settings screen (`Some` if open).
    settings: Option<Settings>,
    /// Set from the `--show-settings` arg (see main.rs) — shows Settings
    /// once on the first `resumed`, then is left `false`.
    show_settings_on_launch: bool,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, show_settings_on_launch: bool) -> Self {
        Self {
            hotkey_manager: None,
            current_hotkeys: vec![HotKey::new(
                Some(Modifiers::CONTROL | Modifiers::SHIFT),
                Code::Digit2,
            )],
            proxy,
            tray: None,
            quit_id: None,
            settings_id: None,
            capture_id: None,
            editor_id: None,
            update_available: None,
            session: None,
            shot_session: None,
            settings: None,
            show_settings_on_launch,
        }
    }

    /// Initializes the hotkey and tray (once, after the event loop starts).
    fn init(&mut self) {
        match GlobalHotKeyManager::new() {
            Ok(manager) => {
                let hks = parse_hotkeys(&store::hotkey_specs());
                self.current_hotkeys = hks;
                if let Err(e) = manager.register_all(&self.current_hotkeys) {
                    eprintln!("ホットキー登録に失敗: {e}");
                }
                self.hotkey_manager = Some(manager);
            }
            Err(e) => eprintln!("ホットキー初期化に失敗: {e}"),
        }

        match build_tray() {
            Ok((tray, quit_id, settings_id, capture_id, editor_id)) => {
                self.tray = Some(tray);
                self.quit_id = Some(quit_id);
                self.settings_id = Some(settings_id);
                self.capture_id = Some(capture_id);
                self.editor_id = Some(editor_id);
            }
            Err(e) => eprintln!("トレイアイコン生成に失敗: {e}"),
        }

        self.maybe_auto_check_update();

        // Syncs the registry to the current Config's value once at
        // startup, so it stays current at the next launch even if the
        // executable's path changed.
        startup::set_enabled(store::snapshot().launch_at_startup);

        println!(
            "pashari 常駐開始: ホットキーで領域選択 / トレイ右クリック → Capture・Editor・Settings・Quit"
        );
    }

    /// Kicks off a background update check at startup if at least
    /// `UPDATE_CHECK_INTERVAL_SECS` has passed since the last check (throttled).
    fn maybe_auto_check_update(&self) {
        let now = unix_now();
        let cfg = store::snapshot();
        if now - cfg.last_update_check < UPDATE_CHECK_INTERVAL_SECS {
            return;
        }
        let mut cfg = cfg;
        cfg.last_update_check = now;
        store::set_and_save(cfg);
        spawn_update_check(self.proxy.clone());
    }

    /// Starts a capture session via the hotkey.
    fn start_session(&mut self, event_loop: &ActiveEventLoop) {
        drain_hotkeys();
        match Overlay::start(event_loop) {
            Ok(session) => {
                session.spawn_snapshot_capture(self.proxy.clone());
                self.session = Some(session);
            }
            Err(e) => eprintln!("キャプチャ開始に失敗: {e}"),
        }
    }

    /// Ends the session: runs the action matching its result, and returns to idle.
    fn finish_session(&mut self) {
        if let Some(mut session) = self.session.take() {
            handle_outcome(session.take_outcome());
            // session is dropped here, closing every window.
        }
        drain_hotkeys();
    }

    /// Starts a screenshot region-selection session in a separate window
    /// while recording (doesn't double up on recording — Record is disabled for this session).
    fn start_shot_session(&mut self, event_loop: &ActiveEventLoop) {
        drain_hotkeys();
        match Overlay::start(event_loop) {
            Ok(mut session) => {
                session.set_allow_record(false);
                session.spawn_snapshot_capture(self.proxy.clone());
                self.shot_session = Some(session);
            }
            Err(e) => eprintln!("スクショ用セッション開始に失敗: {e}"),
        }
    }

    /// Ends the screenshot session; runs the same follow-up as `finish_session`.
    fn finish_shot_session(&mut self) {
        if let Some(mut session) = self.shot_session.take() {
            handle_outcome(session.take_outcome());
        }
        drain_hotkeys();
    }

    /// Opens the settings screen (only while idle). Brings it to the front if already open.
    fn open_settings(&mut self, event_loop: &ActiveEventLoop) {
        if self.settings.is_none() && self.session.is_none() {
            self.settings = Some(Settings::open(
                event_loop,
                self.update_available.clone(),
                self.proxy.clone(),
            ));
        }
        if let Some(settings) = self.settings.as_ref() {
            settings.focus();
        }
    }

    /// Applies an update-check result (shared by the automatic startup
    /// check and a manual check from the settings screen). For
    /// `Ok(None)`/`Err`, keeps any already-found new version as-is.
    fn handle_update_check_result(&mut self, result: Result<Option<ReleaseInfo>, String>) {
        match &result {
            Ok(Some(info)) => {
                println!("Found a new version: v{}", info.version);
                self.update_available = Some(info.clone());
            }
            Ok(None) => println!("pashari is up to date (v{})", update::CURRENT_VERSION),
            Err(e) => eprintln!("Update check failed: {e}"),
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.set_update_check_result(&result);
        }
    }

    /// Applies a downloaded update: installs it (which, on success, means
    /// this process is about to exit and hand off to the new one — see
    /// `update::relaunch_portable`/`relaunch_installer`) or reports the
    /// failure back to the settings screen.
    fn handle_update_ready(
        &mut self,
        result: Result<update::UpdateArtifact, String>,
        event_loop: &ActiveEventLoop,
    ) {
        let install_result = match result {
            Ok(update::UpdateArtifact::Portable(path)) => update::relaunch_portable(&path),
            Ok(update::UpdateArtifact::Installer(path)) => update::relaunch_installer(&path),
            Err(e) => Err(e),
        };
        match install_result {
            Ok(()) => event_loop.exit(),
            Err(e) => {
                eprintln!("Update failed: {e}");
                if let Some(settings) = self.settings.as_mut() {
                    settings.set_update_install_error(e);
                }
            }
        }
    }

    /// Closes the settings screen, applying it on Save. Launches the
    /// editor if a Recent-tab session was opened. If the session fails to
    /// load, reports the error without closing the settings screen (the
    /// editor is a fire-and-forget separate process, so closing anyway
    /// would make it look to the user like nothing happened).
    fn close_settings(&mut self, result: SettingsResult) {
        if let SettingsResult::OpenSession(dir) = &result
            && let Err(e) = editor::session_is_loadable(dir)
        {
            shell::show_error_dialog(&format!("セッションを読み込めませんでした\n{e}"));
            return;
        }
        self.settings = None;
        match result {
            SettingsResult::Saved(saved) => {
                let SavedSettings {
                    hotkey,
                    save_dir_png,
                    save_dir_mp4,
                    save_dir_gif,
                    external_editor,
                    record_show_cursor,
                    record_bitrate_mbps,
                    record_max_width,
                    record_max_height,
                    record_show_click_ripple,
                    record_click_color_left,
                    record_click_color_right,
                    record_audio_output_device,
                    record_audio_input_device,
                    record_audio_sample_rate,
                    record_strip_silent_audio,
                    uploaders,
                    hotkey_undo,
                    hotkey_redo,
                    hotkey_reuse_region,
                    hotkey_clear_selection,
                    hotkey_save_as,
                    hotkey_edit_external,
                    hotkey_quit,
                    hotkey_menu_save,
                    hotkey_menu_copy,
                    hotkey_menu_edit,
                    hotkey_menu_upload,
                    hotkey_menu_record,
                    hotkey_editor_reset_zoom,
                    hotkey_editor_tool_select,
                    hotkey_editor_tool_arrow,
                    hotkey_editor_tool_polyline,
                    hotkey_editor_tool_draw,
                    hotkey_editor_tool_rect,
                    hotkey_editor_tool_ellipse,
                    hotkey_editor_tool_text,
                    hotkey_editor_tool_number_marker,
                    session_history_limit,
                    launch_at_startup,
                    filename_format,
                } = *saved;
                let hotkeys_cfg = HotkeyConfig {
                    hotkey,
                    hotkey_undo,
                    hotkey_redo,
                    hotkey_reuse_region,
                    hotkey_clear_selection,
                    hotkey_save_as,
                    hotkey_edit_external,
                    hotkey_quit,
                    hotkey_menu_save,
                    hotkey_menu_copy,
                    hotkey_menu_edit,
                    hotkey_menu_upload,
                    hotkey_menu_record,
                    hotkey_editor_reset_zoom,
                    hotkey_editor_tool_select,
                    hotkey_editor_tool_arrow,
                    hotkey_editor_tool_polyline,
                    hotkey_editor_tool_draw,
                    hotkey_editor_tool_rect,
                    hotkey_editor_tool_ellipse,
                    hotkey_editor_tool_text,
                    hotkey_editor_tool_number_marker,
                };
                let uploaders_cfg = UploaderConfig { uploaders };
                // Everything besides the fields below (e.g. recording setup's last-used settings) is kept as-is.
                let cfg = Config {
                    save_dir_png,
                    save_dir_mp4,
                    save_dir_gif,
                    external_editor,
                    record_show_cursor,
                    record_bitrate_mbps,
                    record_max_width,
                    record_max_height,
                    record_show_click_ripple,
                    record_click_color_left,
                    record_click_color_right,
                    record_audio_output_device,
                    record_audio_input_device,
                    record_audio_sample_rate,
                    record_strip_silent_audio,
                    session_history_limit,
                    launch_at_startup,
                    filename_format,
                    ..store::snapshot()
                };
                self.apply_settings(cfg, hotkeys_cfg, uploaders_cfg);
            }
            SettingsResult::OpenSession(dir) => spawn_editor_process(Some(&dir)),
            SettingsResult::Cancelled => {}
        }
        drain_hotkeys();
    }

    /// Saves the new settings and re-registers hotkeys.
    fn apply_settings(
        &mut self,
        cfg: Config,
        hotkeys_cfg: HotkeyConfig,
        uploaders_cfg: UploaderConfig,
    ) {
        let specs = hotkeys_cfg.hotkey.clone();
        let launch_at_startup = cfg.launch_at_startup;
        store::set_and_save(cfg);
        store::hotkeys::set_and_save(hotkeys_cfg);
        store::uploaders::set_and_save(uploaders_cfg);
        startup::set_enabled(launch_at_startup);

        if let Some(mgr) = self.hotkey_manager.as_ref() {
            let _ = mgr.unregister_all(&self.current_hotkeys);
            let hks = parse_hotkeys(&specs);
            if let Err(e) = mgr.register_all(&hks) {
                eprintln!("ホットキー再登録に失敗: {e}");
            }
            self.current_hotkeys = hks;
        }
        println!("設定を保存しました");
    }

    fn poll_menu(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if self.quit_id.as_ref() == Some(&ev.id) {
                event_loop.exit();
            } else if self.settings_id.as_ref() == Some(&ev.id) {
                self.open_settings(event_loop);
            } else if self.capture_id.as_ref() == Some(&ev.id) {
                // Only while idle, or while recording setup/recording is
                // in progress and the screenshot session is unused
                // (ignored otherwise, including while settings are shown).
                if self.session.is_none() && self.settings.is_none() {
                    self.start_session(event_loop);
                } else if self.settings.is_none()
                    && self.shot_session.is_none()
                    && self.session.as_ref().is_some_and(Overlay::in_record_flow)
                {
                    self.start_shot_session(event_loop);
                }
            } else if self.editor_id.as_ref() == Some(&ev.id) {
                // A separate process, so it can launch regardless of the resident app's state (multiple allowed).
                spawn_editor_process(None);
            }
        }
    }

    /// Watches clicks on the tray icon itself (menu item clicks go
    /// through `poll_menu`). A left click opens Settings; a right click
    /// opens the OS-standard menu, so nothing is done here for that.
    fn poll_tray_icon(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                self.open_settings(event_loop);
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.hotkey_manager.is_none() && self.tray.is_none() {
            // With the default (WhenFocused), the instant the overlay
            // fails to reach the foreground, magnifier mode (which drives
            // the cursor from raw input) stops responding entirely.
            // Listening unconditionally, independent of focus, avoids that.
            event_loop.listen_device_events(DeviceEvents::Always);
            self.init();
            if self.show_settings_on_launch {
                self.show_settings_on_launch = false;
                self.open_settings(event_loop);
            }
        }
    }

    /// A request from another process (one that tried to launch a duplicate instance).
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::ShowSettings => self.open_settings(event_loop),
            UserEvent::UpdateCheckResult(result) => self.handle_update_check_result(result),
            UserEvent::UpdateReady(result) => self.handle_update_ready(result, event_loop),
            UserEvent::SnapshotReady(hwnd, snapshot) => {
                // Whichever session (if either is still around) owns this
                // hwnd; dropped silently if the session already ended
                // before the background enumeration finished.
                if let Some(s) = self.session.as_mut().filter(|s| s.owns_hwnd(hwnd)) {
                    s.set_snapshot(snapshot);
                } else if let Some(s) = self.shot_session.as_mut().filter(|s| s.owns_hwnd(hwnd)) {
                    s.set_snapshot(snapshot);
                }
            }
        }
    }

    /// Routes the event based on which session owns the window id. With a
    /// screenshot session opened in a separate window while recording,
    /// two `Overlay`s can coexist, so the event must go to whichever one
    /// actually owns that window rather than always going to `session`
    /// unconditionally — otherwise, e.g., an Esc/close event meant for
    /// the screenshot side could wrongly reach the recording `Overlay`
    /// and stop the recording unintentionally.
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(session) = self.session.as_mut()
            && session.owns_window(id)
        {
            session.window_event(event_loop, id, event);
            if session.finished() {
                self.finish_session();
            }
        } else if let Some(shot) = self.shot_session.as_mut()
            && shot.owns_window(id)
        {
            shot.window_event(event_loop, id, event);
            if shot.finished() {
                self.finish_shot_session();
            }
        } else if let Some(settings) = self.settings.as_mut()
            && let Some(result) = settings.handle_event(event)
        {
            self.close_settings(result);
        }
    }

    /// `DeviceEvent` is window-independent (for the magnifier's raw
    /// input). On the premise that if `shot_session` exists, `session`
    /// must be recording (the magnifier only applies during the
    /// selection phase), `shot_session` takes priority when present.
    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        if let Some(shot) = self.shot_session.as_mut() {
            shot.device_event(event_loop, id, event);
            if shot.finished() {
                self.finish_shot_session();
            }
        } else if let Some(session) = self.session.as_mut() {
            session.device_event(event_loop, id, event);
            if session.finished() {
                self.finish_session();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_menu(event_loop);
        self.poll_tray_icon(event_loop);

        // Whichever ControlFlow is set last wins. Calls shot_session
        // first and session second, so the recording elapsed-time
        // display's 1Hz WaitUntil doesn't get overwritten by the
        // screenshot side's plain Wait.
        if let Some(shot) = self.shot_session.as_mut() {
            shot.about_to_wait(event_loop);
            if shot.finished() {
                self.finish_shot_session();
            }
        }
        if let Some(session) = self.session.as_mut() {
            session.about_to_wait(event_loop);
            if session.finished() {
                self.finish_session();
            }
        }

        // Whether a new session can start: only while idle, or while
        // "recording setup/recording is in progress and the screenshot
        // session is unused" (never doubles up on recording). The hotkey
        // still works even while the settings screen is shown (entering
        // capture without closing it — any edits before Cancel/Save and
        // the currently shown tab are preserved).
        let can_start = self.session.is_none()
            || (self.shot_session.is_none()
                && self.session.as_ref().is_some_and(Overlay::in_record_flow));
        if can_start {
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                if ev.state == HotKeyState::Pressed
                    && self.current_hotkeys.iter().any(|hk| hk.id() == ev.id)
                {
                    if self.session.is_none() {
                        self.start_session(event_loop);
                    } else {
                        self.start_shot_session(event_loop);
                    }
                    break;
                }
            }
        }

        if self.session.is_some() || self.shot_session.is_some() {
            return; // Whichever is alive decides ControlFlow while it exists.
        }
        // Even while the settings screen is shown, plain Wait would only
        // wake on window activity and miss hotkeys, so polling continues
        // at the same interval as idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + IDLE_POLL));
    }
}

/// Runs the action matching a session's result (regular or screenshot).
/// Shared follow-up for `finish_session`/`finish_shot_session`.
fn handle_outcome(outcome: Option<Outcome>) {
    match outcome {
        Some(Outcome::Captured { action, shot }) => match action {
            Action::Save => match export::save_png(&shot) {
                Ok(p) => {
                    println!("saved: {}", p.display());
                    spawn_reveal(p);
                }
                Err(e) => eprintln!("保存に失敗: {e}"),
            },
            Action::Copy => match export::copy_to_clipboard(&shot) {
                Ok(()) => println!("copied: {}x{}", shot.width, shot.height),
                Err(e) => eprintln!("コピーに失敗: {e}"),
            },
            // Edit launches the editor as a separate process (multiple instances allowed).
            Action::Edit => spawn_editor(&shot),
            // EditExternal (Shift+E) hands off to the configured external editor.
            Action::EditExternal => spawn_editor_external(&shot),
            // Upload runs asynchronously on another thread (doesn't block the UI).
            Action::Upload => spawn_upload(shot),
            // Record is already handled inside overlay.
            Action::Record => {}
            // Quit never actually reaches here, since overlay ends
            // without producing an Outcome for it (kept only for exhaustiveness).
            Action::Quit => {}
        },
        Some(Outcome::Recorded(path)) => {
            println!("saved: {}", path.display());
            spawn_reveal(path);
        }
        Some(Outcome::Saved(path)) => {
            println!("saved: {}", path.display());
            spawn_reveal(path);
        }
        None => println!("cancelled"),
    }
}

/// Writes the cropped image to a temp png and launches the bundled Editor (E key).
fn spawn_editor(shot: &export::Shot) {
    match export::save_temp_png(shot) {
        Ok(path) => spawn_editor_process(Some(&path)),
        Err(e) => eprintln!("エディタ用の一時画像書き出しに失敗: {e}"),
    }
}

/// Writes the cropped image to a temp png and hands it off to the
/// configured external editor (Shift+E). No-op if unset (in practice
/// this shouldn't be reached, since `overlay::trigger`'s
/// `Action::EditExternal` only fires when an external editor is configured).
fn spawn_editor_external(shot: &export::Shot) {
    let Some(external) = store::external_editor() else {
        return;
    };
    match export::save_temp_png(shot) {
        Ok(path) => spawn_external_editor(&external, Some(&path)),
        Err(e) => eprintln!("エディタ用の一時画像書き出しに失敗: {e}"),
    }
}

/// Launches the bundled Editor (blank if `png_path` is `None`; used from
/// the tray's Editor, resuming a Recent-tab session, and `Action::Edit`).
/// Launched as a separate process (`pashari editor [<png>]`) and left
/// running (dropping `Child` doesn't stop it on Windows). Multiple
/// instances can run at once.
fn spawn_editor_process(png_path: Option<&Path>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("自身の実行パス取得に失敗: {e}");
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("editor");
    if let Some(p) = png_path {
        cmd.arg(p);
    }
    match cmd.spawn() {
        Ok(_child) => match png_path {
            Some(p) => println!("editor launched: {}", p.display()),
            None => println!("editor launched (blank)"),
        },
        Err(e) => eprintln!("エディタ起動に失敗: {e}"),
    }
}

/// The current time (UNIX seconds).
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Runs the update check on another thread and sends the result back to
/// the event loop via `proxy` as `UserEvent::UpdateCheckResult` (a
/// fire-and-forget thread like `spawn_upload`, but differs in returning its result through `UserEvent`).
fn spawn_update_check(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let result = update::check_latest();
        let _ = proxy.send_event(UserEvent::UpdateCheckResult(result));
    });
}

/// Uploads to every enabled uploader at once, on another thread
/// (fire-and-forget, doesn't block the UI). Copies the successful URLs to
/// the clipboard together, newline-separated.
fn spawn_upload(shot: export::Shot) {
    let profiles = store::enabled_uploaders();
    if profiles.is_empty() {
        eprintln!("有効なアップローダーがありません（Settings で設定してください）");
        return;
    }
    std::thread::spawn(move || {
        let mut urls = Vec::new();
        for profile in &profiles {
            match upload::upload(&shot, profile) {
                Ok(url) => {
                    println!("uploaded via {}: {url}", profile.name);
                    urls.push(url);
                }
                Err(e) => eprintln!("{} へのアップロードに失敗: {e}", profile.name),
            }
        }
        if urls.is_empty() {
            return;
        }
        let text = urls.join("\n");
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.clone())) {
            Ok(()) => println!("{} 件のURLをクリップボードへコピーしました", urls.len()),
            Err(e) => {
                eprintln!("クリップボードへのコピーに失敗: {e}");
                println!("{text}");
            }
        }
    });
}

/// Opens and selects the output file in Explorer (on another thread).
/// `SHOpenFolderAndSelectItems`'s first call (a cold start of COM/shell
/// extensions) can take several seconds, and calling it synchronously on
/// the main thread would delay tearing down the recording border/control
/// bar (the session's drop) along with it, making them visibly linger.
/// Made fire-and-forget so it doesn't block the UI.
fn spawn_reveal(path: std::path::PathBuf) {
    std::thread::spawn(move || shell::reveal_and_select(&path));
}

/// Launches the external editor set in the settings, with the image path (if any) as an argument.
fn spawn_external_editor(exe: &str, png_path: Option<&Path>) {
    let mut cmd = std::process::Command::new(exe);
    if let Some(p) = png_path {
        cmd.arg(p);
    }
    match cmd.spawn() {
        Ok(_child) => match png_path {
            Some(p) => println!("external editor launched: {exe} ({})", p.display()),
            None => println!("external editor launched: {exe}"),
        },
        Err(e) => eprintln!("外部エディタの起動に失敗（{exe}）: {e}"),
    }
}

/// `build_tray`'s return value: the tray plus each of the Quit, Settings, Capture, Editor `MenuId`s.
type TrayHandles = (TrayIcon, MenuId, MenuId, MenuId, MenuId);

/// Creates the tray icon (with a Capture / Editor / Settings / Quit menu).
fn build_tray() -> Result<TrayHandles, Box<dyn std::error::Error>> {
    let menu = Menu::new();
    let capture = MenuItem::new("Capture", true, None);
    let editor = MenuItem::new("Editor", true, None);
    let settings = MenuItem::new("Settings", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append(&capture)?;
    menu.append(&editor)?;
    menu.append(&settings)?;
    menu.append(&quit)?;
    let icon = load_tray_icon()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("pashari")
        .with_menu(Box::new(menu))
        // Reserves a left click for opening Settings (the menu only opens on right click).
        .with_menu_on_left_click(false)
        .with_icon(icon)
        .build()?;
    Ok((
        tray,
        quit.id().clone(),
        settings.id().clone(),
        capture.id().clone(),
        editor.id().clone(),
    ))
}

/// Decodes the embedded tray icon image into an `Icon`.
fn load_tray_icon() -> Result<Icon, tray_icon::BadIcon> {
    let img = image::load_from_memory(include_bytes!("../assets/tray_icon.png"))
        .expect("トレイアイコン画像のデコードに失敗")
        .to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h)
}

/// Discards any queued hotkey events.
fn drain_hotkeys() {
    while GlobalHotKeyEvent::receiver().try_recv().is_ok() {}
}

/// Converts a list of spec strings into `HotKey`s (unparseable entries
/// are dropped). Falls back to one default (Ctrl+Shift+2) only if none parsed.
fn parse_hotkeys(specs: &[String]) -> Vec<HotKey> {
    let hks: Vec<HotKey> = specs.iter().filter_map(|s| hotkey::parse(s)).collect();
    if hks.is_empty() {
        eprintln!("有効なホットキーがありません。既定(Ctrl+Shift+2)を使います");
        vec![HotKey::new(
            Some(Modifiers::CONTROL | Modifiers::SHIFT),
            Code::Digit2,
        )]
    } else {
        hks
    }
}
