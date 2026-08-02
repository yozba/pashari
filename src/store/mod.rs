//! The config file (`%APPDATA%\pashari\config.toml`).
//!
//! Kept minimal — only what's needed. Auto-generates a commented template
//! on first run. Held in a `Mutex` so the GUI (tray -> Settings) can
//! update it live.
//!
//! Hotkeys ([`hotkeys`]) and custom uploaders ([`uploaders`]) are
//! different in nature from the other scalar settings (the former is a
//! 19-field block, the latter a variable-length list containing secret
//! tokens), so they're split into their own files.

pub mod hotkeys;
pub mod uploaders;

pub use uploaders::UploaderProfile;

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

/// One of the items the region-selection action menu can show: the
/// selection-size display, the aspect-ratio-lock dropdown, or one of the
/// region-selection actions (also usable as hotkeys — see
/// `store::hotkeys::HotkeyConfig`). Order and membership in
/// `Config::menu_buttons` control the menu's layout (settings GUI's
/// General tab).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuButton {
    Size,
    AspectRatio,
    Save,
    Copy,
    Edit,
    Upload,
    Video,
    Quit,
    Undo,
    Redo,
    ReuseRegion,
    ClearSelection,
    SaveAs,
    EditExternal,
    /// A visible vertical line between buttons — layout-only, not an
    /// action (no hotkey, never disabled, absorbs clicks without firing
    /// anything). Unlike most other variants, more than one can appear in
    /// `Config::menu_buttons` at once (see `repeatable`).
    Divider,
    /// Like `Divider`, but transparent — just opens up space between the
    /// buttons on either side, same width as `Divider`. Also repeatable.
    Spacer,
}

impl MenuButton {
    /// Every item that can appear in the menu, in a fixed canonical order
    /// (used to fill the settings GUI's "available" pool with anything not
    /// in the saved/visible list — see `repeatable` for why `Divider`/
    /// `Spacer` always stay in that pool regardless of how many are shown).
    pub const ALL: [MenuButton; 16] = [
        Self::Size,
        Self::AspectRatio,
        Self::Save,
        Self::Copy,
        Self::Edit,
        Self::Upload,
        Self::Video,
        Self::Quit,
        Self::Undo,
        Self::Redo,
        Self::ReuseRegion,
        Self::ClearSelection,
        Self::SaveAs,
        Self::EditExternal,
        Self::Divider,
        Self::Spacer,
    ];

    /// Whether more than one of this item can be shown at once. Everything
    /// except `Divider`/`Spacer` is a singleton — added to the menu at
    /// most once, so placing it removes it from the "available" pool and
    /// removing it returns it there. `Divider`/`Spacer` instead always
    /// stay available (they're layout aids, not distinct actions), and
    /// each placed copy is its own independent instance.
    pub fn repeatable(self) -> bool {
        matches!(self, Self::Divider | Self::Spacer)
    }

    /// The default *visible* set — the menu's original fixed layout, before
    /// this customization feature existed. The rest of `ALL` starts in the
    /// "available" pool instead, so a fresh install isn't cluttered.
    const DEFAULT_VISIBLE: [MenuButton; 8] = [
        Self::Size,
        Self::AspectRatio,
        Self::Save,
        Self::Copy,
        Self::Edit,
        Self::Upload,
        Self::Video,
        Self::Quit,
    ];

    /// Short label, shown both under the real menu's square buttons and on
    /// the settings GUI's chips — must stay short enough to fit a small
    /// square (existing labels like "Upload"/"Record" are the width budget
    /// to stay within).
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::AspectRatio => "Aspect",
            Self::Save => "Save",
            Self::Copy => "Copy",
            Self::Edit => "Edit",
            Self::Upload => "Upload",
            Self::Video => "Video",
            Self::Quit => "Quit",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::ReuseRegion => "Reuse",
            Self::ClearSelection => "Reset",
            Self::SaveAs => "Save As",
            Self::EditExternal => "Edit Ext",
            Self::Divider => "Divider",
            Self::Spacer => "Spacer",
        }
    }

    /// The `#[serde(rename_all = "snake_case")]` string this variant
    /// (de)serializes as, for hand-writing the toml array in `render_toml`.
    fn toml_name(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::AspectRatio => "aspect_ratio",
            Self::Save => "save",
            Self::Copy => "copy",
            Self::Edit => "edit",
            Self::Upload => "upload",
            Self::Video => "video",
            Self::Quit => "quit",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::ReuseRegion => "reuse_region",
            Self::ClearSelection => "clear_selection",
            Self::SaveAs => "save_as",
            Self::EditExternal => "edit_external",
            Self::Divider => "divider",
            Self::Spacer => "spacer",
        }
    }
}

/// The config. Unset fields fall back to defaults.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Screenshot (png) save location. `%USERPROFILE%\Pictures\pashari` if empty.
    pub save_dir_png: String,
    /// mp4 save location. `%USERPROFILE%\Pictures\pashari` if empty.
    pub save_dir_mp4: String,
    /// gif save location. `%USERPROFILE%\Pictures\pashari` if empty.
    pub save_dir_gif: String,
    /// The last recording output format ("mp4" | "gif"), used as the
    /// initial value the next time recording setup opens.
    pub record_format: String,
    /// The last desktop-audio toggle state.
    pub record_desktop_audio: bool,
    /// The last mic toggle state.
    pub record_mic: bool,
    /// Whether to show the mouse cursor in the recording.
    pub record_show_cursor: bool,
    /// mp4 recording bitrate (Mbps).
    pub record_bitrate_mbps: u32,
    /// Max recording width (px); scaled down preserving aspect ratio if
    /// exceeded. 0 disables it (no limit, recorded at the selection's actual size).
    pub record_max_width: u32,
    /// Max recording height (px). 0 disables it (no limit).
    pub record_max_height: u32,
    /// Whether to show a ripple expanding from each click in the recording.
    pub record_show_click_ripple: bool,
    /// Left-click ripple color (`0x00RRGGBB`).
    pub record_click_color_left: u32,
    /// Right-click ripple color (`0x00RRGGBB`).
    pub record_click_color_right: u32,
    /// The device name used for desktop-audio loopback. Uses the
    /// system's default device if empty.
    pub record_audio_output_device: String,
    /// The device name used for mic input. Uses the system's default device if empty.
    pub record_audio_input_device: String,
    /// The target sample rate (Hz) for recorded audio. One of the
    /// options offered in the Video tab's dropdown (44100/48000).
    pub record_audio_sample_rate: u32,
    /// Whether to strip the mp4's audio track after the fact if the
    /// recording turned out to be effectively silent throughout
    /// (whether audio was never turned on, or was on but silent anyway).
    pub record_strip_silent_audio: bool,
    /// The presets cycled through each time the FPS button is pressed
    /// during recording setup (freely add/remove).
    pub record_fps_presets: Vec<u32>,
    /// The last-chosen recording FPS.
    pub record_fps: u32,
    /// The external editor's executable path. Used for Shift+E during
    /// region selection when set (E always opens the bundled Editor).
    pub external_editor: String,

    /// Max number of Editor session history entries kept in the Recent tab.
    pub session_history_limit: usize,

    /// The time (UNIX seconds) of the last update check, used to
    /// throttle the automatic startup check (0 = never checked).
    pub last_update_check: i64,

    /// Whether to launch automatically on Windows login.
    pub launch_at_startup: bool,

    /// The save filename template. Date/time can use chrono's strftime
    /// format (%Y/%m/%d/%H/%M/%S etc) directly. The counter is %n (avoids
    /// collisions) / %#n (persistent; specify zero-padding width like
    /// %04n). Doesn't include the extension (added automatically by output_path).
    pub filename_format: String,
    /// The next value to use for %#n (persistent counter). Doesn't increment unless %#n is used.
    pub filename_counter: u32,

    /// The region-selection action menu's buttons, in display order.
    /// Only the *visible* ones are listed — anything from
    /// `MenuButton::ALL` missing here is hidden (settings GUI's General
    /// tab). No separate visibility flag, so there's nothing to default
    /// per-item when this whole list is present but a variant isn't in it.
    pub menu_buttons: Vec<MenuButton>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_dir_png: String::new(),
            save_dir_mp4: String::new(),
            save_dir_gif: String::new(),
            record_format: "mp4".into(),
            record_desktop_audio: false,
            record_mic: false,
            record_show_cursor: true,
            record_bitrate_mbps: 15,
            record_max_width: 0,
            record_max_height: 0,
            record_show_click_ripple: false,
            record_click_color_left: 0x00FF_0000,
            record_click_color_right: 0x0000_00FF,
            record_audio_output_device: String::new(),
            record_audio_input_device: String::new(),
            record_audio_sample_rate: 48_000,
            record_strip_silent_audio: true,
            record_fps_presets: vec![15, 24, 30, 60],
            record_fps: 30,
            external_editor: String::new(),
            session_history_limit: 10,
            last_update_check: 0,
            launch_at_startup: false,
            filename_format: "pashari_%Y-%m-%d_%H-%M-%S".into(),
            filename_counter: 1,
            menu_buttons: MenuButton::DEFAULT_VISIBLE.to_vec(),
        }
    }
}

static CONFIG: LazyLock<Mutex<Config>> = LazyLock::new(|| Mutex::new(Config::default()));

/// Loads the config file (generates a template if missing). Called once at startup.
pub fn init() {
    *CONFIG.lock().unwrap() = load_or_create();
    hotkeys::init();
    uploaders::init();
}

/// Returns a clone of the current config (e.g. for the GUI's initial display).
pub fn snapshot() -> Config {
    CONFIG.lock().unwrap().clone()
}

/// The save folder for an extension ("png"/"mp4"/"gif"); empty means use the default.
pub fn save_dir_for(ext: &str) -> String {
    let cfg = CONFIG.lock().unwrap();
    match ext {
        "mp4" => cfg.save_dir_mp4.clone(),
        "gif" => cfg.save_dir_gif.clone(),
        _ => cfg.save_dir_png.clone(),
    }
}

/// The global hotkey spec strings (multiple allowed).
pub fn hotkey_specs() -> Vec<String> {
    hotkeys::snapshot().hotkey
}

/// The external editor's executable path used for Shift+E. `None` if unset.
pub fn external_editor() -> Option<String> {
    let cfg = CONFIG.lock().unwrap();
    let p = cfg.external_editor.trim();
    if p.is_empty() {
        None
    } else {
        Some(p.to_string())
    }
}

/// The enabled (`enabled: true`) uploaders. Upload sends to all of them
/// at once (Upload is unavailable if there are none).
pub fn enabled_uploaders() -> Vec<UploaderProfile> {
    uploaders::snapshot()
        .uploaders
        .into_iter()
        .filter(|u| u.enabled)
        .collect()
}

/// The region-selection action menu's buttons, in display order (read
/// fresh on every menu build, same as `enabled_uploaders`).
pub fn menu_button_order() -> Vec<MenuButton> {
    CONFIG.lock().unwrap().menu_buttons.clone()
}

/// Updates the config and writes it out as toml (called from the GUI's Save).
pub fn set_and_save(cfg: Config) {
    write_to_disk(&cfg);
    *CONFIG.lock().unwrap() = cfg;
}

/// Writes `cfg` to the config file (doesn't lock `CONFIG`, so this can
/// also be called while `CONFIG` is already locked).
fn write_to_disk(cfg: &Config) {
    if let Some(path) = config_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, render_toml(cfg)) {
            eprintln!("設定の保存に失敗: {e}");
        }
    }
}

/// The save filename template string.
pub fn filename_format() -> String {
    CONFIG.lock().unwrap().filename_format.clone()
}

/// Returns the value to use for `%#n` (persistent counter), incrementing
/// the internal counter for next time and writing it back to the config
/// file immediately (so the sequence continues across app restarts).
/// Only called for templates containing `%#n`.
pub fn next_filename_counter() -> u32 {
    let mut cfg = CONFIG.lock().unwrap();
    let n = cfg.filename_counter;
    cfg.filename_counter = n.saturating_add(1);
    write_to_disk(&cfg);
    n
}

/// The config file's path (`%APPDATA%\pashari\config.toml`).
fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(appdata).join("pashari").join("config.toml"))
}

fn load_or_create() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };

    if let Ok(text) = std::fs::read_to_string(&path) {
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => return cfg,
            Err(e) => {
                eprintln!("設定ファイルの解釈に失敗（既定を使用）: {e}");
                eprintln!(
                    "ヒント: バックスラッシュを含むパスは ' ' で囲む（例 save_dir = 'D:\\dir'）か、/ を使ってください。"
                );
                return Config::default();
            }
        }
    }

    // Writes out a template if missing (continues with defaults on failure).
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&path, render_toml(&Config::default())).is_ok() {
        println!("設定ファイルを作成しました: {}", path.display());
    }
    Config::default()
}

/// Generates commented toml (shared by first-run generation and GUI saves).
fn render_toml(c: &Config) -> String {
    format!(
        r#"# pashari 設定ファイル。トレイ → Settings の GUI から変更できます（手編集も可）。
# 変更は GUI の Save で即反映、手編集の場合は再起動で反映されます。
#
# ホットキーは hotkeys.toml、カスタムアップローダーは uploaders.toml に
# それぞれ分かれています。

# 保存先フォルダ（種類ごとに別々に指定可。空なら %USERPROFILE%\Pictures\pashari）。
# バックスラッシュを含むパスは ' '（シングルクォート）で囲むとそのまま書けます。
save_dir_png = '{}'
save_dir_mp4 = '{}'
save_dir_gif = '{}'

# 保存ファイル名のテンプレート（拡張子は含めない。自動で付きます）。
# 日付/時刻は chrono の strftime 書式がそのまま使えます（例: %Y %m %d
# %H %M %S %A %j）。カウンタは %n（保存先に同名ファイルが無い最小の
# 番号を探す）/ %#n（前回の続きから増え続ける永続カウンタ）。ゼロ埋め
# 桁数は %04n のように指定（省略時は4桁）。
filename_format = '{}'
# %#n（永続カウンタ）で次に使う値（自動的に増えます。手編集も可）。
filename_counter = {}

# 録画準備画面の初期値（前回使ったものを自動的に覚えています。手編集不要）。
record_format = "{}"
record_desktop_audio = {}
record_mic = {}
# 録画にマウスカーソルを映すかどうか。
record_show_cursor = {}
# mp4 録画のビットレート（Mbps）。
record_bitrate_mbps = {}
# 録画の幅/高さの上限（px）。それぞれ 0 なら無効（無制限）。
record_max_width = {}
record_max_height = {}
# クリック位置に広がる波紋を録画に映すかどうか。
record_show_click_ripple = {}
# 左/右クリック波紋の色（0xRRGGBB）。
record_click_color_left = 0x{:06X}
record_click_color_right = 0x{:06X}

# 録画で使う音声デバイス名。空ならシステムの既定デバイスを使います
# （設定GUIの Video タブから選べます。手編集も可）。
record_audio_output_device = '{}'
record_audio_input_device = '{}'
# 録画音声の目標サンプルレート（Hz）。
record_audio_sample_rate = {}
# 録画全体を通して音声が実質無音だった場合、mp4 の音声トラックを
# 事後除去するかどうか。
record_strip_silent_audio = {}

# 録画準備画面の FPS ボタンを押すたびに切り替える候補。自由に増減できます。
record_fps_presets = [{}]
# 前回選んだ FPS（自動的に覚えています。手編集も可）。
record_fps = {}

# 外部エディタの実行ファイルパス（設定GUIの Editor タブから選べます）。
# 設定されていれば、領域選択中の Shift+E でこのエディタへ渡せます
# （通常の Edit / E キーは常に同梱の Editor を開きます）。
external_editor = '{}'

# 設定GUIの Recent タブに保持する Editor セッション履歴の最大件数。
session_history_limit = {}
# 最後にアップデート確認をした時刻（UNIX秒。自動的に更新されます。手編集不要）。
last_update_check = {}
# Windows ログイン時に自動起動するかどうか。
launch_at_startup = {}

# 領域選択メニューに表示するボタンと並び順（設定GUIの General タブから
# 変更できます）。ここに無い項目は非表示になります。
menu_buttons = [{}]
"#,
        c.save_dir_png,
        c.save_dir_mp4,
        c.save_dir_gif,
        c.filename_format,
        c.filename_counter,
        c.record_format,
        c.record_desktop_audio,
        c.record_mic,
        c.record_show_cursor,
        c.record_bitrate_mbps,
        c.record_max_width,
        c.record_max_height,
        c.record_show_click_ripple,
        c.record_click_color_left,
        c.record_click_color_right,
        c.record_audio_output_device,
        c.record_audio_input_device,
        c.record_audio_sample_rate,
        c.record_strip_silent_audio,
        c.record_fps_presets
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        c.record_fps,
        c.external_editor,
        c.session_history_limit,
        c.last_update_check,
        c.launch_at_startup,
        c.menu_buttons
            .iter()
            .map(|b| format!("\"{}\"", b.toml_name()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_toml_round_trips_last_update_check() {
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&Config::default()))
                .unwrap()
                .last_update_check,
            0
        );
        let cfg = Config {
            last_update_check: 1_700_000_000,
            ..Config::default()
        };
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&cfg))
                .unwrap()
                .last_update_check,
            1_700_000_000
        );
    }

    #[test]
    fn render_toml_round_trips_record_max_width_and_height() {
        let default = toml::from_str::<Config>(&render_toml(&Config::default())).unwrap();
        assert_eq!(default.record_max_width, 0);
        assert_eq!(default.record_max_height, 0);

        let cfg = Config {
            record_max_width: 1920,
            record_max_height: 1080,
            ..Config::default()
        };
        let round_tripped = toml::from_str::<Config>(&render_toml(&cfg)).unwrap();
        assert_eq!(round_tripped.record_max_width, 1920);
        assert_eq!(round_tripped.record_max_height, 1080);
    }

    #[test]
    fn render_toml_round_trips_record_audio_sample_rate() {
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&Config::default()))
                .unwrap()
                .record_audio_sample_rate,
            48_000
        );
        let cfg = Config {
            record_audio_sample_rate: 44_100,
            ..Config::default()
        };
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&cfg))
                .unwrap()
                .record_audio_sample_rate,
            44_100
        );
    }

    #[test]
    fn render_toml_round_trips_menu_buttons() {
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&Config::default()))
                .unwrap()
                .menu_buttons,
            MenuButton::DEFAULT_VISIBLE.to_vec()
        );
        // Round-trips one of the new (non-default-visible) variants too,
        // including *multiple* dividers/spacers (the repeatable variants).
        let cfg = Config {
            menu_buttons: vec![
                MenuButton::Quit,
                MenuButton::Divider,
                MenuButton::Save,
                MenuButton::Spacer,
                MenuButton::Divider,
                MenuButton::Undo,
            ],
            ..Config::default()
        };
        assert_eq!(
            toml::from_str::<Config>(&render_toml(&cfg))
                .unwrap()
                .menu_buttons,
            vec![
                MenuButton::Quit,
                MenuButton::Divider,
                MenuButton::Save,
                MenuButton::Spacer,
                MenuButton::Divider,
                MenuButton::Undo,
            ]
        );
    }

    #[test]
    fn only_divider_and_spacer_are_repeatable() {
        for b in MenuButton::ALL {
            let expected = matches!(b, MenuButton::Divider | MenuButton::Spacer);
            assert_eq!(b.repeatable(), expected, "{b:?}");
        }
    }

    #[test]
    fn menu_buttons_missing_from_toml_falls_back_to_the_default_button_list() {
        let toml_without_menu_buttons = render_toml(&Config::default())
            .lines()
            .filter(|l| !l.starts_with("menu_buttons"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            toml::from_str::<Config>(&toml_without_menu_buttons)
                .unwrap()
                .menu_buttons,
            MenuButton::DEFAULT_VISIBLE.to_vec()
        );
    }

    #[test]
    fn render_toml_round_trips_record_strip_silent_audio() {
        assert!(
            toml::from_str::<Config>(&render_toml(&Config::default()))
                .unwrap()
                .record_strip_silent_audio
        );
        let cfg = Config {
            record_strip_silent_audio: false,
            ..Config::default()
        };
        assert!(
            !toml::from_str::<Config>(&render_toml(&cfg))
                .unwrap()
                .record_strip_silent_audio
        );
    }

    #[test]
    fn render_toml_round_trips_filename_format_and_counter() {
        let default_text = render_toml(&Config::default());
        let default_parsed: Config =
            toml::from_str(&default_text).expect("生成した toml が解釈できるはず");
        assert_eq!(default_parsed.filename_format, "pashari_%Y-%m-%d_%H-%M-%S");
        assert_eq!(default_parsed.filename_counter, 1);

        let cfg = Config {
            filename_format: "shot_%#04n_%Y%m%d".into(),
            filename_counter: 42,
            ..Config::default()
        };
        let parsed: Config =
            toml::from_str(&render_toml(&cfg)).expect("生成した toml が解釈できるはず");
        assert_eq!(parsed.filename_format, "shot_%#04n_%Y%m%d");
        assert_eq!(parsed.filename_counter, 42);
    }

    #[test]
    fn render_toml_round_trips_audio_device_names() {
        // The default (empty string = system default device).
        let default_text = render_toml(&Config::default());
        let default_parsed: Config =
            toml::from_str(&default_text).expect("生成した toml が解釈できるはず");
        assert_eq!(default_parsed.record_audio_output_device, "");
        assert_eq!(default_parsed.record_audio_input_device, "");

        // When an actual device name is chosen.
        let cfg = Config {
            record_audio_output_device: "Speakers (Realtek Audio)".into(),
            record_audio_input_device: "Microphone Array".into(),
            ..Default::default()
        };
        let text = render_toml(&cfg);
        let parsed: Config = toml::from_str(&text).expect("生成した toml が解釈できるはず");
        assert_eq!(
            parsed.record_audio_output_device,
            "Speakers (Realtek Audio)"
        );
        assert_eq!(parsed.record_audio_input_device, "Microphone Array");
    }
}
