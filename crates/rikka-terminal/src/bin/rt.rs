//! `rt` — thin launcher for RikkaTerminal.
//!
//! Receives argv, hands it verbatim to `rikka-terminal.exe` sitting next to
//! itself, and exits without waiting: the terminal owns all wt-compatible
//! parsing (see `src/cli.rs`), this binary owns nothing. Deliberately
//! dependency-light so it stays a few hundred KB instead of duplicating the
//! 13 MB product, and deliberately a separate process boundary: when
//! `-w/--window` routing to a running instance lands, THIS is where the
//! instance pipe gets talked to, without ever booting the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn fail(msg: &str) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::HSTRING;
        MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from("rt (RikkaTerminal launcher)"),
            MB_OK | MB_ICONERROR,
        );
    }
    #[cfg(not(windows))]
    eprintln!("{msg}");
    std::process::exit(1);
}

fn main() {
    let target_name = if cfg!(windows) {
        "rikka-terminal.exe"
    } else {
        "rikka-terminal"
    };
    let target = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(target_name)))
        .filter(|p| p.exists());
    let Some(target) = target else {
        fail(&format!(
            "{target_name} が rt と同じフォルダに見つかりません"
        ));
        return;
    };
    // Inherit cwd and environment (relative -d paths resolve in the child);
    // spawn and leave — the terminal outlives the launcher.
    if let Err(e) = std::process::Command::new(&target)
        .args(std::env::args_os().skip(1))
        .spawn()
    {
        fail(&format!("起動に失敗しました: {e}"));
    }
}
