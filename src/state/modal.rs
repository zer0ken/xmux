//! Runtime modal data and its representation-local updates.

use super::{ModalKind, RowRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteChoice {
    Command(crate::model::keys::KeyCommand),
    Login(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Filter,
    New,
    Logout,
    /// Jump to a session by its number (the user-facing name: a `card` is the visual
    /// row, the session is what it stands for). Unlike the other modes this one acts
    /// WHILE it is open: every edit moves the selection while the number names a card,
    /// so the number is a live cursor rather than a value submitted at the end. Enter
    /// closes the popup when the number names a card and flashes the valid range while
    /// leaving it open otherwise; Esc restores where the jump started.
    Jump,
}

pub(crate) struct Input {
    pub(crate) mode: InputMode,
    /// [`InputMode::Logout`] only: what the logout does, as `(label, value)` rows the
    /// confirm states above its field. Captured when it opens, like `source`.
    pub(crate) facts: Vec<(&'static str, String)>,
    pub(crate) buffer: String,
    /// Caret position as a char index into `buffer` (`0..=buffer char count`). Every
    /// edit and movement keeps it in range; the entry line renders a block caret at
    /// this column, so editing is no longer append-only.
    pub(crate) cursor: usize,
    /// The create source captured when the input opened, so the action lands on the
    /// host the user was on, not wherever streaming results moved the selection by
    /// the time they pressed Enter.
    pub(crate) source: Option<String>,
    /// [`InputMode::Jump`] only: the card the selection was on when the popup opened,
    /// held by IDENTITY (not row index) so a rebuild during the jump cannot restore
    /// onto the wrong card. Esc returns here; Enter leaves the selection where the
    /// live jump already put it.
    pub(crate) restore: Option<RowRef>,
    /// [`InputMode::Filter`] only: the filter the input opened from, restored on Esc.
    /// The filter applies live while the input is open, so cancelling must undo every
    /// edit back to this value.
    pub(crate) restore_filter: Option<String>,
}

impl Input {
    /// Builds an input with the caret at the END of `buffer`, so a prefilled name
    /// (rename / filter) is ready to edit from its tail. The one constructor keeps
    /// the caret-init rule in a single place.
    pub(crate) fn new(mode: InputMode, buffer: String, source: Option<String>) -> Self {
        let cursor = buffer.chars().count();
        Input {
            mode,
            facts: Vec::new(),
            buffer,
            cursor,
            source,
            restore: None,
            restore_filter: None,
        }
    }

    /// Inserts `c` at the caret and advances past it. Char-indexed so multi-byte
    /// (CJK) text stays correct.
    pub(crate) fn insert(&mut self, c: char) {
        let mut chars: Vec<char> = self.buffer.chars().collect();
        let index = self.cursor.min(chars.len());
        chars.insert(index, c);
        self.cursor = index + 1;
        self.buffer = chars.into_iter().collect();
    }

    /// Deletes the char before the caret (Backspace).
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.buffer.chars().collect();
        let index = self.cursor.min(chars.len());
        if index == 0 {
            return;
        }
        chars.remove(index - 1);
        self.cursor = index - 1;
        self.buffer = chars.into_iter().collect();
    }

    /// Deletes the char at the caret (Delete); a no-op at end of line.
    pub(crate) fn delete(&mut self) {
        let mut chars: Vec<char> = self.buffer.chars().collect();
        if self.cursor >= chars.len() {
            return;
        }
        chars.remove(self.cursor);
        self.buffer = chars.into_iter().collect();
    }

    /// Deletes the word (and any run of spaces) before the caret (Ctrl-W).
    pub(crate) fn delete_word_before(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let end = self.cursor.min(chars.len());
        let mut index = end;
        while index > 0 && chars[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
        let mut chars = chars;
        chars.drain(index..end);
        self.cursor = index;
        self.buffer = chars.into_iter().collect();
    }

    /// Clears the whole line (Ctrl-U).
    pub(crate) fn clear_line(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn right(&mut self) {
        if self.cursor < self.buffer.chars().count() {
            self.cursor += 1;
        }
    }

    pub(crate) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }
}

/// The single open modal, if any: at most one popup or inline input. Modeling it as
/// one `Option` (not independent fields) makes the
/// modals' mutual exclusion structural: opening one drops whatever was open, and
/// the compiler guarantees two can never coexist, so the hand-maintained "clear
/// the others" invariant cannot drift. Lives on [`crate::state::State`]; the
/// switcher owns only the behavior and the transient popup geometry (drag offset
/// / drawn rect).
///
/// The input carries several owned strings (label, buffer, a create source, a
/// jump restore reference, a filter restore value), so it is boxed to keep the
/// enum small; callers pattern-match through the box and never see the pointer.
pub(crate) enum Modal {
    /// The help `prefix ?` opens. `query` is what has been typed to search it, and
    /// `scroll` counts the rows scrolled past from the top of what matches. `decoder`
    /// lives as long as the help, so a key split across two reads is still one key.
    Help {
        query: String,
        scroll: usize,
        decoder: crate::display::decode::KeyDecoder,
    },
    /// The history `prefix m` opens. `scroll` counts the records scrolled past from the
    /// newest, which the list starts at.
    History {
        scroll: usize,
    },
    /// The table of the hosts to check `prefix h` opens. `selected` is the row the keys
    /// are on, and `open` records an Enter the switcher has yet to act on.
    Check {
        selected: usize,
        open: bool,
    },
    Palette {
        query: String,
        selected: usize,
        open: bool,
        decoder: crate::display::decode::KeyDecoder,
    },
    Input(Box<Input>),
}

/// True while a popup that takes every key is open.
pub(crate) fn is_reader(modal: &Option<Modal>) -> bool {
    matches!(
        modal,
        Some(
            Modal::Help { .. }
                | Modal::History { .. }
                | Modal::Check { .. }
                | Modal::Palette { .. }
        )
    )
}

/// True while a centered modal popup is open. Every modal is one today, so this
/// is `is_some()`; it stays a named predicate because callers ask the QUESTION
/// ("is a draggable popup on screen?"), not the representation.
pub(crate) fn is_popup_open(modal: &Option<Modal>) -> bool {
    modal.is_some()
}

/// True while an inline input (filter / new session) is open.
pub(crate) fn is_inputting(modal: &Option<Modal>) -> bool {
    matches!(modal, Some(Modal::Input(_)))
}

/// Which kind of modal is open. The focus machine derives its modal dimension from
/// this each loop-top, so focus can never mirror-and-desync from the open popup.
pub(crate) fn modal_kind(modal: &Option<Modal>) -> Option<ModalKind> {
    modal.as_ref().map(|_| ModalKind::Popup)
}

/// Feeds a raw key read to a read-only popup (the help or the history), tmux view-mode
/// style. While one is open every key is consumed (returns true, so nothing reaches the
/// nav or the terminal view). A lone Esc closes either. In the history `q` closes it too,
/// `↑`/`↓` (or `k`/`j`) scroll one record and `PgUp`/`PgDn` ten. The help is searched by
/// typing: a printable key extends the query and Backspace shortens it, each returning the
/// view to the top of what matches, and the arrows, `PgUp`/`PgDn`, and `Home`/`End`
/// scroll. Every other key is swallowed. Returns false when neither is open, so the read
/// falls through to normal routing.
pub(crate) fn feed_reader(modal: &mut Option<Modal>, bytes: &[u8]) -> bool {
    use ratatui::crossterm::event::KeyCode;
    if !is_reader(modal) {
        return false;
    }
    if let Some(Modal::Help {
        query,
        scroll,
        decoder,
    }) = modal
    {
        for key in decoder.feed(bytes) {
            match key.code {
                KeyCode::Esc => {
                    *modal = None;
                    return true;
                }
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = scroll.saturating_add(1),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::Home => *scroll = 0,
                KeyCode::End => *scroll = usize::MAX,
                KeyCode::Backspace => {
                    query.pop();
                    *scroll = 0;
                }
                // Ctrl-U clears the query, as it clears an input row.
                KeyCode::Char('\u{15}') => {
                    query.clear();
                    *scroll = 0;
                }
                KeyCode::Char(c) if !c.is_control() => {
                    query.push(c);
                    *scroll = 0;
                }
                _ => {}
            }
        }
        return true;
    }
    if let Some(Modal::Palette {
        query,
        selected,
        open,
        decoder,
    }) = modal
    {
        for key in decoder.feed(bytes) {
            match key.code {
                KeyCode::Esc => {
                    *modal = None;
                    return true;
                }
                KeyCode::Enter => *open = true,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = selected.saturating_add(1),
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                KeyCode::Char('\u{15}') => {
                    query.clear();
                    *selected = 0;
                }
                KeyCode::Char(c) if !c.is_control() => {
                    query.push(c);
                    *selected = 0;
                }
                _ => {}
            }
        }
        return true;
    }
    // `q`, or a real Esc (a lone ESC, not the ESC `[` that starts an arrow/CSI).
    let esc = bytes.contains(&0x1b) && !bytes.windows(2).any(|w| w == [0x1b, b'[']);
    if let Some(Modal::Check { selected, open }) = modal {
        if bytes.contains(&b'q') || esc {
            *modal = None;
            return true;
        }
        match bytes {
            b"k" | b"\x1b[A" => *selected = selected.saturating_sub(1),
            b"j" | b"\x1b[B" => *selected = selected.saturating_add(1),
            b"\r" | b"\n" => *open = true,
            _ => {}
        }
        return true;
    }
    if bytes.contains(&b'q') || esc {
        *modal = None;
        return true;
    }
    if let Some(Modal::History { scroll }) = modal {
        let step: isize = match bytes {
            b"k" | b"\x1b[A" => -1,
            b"j" | b"\x1b[B" => 1,
            b"\x1b[5~" => -10,
            b"\x1b[6~" => 10,
            _ => 0,
        };
        *scroll = scroll.saturating_add_signed(step);
    }
    true
}
