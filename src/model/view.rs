use crate::model::FailureKind;
use crate::session::Address;

/// The screen that fills the terminal view in place of a mux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewScreen {
    /// A selected host is being scanned, or the initial scan has no selected card yet.
    Scanning,
    /// The session xmux runs in. Mirroring it would attach a second client to the session
    /// holding xmux, move the user's client, and paint xmux inside itself.
    SelfSession,
    /// The host could not be reached.
    Unreachable,
    /// The host failed in a way the login pane can answer.
    Login,
    /// The host answered, but its session listing could not be parsed.
    ListFailed,
    /// The host answered and serves no session.
    Empty,
    /// A selected source section with sessions to inspect.
    HostInfo,
    /// A host that answered through at least one of its sources: how it is reached and
    /// which sources it serves.
    Host,
    /// The root of the hierarchy, shown from launch until the user first executes a
    /// target: the scan progress and every card in nav order. The selection highlights on
    /// it and attaches nothing.
    Landing,
}

/// The session confirmed into the terminal view, as the screen choice reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedDisplay<'a> {
    pub address: &'a Address,
    /// A full re-scan turned this session's card into its host card, and the selection
    /// has not moved since. Only then may a scanning host card keep this session's grid.
    pub collapsed_into_selection: bool,
}

/// Chooses the terminal view screen from domain facts; rendering paints the screen this
/// returns and never chooses one itself. A selection without a session (a source's or
/// a host's card) gets that card's screen, never a session's grid. A selected host card
/// that is scanning shows its scanning screen, never another source's grid. The one exception
/// is a full re-scan that collapsed the selected session card into its own host card:
/// that session's grid stays until the selection moves.
pub fn choose_view_screen(
    selected_source: Option<&str>,
    selected_address: Option<&Address>,
    failure: Option<FailureKind>,
    scanning: bool,
    empty: bool,
    own_session: Option<&Address>,
    displayed: Option<ConfirmedDisplay<'_>>,
) -> Option<ViewScreen> {
    if selected_address.is_some() && selected_address == own_session {
        return Some(ViewScreen::SelfSession);
    }
    let Some(source) = selected_source else {
        return (scanning && displayed.is_none()).then_some(ViewScreen::Scanning);
    };
    match failure {
        Some(FailureKind::Blocked) => return Some(ViewScreen::Login),
        Some(FailureKind::ListFailed) => return Some(ViewScreen::ListFailed),
        Some(FailureKind::Unreachable) => return Some(ViewScreen::Unreachable),
        None => {}
    }
    if selected_address.is_some() {
        return None;
    }
    if scanning {
        let kept =
            displayed.is_some_and(|d| d.collapsed_into_selection && d.address.source == source);
        return (!kept).then_some(ViewScreen::Scanning);
    }
    Some(if empty {
        ViewScreen::Empty
    } else {
        ViewScreen::HostInfo
    })
}

/// Chooses a host's screen from the state of the host as a whole. `failure` is the
/// failure every one of its sources shares (a host is down only when none of its sources
/// connected), and `scanning` says every source is still waiting on its first answer. A
/// host that is neither is reachable, whatever each source answered.
pub fn choose_host_screen(failure: Option<FailureKind>, scanning: bool) -> ViewScreen {
    match failure {
        Some(FailureKind::Blocked) => ViewScreen::Login,
        Some(FailureKind::Unreachable | FailureKind::ListFailed) => ViewScreen::Unreachable,
        None if scanning => ViewScreen::Scanning,
        None => ViewScreen::Host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FailureKind;
    use crate::session::Address;

    fn address(source: &str, session: &str) -> Address {
        Address::new(source, session)
    }

    fn shown(address: &Address) -> Option<ConfirmedDisplay<'_>> {
        Some(ConfirmedDisplay {
            address,
            collapsed_into_selection: false,
        })
    }

    fn collapsed(address: &Address) -> Option<ConfirmedDisplay<'_>> {
        Some(ConfirmedDisplay {
            address,
            collapsed_into_selection: true,
        })
    }

    #[test]
    fn screen_choice_covers_each_settled_state() {
        let selected = address("prod", "work");
        let other = address("local", "edit");

        assert_eq!(
            choose_view_screen(None, None, None, false, false, None, None),
            None
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                None,
                Some(FailureKind::Blocked),
                false,
                false,
                None,
                None,
            ),
            Some(ViewScreen::Login)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                None,
                Some(FailureKind::Unreachable),
                false,
                false,
                None,
                None,
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None, None),
            Some(ViewScreen::Empty)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, false, None, shown(&other)),
            Some(ViewScreen::HostInfo)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                Some(&selected),
                None,
                false,
                false,
                Some(&selected),
                None,
            ),
            Some(ViewScreen::SelfSession)
        );
    }

    #[test]
    fn screen_choice_preserves_precedence() {
        let selected = address("prod", "work");

        assert_eq!(
            choose_view_screen(
                Some("prod"),
                Some(&selected),
                Some(FailureKind::Blocked),
                true,
                true,
                Some(&selected),
                None,
            ),
            Some(ViewScreen::SelfSession)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                None,
                Some(FailureKind::Unreachable),
                true,
                true,
                None,
                None,
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, None),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None, None),
            Some(ViewScreen::Empty)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                Some(&selected),
                None,
                false,
                false,
                None,
                None
            ),
            None
        );
        assert_eq!(
            choose_view_screen(Some("prod"), Some(&selected), None, true, true, None, None),
            None,
            "a scanning host must not replace a selected session"
        );
    }

    #[test]
    fn a_scanning_host_card_never_shows_another_sources_grid() {
        let other = address("local", "edit");
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, shown(&other)),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                None,
                None,
                true,
                true,
                None,
                collapsed(&other)
            ),
            Some(ViewScreen::Scanning),
            "a collapse into another source's card keeps nothing"
        );
    }

    #[test]
    fn a_full_rescan_collapse_keeps_its_own_session_grid() {
        let work = address("prod", "work");
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, collapsed(&work)),
            None,
            "the selection still sits where the re-scan collapsed it"
        );
    }

    #[test]
    fn a_moved_selection_ends_the_collapse_exception() {
        let work = address("prod", "work");
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, shown(&work)),
            Some(ViewScreen::Scanning),
            "the same source's grid without the collapse is not kept"
        );
    }

    #[test]
    fn a_host_screen_follows_the_host_as_a_whole() {
        assert_eq!(
            choose_host_screen(Some(FailureKind::Blocked), false),
            ViewScreen::Login
        );
        assert_eq!(
            choose_host_screen(Some(FailureKind::Unreachable), true),
            ViewScreen::Unreachable
        );
        assert_eq!(choose_host_screen(None, true), ViewScreen::Scanning);
        assert_eq!(choose_host_screen(None, false), ViewScreen::Host);
    }

    #[test]
    fn the_initial_scan_without_a_card_animates() {
        let other = address("local", "edit");
        assert_eq!(
            choose_view_screen(None, None, None, true, false, None, None),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(None, None, None, true, false, None, shown(&other)),
            None,
            "a confirmed display without a selected card keeps its grid"
        );
    }
}
