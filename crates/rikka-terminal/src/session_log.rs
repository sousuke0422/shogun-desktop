//! Tera Term-style session logging: the raw PTY output stream teed into a
//! timestamped file, toggled per tab with Ctrl+Shift+L (● in the tab while
//! recording). Output is logged verbatim — escape sequences included — so
//! the file replays faithfully with `type`/`cat` into a terminal.
//!
//! Keystrokes are a SEPARATE `.input.log` file and strictly opt-in
//! (`[logging] log_input`): an input log records typed passwords.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use rikka_terminal_core::TerminalSession;

use crate::config::LoggingSection;

static CFG: RwLock<Option<LoggingSection>> = RwLock::new(None);

/// Stash the `[logging]` config — at startup AND on config hot-reload
/// (the RwLock, not a OnceLock, exists for the reload).
pub fn init(cfg: LoggingSection) {
    *CFG.write().unwrap() = Some(cfg);
}

fn config() -> LoggingSection {
    CFG.read().unwrap().clone().unwrap_or_default()
}

/// Ctrl+Shift+L: stop when recording, start otherwise. Outcome goes to the
/// log — the tab's ● is the user-facing signal.
pub fn toggle(session: &TerminalSession) {
    if session.logging_active() {
        session.set_logging(None, None);
        log::info!("session logging stopped");
        return;
    }
    match start(session) {
        Ok(path) => log::info!("session logging started: {}", path.display()),
        Err(e) => log::warn!("session logging failed to start: {e}"),
    }
}

/// `[logging] auto_start = true`: begin logging the moment a tab is born.
/// Called from `hub::new_tab` — the single funnel every tab passes through
/// (local spawns, handoffs, adopted moves; a moved tab continues into a
/// fresh file on the receiving side).
pub fn auto_start(session: &TerminalSession) {
    if config().auto_start.unwrap_or(false) && !session.logging_active() {
        if let Err(e) = start(session) {
            log::warn!("session auto-logging failed to start: {e}");
        }
    }
}

fn start(session: &TerminalSession) -> io::Result<PathBuf> {
    let cfg = config();
    let dir = cfg
        .directory
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_dir);
    std::fs::create_dir_all(&dir)?;
    let stamp = timestamp();
    let (path, output) = open_unique(&dir, &stamp, "log")?;
    let input = if cfg.log_input.unwrap_or(false) {
        Some(open_unique(&dir, &stamp, "input.log")?.1)
    } else {
        None
    };
    session.set_logging(Some(output), input);
    Ok(path)
}

/// Default save dir: `~/Documents/rikka-terminal-logs` (Documents so the
/// files are findable without knowing the config; the folder is only
/// created when logging actually starts).
fn default_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let docs = home.join("Documents");
    if docs.is_dir() { docs } else { home }.join("rikka-terminal-logs")
}

/// `create_new` with a `-N` suffix retry: two tabs toggled within the same
/// second must land in separate files, never interleave into one.
fn open_unique(dir: &Path, stamp: &str, ext: &str) -> io::Result<(PathBuf, File)> {
    for n in 0..100 {
        let name = if n == 0 {
            format!("rikka_{stamp}.{ext}")
        } else {
            format!("rikka_{stamp}-{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free log filename",
    ))
}

/// Local wall-clock `YYYYMMDD_HHMMSS` for the filename.
#[cfg(windows)]
fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// UTC fallback for non-Windows builds (no date/time dependency in-tree;
/// Howard Hinnant's civil-from-days).
#[cfg(not(windows))]
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    let z = (secs / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names_never_collide() {
        let dir = std::env::temp_dir().join(format!("rikka-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, _) = open_unique(&dir, "20260101_000000", "log").unwrap();
        let (b, _) = open_unique(&dir, "20260101_000000", "log").unwrap();
        assert_ne!(a, b);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timestamp_is_datetime_shaped() {
        let t = timestamp();
        assert_eq!(t.len(), 15, "{t}");
        assert_eq!(t.as_bytes()[8], b'_', "{t}");
    }
}
