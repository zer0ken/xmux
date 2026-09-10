//! The leaf value types a `Mux` method returns: how a session's death is
//! detected (`DeathSignal`), where change events come from (`EventSource`), and the
//! captured display tty (`DisplayTty`). No logic — these are the shapes the supervisor
//! matches on. `DeathSignal` is defined HERE and nowhere else; the death-as-a-push
//! helpers in `model::death` build over this one enum.

/// How a Host detects that a displayed session/attachment died, so a `switch-client`
/// is never aimed at a detached/dead tty (the blank-pane class). One PUSH per mux.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeathSignal {
    /// The session's display PTY hit master EOF. PerSession (psmux): the attachment
    /// dying IS the session dying.
    Eof,
    /// Also watch `~/.psmux/<name>.port`; its disappearance means the per-session
    /// server is gone even if a stale PTY lingers. PerSession.
    PathStat { dir_is_psmux_registry: bool },
    /// tmux's `%client-detached <client_tty>` control NOTICE, filtered against the
    /// host's captured display tty (an unrelated client's detach is ignored). Shared.
    ControlNotice,
}

/// Where a host's session/window change events come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventSource {
    /// A live `-CC` control-mode child pushes `%`-notices.
    Control,
    /// No push stream. The host is enumerated when something asks for it - the launch
    /// scan, a user re-scan, or a login - and never on a cadence of its own.
    Poll,
}

/// xmux's own display-client tty, captured in memory (not a `/tmp` file). Passed to
/// `Mux::switch_in_place` so its `SwitchPlan` targets xmux's display client, and filtered
/// against by `DeathSignal::ControlNotice`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayTty(pub Option<String>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn death_signal_variants_are_distinct() {
        assert_ne!(DeathSignal::Eof, DeathSignal::ControlNotice);
        assert_eq!(
            DeathSignal::PathStat {
                dir_is_psmux_registry: true
            },
            DeathSignal::PathStat {
                dir_is_psmux_registry: true
            }
        );
    }

    #[test]
    fn event_source_names_a_channel_and_carries_no_cadence() {
        // Poll is a channel KIND, not a schedule: it holds no interval, so nothing
        // downstream can read one out of it and start re-enumerating on its own.
        assert_eq!(EventSource::Poll, EventSource::Poll);
        assert_ne!(EventSource::Control, EventSource::Poll);
    }

    #[test]
    fn display_tty_default_is_none() {
        assert_eq!(DisplayTty::default(), DisplayTty(None));
    }
}
