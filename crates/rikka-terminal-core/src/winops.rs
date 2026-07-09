//! Passive XTWINOPS side-scanner for `CSI 16 t` — "report character cell
//! size in pixels", answered as `CSI 6 ; height ; width t`.
//!
//! alacritty's handler answers ops 14 (text area px) and 18 (text area
//! cells) but silently drops op 16, and on Windows there is no TIOCGWINSZ,
//! so op 16 is exactly the query image-drawing applications (yazi) use to
//! size sixel rasters: an unanswered query reads as cell size (0,0) and the
//! application skips emitting images entirely. Same passive-observer pattern
//! as the OSC / APC / sixel / XTVERSION scanners.

pub struct WinopsScanner {
    state: State,
}

#[derive(PartialEq)]
enum State {
    Ground,
    Esc,
    /// Inside `CSI`, accumulating bare parameter digits.
    Csi {
        params: u32,
        any_digit: bool,
    },
}

impl Default for WinopsScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl WinopsScanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
        }
    }

    /// Feed one byte; `true` exactly when a bare `CSI 16 t` completes.
    /// Parameters with separators (`CSI 1;6t`) or other finals fall back to
    /// ground — those belong to the real parser.
    pub fn advance(&mut self, byte: u8) -> bool {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Esc;
                }
                false
            }
            State::Esc => {
                self.state = if byte == b'[' {
                    State::Csi {
                        params: 0,
                        any_digit: false,
                    }
                } else if byte == 0x1b {
                    State::Esc
                } else {
                    State::Ground
                };
                false
            }
            State::Csi { params, any_digit } => match byte {
                b'0'..=b'9' => {
                    self.state = State::Csi {
                        params: params.saturating_mul(10) + u32::from(byte - b'0'),
                        any_digit: true,
                    };
                    false
                }
                b't' => {
                    self.state = State::Ground;
                    any_digit && params == 16
                }
                0x1b => {
                    self.state = State::Esc;
                    false
                }
                _ => {
                    self.state = State::Ground;
                    false
                }
            },
        }
    }
}

/// The `CSI 6 ; height ; width t` reply for a cell of `cw`×`ch` device
/// pixels. Unknown metrics (pre-first-resize) fall back to a common 10×20
/// instead of a (0,0) that would make applications give up on images.
pub fn cell_size_reply(cw: usize, ch: usize) -> Vec<u8> {
    let (cw, ch) = if cw <= 1 { (10, 20) } else { (cw, ch) };
    format!("\x1b[6;{ch};{cw}t").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(bytes: &[u8]) -> usize {
        let mut s = WinopsScanner::new();
        bytes.iter().filter(|&&b| s.advance(b)).count()
    }

    #[test]
    fn matches_bare_csi_16_t() {
        assert_eq!(hits(b"\x1b[16t"), 1);
        assert_eq!(hits(b"ab\x1b[16tcd\x1b[16t"), 2);
    }

    #[test]
    fn ignores_other_winops_and_separated_params() {
        assert_eq!(hits(b"\x1b[14t"), 0);
        assert_eq!(hits(b"\x1b[18t"), 0);
        assert_eq!(hits(b"\x1b[1;6t"), 0);
        assert_eq!(hits(b"\x1b[t"), 0);
        assert_eq!(hits(b"\x1b[16m"), 0);
    }

    #[test]
    fn reply_shape_and_fallback() {
        assert_eq!(cell_size_reply(13, 28), b"\x1b[6;28;13t");
        assert_eq!(cell_size_reply(0, 0), b"\x1b[6;20;10t");
    }
}
