//! Checks GitHub Releases for a newer version and, when the user clicks
//! "Update" in Settings, downloads and installs it in place.
//!
//! Two independent update mechanisms, chosen at runtime by checking
//! whether the running exe sits where Inno Setup installed it (see
//! `update_target`/`is_installed_here`):
//! - Portable: the running exe is renamed aside, the downloaded exe takes
//!   its place, and the new exe is relaunched (`relaunch_portable`).
//! - Installed: the downloaded installer runs silently and relaunches the
//!   app itself (`relaunch_installer`; see `installer/pashari.iss`'s
//!   `AppMutex`/`[Run]` for how it waits for this process to exit and
//!   launches the new one afterward).
//!
//! This is an unsigned binary, so self-replacing it isn't risk-free (AV
//! false positives, a botched install leaving a broken app). The portable
//! path rolls back the exe rename on failure so a partial write can't
//! leave the app without an executable, but there's no way to eliminate
//! this risk entirely.

use std::path::{Path, PathBuf};

use serde_json::Value;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_VALUE_TYPE, RegCloseKey,
    RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::core::HSTRING;

/// This build's version (`Cargo.toml`'s `version`).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const REPO_API: &str = "https://api.github.com/repos/yozba/pashari/releases/latest";

/// The uninstall registry subkey Inno Setup creates for this app (the
/// GUID is `installer/pashari.iss`'s fixed `AppId` — never changes).
const UNINSTALL_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{59CEB6BF-D0A0-4B1C-9C5D-56068B5FD365}_is1";

/// Env var a relaunched portable exe checks at startup (see
/// `wait_for_relaunch_signal`) to wait for the old process — spawning it,
/// this process's own pid — to exit before starting normally.
const RELAUNCH_WAIT_PID_VAR: &str = "PASHARI_RELAUNCH_WAIT_PID";

/// Info about a newer release that was found.
#[derive(Clone)]
pub struct ReleaseInfo {
    /// The version string with the tag's leading `v` stripped (e.g. "0.6.0").
    pub version: String,
    /// The release page URL — used as a fallback when this build can't
    /// self-update (an old release predating `exe_url`/`setup_url`).
    pub url: String,
    /// The bare `pashari.exe` asset, for updating a portable install.
    pub exe_url: Option<String>,
    /// The `*-setup.exe` installer asset, for updating an installed copy.
    pub setup_url: Option<String>,
}

/// Which asset to fetch to update this running copy, and how to install
/// it once downloaded.
pub enum UpdateTarget {
    Portable(String),
    Installer(String),
}

/// A downloaded update, ready to install.
pub enum UpdateArtifact {
    Portable(PathBuf),
    Installer(PathBuf),
}

/// Fetches the latest release, returning it if newer than the current version.
pub fn check_latest() -> Result<Option<ReleaseInfo>, String> {
    let value: Value = ureq::get(REPO_API)
        .header("User-Agent", "pashari-update-check")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;

    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("tag_name が取得できません")?;
    let version = tag.trim_start_matches('v').to_string();
    let url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("https://github.com/yozba/pashari/releases")
        .to_string();
    let (exe_url, setup_url) = value
        .get("assets")
        .and_then(Value::as_array)
        .map(|assets| pick_asset_urls(assets))
        .unwrap_or((None, None));

    if is_newer(&version, CURRENT_VERSION) {
        Ok(Some(ReleaseInfo {
            version,
            url,
            exe_url,
            setup_url,
        }))
    } else {
        Ok(None)
    }
}

/// Picks the bare-exe and installer asset URLs out of a release's
/// `assets` array, by name (`"pashari.exe"` exactly, and anything ending
/// in `"-setup.exe"`) — an OS-independent pure function, directly
/// testable against a fixture JSON array.
fn pick_asset_urls(assets: &[Value]) -> (Option<String>, Option<String>) {
    let mut exe_url = None;
    let mut setup_url = None;
    for asset in assets {
        let name = asset.get("name").and_then(Value::as_str).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(Value::as_str)
            .map(String::from);
        if name == "pashari.exe" {
            exe_url = url;
        } else if name.ends_with("-setup.exe") {
            setup_url = url;
        }
    }
    (exe_url, setup_url)
}

/// A simple numeric "X.Y.Z" comparison (no pre-release identifier
/// support; sufficient since this project's tags are always `vX.Y.Z`).
/// Missing/invalid components are treated as `0`; never panics.
pub fn is_newer(latest: &str, current: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

fn parse_version(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Which asset to fetch for updating this running copy, based on whether
/// it's installed (Inno Setup) or portable. `None` if the release doesn't
/// have the asset this build needs (an old release predating this
/// feature) — the caller should fall back to opening the release page.
pub fn update_target(info: &ReleaseInfo) -> Option<UpdateTarget> {
    if is_installed_here() {
        info.setup_url.clone().map(UpdateTarget::Installer)
    } else {
        info.exe_url.clone().map(UpdateTarget::Portable)
    }
}

/// Whether the running exe's directory matches where Inno Setup installed
/// it (checked via the uninstall registry key it creates — tried
/// per-user first, since `PrivilegesRequired=lowest` makes that the
/// default install type).
fn is_installed_here() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(exe_dir) = exe.parent() else {
        return false;
    };
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .filter_map(read_install_location)
        .any(|loc| paths_match(&loc, exe_dir))
}

fn paths_match(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Reads `InstallLocation` from the uninstall subkey under `root`, if present.
fn read_install_location(root: HKEY) -> Option<PathBuf> {
    // SAFETY: opens a fixed uninstall subkey read-only, reads one string
    // value into a heap buffer sized from a first query, and always
    // closes the handle before returning.
    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(
            root,
            &HSTRING::from(UNINSTALL_SUBKEY),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return None;
        }
        let value_name = HSTRING::from("InstallLocation");
        let mut reg_type = REG_VALUE_TYPE::default();
        let mut byte_len: u32 = 0;
        let sized = RegQueryValueExW(
            hkey,
            &value_name,
            None,
            Some(&mut reg_type),
            None,
            Some(&mut byte_len),
        );
        if sized.is_err() || byte_len == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; byte_len as usize];
        let read = RegQueryValueExW(
            hkey,
            &value_name,
            None,
            Some(&mut reg_type),
            Some(buf.as_mut_ptr()),
            Some(&mut byte_len),
        );
        let _ = RegCloseKey(hkey);
        if read.is_err() {
            return None;
        }
        // REG_SZ is UTF-16LE, null-terminated.
        let words: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&words);
        let s = s.trim_end_matches('\0').trim();
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

/// Downloads `target`'s asset to a temp file, returning the downloaded
/// artifact (not yet installed).
pub fn download_update(target: UpdateTarget) -> Result<UpdateArtifact, String> {
    let (url, file_name, wrap): (_, _, fn(PathBuf) -> UpdateArtifact) = match &target {
        UpdateTarget::Portable(url) => (url, "pashari-update.exe", UpdateArtifact::Portable),
        UpdateTarget::Installer(url) => {
            (url, "pashari-update-setup.exe", UpdateArtifact::Installer)
        }
    };

    let dir = std::env::temp_dir().join("pashari-update");
    std::fs::create_dir_all(&dir).map_err(|e| format!("一時フォルダの作成に失敗: {e}"))?;
    let dest = dir.join(file_name);

    let mut resp = ureq::get(url)
        .header("User-Agent", "pashari-update")
        .call()
        .map_err(|e| format!("ダウンロードに失敗: {e}"))?;
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&dest).map_err(|e| format!("ファイル作成に失敗: {e}"))?;
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("ダウンロードに失敗: {e}"))?;
    drop(file);

    Ok(wrap(dest))
}

/// Replaces the running portable exe with `new_exe` and launches it,
/// telling it (via an env var) to wait for this process to exit before
/// starting normally — avoiding a race with `single_instance`'s Mutex,
/// which isn't released until this process actually terminates.
pub fn relaunch_portable(new_exe: &Path) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|e| format!("自身のパス取得に失敗: {e}"))?;
    let backup = current.with_extension("exe.old");
    // Best-effort: clear out a leftover from a previous update that
    // failed to clean up (cleanup_old_exe normally handles this on
    // startup, but a rename below would fail if one is already there).
    let _ = std::fs::remove_file(&backup);

    std::fs::rename(&current, &backup).map_err(|e| format!("実行中のexeの退避に失敗: {e}"))?;
    if let Err(e) = std::fs::rename(new_exe, &current) {
        // Roll back so the app isn't left without an exe at its own path.
        let _ = std::fs::rename(&backup, &current);
        return Err(format!("新しいexeへの置き換えに失敗: {e}"));
    }

    let pid = std::process::id();
    std::process::Command::new(&current)
        .env(RELAUNCH_WAIT_PID_VAR, pid.to_string())
        // Shows Settings on the new instance so the update is visibly
        // confirmed instead of silently minimizing to the tray (see main.rs).
        .arg("--show-settings")
        .spawn()
        .map_err(|e| format!("新しいexeの起動に失敗: {e}"))?;
    Ok(())
}

/// Silently runs the downloaded installer (`installer/pashari.iss`'s
/// `AppMutex`/`[Run]` handle waiting for this process to exit and
/// relaunching the app afterward).
pub fn relaunch_installer(setup_path: &Path) -> Result<(), String> {
    std::process::Command::new(setup_path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .spawn()
        .map_err(|e| format!("インストーラの起動に失敗: {e}"))?;
    Ok(())
}

/// Called once at the very start of `main()`. If `RELAUNCH_WAIT_PID_VAR`
/// is set (this process was just spawned by `relaunch_portable`), waits
/// up to a few seconds for that pid to exit before returning, so this
/// process doesn't race the old one for `single_instance`'s Mutex.
/// Best-effort: proceeds immediately if the pid is already gone, the wait
/// fails, or the var isn't set at all.
pub fn wait_for_relaunch_signal() {
    let Ok(pid_str) = std::env::var(RELAUNCH_WAIT_PID_VAR) else {
        return;
    };
    let Ok(pid) = pid_str.parse::<u32>() else {
        return;
    };
    // SAFETY: opens the given pid with only SYNCHRONIZE access (wait-only,
    // no ability to affect the process), waits with a bounded timeout,
    // and closes the handle via `windows`'s `Owned`/`Drop` handling on scope exit.
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(handle, 5000);
            let _ = CloseHandle(handle);
        }
    }
}

/// Best-effort cleanup of a leftover `<exe>.exe.old` from a previous
/// portable update (see `relaunch_portable`). Called once at startup
/// after this process is confirmed to be the sole instance.
pub fn cleanup_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_newer_compares_major_minor_patch_numerically() {
        assert!(is_newer("0.6.0", "0.5.0"));
        assert!(is_newer("0.5.10", "0.5.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.5.0", "0.6.0"));
    }

    #[test]
    fn is_newer_treats_malformed_or_short_versions_as_zero_without_panicking() {
        assert!(!is_newer("garbage", "0.0.0"));
        assert!(is_newer("1", "0.9.9"));
        assert!(!is_newer("0.5", "0.5.1"));
    }

    #[test]
    fn pick_asset_urls_matches_bare_exe_and_setup_installer_by_name() {
        let assets = vec![
            json!({"name": "pashari-v1.0.0-windows-x64.zip", "browser_download_url": "https://x/zip"}),
            json!({"name": "pashari.exe", "browser_download_url": "https://x/exe"}),
            json!({"name": "pashari-1.0.0-setup.exe", "browser_download_url": "https://x/setup"}),
        ];
        let (exe, setup) = pick_asset_urls(&assets);
        assert_eq!(exe.as_deref(), Some("https://x/exe"));
        assert_eq!(setup.as_deref(), Some("https://x/setup"));
    }

    #[test]
    fn pick_asset_urls_returns_none_for_missing_assets() {
        let assets = vec![
            json!({"name": "pashari-v1.0.0-windows-x64.zip", "browser_download_url": "https://x/zip"}),
        ];
        let (exe, setup) = pick_asset_urls(&assets);
        assert_eq!(exe, None);
        assert_eq!(setup, None);
    }

    #[test]
    fn pick_asset_urls_ignores_assets_missing_a_download_url() {
        let assets = vec![json!({"name": "pashari.exe"})];
        let (exe, _) = pick_asset_urls(&assets);
        assert_eq!(exe, None);
    }
}
