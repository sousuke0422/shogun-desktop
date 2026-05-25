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
