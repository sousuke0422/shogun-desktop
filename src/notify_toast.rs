//! Desktop toast delivery for OSC 9 / OSC 777 terminal notifications.
//!
//! Focus suppression (Ghostty's `requireFocus` behavior — don't toast the
//! surface the user is looking at) is decided by the callers in `window.rs` /
//! `shell_window.rs`; this module only performs the platform delivery.

/// AppUserModelID toasts are attributed to. For an unpackaged Win32 app the
/// documented path is registering the AUMID under
/// `HKCU\Software\Classes\AppUserModelId` with a `DisplayName`; without that
/// the toast is silently dropped, so registration failure falls back to the
/// PowerShell AUMID (attributed to "Windows PowerShell", but visible).
#[cfg(windows)]
const AUMID: &str = "Shogun.Desktop";

/// Show a desktop notification. Fire-and-forget: delivery runs on a detached
/// thread (WinRT/COM calls must not stall the UI thread) and failures are
/// intentionally silent — a lost toast must never disturb the terminal.
#[cfg(windows)]
pub fn show(title: &str, body: &str) {
    use std::sync::OnceLock;
    static APP_ID: OnceLock<&'static str> = OnceLock::new();

    let title = title.to_owned();
    let body = body.to_owned();
    std::thread::spawn(move || {
        let app_id = *APP_ID.get_or_init(|| {
            if register_app_id() {
                AUMID
            } else {
                tauri_winrt_notification::Toast::POWERSHELL_APP_ID
            }
        });
        let _ = tauri_winrt_notification::Toast::new(app_id)
            .title(&title)
            .text1(&body)
            .sound(Some(tauri_winrt_notification::Sound::Default))
            .show();
    });
}

#[cfg(windows)]
fn register_app_id() -> bool {
    use std::os::windows::process::CommandExt;
    // Suppress the console window a spawned reg.exe would otherwise flash.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg.exe")
        .args([
            "add",
            r"HKCU\Software\Classes\AppUserModelId\Shogun.Desktop",
            "/v",
            "DisplayName",
            "/t",
            "REG_SZ",
            "/d",
            "将軍デスクトップ",
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// macOS: NSUserNotificationCenter via mac-notification-sys — the same
/// thin-native-wrapper approach as tauri-winrt-notification on Windows (and
/// what notify-rust wraps internally; notify-rust itself was rejected for
/// dragging in the discontinued async-std runtime). Attribution needs our
/// bundle identifier (assets/Info.plist), registered once. Fire-and-forget
/// like the Windows path: `send()` may wait on the delegate, so it runs on a
/// detached thread and failures stay silent.
#[cfg(target_os = "macos")]
pub fn show(title: &str, body: &str) {
    use std::sync::Once;
    static APP: Once = Once::new();

    let title = title.to_owned();
    let body = body.to_owned();
    std::thread::spawn(move || {
        APP.call_once(|| {
            let _ = mac_notification_sys::set_application("app.rikkalab.shogun-desktop");
        });
        let _ = mac_notification_sys::Notification::default()
            .title(&title)
            .message(&body)
            .send();
    });
}

/// Linux: planned as a zero-dependency `notify-send` spawn when the Linux
/// port lands (libnotify CLI, present on effectively every desktop distro).
#[cfg(not(any(windows, target_os = "macos")))]
pub fn show(_title: &str, _body: &str) {}
