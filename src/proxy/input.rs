//! Byte-level prefix detection on the host input stream. The prefix is a C0
//! control byte, so it cannot collide with UTF-8 continuation bytes or appear
//! mid-CSI; bracketed paste is the only framing the matcher must respect.
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum InAction {
    Forward(Vec<u8>),
    OpenPicker,
}

#[derive(PartialEq)]
enum State { Idle, Armed(Instant) }

pub struct InputMachine {
    prefix: u8,
    action_key: u8,
    timeout: Duration,
    state: State,
    in_paste: bool,
    paste_scan: Vec<u8>, // rolling tail to detect the paste markers
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl InputMachine {
    pub fn new(prefix: u8, action_key: u8, timeout: Duration) -> Self {
        Self { prefix, action_key, timeout, state: State::Idle, in_paste: false, paste_scan: Vec::new() }
    }

    fn track_paste(&mut self, byte: u8) {
        self.paste_scan.push(byte);
        if self.paste_scan.len() > PASTE_START.len().max(PASTE_END.len()) {
            self.paste_scan.remove(0);
        }
        if !self.in_paste && self.paste_scan.ends_with(PASTE_START) {
            self.in_paste = true;
        } else if self.in_paste && self.paste_scan.ends_with(PASTE_END) {
            self.in_paste = false;
        }
    }

    pub fn feed(&mut self, byte: u8, now: Instant) -> Vec<InAction> {
        // Inside a paste, everything is literal — never arm.
        if self.in_paste {
            self.track_paste(byte);
            return vec![InAction::Forward(vec![byte])];
        }
        match self.state {
            State::Idle => {
                if byte == self.prefix {
                    self.state = State::Armed(now);
                    Vec::new()
                } else {
                    self.track_paste(byte);
                    vec![InAction::Forward(vec![byte])]
                }
            }
            State::Armed(t) => {
                // A stale prefix (older than the arm window) never captures the
                // next key: disarm and reprocess the byte as if freshly Idle.
                if now.duration_since(t) >= self.timeout {
                    self.state = State::Idle;
                    return self.feed(byte, now);
                }
                self.state = State::Idle;
                if byte == self.action_key {
                    vec![InAction::OpenPicker]
                } else if byte == self.prefix {
                    // Double-tap → exactly one literal prefix to the child.
                    vec![InAction::Forward(vec![self.prefix])]
                } else {
                    // Incomplete sequence: the prefix is NOT forwarded (that would
                    // trigger the child app's own binding) and the unrecognised
                    // follow-up key is swallowed — pressing the prefix entered
                    // command mode.
                    Vec::new()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn m() -> InputMachine { InputMachine::new(0x07, b's', Duration::from_millis(400)) }
    fn fwd(a: &[InAction]) -> Vec<u8> {
        a.iter().flat_map(|x| match x { InAction::Forward(b) => b.clone(), _ => vec![] }).collect()
    }

    #[test]
    fn plain_bytes_pass_through() {
        let mut im = m();
        let now = Instant::now();
        assert_eq!(fwd(&im.feed(b'a', now)), vec![b'a']);
        assert_eq!(fwd(&im.feed(b'b', now)), vec![b'b']);
    }

    #[test]
    fn prefix_then_action_opens_picker() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty(), "prefix is swallowed while arming");
        let out = im.feed(b's', t);
        assert!(matches!(out.as_slice(), [InAction::OpenPicker]));
    }

    #[test]
    fn double_prefix_sends_one_literal() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        assert_eq!(fwd(&im.feed(0x07, t)), vec![0x07]);
    }

    #[test]
    fn prefix_then_other_is_swallowed() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        let out = im.feed(b'x', t);
        assert!(fwd(&out).is_empty(), "incomplete sequence forwards nothing");
        assert!(!out.iter().any(|a| matches!(a, InAction::OpenPicker)));
    }

    #[test]
    fn stale_prefix_reprocesses_next_key() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        let stale = t + Duration::from_millis(401);
        // The stale prefix is dropped; the plain key is reprocessed and forwarded.
        assert_eq!(fwd(&im.feed(b'a', stale)), vec![b'a']);
        // A stale prefix must not arm the action key.
        assert!(im.feed(0x07, t).is_empty());
        let out = im.feed(b's', stale);
        assert_eq!(fwd(&out), vec![b's']);
        assert!(!out.iter().any(|a| matches!(a, InAction::OpenPicker)));
    }

    #[test]
    fn prefix_inside_bracketed_paste_is_literal() {
        let mut im = m();
        let t = Instant::now();
        // enter paste: ESC [ 2 0 0 ~
        for b in b"\x1b[200~" { let _ = im.feed(*b, t); }
        // a 0x07 inside the paste must be forwarded, never open the picker
        let out = im.feed(0x07, t);
        assert_eq!(fwd(&out), vec![0x07]);
        assert!(!out.iter().any(|a| matches!(a, InAction::OpenPicker)));
        // leave paste: ESC [ 2 0 1 ~ — afterwards the prefix arms again
        for b in b"\x1b[201~" { let _ = im.feed(*b, t); }
        assert!(im.feed(0x07, t).is_empty());
        assert!(matches!(im.feed(b's', t).as_slice(), [InAction::OpenPicker]));
    }
}
