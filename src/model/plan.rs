//! The leaf value types a `Mux` method returns: how a session's death is
//! detected (`DeathSignal`), where change events come from (`EventSource`), and the
//! captured display tty (`DisplayTty`). No logic — these are the shapes the supervisor
//! matches on. `DeathSignal` is defined HERE and nowhere else; Phase 3's death wiring
//! adds free helpers over this one enum.

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
    /// No push stream; re-enumerate on this cadence.
    Poll { interval_ms: u64 },
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
    fn event_source_poll_carries_interval() {
        assert_eq!(
            EventSource::Poll { interval_ms: 1500 },
            EventSource::Poll { interval_ms: 1500 }
        );
        assert_ne!(
            EventSource::Control,
            EventSource::Poll { interval_ms: 1500 }
        );
    }

    #[test]
    fn display_tty_default_is_none() {
        assert_eq!(DisplayTty::default(), DisplayTty(None));
    }
}
