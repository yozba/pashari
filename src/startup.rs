//! Auto-launch-on-Windows-login setting (reads/writes the `HKCU\...\Run`
//! key). Never blocks launch itself on failure (just logged).

use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::core::HSTRING;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "pashari";

/// Sets whether to auto-launch on Windows login, according to `enabled`
/// (adds/removes a value under the `HKCU\...\Run` registry key). If the
/// executable's path can't be obtained, or the registry operation fails,
/// this is just logged and ignored.
pub fn set_enabled(enabled: bool) {
    // A debug build has no `windows_subsystem = "windows"` (see main.rs),
    // so registering its path would leave a console window open on every
    // login. Leave the registry untouched rather than either writing a
    // debug path or deleting a legitimate release registration just
    // because a debug session happens to run with this setting on.
    if enabled && cfg!(debug_assertions) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("スタートアップ設定に失敗: 実行ファイルのパスが取得できません");
        return;
    };
    // SAFETY: a simple call that opens a fixed HKCU subkey, sets/removes
    // one value, and closes it. The opened handle is always closed with RegCloseKey.
    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .is_err()
        {
            eprintln!("スタートアップ設定に失敗: レジストリキーを開けません");
            return;
        }
        if enabled {
            // Quoted so it doesn't break if the path contains spaces.
            let quoted = HSTRING::from(format!("\"{}\"", exe.display()));
            // REG_SZ is passed as a null-terminated UTF-16LE byte sequence.
            let bytes: &[u8] =
                std::slice::from_raw_parts(quoted.as_ptr() as *const u8, (quoted.len() + 1) * 2);
            let _ = RegSetValueExW(hkey, &HSTRING::from(VALUE_NAME), 0, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(hkey, &HSTRING::from(VALUE_NAME));
        }
        let _ = RegCloseKey(hkey);
    }
}
