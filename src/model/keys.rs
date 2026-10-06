//! The one key table: every key xmux binds, with the words that name it. Both focus
//! paths resolve a prefix command through this table, and the help, the prefix key list,
//! and the hint after a selection move are built from it, so what a surface says a key
//! does and what the key does are read from one place.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::model::NavPosition;

/// An arrow key, named by the direction it points on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Down,
    Left,
    Right,
}

impl Arrow {
    pub const ALL: [Arrow; 4] = [Arrow::Up, Arrow::Down, Arrow::Left, Arrow::Right];

    /// Whether this arrow points from the nav toward the terminal view at `position`. The
    /// pair facing the terminal's side names the terminal and the other pair the nav, so
    /// the pair flips with the nav on the right or below. Both focus paths (nav focus and
    /// terminal focus) resolve their prefix arrows through this, so a change to one path
    /// is a change to both.
    pub fn faces_terminal(self, position: NavPosition) -> bool {
        matches!(self, Arrow::Right | Arrow::Down) == position.forward_arrows_face_terminal()
    }

    /// The final byte of the arrow's CSI sequence (`ESC [ A` is up).
    pub fn csi_final(self) -> u8 {
        match self {
            Arrow::Up => b'A',
            Arrow::Down => b'B',
            Arrow::Right => b'C',
            Arrow::Left => b'D',
        }
    }

    fn from_csi_final(b: u8) -> Option<Arrow> {
        Arrow::ALL.into_iter().find(|a| a.csi_final() == b)
    }

    fn from_code(code: KeyCode) -> Option<Arrow> {
        match code {
            KeyCode::Up => Some(Arrow::Up),
            KeyCode::Down => Some(Arrow::Down),
            KeyCode::Left => Some(Arrow::Left),
            KeyCode::Right => Some(Arrow::Right),
            _ => None,
        }
    }
}

/// One key as the command after a prefix reads it. Both focus paths reduce what they
/// received to a chord before they look it up, the nav path from a decoded key and the
/// terminal path from raw bytes, so the two cannot read one key two ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chord {
    /// A printable character that is not a digit.
    Char(char),
    /// Any digit, which opens the card jump holding it.
    Digit,
    Tab,
    Arrow(Arrow),
    CtrlArrow(Arrow),
    /// The prefix pressed a second time.
    Prefix,
}

impl Chord {
    /// The chord a decoded key is, or `None` for a key no command could be bound to.
    pub fn from_key(key: &KeyEvent, prefix: u8) -> Option<Chord> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(arrow) = Arrow::from_code(key.code) {
            return Some(if ctrl {
                Chord::CtrlArrow(arrow)
            } else {
                Chord::Arrow(arrow)
            });
        }
        match key.code {
            KeyCode::Tab | KeyCode::Char('\t') => Some(Chord::Tab),
            KeyCode::Char(c) if c as u32 == prefix as u32 => Some(Chord::Prefix),
            KeyCode::Char(c) if c.is_ascii_digit() => Some(Chord::Digit),
            KeyCode::Char(c) if !c.is_control() => Some(Chord::Char(c)),
            _ => None,
        }
    }

    /// The chord at the start of `bytes` and how many bytes it spans. A byte no chord
    /// starts with is one byte of nothing, so the caller steps past it alone.
    pub fn from_bytes(bytes: &[u8], prefix: u8) -> (Option<Chord>, usize) {
        let Some(&b0) = bytes.first() else {
            return (None, 0);
        };
        if b0 == prefix {
            return (Some(Chord::Prefix), 1);
        }
        if b0 == 0x1b && bytes.len() >= 6 && bytes[1] == b'[' && &bytes[2..5] == b"1;5" {
            if let Some(arrow) = Arrow::from_csi_final(bytes[5]) {
                return (Some(Chord::CtrlArrow(arrow)), 6);
            }
        }
        if b0 == 0x1b && bytes.len() >= 3 && bytes[1] == b'[' {
            if let Some(arrow) = Arrow::from_csi_final(bytes[2]) {
                return (Some(Chord::Arrow(arrow)), 3);
            }
        }
        match b0 {
            b'\t' => (Some(Chord::Tab), 1),
            b'0'..=b'9' => (Some(Chord::Digit), 1),
            0x20..=0x7e => (Some(Chord::Char(b0 as char)), 1),
            _ => (None, 1),
        }
    }
}

/// What a bound key does, independent of which focus path read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCommand {
    /// Open the card jump holding the digit.
    Jump,
    /// Open the filter.
    Filter,
    /// Move focus to the other view.
    FocusToggle,
    /// Focus the terminal view.
    FocusTerminal,
    /// Focus the nav.
    FocusNav,
    /// Start a session on the selected host.
    NewSession,
    /// Re-scan the selected card's host alone.
    RescanHost,
    /// Re-scan every host.
    Rescan,
    Logout,
    /// Select the current source's section information screen.
    HostInfo,
    /// Toggle the table of host problems.
    Check,
    /// Search and run a named command.
    Palette,
    /// Collapse or expand the nav.
    Collapse,
    /// Toggle auto-hide-nav.
    AutoHide,
    /// Place the nav one side clockwise.
    Position,
    /// Move the side view border by this many columns (positive is right).
    Width(i32),
    /// Move the band view border by this many rows (positive is down).
    Height(i32),
    /// Toggle the history.
    History,
    /// Toggle the help.
    Help,
    /// Quit xmux.
    Quit,
    /// Send one literal prefix byte to the pane.
    LiteralPrefix,
}

/// The groups the table is read in. The prefix key list shows the prefix sections; the
/// help shows every section in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Keys the nav reads bare while it holds the focus.
    Move,
    /// Keys a host's or a source's screen reads in the terminal view.
    Screen,
    Navigate,
    Sessions,
    View,
    App,
    Mouse,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Move,
        Section::Screen,
        Section::Navigate,
        Section::Sessions,
        Section::View,
        Section::App,
        Section::Mouse,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Move => "move (nav focus)",
            Section::Screen => "machine and mux screens",
            Section::Navigate => "navigate",
            Section::Sessions => "sessions",
            Section::View => "view",
            Section::App => "app",
            Section::Mouse => "mouse",
        }
    }
}

/// How an entry's keys are pressed.
#[derive(Clone, Copy, Debug)]
pub enum Keys {
    /// Chords pressed after the prefix, each with the command it runs.
    Prefix(&'static [(Chord, KeyCommand)]),
    /// The arrow pair after the prefix that faces the terminal (`true`) or the nav
    /// (`false`). Which arrows those are depends on the nav's side.
    PrefixArrows { toward_terminal: bool },
    /// Keys the nav reads bare while it holds the focus, with `command` when the entry is
    /// a command a hint can offer.
    Bare(Option<KeyCommand>),
    /// Keys a host's or a source's screen reads bare, by the code each arrives as.
    Screen(&'static [KeyCode]),
    /// A mouse gesture.
    Mouse,
}

/// One row of the key table.
#[derive(Clone, Copy, Debug)]
pub struct KeyEntry {
    pub section: Section,
    pub keys: Keys,
    /// The keys as written, without the prefix. Empty for the prefix itself (the
    /// configured prefix is written there) and for an arrow pair (the pair the placement
    /// makes active is written there).
    pub label: &'static str,
    /// The help's description.
    pub help: &'static str,
    /// The prefix key list's description when it has the room.
    pub long: &'static str,
    /// The description the key list and the hint shorten to. Never empty: a key is never
    /// shown without a name.
    pub short: &'static str,
    /// Which keys the key list gives up first when it cannot fit them all: the highest
    /// rank goes first, and rank 0 is never given up.
    pub rank: u8,
}

impl KeyEntry {
    /// Whether the entry is pressed after the prefix.
    pub fn prefixed(&self) -> bool {
        matches!(self.keys, Keys::Prefix(_) | Keys::PrefixArrows { .. })
    }

    /// The command this entry runs for `chord` at `position`, if the chord is one of its
    /// keys.
    pub fn command_for(&self, chord: Chord, position: NavPosition) -> Option<KeyCommand> {
        match self.keys {
            Keys::Prefix(map) => map.iter().find(|(c, _)| *c == chord).map(|(_, k)| *k),
            Keys::PrefixArrows { toward_terminal } => match chord {
                Chord::Arrow(a) if a.faces_terminal(position) == toward_terminal => {
                    Some(if toward_terminal {
                        KeyCommand::FocusTerminal
                    } else {
                        KeyCommand::FocusNav
                    })
                }
                _ => None,
            },
            Keys::Bare(_) | Keys::Screen(_) | Keys::Mouse => None,
        }
    }

    /// Every chord this entry binds after the prefix at `position`.
    pub fn chords(&self, position: NavPosition) -> Vec<Chord> {
        match self.keys {
            Keys::Prefix(map) => map.iter().map(|(c, _)| *c).collect(),
            Keys::PrefixArrows { toward_terminal } => Arrow::ALL
                .into_iter()
                .filter(|a| a.faces_terminal(position) == toward_terminal)
                .map(Chord::Arrow)
                .collect(),
            Keys::Bare(_) | Keys::Screen(_) | Keys::Mouse => Vec::new(),
        }
    }

    /// The keys as written for a reader, without the prefix.
    pub fn key_label(&self, prefix: &str, position: NavPosition) -> String {
        match self.keys {
            Keys::PrefixArrows { toward_terminal } => {
                let forward = position.forward_arrows_face_terminal() == toward_terminal;
                if forward { "→/↓" } else { "←/↑" }.to_string()
            }
            _ if self.label.is_empty() => prefix.to_string(),
            _ => self.label.to_string(),
        }
    }

    /// The keys as written in full: the prefix before a prefixed entry's keys.
    pub fn full_label(&self, prefix: &str, position: NavPosition) -> String {
        let keys = self.key_label(prefix, position);
        if self.prefixed() {
            format!("{prefix} {keys}")
        } else {
            keys
        }
    }

    /// Whether this entry runs `command`.
    pub fn runs(&self, command: KeyCommand) -> bool {
        match self.keys {
            Keys::Prefix(map) => map.iter().any(|(_, k)| *k == command),
            Keys::PrefixArrows { toward_terminal } => {
                command
                    == if toward_terminal {
                        KeyCommand::FocusTerminal
                    } else {
                        KeyCommand::FocusNav
                    }
            }
            Keys::Bare(c) => c == Some(command),
            Keys::Screen(_) | Keys::Mouse => false,
        }
    }
}

/// Every key xmux binds, in reading order inside each section.
pub static TABLE: &[KeyEntry] = &[
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "↑/↓ · j/k",
        help: "move one card",
        long: "move one card",
        short: "move",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "←/→ · h/l",
        help: "previous / next section (the machine cards as one)",
        long: "previous / next section",
        short: "section",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "PgUp/PgDn",
        help: "jump by ten cards",
        long: "ten cards",
        short: "page",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "Home/End",
        help: "first / last card",
        long: "first / last card",
        short: "ends",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "Ctrl+↑/↓",
        help: "up to the mux, then the machine / back down to the child",
        long: "up / down a level",
        short: "level",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(Some(KeyCommand::FocusTerminal)),
        label: "Enter",
        help: "focus the terminal view",
        long: "focus terminal view",
        short: "terminal",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "i",
        help: "select the current mux and show its screen",
        long: "info screen of this mux",
        short: "info",
        rank: 0,
    },
    KeyEntry {
        section: Section::Move,
        keys: Keys::Bare(None),
        label: "Esc",
        help: "clear the applied filter",
        long: "clear the filter",
        short: "clear",
        rank: 0,
    },
    KeyEntry {
        section: Section::Screen,
        keys: Keys::Screen(&[KeyCode::Up, KeyCode::Down, KeyCode::Tab, KeyCode::BackTab]),
        label: "↑/↓ · Tab",
        help: "select the previous / next link, S-Tab back (terminal focus)",
        long: "select a link",
        short: "link",
        rank: 0,
    },
    KeyEntry {
        section: Section::Screen,
        keys: Keys::Screen(&[KeyCode::Enter]),
        label: "Enter",
        help: "open the screen the selected link names (terminal focus)",
        long: "open the link",
        short: "open",
        rank: 0,
    },
    KeyEntry {
        section: Section::Screen,
        keys: Keys::Screen(&[KeyCode::Char('d')]),
        label: "d",
        help: "unfold / fold the full failure diagnostic (either focus)",
        long: "diagnostic details",
        short: "details",
        rank: 0,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::Prefix(&[(Chord::Digit, KeyCommand::Jump)]),
        label: "1-9",
        help: "jump to a card by its number (keep typing for 10+)",
        long: "jump to card number",
        short: "jump",
        rank: 0,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::Prefix(&[(Chord::Char('/'), KeyCommand::Filter)]),
        label: "/",
        help: "fuzzy filter <source>/<name>",
        long: "filter cards",
        short: "filter",
        rank: 1,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::Prefix(&[(Chord::Tab, KeyCommand::FocusToggle)]),
        label: "Tab",
        help: "toggle focus between the nav and the terminal",
        long: "toggle focus",
        short: "focus",
        rank: 2,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::PrefixArrows {
            toward_terminal: true,
        },
        label: "",
        help: "focus the terminal view (the pair facing the terminal's side)",
        long: "focus terminal view",
        short: "terminal",
        rank: 2,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::PrefixArrows {
            toward_terminal: false,
        },
        label: "",
        help: "focus the nav (the pair facing the nav's side)",
        long: "focus nav",
        short: "nav",
        rank: 3,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('i'), KeyCommand::HostInfo)]),
        label: "i",
        help: "select the current mux and show its screen",
        long: "info screen of this mux",
        short: "info",
        rank: 2,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('n'), KeyCommand::NewSession)]),
        label: "n",
        help: "new session on the selected mux",
        long: "new session",
        short: "new",
        rank: 1,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('r'), KeyCommand::RescanHost)]),
        label: "r",
        help: "rescan the selected card's machine only",
        long: "rescan this machine",
        short: "rescan",
        rank: 1,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('R'), KeyCommand::Rescan)]),
        label: "R",
        help: "rescan every machine",
        long: "rescan all machines",
        short: "rescan all",
        rank: 2,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('L'), KeyCommand::Logout)]),
        label: "L",
        help: "log out of the selected SSH machine",
        long: "log out of this machine",
        short: "log out",
        rank: 3,
    },
    KeyEntry {
        section: Section::Sessions,
        keys: Keys::Prefix(&[(Chord::Char('h'), KeyCommand::Check)]),
        label: "h",
        help: "machine problems, by cause: ↑/↓ move, Enter opens the machine, Esc closes",
        long: "machine problems",
        short: "problems",
        rank: 2,
    },
    KeyEntry {
        section: Section::Navigate,
        keys: Keys::Prefix(&[(Chord::Char(':'), KeyCommand::Palette)]),
        label: ":",
        help: "command palette: type to search, Enter runs, Esc closes",
        long: "command palette",
        short: "commands",
        rank: 2,
    },
    KeyEntry {
        section: Section::View,
        keys: Keys::Prefix(&[(Chord::Char('z'), KeyCommand::Collapse)]),
        label: "z",
        help: "collapse / expand the nav",
        long: "collapse nav",
        short: "collapse",
        rank: 3,
    },
    KeyEntry {
        section: Section::View,
        keys: Keys::Prefix(&[(Chord::Char('t'), KeyCommand::AutoHide)]),
        label: "t",
        help: "toggle auto-hide-nav (║ view border = on)",
        long: "toggle nav auto-hide",
        short: "auto-hide",
        rank: 4,
    },
    KeyEntry {
        section: Section::View,
        keys: Keys::Prefix(&[(Chord::Char('p'), KeyCommand::Position)]),
        label: "p",
        help: "place the nav one side clockwise (left · top · right · bottom · default)",
        long: "place nav",
        short: "place",
        rank: 4,
    },
    KeyEntry {
        section: Section::View,
        keys: Keys::Prefix(&[
            (Chord::CtrlArrow(Arrow::Left), KeyCommand::Width(-1)),
            (Chord::CtrlArrow(Arrow::Right), KeyCommand::Width(1)),
        ]),
        label: "C-←/→",
        help: "resize the nav width; a bare C-←/→ repeats for a moment",
        long: "resize nav width",
        short: "width",
        rank: 6,
    },
    KeyEntry {
        section: Section::View,
        keys: Keys::Prefix(&[
            (Chord::CtrlArrow(Arrow::Up), KeyCommand::Height(-1)),
            (Chord::CtrlArrow(Arrow::Down), KeyCommand::Height(1)),
        ]),
        label: "C-↑/↓",
        help: "resize the nav height; a bare C-↑/↓ repeats for a moment",
        long: "resize nav height",
        short: "height",
        rank: 6,
    },
    KeyEntry {
        section: Section::App,
        keys: Keys::Prefix(&[(Chord::Char('m'), KeyCommand::History)]),
        label: "m",
        help: "message history of results and background events",
        long: "message history",
        short: "history",
        rank: 2,
    },
    KeyEntry {
        section: Section::App,
        keys: Keys::Prefix(&[(Chord::Char('?'), KeyCommand::Help)]),
        label: "?",
        help: "this help: type to search, ↑/↓ scroll, Esc closes",
        long: "help and glyphs",
        short: "help",
        rank: 0,
    },
    KeyEntry {
        section: Section::App,
        keys: Keys::Prefix(&[(Chord::Char('q'), KeyCommand::Quit)]),
        label: "q",
        help: "quit xmux",
        long: "quit xmux",
        short: "quit",
        rank: 0,
    },
    KeyEntry {
        section: Section::App,
        keys: Keys::Prefix(&[(Chord::Prefix, KeyCommand::LiteralPrefix)]),
        label: "",
        help: "send one literal prefix to the pane (terminal focus)",
        long: "send prefix key",
        short: "send",
        rank: 5,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "click a card",
        help: "open it and focus the terminal",
        long: "open it",
        short: "open",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "point at a card",
        help: "preview it in the terminal view (nav focus)",
        long: "preview it",
        short: "preview",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "click a screen link",
        help: "open the screen it names",
        long: "open the link",
        short: "link",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "click a view",
        help: "focus that view",
        long: "focus it",
        short: "focus",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "click a collapsed nav",
        help: "expand the nav",
        long: "expand it",
        short: "expand",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "drag the view border",
        help: "resize the nav; past its minimum, collapse it",
        long: "resize the nav",
        short: "resize",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "click a toast",
        help: "dismiss it",
        long: "dismiss it",
        short: "dismiss",
        rank: 0,
    },
    KeyEntry {
        section: Section::Mouse,
        keys: Keys::Mouse,
        label: "terminal focus",
        help: "keys, wheel and clicks go to the pane (the mux needs its own mouse mode)",
        long: "input goes to the pane",
        short: "pane",
        rank: 0,
    },
];

/// The command a chord after the prefix runs at `position`, read from [`TABLE`].
pub fn prefix_command(chord: Chord, position: NavPosition) -> Option<KeyCommand> {
    TABLE.iter().find_map(|e| e.command_for(chord, position))
}

/// The table entry that runs `command`.
pub fn entry_for(command: KeyCommand) -> Option<&'static KeyEntry> {
    TABLE.iter().find(|e| e.runs(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_chord_is_bound_twice_at_any_position() {
        for position in [
            NavPosition::Left,
            NavPosition::Top,
            NavPosition::Right,
            NavPosition::Bottom,
        ] {
            let mut seen: Vec<Chord> = Vec::new();
            for entry in TABLE {
                for chord in entry.chords(position) {
                    assert!(
                        !seen.contains(&chord),
                        "{chord:?} is bound twice at {position:?}"
                    );
                    seen.push(chord);
                }
            }
        }
    }

    #[test]
    fn every_entry_has_a_name_at_every_length() {
        for entry in TABLE {
            assert!(
                !entry.help.is_empty() && !entry.long.is_empty() && !entry.short.is_empty(),
                "{entry:?}"
            );
            assert!(entry.short.chars().count() <= entry.long.chars().count());
        }
    }

    #[test]
    fn a_key_and_its_bytes_read_as_one_chord() {
        let p = 0x07;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            Chord::from_key(&key(KeyCode::Char('n')), p),
            Some(Chord::Char('n'))
        );
        assert_eq!(Chord::from_bytes(b"n", p), (Some(Chord::Char('n')), 1));
        assert_eq!(
            Chord::from_key(&key(KeyCode::Char('7')), p),
            Some(Chord::Digit)
        );
        assert_eq!(Chord::from_bytes(b"7", p), (Some(Chord::Digit), 1));
        assert_eq!(
            Chord::from_key(&key(KeyCode::Char('\t')), p),
            Some(Chord::Tab)
        );
        assert_eq!(Chord::from_bytes(b"\t", p), (Some(Chord::Tab), 1));
        assert_eq!(
            Chord::from_key(&key(KeyCode::Char('\x07')), p),
            Some(Chord::Prefix)
        );
        assert_eq!(Chord::from_bytes(b"\x07", p), (Some(Chord::Prefix), 1));
        assert_eq!(
            Chord::from_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), p),
            Some(Chord::CtrlArrow(Arrow::Left))
        );
        assert_eq!(
            Chord::from_bytes(b"\x1b[1;5D", p),
            (Some(Chord::CtrlArrow(Arrow::Left)), 6)
        );
        assert_eq!(
            Chord::from_bytes(b"\x1b[B", p),
            (Some(Chord::Arrow(Arrow::Down)), 3)
        );
        assert_eq!(Chord::from_bytes(b"\x1b", p), (None, 1));
    }

    #[test]
    fn the_nav_structure_keys_are_bound_in_the_table() {
        for position in [NavPosition::Left, NavPosition::Bottom] {
            for (c, command) in [('h', KeyCommand::Check), ('r', KeyCommand::RescanHost)] {
                assert_eq!(prefix_command(Chord::Char(c), position), Some(command));
                assert!(entry_for(command).is_some_and(|e| e.prefixed()));
            }
            assert_eq!(prefix_command(Chord::Char('l'), position), None);
        }
    }

    #[test]
    fn the_lowercase_rescan_key_asks_this_host_and_the_uppercase_one_every_host() {
        for position in [NavPosition::Left, NavPosition::Bottom] {
            assert_eq!(
                prefix_command(Chord::Char('r'), position),
                Some(KeyCommand::RescanHost)
            );
            assert_eq!(
                prefix_command(Chord::Char('R'), position),
                Some(KeyCommand::Rescan)
            );
        }
        assert_eq!(
            entry_for(KeyCommand::RescanHost).map(|e| e.label),
            Some("r")
        );
        assert_eq!(entry_for(KeyCommand::Rescan).map(|e| e.label), Some("R"));
    }
}
