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
}

/// Chooses the terminal view screen from domain facts.
pub fn choose_view_screen(
    selected_source: Option<&str>,
    selected_address: Option<&Address>,
    failure: Option<FailureKind>,
    scanning: bool,
    empty: bool,
    own_session: Option<&Address>,
) -> Option<ViewScreen> {
    if selected_address.is_some() && selected_address == own_session {
        return Some(ViewScreen::SelfSession);
    }
    if selected_source.is_none() {
        return scanning.then_some(ViewScreen::Scanning);
    }
    match failure {
        Some(FailureKind::Blocked) => return Some(ViewScreen::Login),
        Some(FailureKind::ListFailed) => return Some(ViewScreen::ListFailed),
        Some(FailureKind::Unreachable) => return Some(ViewScreen::Unreachable),
        None => {}
    }
    if scanning {
        return Some(ViewScreen::Scanning);
    }
    empty.then_some(ViewScreen::Empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FailureKind;
    use crate::session::Address;

    fn address(source: &str, session: &str) -> Address {
        Address::new(source, session)
    }

    #[test]
    fn screen_choice_covers_each_settled_state() {
        let selected = address("prod", "work");

        assert_eq!(
            choose_view_screen(None, None, None, false, false, None),
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
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None),
            Some(ViewScreen::Empty)
        );
        assert_eq!(
            choose_view_screen(
                Some("prod"),
                Some(&selected),
                None,
                false,
                false,
                Some(&selected),
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
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(None, None, None, true, false, None),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None),
            Some(ViewScreen::Empty)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), Some(&selected), None, false, false, None),
            None
        );
    }
}
