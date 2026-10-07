//! The input modes a session's client sets on the terminal it believes it writes to.
//! The client's terminal is xmux's PTY, so the client asks it, not xmux's own terminal,
//! for bracketed paste and the like, and input xmux forwards has to be shaped the way
//! the client asked. The modes are read from the client's output by a scanner of their
//! own rather than from the grid's emulator, because the grid is wiped on a session
//! switch while the client keeps believing the modes it set.

/// The input modes one client has set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputModes {
    /// `?2004`: a paste arrives between `ESC[200~` and `ESC[201~`.
    pub bracketed_paste: bool,
    /// `?1004`: the client is told with `ESC[I` and `ESC[O` when it gains and loses the
    /// focus.
    pub focus_events: bool,
}

/// Longest CSI parameter run kept; a longer one is not a mode change and is skipped.
const MAX_CSI: usize = 64;

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
}

/// Reads [`InputModes`] out of a client's output, one chunk at a time, carrying a
/// sequence split across chunks.
#[derive(Debug, Default)]
pub struct ModeScanner {
    modes: InputModes,
    state: State,
    csi: Vec<u8>,
}

impl ModeScanner {
    pub fn modes(&self) -> InputModes {
        self.modes
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Escape;
                    }
                }
                State::Escape => match b {
                    b'[' => {
                        self.csi.clear();
                        self.state = State::Csi;
                    }
                    // RIS: a full reset clears every mode.
                    b'c' => {
                        self.modes = InputModes::default();
                        self.state = State::Ground;
                    }
                    0x1b => {}
                    _ => self.state = State::Ground,
                },
                State::Csi => match b {
                    0x1b => self.state = State::Escape,
                    0x18 | 0x1a => self.state = State::Ground,
                    0x40..=0x7e => {
                        self.dispatch(b);
                        self.state = State::Ground;
                    }
                    0x20..=0x3f if self.csi.len() < MAX_CSI => self.csi.push(b),
                    0x20..=0x3f => self.state = State::Ground,
                    // A control byte inside a CSI is executed by a terminal, not part of it.
                    _ => {}
                },
            }
        }
    }

    fn dispatch(&mut self, final_byte: u8) {
        let Some(params) = self.csi.strip_prefix(b"?") else {
            return;
        };
        let set = match final_byte {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        for param in params.split(|&b| b == b';') {
            match param {
                b"2004" => self.modes.bracketed_paste = set,
                b"1004" => self.modes.focus_events = set,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes_after(chunks: &[&[u8]]) -> InputModes {
        let mut scanner = ModeScanner::default();
        for chunk in chunks {
            scanner.feed(chunk);
        }
        scanner.modes()
    }

    #[test]
    fn bracketed_paste_follows_the_last_set_or_reset() {
        assert!(!modes_after(&[b"plain output"]).bracketed_paste);
        assert!(modes_after(&[b"\x1b[?2004h"]).bracketed_paste);
        assert!(!modes_after(&[b"\x1b[?2004h", b"\x1b[?2004l"]).bracketed_paste);
        assert!(
            modes_after(&[b"\x1b[?1049;2004h"]).bracketed_paste,
            "in a list"
        );
    }

    #[test]
    fn focus_events_follow_their_own_mode() {
        let modes = modes_after(&[b"\x1b[?1004h"]);
        assert!(modes.focus_events && !modes.bracketed_paste);
        assert!(!modes_after(&[b"\x1b[?1004h\x1b[?1004l"]).focus_events);
    }

    #[test]
    fn a_sequence_split_across_chunks_still_counts() {
        assert!(modes_after(&[b"out\x1b", b"[?20", b"04h"]).bracketed_paste);
    }

    #[test]
    fn other_sequences_and_a_reset_leave_no_mode_behind() {
        assert!(
            !modes_after(&[b"\x1b[2004h"]).bracketed_paste,
            "not private"
        );
        assert!(!modes_after(&[b"\x1b[>2004h"]).bracketed_paste);
        assert!(!modes_after(&[b"\x1b[?2004h\x1bc"]).bracketed_paste, "RIS");
        assert!(
            !modes_after(&[b"\x1b[?20\x1804h"]).bracketed_paste,
            "CAN cancels"
        );
    }
}
