//! XTVERSION (`CSI > 0 q`) — report terminal name and version.
//!
//! Applications (yazi, notcurses, …) identify the terminal emulator with
//! XTVERSION before deciding which capabilities to use. vte 0.13 has no
//! handler hook for it, so — like OSC 9 progress and the kitty APC — a
//! passive scanner watches the raw PTY stream and the reader thread writes
//! the `DCS > | name version ST` reply back to the PTY.

/// Detects `ESC [ > [0] q` (XTVERSION query). Any other parameter or final
/// byte is ignored (DA2 is `ESC [ > c`, modifyOtherKeys is `ESC [ > 4 ; m`…).
pub struct XtversionScanner {
    state: State,
    /// Precomputed `DCS >| <identity> ST` reply. The identity string comes
    /// from the embedding application (it may be honest or a deliberate
    /// masquerade — e.g. "ghostty x.y.z" so emulator-sniffing apps enable
    /// capabilities this engine actually implements).
    reply: Vec<u8>,
}

#[derive(PartialEq)]
enum State {
    Ground,
    Esc,
    Csi,
    /// After `CSI >`: accumulating parameter digits.
    Private {
        params: u32,
        any_digit: bool,
    },
}

impl XtversionScanner {
    pub fn new(identity: &str) -> Self {
        Self {
            state: State::Ground,
            reply: format!("\x1bP>|{identity}\x1b\\").into_bytes(),
        }
    }

    /// Feed one PTY byte; returns the XTVERSION reply to write back when a
    /// query completes.
    pub fn advance(&mut self, byte: u8) -> Option<Vec<u8>> {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Esc;
                }
                None
            }
            State::Esc => {
                self.state = match byte {
                    b'[' => State::Csi,
                    0x1b => State::Esc,
                    _ => State::Ground,
                };
                None
            }
            State::Csi => {
                self.state = match byte {
                    b'>' => State::Private {
                        params: 0,
                        any_digit: false,
                    },
                    0x1b => State::Esc,
                    _ => State::Ground,
                };
                None
            }
            State::Private { params, any_digit } => match byte {
                b'0'..=b'9' => {
                    self.state = State::Private {
                        params: (params * 10 + u32::from(byte - b'0')).min(9999),
                        any_digit: true,
                    };
                    None
                }
                b'q' => {
                    self.state = State::Ground;
                    // XTVERSION is `CSI > q` or `CSI > 0 q` only.
                    if params == 0 || !any_digit {
                        Some(self.reply.clone())
                    } else {
                        None
                    }
                }
                0x1b => {
                    self.state = State::Esc;
                    None
                }
                _ => {
                    self.state = State::Ground;
                    None
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut s = XtversionScanner::new("shogun-desktop test");
        bytes.iter().filter_map(|&b| s.advance(b)).collect()
    }

    #[test]
    fn responds_to_xtversion_with_and_without_param() {
        for query in [&b"\x1b[>q"[..], &b"\x1b[>0q"[..]] {
            let replies = feed(query);
            assert_eq!(replies.len(), 1, "query {query:?}");
            let text = String::from_utf8(replies[0].clone()).unwrap();
            assert!(text.starts_with("\x1bP>|shogun-desktop test"));
            assert!(text.ends_with("\x1b\\"));
        }
    }

    #[test]
    fn reply_carries_the_configured_identity() {
        let mut s = XtversionScanner::new("ghostty 1.1.3");
        let reply = b"\x1b[>0q".iter().find_map(|&b| s.advance(b)).unwrap();
        assert_eq!(reply, b"\x1bP>|ghostty 1.1.3\x1b\\");
    }

    #[test]
    fn ignores_da2_and_other_private_sequences() {
        assert!(feed(b"\x1b[>c").is_empty()); // DA2
        assert!(feed(b"\x1b[>4;2m").is_empty()); // modifyOtherKeys
        assert!(feed(b"\x1b[>1q").is_empty()); // non-zero param
        assert!(feed(b"\x1b[0q").is_empty()); // not private
        assert!(feed(b"plain text q").is_empty());
    }
}
