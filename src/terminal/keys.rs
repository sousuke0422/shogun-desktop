pub fn key_to_bytes(keystroke: &gpui::Keystroke) -> Vec<u8> {
    let ctrl = keystroke.modifiers.control;
    match keystroke.key.as_str() {
        "enter" => b"\r".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "tab" => b"\t".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        k if ctrl && k.len() == 1 => {
            let ch = k.chars().next().unwrap().to_ascii_lowercase() as u8;
            if ch >= b'a' && ch <= b'z' {
                vec![ch - b'a' + 1]
            } else {
                k.as_bytes().to_vec()
            }
        }
        k => k.as_bytes().to_vec(),
    }
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

    fn ks_ctrl(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key_char: None,
        }
    }

    #[test]
    fn enter_maps_to_cr() {
        assert_eq!(key_to_bytes(&ks("enter")), b"\r");
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
    fn page_keys_map_to_ansi_sequences() {
        assert_eq!(key_to_bytes(&ks("pageup")), b"\x1b[5~");
        assert_eq!(key_to_bytes(&ks("pagedown")), b"\x1b[6~");
    }

    #[test]
    fn ctrl_letter_maps_to_control_codes() {
        assert_eq!(key_to_bytes(&ks_ctrl("a")), b"\x01");
        assert_eq!(key_to_bytes(&ks_ctrl("c")), b"\x03");
        assert_eq!(key_to_bytes(&ks_ctrl("z")), b"\x1a");
    }

    #[test]
    fn plain_char_passes_through() {
        assert_eq!(key_to_bytes(&ks("x")), b"x");
    }
}
