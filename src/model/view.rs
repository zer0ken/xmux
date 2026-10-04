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
}

/// Chooses the terminal view screen from domain facts. A confirmed display keeps
/// its grid during a scan; only a scan without one receives the animation.
pub fn choose_view_screen(
    selected_source: Option<&str>,
    selected_address: Option<&Address>,
    failure: Option<FailureKind>,
    scanning: bool,
    empty: bool,
    own_session: Option<&Address>,
    // Whether a session has already been confirmed into the terminal view.
    confirmed_display: bool,
) -> Option<ViewScreen> {
    if selected_address.is_some() && selected_address == own_session {
        return Some(ViewScreen::SelfSession);
    }
    if selected_source.is_none() {
        return (scanning && !confirmed_display).then_some(ViewScreen::Scanning);
    }
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
        return (!confirmed_display).then_some(ViewScreen::Scanning);
    }
    Some(if empty {
        ViewScreen::Empty
    } else {
        ViewScreen::HostInfo
    })
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
            choose_view_screen(None, None, None, false, false, None, false),
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
                false,
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
                false,
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None, false),
            Some(ViewScreen::Empty)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, false, None, true),
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
                false,
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
                false,
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
                false,
            ),
            Some(ViewScreen::Unreachable)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, false),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(None, None, None, true, false, None, false),
            Some(ViewScreen::Scanning)
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, false, true, None, false),
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
                false
            ),
            None
        );
        assert_eq!(
            choose_view_screen(Some("prod"), Some(&selected), None, true, true, None, false),
            None,
            "a scanning host must not replace a selected session"
        );
        assert_eq!(
            choose_view_screen(Some("prod"), None, None, true, true, None, true),
            None,
            "a full rescan keeps the confirmed display"
        );
    }
}
