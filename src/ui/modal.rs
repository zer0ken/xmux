//! The switcher's modal surfaces: the single open-modal enum ([`Modal`]) and its
//! variants (help / inline input / kill confirm / context menu), plus the data
//! types they carry. The switcher owns the modal *behavior* and the transient
//! popup geometry; this module owns the modal data model. Side-effect-free.

use ratatui::layout::Rect;

use crate::session::Session;
use crate::ui::tree::RowRef;

/// An armed kill confirm (awaiting y/n). One slot enforces "at most one armed".
#[derive(Debug, Clone)]
pub(crate) enum PendingKill {
    Session(Session),
    /// (source, session, target="session:window")
    Window {
        source: String,
        session: String,
        target: String,
    },
}

/// One context-menu entry. The variant drives the action taken on release; the
/// label is the row text. Words match the rest of the tree UI ("focus the terminal",
/// "new", "rename", "kill" — never "open"/"split", which are not used elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuItem {
    Focus,
    NewSession,
    NewWindow,
    Rename,
    Kill,
}

impl MenuItem {
    pub(crate) fn label(self) -> &'static str {
        match self {
            MenuItem::Focus => "focus",
            MenuItem::NewSession => "new session",
            MenuItem::NewWindow => "new window",
            MenuItem::Rename => "rename",
            MenuItem::Kill => "kill",
        }
    }
}

/// What the app must do after a menu release. Most items are handled inside the
/// switcher (they open an input or arm a kill); `FocusTerminal` is the one outcome the
/// app owns (the "focus" item moves focus to the terminal view).
pub enum MenuOutcome {
    None,
    Handled,
    FocusTerminal,
}

/// An open right-click context menu. `target` is the node it acts on, re-located by
/// identity at release so a tree rebuild during the brief hold cannot misfire on a
/// stale row. `title` names that node (shown in the box's top border, like tmux's
/// menu title — so the menu reads as "actions for <this node>"). `rect` is the
/// bordered box in 0-based screen coords; `hovered` is the highlighted item.
pub(crate) struct Menu {
    pub(crate) target: RowRef,
    pub(crate) title: String,
    pub(crate) rect: Rect,
    pub(crate) items: Vec<MenuItem>,
    pub(crate) hovered: Option<usize>,
}

/// An active border-drag of a modal popup: the grabbed screen cell and the
/// `popup_offset` at grab time, so motion can compute the new offset.
#[derive(Clone, Copy)]
pub(crate) struct PopupDrag {
    pub(crate) grab: (u16, u16),
    pub(crate) origin: (i16, i16),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Filter,
    New,
    NewWindow,
    SplitWindow,
    Rename,
}

pub(crate) struct Input {
    pub(crate) mode: InputMode,
    pub(crate) label: String,
    pub(crate) buffer: String,
    /// The create source / rename target captured when the input opened, so the
    /// action lands on the node the user was on — not wherever streaming results
    /// moved the selection by the time they pressed Enter.
    pub(crate) source: Option<String>,
    pub(crate) sess: Option<Session>,
    /// The split target (`session:window`) for [`InputMode::SplitWindow`].
    pub(crate) target: Option<String>,
}

/// The single open modal, if any — at most one of help / inline input / kill
/// confirm / context menu. Modeling it as one `Option` (not four independent
/// fields) makes the modals' mutual exclusion structural: opening one drops
/// whatever was open, and the compiler guarantees two can never coexist, so the
/// hand-maintained "clear the others" invariant cannot drift. Lives on
/// [`crate::state::State`]; the switcher owns only the behavior and the transient
/// popup geometry (drag offset / drawn rect).
pub(crate) enum Modal {
    Help,
    Input(Input),
    Kill(PendingKill),
    Menu(Menu),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_help_variant_constructs() {
        let m = Modal::Help;
        assert!(matches!(m, Modal::Help));
    }
}
