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
    /// The kitty keyboard protocol flags in force: the top of the flag stack the client
    /// pushed for the screen it is on, 0 when it pushed none.
    pub keyboard_flags: u8,
    /// The mouse events the client asked for.
    pub mouse: MouseMode,
    /// `?1006`, `?1015`, and `?1005`: the mouse report forms the client enabled.
    pub mouse_sgr: bool,
    pub mouse_urxvt: bool,
    pub mouse_utf8: bool,
}

/// The mouse events a client asked for; the last mode set is the one in force.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// `?9`: button presses.
    Press,
    /// `?1000`: presses and releases.
    PressRelease,
    /// `?1002`: those, and motion while a button is held.
    ButtonMotion,
    /// `?1003`: those, and every motion.
    AnyMotion,
}

/// Longest CSI parameter run kept; a longer one is not a mode change and is skipped.
const MAX_CSI: usize = 64;
/// Deepest keyboard flag stack kept; a push onto a full stack drops its oldest entry, as
/// the protocol asks of a terminal.
const MAX_KEYBOARD_STACK: usize = 16;

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
    /// The keyboard flag stacks of the main and the alternate screen, which the protocol
    /// keeps apart.
    keyboard: [Vec<u8>; 2],
    alternate: bool,
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
                        self.keyboard = Default::default();
                        self.alternate = false;
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
        let Some((&marker, params)) = self.csi.split_first() else {
            return;
        };
        let mut numbers = params.split(|&b| b == b';').map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|p| p.parse::<u32>().ok())
        });
        let mut number = |default: u32| numbers.next().flatten().unwrap_or(default);
        let screen = usize::from(self.alternate);
        match (marker, final_byte) {
            (b'?', b'h' | b'l') => {
                let set = final_byte == b'h';
                for param in params.split(|&b| b == b';') {
                    match param {
                        b"2004" => self.modes.bracketed_paste = set,
                        b"1004" => self.modes.focus_events = set,
                        b"1049" | b"1047" | b"47" => self.alternate = set,
                        b"9" => set_mouse(&mut self.modes, MouseMode::Press, set),
                        b"1000" => set_mouse(&mut self.modes, MouseMode::PressRelease, set),
                        b"1002" => set_mouse(&mut self.modes, MouseMode::ButtonMotion, set),
                        b"1003" => set_mouse(&mut self.modes, MouseMode::AnyMotion, set),
                        b"1006" => self.modes.mouse_sgr = set,
                        b"1015" => self.modes.mouse_urxvt = set,
                        b"1005" => self.modes.mouse_utf8 = set,
                        _ => {}
                    }
                }
            }
            (b'>', b'u') => {
                let flags = number(0) as u8;
                let stack = &mut self.keyboard[screen];
                if stack.len() == MAX_KEYBOARD_STACK {
                    stack.remove(0);
                }
                stack.push(flags);
            }
            (b'<', b'u') => {
                let stack = &mut self.keyboard[screen];
                let n = (number(1).max(1) as usize).min(stack.len());
                stack.truncate(stack.len() - n);
            }
            (b'=', b'u') => {
                let flags = number(0) as u8;
                let how = number(1);
                let stack = &mut self.keyboard[screen];
                if stack.is_empty() {
                    stack.push(0);
                }
                let top = stack.last_mut().expect("a pushed entry");
                match how {
                    1 => *top = flags,
                    2 => *top |= flags,
                    3 => *top &= !flags,
                    _ => {}
                }
            }
            _ => return,
        }
        self.modes.keyboard_flags = self.keyboard[usize::from(self.alternate)]
            .last()
            .copied()
            .unwrap_or(0);
    }
}

/// Setting a mouse mode replaces the one in force; resetting it turns the mouse off only
/// when it is the one in force, as a terminal does.
fn set_mouse(modes: &mut InputModes, mode: MouseMode, set: bool) {
    if set {
        modes.mouse = mode;
    } else if modes.mouse == mode {
        modes.mouse = MouseMode::Off;
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
    fn keyboard_flags_follow_the_stack_of_the_screen_in_use() {
        assert_eq!(modes_after(&[b"\x1b[>1u"]).keyboard_flags, 1);
        assert_eq!(modes_after(&[b"\x1b[>1u\x1b[>3u"]).keyboard_flags, 3);
        assert_eq!(modes_after(&[b"\x1b[>1u\x1b[>3u\x1b[<u"]).keyboard_flags, 1);
        assert_eq!(modes_after(&[b"\x1b[>1u\x1b[<5u"]).keyboard_flags, 0);
        assert_eq!(
            modes_after(&[b"\x1b[=5u"]).keyboard_flags,
            5,
            "set on no entry"
        );
        assert_eq!(
            modes_after(&[b"\x1b[>1u\x1b[=8;2u"]).keyboard_flags,
            9,
            "or"
        );
        assert_eq!(
            modes_after(&[b"\x1b[>9u\x1b[=1;3u"]).keyboard_flags,
            8,
            "and not"
        );
        // The alternate screen keeps a stack of its own.
        let alternate = modes_after(&[b"\x1b[>1u\x1b[?1049h"]);
        assert_eq!(alternate.keyboard_flags, 0);
        let back = modes_after(&[b"\x1b[>1u\x1b[?1049h\x1b[>3u\x1b[?1049l"]);
        assert_eq!(back.keyboard_flags, 1);
        assert_eq!(modes_after(&[b"\x1b[>1u\x1bc"]).keyboard_flags, 0, "RIS");
        assert_eq!(
            modes_after(&[b"\x1b[>4;2m"]).keyboard_flags,
            0,
            "not this protocol"
        );
    }

    #[test]
    fn the_last_mouse_mode_set_is_in_force() {
        assert_eq!(modes_after(&[b""]).mouse, MouseMode::Off);
        assert_eq!(
            modes_after(&[b"\x1b[?1000h"]).mouse,
            MouseMode::PressRelease
        );
        let both = modes_after(&[b"\x1b[?1000h\x1b[?1002h\x1b[?1006h"]);
        assert_eq!(both.mouse, MouseMode::ButtonMotion);
        assert!(both.mouse_sgr);
        assert_eq!(
            modes_after(&[b"\x1b[?1002h\x1b[?1000l"]).mouse,
            MouseMode::ButtonMotion,
            "resetting a mode not in force changes nothing"
        );
        assert_eq!(
            modes_after(&[b"\x1b[?1003h\x1b[?1003l"]).mouse,
            MouseMode::Off
        );
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
