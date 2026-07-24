//! Key-down → PTY bytes.
//!
//! Two encodings, selected by the terminal's mode bits:
//!
//! - **Legacy** (default): classic xterm sequences, plus the standard
//!   `CSI 1;{mods}X` / `CSI {n};{mods}~` forms for modified functional keys
//!   and ESC-prefixing for Alt.
//! - **Kitty keyboard protocol**: when the running application pushed the
//!   progressive-enhancement flags (`CSI > flags u` — tracked by the vendored
//!   alacritty_terminal, enabled via `Config::kitty_keyboard`). Implemented
//!   flags: *disambiguate escape codes* (1) and *report all keys as escape
//!   codes* (8). *Report event types* (2) is tolerated but key-up/repeat
//!   marks are not emitted (only key-downs reach this path); *alternate keys*
//!   (4) and *associated text* (16) are not emitted — all four are legal to
//!   omit, applications must treat them as best-effort.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Key-down → PTY bytes, with the text-input-path guard shared by every
/// terminal window.
///
/// Returns `None` when the key must be left to the platform text-input path:
/// printable keys without ctrl/alt/cmd also arrive as WM_CHAR / IME commits
/// and are delivered to the registered input handler
/// (`replace_text_in_range`) — sending them here as well would double every
/// character. Also `None` for unmapped keys (nothing to send).
///
/// `Some(bytes)` means the caller should send the bytes and stop propagation
/// so GPUI's own actions (tab focus-cycling etc.) never see the key —
/// stopping propagation also suppresses the platform WM_CHAR, so a key is
/// consumed by exactly one of the two paths.
pub fn key_to_pty_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC) {
        return kitty_key_bytes(keystroke, mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC));
    }
    if printable_text(keystroke).is_some() {
        return None;
    }
    let bytes = key_to_bytes(keystroke);
    (!bytes.is_empty()).then_some(bytes)
}

/// The keystroke's plain-text spelling when it is a printable, unmodified
/// key — exactly what the WM_CHAR path would deliver. The single source
/// of the "printable" judgement [`key_to_pty_bytes`] defers on.
pub fn printable_text(keystroke: &Keystroke) -> Option<&str> {
    let m = &keystroke.modifiers;
    if m.control || m.alt || m.platform {
        return None;
    }
    keystroke
        .key_char
        .as_deref()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
}

/// Per-recipient byte spellings of one keystroke for a broadcast: each
/// target's own terminal mode picks its encoding (kitty CSI u vs legacy),
/// so a mixed-mode fan-out never receives another pane's spelling. `None`
/// = no target's mode consumes the key — it must keep propagating so the
/// platform WM_CHAR path delivers the text (the caller must NOT stop
/// propagation). `Some(plans)` = consumed; entries that are `None` inside
/// want [`printable_text`] delivered inline, because stopping propagation
/// suppresses WM_CHAR for every target at once.
pub fn key_delivery_plans(
    keystroke: &Keystroke,
    modes: &[TermMode],
) -> Option<Vec<Option<Vec<u8>>>> {
    let plans: Vec<Option<Vec<u8>>> = modes
        .iter()
        .map(|mode| key_to_pty_bytes(keystroke, *mode))
        .collect();
    plans.iter().any(Option::is_some).then_some(plans)
}

/// Legacy modifier parameter: `1 + bitmask` (shift=1, alt=2, ctrl=4,
/// super=8), or 0 when unmodified (the parameter is omitted entirely).
fn legacy_mods(m: &gpui::Modifiers) -> u32 {
    let mask = (m.shift as u32)
        | ((m.alt as u32) << 1)
        | ((m.control as u32) << 2)
        | ((m.platform as u32) << 3);
    if mask == 0 { 0 } else { mask + 1 }
}

pub fn key_to_bytes(keystroke: &Keystroke) -> Vec<u8> {
    let m = &keystroke.modifiers;
    let ctrl = m.control;
    let shift = m.shift;
    let mods = legacy_mods(m);
    // `CSI 1;{mods}X` home/end/arrow form (xterm modifyCursorKeys=2 default).
    let csi_letter = |c: char| {
        if mods == 0 {
            format!("\x1b[{c}").into_bytes()
        } else {
            format!("\x1b[1;{mods}{c}").into_bytes()
        }
    };
    // `CSI {n};{mods}~` tilde form (insert/delete/page/F5+).
    let csi_tilde = |n: u32| {
        if mods == 0 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{mods}~").into_bytes()
        }
    };
    // F1-F4: SS3 when plain, CSI 1;{mods}X when modified (xterm behavior).
    let ss3_or_csi = |c: char| {
        if mods == 0 {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[1;{mods}{c}").into_bytes()
        }
    };
    match keystroke.key.as_str() {
        // Alt = ESC prefix (the traditional "meta" encoding), the same
        // convention the single-char arm below already uses: Alt+Enter → ESC
        // CR, Alt+Backspace → ESC DEL (readline backward-kill-word),
        // Alt+Escape → ESC ESC. Without this the fixed-byte control keys drop
        // Alt entirely, so an app that binds Alt+Enter (e.g. inserting a soft
        // newline) can't tell it apart from a bare Enter.
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "escape" if m.alt => b"\x1b\x1b".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        // Back-tab: Shift+Tab must send CSI Z (used by Claude Code's
        // shift+tab mode cycling), not a plain tab.
        "tab" if shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "space" => b" ".to_vec(),
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => csi_tilde(2),
        "delete" => csi_tilde(3),
        "pageup" => csi_tilde(5),
        "pagedown" => csi_tilde(6),
        "f1" => ss3_or_csi('P'),
        "f2" => ss3_or_csi('Q'),
        "f3" => ss3_or_csi('R'),
        "f4" => ss3_or_csi('S'),
        "f5" => csi_tilde(15),
        "f6" => csi_tilde(17),
        "f7" => csi_tilde(18),
        "f8" => csi_tilde(19),
        "f9" => csi_tilde(20),
        "f10" => csi_tilde(21),
        "f11" => csi_tilde(23),
        "f12" => csi_tilde(24),
        k if k.chars().count() == 1 => {
            // Alt = ESC prefix (works with plain chars AND ctrl codes:
            // alt+x → ESC x, ctrl+alt+c → ESC 0x03).
            let mut out = Vec::new();
            if m.alt {
                out.push(0x1b);
            }
            if ctrl {
                let ch = k.chars().next().unwrap().to_ascii_lowercase() as u8;
                if ch.is_ascii_lowercase() {
                    out.push(ch - b'a' + 1);
                } else {
                    out.extend_from_slice(k.as_bytes());
                }
            } else {
                out.extend_from_slice(k.as_bytes());
            }
            out
        }
        // Multi-char key NAMES ("capslock", …) must never be sent as literal
        // text (pressing space used to type "space"). Fall back to the text
        // the key would insert, if any; otherwise swallow the key.
        _ => keystroke
            .key_char
            .as_ref()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Kitty keyboard protocol
// ─────────────────────────────────────────────────────────────────────────────

/// Kitty modifier parameter: always `1 + bitmask`, never omitted logically
/// (a bare `1` IS omitted in the serialization, per spec).
fn kitty_mods(m: &gpui::Modifiers) -> u32 {
    1 + ((m.shift as u32)
        | ((m.alt as u32) << 1)
        | ((m.control as u32) << 2)
        | ((m.platform as u32) << 3))
}

/// Encode a key under the kitty protocol.
///
/// `all_keys` = the REPORT_ALL_KEYS_AS_ESC flag (8). Without it (flag 1,
/// disambiguate only), plain text keys return `None` and stay on the
/// platform text-input path, and unmodified Enter/Tab/Backspace keep their
/// legacy bytes — the spec carves those out so `reset` can still be typed
/// after a crashed program leaves the mode on.
fn kitty_key_bytes(keystroke: &Keystroke, all_keys: bool) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let mods = kitty_mods(m);
    let csi_u = |cp: u32| {
        if mods == 1 {
            format!("\x1b[{cp}u").into_bytes()
        } else {
            format!("\x1b[{cp};{mods}u").into_bytes()
        }
    };
    // Functional keys keep their legacy CSI forms in the kitty protocol
    // (with the mods parameter attached when modified).
    let csi_letter = |c: char| {
        if mods == 1 {
            format!("\x1b[{c}").into_bytes()
        } else {
            format!("\x1b[1;{mods}{c}").into_bytes()
        }
    };
    let csi_tilde = |n: u32| {
        if mods == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{mods}~").into_bytes()
        }
    };
    let ss3_or_csi = |c: char| {
        if mods == 1 {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[1;{mods}{c}").into_bytes()
        }
    };
    let out = match keystroke.key.as_str() {
        // The flagship disambiguation: Esc gets its own unambiguous code.
        "escape" => csi_u(27),
        // Enter/Tab/Backspace: legacy bytes while unmodified in
        // disambiguate-only mode (spec exception), CSI u otherwise.
        "enter" if !all_keys && mods == 1 => b"\r".to_vec(),
        "enter" => csi_u(13),
        "tab" if !all_keys && mods == 1 => b"\t".to_vec(),
        "tab" => csi_u(9), // shift-tab = CSI 9;2u here, CSI Z is legacy-only
        "backspace" if !all_keys && mods == 1 => b"\x7f".to_vec(),
        "backspace" => csi_u(127),
        // Space is text while it can be; CSI u once modified or in all-keys.
        "space" if !all_keys && mods <= 2 => return None,
        "space" => csi_u(32),
        "up" => csi_letter('A'),
        "down" => csi_letter('B'),
        "right" => csi_letter('C'),
        "left" => csi_letter('D'),
        "home" => csi_letter('H'),
        "end" => csi_letter('F'),
        "insert" => csi_tilde(2),
        "delete" => csi_tilde(3),
        "pageup" => csi_tilde(5),
        "pagedown" => csi_tilde(6),
        "f1" => ss3_or_csi('P'),
        "f2" => ss3_or_csi('Q'),
        "f3" => ss3_or_csi('R'),
        "f4" => ss3_or_csi('S'),
        "f5" => csi_tilde(15),
        "f6" => csi_tilde(17),
        "f7" => csi_tilde(18),
        "f8" => csi_tilde(19),
        "f9" => csi_tilde(20),
        "f10" => csi_tilde(21),
        "f11" => csi_tilde(23),
        "f12" => csi_tilde(24),
        k if k.chars().count() == 1 => {
            // gpui reports the unshifted lowercase key ("A" arrives as key
            // "a" + shift), which is exactly the kitty base-layout codepoint.
            let cp = k.chars().next().unwrap().to_ascii_lowercase() as u32;
            if !all_keys && !m.control && !m.alt && !m.platform {
                // Plain / shift-only text: stays on the text-input path.
                return None;
            }
            csi_u(cp)
        }
        // Bare modifiers, capslock, etc. — kitty has codes for these under
        // all-keys mode (57441+) but nothing requires emitting them.
        _ => return None,
    };
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn ks(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            modifiers: Modifiers::default(),
            key_char: None,
        }
    }

    fn ks_mod(key: &str, ctrl: bool, alt: bool, shift: bool) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            modifiers: Modifiers {
                control: ctrl,
                alt,
                shift,
                ..Default::default()
            },
            key_char: None,
        }
    }

    fn ks_ctrl(key: &str) -> Keystroke {
        ks_mod(key, true, false, false)
    }

    #[test]
    fn enter_maps_to_cr() {
        assert_eq!(key_to_bytes(&ks("enter")), b"\r");
    }

    #[test]
    fn delivery_plans_encode_per_target_mode() {
        // Enter across a kitty-mode pane and a legacy pane: each target
        // gets ITS OWN spelling — never the focused pane's.
        let kitty = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::DISAMBIGUATE_ESC_CODES;
        let plans = key_delivery_plans(&ks("enter"), &[kitty, TermMode::empty()]).unwrap();
        assert_eq!(plans[0].as_deref().unwrap(), b"\x1b[13u");
        assert_eq!(plans[1].as_deref().unwrap(), b"\r");
    }

    #[test]
    fn delivery_plans_printable_mixed_and_unconsumed() {
        let a = Keystroke {
            key: "a".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("a".to_string()),
        };
        // All-legacy printable: nobody consumes — the WM_CHAR path owns it.
        assert!(key_delivery_plans(&a, &[TermMode::empty(), TermMode::empty()]).is_none());
        // Kitty + legacy: consumed (kitty spells it CSI u); the legacy
        // target's None slot asks for the printable text inline.
        let kitty = TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::DISAMBIGUATE_ESC_CODES;
        let plans = key_delivery_plans(&a, &[kitty, TermMode::empty()]).unwrap();
        assert!(plans[0].is_some());
        assert!(plans[1].is_none());
        assert_eq!(printable_text(&a), Some("a"));
    }

    #[test]
    fn escape_maps_to_esc() {
        assert_eq!(key_to_bytes(&ks("escape")), b"\x1b");
    }

    #[test]
    fn backspace_maps_to_del() {
        assert_eq!(key_to_bytes(&ks("backspace")), b"\x7f");
    }

    #[test]
    fn tab_maps_to_tab() {
        assert_eq!(key_to_bytes(&ks("tab")), b"\t");
    }

    #[test]
    fn arrow_keys_map_to_ansi_sequences() {
        assert_eq!(key_to_bytes(&ks("up")), b"\x1b[A");
        assert_eq!(key_to_bytes(&ks("down")), b"\x1b[B");
        assert_eq!(key_to_bytes(&ks("right")), b"\x1b[C");
        assert_eq!(key_to_bytes(&ks("left")), b"\x1b[D");
    }

    #[test]
    fn modified_arrows_use_csi_1_mods_form() {
        // ctrl+right = CSI 1;5C (word jump in shells)
        assert_eq!(
            key_to_bytes(&ks_mod("right", true, false, false)),
            b"\x1b[1;5C"
        );
        // shift+up = CSI 1;2A
        assert_eq!(
            key_to_bytes(&ks_mod("up", false, false, true)),
            b"\x1b[1;2A"
        );
        // ctrl+shift+left = CSI 1;6D
        assert_eq!(
            key_to_bytes(&ks_mod("left", true, false, true)),
            b"\x1b[1;6D"
        );
    }

    #[test]
    fn modified_home_end_and_tilde_keys() {
        assert_eq!(
            key_to_bytes(&ks_mod("home", true, false, false)),
            b"\x1b[1;5H"
        );
        assert_eq!(
            key_to_bytes(&ks_mod("delete", false, false, true)),
            b"\x1b[3;2~"
        );
        assert_eq!(
            key_to_bytes(&ks_mod("pageup", true, false, false)),
            b"\x1b[5;5~"
        );
    }

    #[test]
    fn page_keys_map_to_ansi_sequences() {
        assert_eq!(key_to_bytes(&ks("pageup")), b"\x1b[5~");
        assert_eq!(key_to_bytes(&ks("pagedown")), b"\x1b[6~");
    }

    #[test]
    fn end_maps_to_csi_f() {
        assert_eq!(key_to_bytes(&ks("end")), b"\x1b[F");
    }

    #[test]
    fn insert_maps_to_tilde_2() {
        assert_eq!(key_to_bytes(&ks("insert")), b"\x1b[2~");
    }

    #[test]
    fn function_keys_map_to_xterm_sequences() {
        assert_eq!(key_to_bytes(&ks("f1")), b"\x1bOP");
        assert_eq!(key_to_bytes(&ks("f4")), b"\x1bOS");
        assert_eq!(key_to_bytes(&ks("f5")), b"\x1b[15~");
        assert_eq!(key_to_bytes(&ks("f12")), b"\x1b[24~");
        // Modified F-keys switch to the CSI form.
        assert_eq!(
            key_to_bytes(&ks_mod("f1", true, false, false)),
            b"\x1b[1;5P"
        );
        assert_eq!(
            key_to_bytes(&ks_mod("f5", false, false, true)),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn ctrl_letter_maps_to_control_codes() {
        assert_eq!(key_to_bytes(&ks_ctrl("a")), b"\x01");
        assert_eq!(key_to_bytes(&ks_ctrl("c")), b"\x03");
        assert_eq!(key_to_bytes(&ks_ctrl("z")), b"\x1a");
    }

    #[test]
    fn alt_prefixes_esc() {
        assert_eq!(key_to_bytes(&ks_mod("x", false, true, false)), b"\x1bx");
        // ctrl+alt+c = ESC + 0x03
        assert_eq!(key_to_bytes(&ks_mod("c", true, true, false)), b"\x1b\x03");
    }

    #[test]
    fn alt_control_keys_esc_prefix() {
        // The fixed-byte control keys must ESC-prefix under Alt too, so apps
        // that bind Alt+Enter / Alt+Backspace can tell them from the plain key.
        assert_eq!(
            key_to_bytes(&ks_mod("enter", false, true, false)),
            b"\x1b\r"
        );
        assert_eq!(
            key_to_bytes(&ks_mod("backspace", false, true, false)),
            b"\x1b\x7f"
        );
        assert_eq!(
            key_to_bytes(&ks_mod("escape", false, true, false)),
            b"\x1b\x1b"
        );
        // Unmodified stays bare.
        assert_eq!(key_to_bytes(&ks("enter")), b"\r");
    }

    #[test]
    fn kitty_alt_enter_is_csi_u_13() {
        // Under the kitty protocol Alt+Enter is CSI 13;3u (mods = 1 + alt bit).
        assert_eq!(
            kitty_key_bytes(&ks_mod("enter", false, true, false), false),
            Some(b"\x1b[13;3u".to_vec())
        );
        // Plain Enter keeps its legacy CR in disambiguate-only mode.
        assert_eq!(kitty_key_bytes(&ks("enter"), false), Some(b"\r".to_vec()));
    }

    #[test]
    fn plain_char_passes_through() {
        assert_eq!(key_to_bytes(&ks("x")), b"x");
    }

    #[test]
    fn space_maps_to_space_char() {
        assert_eq!(key_to_bytes(&ks("space")), b" ");
    }

    #[test]
    fn shift_tab_maps_to_backtab() {
        assert_eq!(key_to_bytes(&ks_mod("tab", false, false, true)), b"\x1b[Z");
    }

    #[test]
    fn delete_and_home_map_to_ansi_sequences() {
        assert_eq!(key_to_bytes(&ks("delete")), b"\x1b[3~");
        assert_eq!(key_to_bytes(&ks("home")), b"\x1b[H");
    }

    #[test]
    fn unknown_named_key_is_swallowed() {
        // Named keys with no insert-text must not be sent as literal text.
        assert_eq!(key_to_bytes(&ks("capslock")), b"");
    }

    #[test]
    fn unknown_named_key_falls_back_to_key_char() {
        let keystroke = Keystroke {
            key: "somekey".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("@".to_string()),
        };
        assert_eq!(key_to_bytes(&keystroke), b"@");
    }

    #[test]
    fn pty_bytes_leaves_printable_keys_to_text_input_path() {
        // "a" typed without modifiers arrives via WM_CHAR too — must not be
        // sent from the key-down path or every character doubles.
        let keystroke = Keystroke {
            key: "a".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("a".to_string()),
        };
        assert_eq!(key_to_pty_bytes(&keystroke, TermMode::empty()), None);
    }

    #[test]
    fn pty_bytes_sends_control_and_named_keys() {
        assert_eq!(
            key_to_pty_bytes(&ks_ctrl("c"), TermMode::empty()),
            Some(b"\x03".to_vec())
        );
        // "enter" has a control key_char ("\r") → not a printable text key.
        let enter = Keystroke {
            key: "enter".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("\r".to_string()),
        };
        assert_eq!(
            key_to_pty_bytes(&enter, TermMode::empty()),
            Some(b"\r".to_vec())
        );
    }

    #[test]
    fn pty_bytes_swallows_unmapped_named_keys() {
        assert_eq!(key_to_pty_bytes(&ks("capslock"), TermMode::empty()), None);
    }

    // ── kitty protocol ───────────────────────────────────────────────────────

    const DISAMBIGUATE: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;

    fn all_keys_mode() -> TermMode {
        TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC
    }

    #[test]
    fn kitty_disambiguate_escape_is_csi_27_u() {
        assert_eq!(
            key_to_pty_bytes(&ks("escape"), DISAMBIGUATE),
            Some(b"\x1b[27u".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_keeps_unmodified_enter_tab_backspace_legacy() {
        // Spec exception: `reset` must remain typable after a crash.
        let enter = Keystroke {
            key: "enter".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("\r".to_string()),
        };
        assert_eq!(key_to_pty_bytes(&enter, DISAMBIGUATE), Some(b"\r".to_vec()));
        assert_eq!(
            key_to_pty_bytes(&ks("backspace"), DISAMBIGUATE),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks("tab"), DISAMBIGUATE),
            Some(b"\t".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_ctrl_letter_is_csi_u() {
        // ctrl+c = CSI 99;5u — the disambiguation that lets apps tell
        // ctrl+c from ctrl+shift+c (and from SIGINT-as-typed).
        assert_eq!(
            key_to_pty_bytes(&ks_ctrl("c"), DISAMBIGUATE),
            Some(b"\x1b[99;5u".to_vec())
        );
        // alt+x = CSI 120;3u
        assert_eq!(
            key_to_pty_bytes(&ks_mod("x", false, true, false), DISAMBIGUATE),
            Some(b"\x1b[120;3u".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_leaves_plain_text_to_input_path() {
        let a = Keystroke {
            key: "a".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("a".to_string()),
        };
        assert_eq!(key_to_pty_bytes(&a, DISAMBIGUATE), None);
        // shift-only is still text ("A" via WM_CHAR).
        assert_eq!(
            key_to_pty_bytes(&ks_mod("a", false, false, true), DISAMBIGUATE),
            None
        );
    }

    #[test]
    fn kitty_functional_keys_keep_csi_forms_with_mods() {
        assert_eq!(
            key_to_pty_bytes(&ks("up"), DISAMBIGUATE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks_mod("up", true, false, false), DISAMBIGUATE),
            Some(b"\x1b[1;5A".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks_mod("delete", false, false, true), DISAMBIGUATE),
            Some(b"\x1b[3;2~".to_vec())
        );
    }

    #[test]
    fn kitty_all_keys_reports_everything_as_csi_u() {
        let a = Keystroke {
            key: "a".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("a".to_string()),
        };
        assert_eq!(
            key_to_pty_bytes(&a, all_keys_mode()),
            Some(b"\x1b[97u".to_vec())
        );
        let enter = Keystroke {
            key: "enter".to_string(),
            modifiers: Modifiers::default(),
            key_char: Some("\r".to_string()),
        };
        assert_eq!(
            key_to_pty_bytes(&enter, all_keys_mode()),
            Some(b"\x1b[13u".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks("space"), all_keys_mode()),
            Some(b"\x1b[32u".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks("backspace"), all_keys_mode()),
            Some(b"\x1b[127u".to_vec())
        );
    }

    #[test]
    fn kitty_shift_tab_is_csi_u_not_backtab() {
        // CSI Z is the legacy encoding; kitty-aware apps expect 9;2u.
        assert_eq!(
            key_to_pty_bytes(&ks_mod("tab", false, false, true), DISAMBIGUATE),
            Some(b"\x1b[9;2u".to_vec())
        );
        assert_eq!(
            key_to_pty_bytes(&ks_mod("tab", false, false, true), all_keys_mode()),
            Some(b"\x1b[9;2u".to_vec())
        );
    }

    #[test]
    fn kitty_bare_modifier_keys_are_swallowed() {
        assert_eq!(key_to_pty_bytes(&ks("capslock"), all_keys_mode()), None);
    }
}
