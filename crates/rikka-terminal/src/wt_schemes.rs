//! Windows Terminal color-scheme compatibility. Same spirit as
//! `wt_profiles`: wt is the source of truth, we read its JSON rather than
//! ask the user to re-enter a palette. A scheme is looked up BY NAME across
//! the places wt itself reads them from:
//!
//! 1. the user's `settings.json` `"schemes": [ … ]`, and
//! 2. fragment extensions — `…/Microsoft/Windows Terminal/Fragments/<app>/
//!    *.json` under both `%LOCALAPPDATA%` and `%ProgramData%`, each of which
//!    may carry its own `"schemes"`. This is where Ubuntu (its WSL app)
//!    drops the "Ubuntu" scheme, which is the motivating case.
//!
//! wt's built-in schemes (Campbell, One Half Dark, …) live compiled inside
//! wt, not in any file, so they resolve only if the user also has them in
//! settings.json. Everything file-backed — including every vendor fragment —
//! works.

use rikka_terminal_core::theme::{Palette, Rgb};
use serde::Deserialize;

/// A Windows Terminal color scheme. wt names color 5 `purple` (not magenta)
/// and uses `#RRGGBB` hex; every field is optional so a partial scheme still
/// parses (missing entries fall back to the built-in palette).
#[derive(Debug, Clone, Deserialize)]
struct WtScheme {
    name: String,
    background: Option<String>,
    foreground: Option<String>,
    #[serde(rename = "selectionBackground")]
    selection_background: Option<String>,
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    purple: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
    #[serde(rename = "brightBlack")]
    bright_black: Option<String>,
    #[serde(rename = "brightRed")]
    bright_red: Option<String>,
    #[serde(rename = "brightGreen")]
    bright_green: Option<String>,
    #[serde(rename = "brightYellow")]
    bright_yellow: Option<String>,
    #[serde(rename = "brightBlue")]
    bright_blue: Option<String>,
    #[serde(rename = "brightPurple")]
    bright_purple: Option<String>,
    #[serde(rename = "brightCyan")]
    bright_cyan: Option<String>,
    #[serde(rename = "brightWhite")]
    bright_white: Option<String>,
}

/// `#RRGGBB` (or `RRGGBB`) → `Rgb`. `None` for anything else.
fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl WtScheme {
    /// Fold this scheme onto `base`, keeping the base value wherever the
    /// scheme omits (or malforms) an entry.
    fn apply_onto(&self, mut base: Palette) -> Palette {
        let set = |slot: &mut Rgb, v: &Option<String>| {
            if let Some(rgb) = v.as_deref().and_then(parse_hex) {
                *slot = rgb;
            }
        };
        set(&mut base.background, &self.background);
        set(&mut base.foreground, &self.foreground);
        set(&mut base.selection, &self.selection_background);
        let ansi = [
            &self.black,
            &self.red,
            &self.green,
            &self.yellow,
            &self.blue,
            &self.purple,
            &self.cyan,
            &self.white,
            &self.bright_black,
            &self.bright_red,
            &self.bright_green,
            &self.bright_yellow,
            &self.bright_blue,
            &self.bright_purple,
            &self.bright_cyan,
            &self.bright_white,
        ];
        for (slot, v) in base.ansi.iter_mut().zip(ansi) {
            set(slot, v);
        }
        base
    }
}

/// Every `schemes[]` entry found in a wt settings/fragment JSON blob.
fn schemes_in(raw: &str) -> Vec<WtScheme> {
    let stripped = crate::wt_profiles::strip_jsonc(raw);
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&stripped) else {
        return Vec::new();
    };
    root.get("schemes")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<WtScheme>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Fragment directories wt scans for extension schemes (both install scopes).
#[cfg(windows)]
fn fragment_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    let mut push = |base: Option<std::ffi::OsString>| {
        if let Some(b) = base {
            v.push(std::path::PathBuf::from(b).join("Microsoft/Windows Terminal/Fragments"));
        }
    };
    push(std::env::var_os("LOCALAPPDATA"));
    push(std::env::var_os("ProgramData"));
    v
}

#[cfg(not(windows))]
fn fragment_dirs() -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Every JSON file two levels below a fragments root (`<root>/<app>/<file>.json`),
/// matching wt's own one-app-subdir layout.
fn fragment_files() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for root in fragment_dirs() {
        let Ok(apps) = std::fs::read_dir(&root) else {
            continue;
        };
        for app in apps.flatten() {
            let Ok(entries) = std::fs::read_dir(app.path()) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
                {
                    files.push(p);
                }
            }
        }
    }
    files
}

/// Resolve a wt color scheme by name and fold it onto `base`. Sources, in
/// precedence order (first match wins): the user's settings.json, then
/// fragment files. `None` when no scheme of that name exists anywhere.
pub fn palette_for(name: &str, base: Palette) -> Option<Palette> {
    let from_settings = crate::wt_profiles::settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|raw| schemes_in(&raw))
        .unwrap_or_default();
    let from_fragments = fragment_files()
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .flat_map(|raw| schemes_in(&raw));

    from_settings
        .into_iter()
        .chain(from_fragments)
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .map(|s| s.apply_onto(base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rikka_terminal_core::theme;

    #[test]
    fn parses_wt_hex() {
        assert_eq!(parse_hex("#300A24"), Some(Rgb::new(0x30, 0x0a, 0x24)));
        assert_eq!(parse_hex("300A24"), Some(Rgb::new(0x30, 0x0a, 0x24)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("nope"), None);
    }

    #[test]
    fn scheme_folds_onto_base_and_keeps_gaps() {
        // A partial Ubuntu-shaped scheme: bg/fg + a couple ANSI, purple→magenta.
        let raw = r##"{
            "schemes": [{
                "name": "Ubuntu",
                "background": "#300A24",
                "foreground": "#EEEEEC",
                "red": "#CC0000",
                "purple": "#75507B"
            }]
        }"##;
        let schemes = schemes_in(raw);
        assert_eq!(schemes.len(), 1);
        let p = schemes[0].apply_onto(theme::DEFAULT);
        assert_eq!(p.background, Rgb::new(0x30, 0x0a, 0x24));
        assert_eq!(p.foreground, Rgb::new(0xEE, 0xEE, 0xEC));
        assert_eq!(p.ansi[1], Rgb::new(0xCC, 0x00, 0x00)); // red set
        assert_eq!(p.ansi[5], Rgb::new(0x75, 0x50, 0x7B)); // purple → magenta slot
        // An unset entry keeps the built-in default.
        assert_eq!(p.ansi[2], theme::DEFAULT.ansi[2]); // green untouched
        assert_eq!(p.selection, theme::DEFAULT.selection);
    }

    #[test]
    fn name_match_is_case_insensitive_via_find() {
        let raw = r#"{"schemes":[{"name":"Solarized Dark"}]}"#;
        let s = schemes_in(raw);
        assert!(
            s.iter()
                .any(|x| x.name.eq_ignore_ascii_case("solarized dark"))
        );
    }
}
