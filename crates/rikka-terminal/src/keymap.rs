//! Configurable tab-management chords — `[keys]` in config.toml. v1 covers
//! the Ctrl+Shift family; the hardwired synonyms stay fixed (Ctrl+M merge
//! where the OS delivers it, Ctrl+Tab/PageUp/PageDown cycling, Ctrl/Shift+
//! Insert copy/paste, Shift+PageUp/PageDown scrollback paging).
//!
//! A chord string is `"mod+mod+key"` — mods from `ctrl`/`shift`/`alt`, the
//! key being gpui's lowercase key name (`t`, `tab`, `f5`, …). An invalid
//! string keeps that action's default and logs a warning, so a typo can
//! never lock the user out of their tabs.

use std::sync::RwLock;

use gpui::Modifiers;

use crate::config::KeysSection;

/// Everything a `[keys]` chord can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    /// Detach into a new window of this process.
    DetachTab,
    /// Detach into its own OS process (Windows; falls back to DetachTab).
    EjectTab,
    /// Move into another window process (Windows).
    MoveTab,
    MergeAll,
    ToggleLogging,
    Copy,
    Paste,
    CycleBack,
    /// Scroll to the previous shell prompt (OSC 133 marks).
    JumpPromptPrev,
    /// Scroll to the next shell prompt.
    JumpPromptNext,
    /// Open/close the scrollback search bar.
    Search,
    OpenSettings,
}

/// One parsed binding: exact modifier set + gpui key name (lowercase).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chord {
    control: bool,
    shift: bool,
    alt: bool,
    key: String,
}

impl Chord {
    /// `"ctrl+shift+t"` → chord. `None` = unparsable (unknown modifier,
    /// missing key).
    fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord {
            control: false,
            shift: false,
            alt: false,
            key: String::new(),
        };
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let (mods, key) = parts.split_at(parts.len().checked_sub(1)?);
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.control = true,
                "shift" => c.shift = true,
                "alt" => c.alt = true,
                _ => return None,
            }
        }
        c.key = key.first()?.to_ascii_lowercase();
        // gpui key names are single lowercase tokens ("t", "tab", "f5") —
        // whitespace means the string was never a chord at all.
        if c.key.is_empty() || c.key.contains(char::is_whitespace) {
            return None;
        }
        Some(c)
    }

    fn matches(&self, m: &Modifiers, key: &str) -> bool {
        self.control == m.control && self.shift == m.shift && self.alt == m.alt && self.key == key
    }
}

pub struct KeyMap {
    bindings: Vec<(Chord, Action)>,
}

impl KeyMap {
    /// The defaults (today's hardcoded chords), each overridable by its
    /// `[keys]` entry.
    fn from_config(keys: &KeysSection) -> KeyMap {
        let ctrl_shift = |key: &str| Chord {
            control: true,
            shift: true,
            alt: false,
            key: key.into(),
        };
        let defaults: [(&Option<String>, &str, Action); 13] = [
            (&keys.new_tab, "t", Action::NewTab),
            (&keys.close_tab, "w", Action::CloseTab),
            (&keys.detach_tab, "d", Action::DetachTab),
            (&keys.eject_tab, "e", Action::EjectTab),
            (&keys.move_tab, "x", Action::MoveTab),
            (&keys.merge_all, "a", Action::MergeAll),
            (&keys.toggle_logging, "l", Action::ToggleLogging),
            (&keys.copy, "c", Action::Copy),
            (&keys.paste, "v", Action::Paste),
            (&keys.cycle_back, "tab", Action::CycleBack),
            (&keys.jump_prompt_prev, "up", Action::JumpPromptPrev),
            (&keys.jump_prompt_next, "down", Action::JumpPromptNext),
            (&keys.search, "f", Action::Search),
        ];
        let mut bindings = Vec::new();
        for (configured, default_key, action) in defaults {
            let chord = match configured.as_deref().map(Chord::parse) {
                Some(Some(c)) => c,
                Some(None) => {
                    log::warn!(
                        "[keys] unparsable chord {:?} for {action:?} — keeping the default",
                        configured.as_deref().unwrap_or_default()
                    );
                    ctrl_shift(default_key)
                }
                None => ctrl_shift(default_key),
            };
            bindings.push((chord, action));
        }
        // Settings opens with Ctrl+, (VSCode's chord). It cannot live in
        // the Ctrl+Shift family above: gpui-Windows normalizes Shift+comma
        // to key "<" WITH SHIFT EATEN, so a ctrl+shift+"," binding never
        // matches a real keystroke.
        let ctrl_comma = || Chord {
            control: true,
            shift: false,
            alt: false,
            key: ",".into(),
        };
        let settings_chord = match keys.settings.as_deref().map(Chord::parse) {
            Some(Some(c)) => c,
            Some(None) => {
                log::warn!(
                    "[keys] unparsable chord {:?} for OpenSettings — keeping the default",
                    keys.settings.as_deref().unwrap_or_default()
                );
                ctrl_comma()
            }
            None => ctrl_comma(),
        };
        bindings.push((settings_chord, Action::OpenSettings));
        KeyMap { bindings }
    }

    fn resolve(&self, m: &Modifiers, key: &str) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(c, _)| c.matches(m, key))
            .map(|&(_, a)| a)
    }
}

static KEYMAP: RwLock<Option<KeyMap>> = RwLock::new(None);

/// Stash the parsed `[keys]` config — at startup AND on config hot-reload
/// (the RwLock, not a OnceLock, exists for the reload).
pub fn init(keys: &KeysSection) {
    *KEYMAP.write().unwrap() = Some(KeyMap::from_config(keys));
}

/// The action bound to this keystroke, if any. Falls back to the built-in
/// defaults when `init` never ran (tests, early input).
pub fn resolve(m: &Modifiers, key: &str) -> Option<Action> {
    if let Some(map) = KEYMAP.read().unwrap().as_ref() {
        return map.resolve(m, key);
    }
    KeyMap::from_config(&KeysSection::default()).resolve(m, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_chord_opens_settings() {
        // Ctrl+, — NOT Ctrl+Shift+,: gpui-Windows turns Shift+comma into
        // key "<" with shift eaten, so the shifted spelling can never
        // match. VSCode's settings chord, conveniently.
        assert_eq!(
            resolve(&mods(true, false, false), ","),
            Some(Action::OpenSettings)
        );
        assert_eq!(resolve(&mods(true, true, false), ","), None);
    }

    fn mods(control: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            control,
            shift,
            alt,
            ..Default::default()
        }
    }

    #[test]
    fn parses_chord_strings() {
        assert_eq!(
            Chord::parse("ctrl+shift+t"),
            Some(Chord {
                control: true,
                shift: true,
                alt: false,
                key: "t".into()
            })
        );
        assert_eq!(
            Chord::parse("Alt+F5"),
            Some(Chord {
                control: false,
                shift: false,
                alt: true,
                key: "f5".into()
            })
        );
        assert_eq!(Chord::parse("super+t"), None, "unknown modifier");
        assert_eq!(Chord::parse(""), None);
    }

    #[test]
    fn defaults_match_the_hardcoded_chords() {
        let km = KeyMap::from_config(&KeysSection::default());
        assert_eq!(
            km.resolve(&mods(true, true, false), "t"),
            Some(Action::NewTab)
        );
        assert_eq!(
            km.resolve(&mods(true, true, false), "tab"),
            Some(Action::CycleBack)
        );
        assert_eq!(km.resolve(&mods(true, false, false), "t"), None);
        assert_eq!(
            km.resolve(&mods(true, true, true), "t"),
            None,
            "alt must not match"
        );
    }

    #[test]
    fn config_overrides_one_action_and_bad_strings_keep_defaults() {
        let keys = KeysSection {
            new_tab: Some("alt+n".into()),
            close_tab: Some("not a chord ~~~".into()),
            ..Default::default()
        };
        let km = KeyMap::from_config(&keys);
        assert_eq!(
            km.resolve(&mods(false, false, true), "n"),
            Some(Action::NewTab)
        );
        assert_eq!(
            km.resolve(&mods(true, true, false), "t"),
            None,
            "old chord unbound"
        );
        assert_eq!(
            km.resolve(&mods(true, true, false), "w"),
            Some(Action::CloseTab),
            "typo keeps the default"
        );
    }
}
