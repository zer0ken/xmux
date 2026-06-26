//! The leaf value types a `Backend` method (or `Transport::lower_switch`) returns:
//! the transport-blind intent for moving a shared attachment (`SwitchPlan`), how a
//! session's death is detected (`DeathSignal`), where change events come from
//! (`EventSource`), and the captured display tty (`DisplayTty`). No logic — these
//! are the shapes the supervisor matches on. `DeathSignal` is defined HERE and
//! nowhere else; Phase 3's death wiring adds free helpers over this one enum.

/// TRANSPORT-BLIND intent: how (or whether) to move a host's ONE shared display
/// attachment onto a session. Produced by `Backend::switch_plan`; lowered to a runnable
/// command by `Transport::lower_switch`. Carries NO transport detail and NO tty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchPlan {
    /// Move the host's single shared client onto `session` (a `switch-client`).
    /// Shared (tmux), local OR remote — the `Transport` decides how to run it.
    Switch { session: String },
    /// This mux keeps one attachment PER SESSION — there is nothing to switch; the
    /// caller spawns/uses the per-session attachment directly. PerSession (psmux).
    PerSessionNoOp,
}

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

/// xmux's own display-client tty, captured in memory (not a `/tmp` file). Read by
/// `Backend::switch_client_argv` to build `switch-client -c <tty>`, and filtered against
/// by `DeathSignal::ControlNotice`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayTty(pub Option<String>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_plan_variants_are_distinct_and_carry_session() {
        assert_eq!(
            SwitchPlan::Switch {
                session: "api".into()
            },
            SwitchPlan::Switch {
                session: "api".into()
            }
        );
        assert_ne!(
            SwitchPlan::PerSessionNoOp,
            SwitchPlan::Switch {
                session: "api".into()
            }
        );
    }

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
