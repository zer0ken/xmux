//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the cockpit loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
use crate::cockpit::Selection;
use crate::session::WindowPanes;
use crate::ui::tree::Group;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The cockpit's canonical runtime state.
#[derive(Default)]
pub struct State {
    /// Inventory — hosts → sessions → windows → panes (all reachable). The single
    /// source of truth every component reads, instead of reaching into the tree.
    // ponytail: flat fields, not an Inventory sub-struct — bundle them if a reader
    // ever needs the whole group at once.
    pub groups: Vec<Group>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
    /// Sources whose `list-sessions` has not yet returned (host shows scanning…).
    pub scanning: HashSet<String>,
    /// Session addresses whose `list-panes` has resolved (success or failure).
    pub panes_loaded: HashSet<String>,
    /// Active fuzzy-filter text (drives the visible tree + the footer).
    pub filter: String,
    /// What the tree cursor points at — the session/window to show.
    pub selection: Selection,
    /// The address whose content is confirmed live in the on-screen terminal view —
    /// the single display truth. The grid is shown only when this matches the
    /// selection's session, so a stale attachment (mid-reattach) renders
    /// "(attaching…)" rather than the previous session: displayed == selected holds
    /// structurally. Set only at confirmation (a synchronous switch, or DisplayReady).
    pub displayed: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// A selection moved and has not yet armed its debounce deadline. The next
    /// [`Action::Tick`] (re)arms `attach_deadline` from this — re-armed on EVERY
    /// pending selection so rapid navigation coalesces into one trailing attach
    /// instead of a per-step storm of switch-client repaints (the freeze).
    pub attach_pending: bool,
    /// The session address last persisted as the user's last-selected, so it is
    /// not rewritten on every window step within the same session.
    pub last_saved_session: String,
    /// The cockpit's focus state machine — which pane keys go to and whether a
    /// modal is open. The single source of truth for focus.
    pub focus: crate::proxy::app::Focus,
    /// The single open modal, if any (help / inline input / kill confirm / context
    /// menu). One Option — not four independent fields — so the modals' mutual
    /// exclusion is structural: opening one drops whatever was open, and two can
    /// never coexist. The switcher owns the modal behavior and the transient popup
    /// geometry (drag offset / drawn rect); this owns which modal is open + its content.
    pub(crate) popup: Option<crate::ui::switcher::Popup>,
}

impl State {
    /// True while a centered modal popup (help / inline input / kill confirm) is
    /// open. These three are draggable and drive [`ModalKind::Popup`]; the context
    /// menu is separate (pointer-anchored).
    ///
    /// [`ModalKind::Popup`]: crate::proxy::app::ModalKind::Popup
    pub fn is_modal_popup_open(&self) -> bool {
        use crate::ui::switcher::Popup;
        matches!(
            self.popup,
            Some(Popup::Help | Popup::Input(_) | Popup::Kill(_))
        )
    }

    /// True while an inline input (filter / rename / new) is open. The cockpit
    /// routes every key to the switcher then, with no focus-switch hijack.
    pub fn is_inputting(&self) -> bool {
        matches!(self.popup, Some(crate::ui::switcher::Popup::Input(_)))
    }

    /// True while the right-click context menu is open.
    pub fn menu_active(&self) -> bool {
        matches!(self.popup, Some(crate::ui::switcher::Popup::Menu(_)))
    }

    /// Which kind of modal is open — the focus machine derives its modal dimension
    /// from this each loop-top, so [`Focus`] can never mirror-and-desync from the
    /// open popup. A centered popup and the context menu are mutually exclusive.
    ///
    /// [`Focus`]: crate::proxy::app::Focus
    pub(crate) fn modal_kind(&self) -> Option<crate::proxy::app::ModalKind> {
        use crate::proxy::app::ModalKind;
        use crate::ui::switcher::Popup;
        match self.popup {
            Some(Popup::Help | Popup::Input(_) | Popup::Kill(_)) => Some(ModalKind::Popup),
            Some(Popup::Menu(_)) => Some(ModalKind::Menu),
            None => None,
        }
    }

    /// Builds the inventory from a complete snapshot: every host is resolved
    /// (reachable or unreachable per its `err`) and every session's panes are
    /// considered known. Other state fields stay default.
    pub fn from_scan(scan: crate::ui::switcher::Scan) -> State {
        let panes_loaded = scan
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|sess| sess.address()))
            .collect();
        State {
            groups: scan.groups,
            panes: scan.panes,
            panes_loaded,
            ..State::default()
        }
    }

    /// Seeds the inventory from the resolved source list alone — no probing — so
    /// the first frame paints host-skeleton rows, each in a scanning state. Other
    /// state fields stay default.
    pub fn from_sources(aliases: Vec<String>) -> State {
        let scanning = aliases.iter().cloned().collect();
        let groups = aliases
            .into_iter()
            .map(|source| Group {
                source,
                err: None,
                sessions: Vec::new(),
            })
            .collect();
        State {
            scanning,
            groups,
            ..State::default()
        }
    }

    /// The single domain-mutation site. Folds one [`Action`] into the state and
    /// returns the side effects to run as [`Command`]s. `apply` touches only `State`
    /// — every external effect (switcher cursor move, attach, prefs persist, quit)
    /// is returned for the run loop to dispatch, so the intent → state → effect flow
    /// has exactly one mutation point.
    ///
    /// The clock and the runtime attach facts enter ONLY as data on [`Action::Tick`]
    /// (`now`/`key_live`/`in_flight`); `apply` never reads `Instant::now()` or any
    /// registry/host state itself.
    ///
    /// [`Action`]: crate::model::Action
    /// [`Command`]: crate::model::Command
    pub fn apply(&mut self, action: crate::model::Action) -> Vec<crate::model::Command> {
        use crate::model::{Action, Command, FocusTarget};
        use std::time::Duration;
        match action {
            Action::Switch { address } => vec![Command::SelectAddress(address)],
            Action::Focus(FocusTarget::Terminal) => {
                self.focus
                    .set_pane_focus(crate::proxy::app::PaneFocus::Terminal);
                Vec::new()
            }
            Action::Focus(FocusTarget::Tree) => {
                self.focus
                    .set_pane_focus(crate::proxy::app::PaneFocus::Tree);
                Vec::new()
            }
            Action::Rescan => vec![Command::Rescan],
            Action::TreeWidth(d) => vec![Command::AdjustTreeWidth(d)],
            Action::ToggleAutoHide => vec![Command::ToggleAutoHide],
            Action::Quit => vec![Command::Quit],
            Action::Select(target) => {
                // Mark the attach pending; do NOT arm the deadline or attach here.
                // The trailing Tick arms the debounce, so rapid navigation coalesces.
                self.selection = target;
                self.attach_pending = true;
                Vec::new()
            }
            Action::Tick {
                now,
                key_live,
                in_flight,
            } => {
                // RE-ARM on every pending selection: a fresh Select between ticks
                // pushes the deadline out, so only the trailing selection attaches
                // (one switch, not a per-step storm — the freeze fix). Re-arming and
                // firing are mutually exclusive on a tick: a just-armed deadline is
                // always in the future, so the elapsed check below cannot fire it.
                if self.attach_pending {
                    self.attach_pending = false;
                    self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    return Vec::new();
                }
                // The debounce deadline has elapsed.
                if self.attach_deadline.is_none_or(|d| now < d) {
                    return Vec::new();
                }
                self.attach_deadline = None;
                if self.selection.is_empty() {
                    return Vec::new();
                }
                let mut cmds = Vec::new();
                // Persist the settled session as last-selected — INDEPENDENT of the
                // attach gate, so it records even when the attach is suppressed (e.g.
                // an in-flight attach on the same shared host while the cursor moves to
                // another of its sessions). Only on an address change, so stepping
                // between windows of one session does not rewrite it.
                let addr = self.selection.address();
                if addr != self.last_saved_session {
                    self.last_saved_session = addr.clone();
                    cmds.push(Command::PersistLastSession(addr));
                }
                // Fire the attach only when the gate holds (selection differs from the
                // confirmed display, or its PTY is gone) and nothing is in flight — the
                // freeze invariant depends on this gate, so it stays exactly as is.
                if self.should_attach(key_live, in_flight) {
                    cmds.push(Command::Attach(self.selection.clone()));
                }
                cmds
            }
        }
    }

    /// Whether to (re)issue an attach for the settled selection. Fire when the
    /// selection differs from what is confirmed on screen, or when its display PTY is
    /// gone (`!key_live` — exited / reaped while the cursor was elsewhere) — but never
    /// while an attach for the key is already in flight, so the async-attach window
    /// cannot spawn a storm of duplicates. The clock and these runtime facts enter as
    /// data on the Tick, never read here directly.
    pub(crate) fn should_attach(&self, key_live: bool, in_flight: bool) -> bool {
        (self.selection != self.displayed || !key_live) && !in_flight
    }

    /// Whether the on-screen grid may be shown: the confirmed display truth must name
    /// the same host+session as the current selection. Window is ignored — the same
    /// session's PTY renders whatever window the mux has active, so a window step needs
    /// no "(attaching…)". A mismatch (mid-reattach, or selection moved off the displayed
    /// session) renders "(attaching…)" instead of the previous session — defect A:
    /// displayed == selected, structurally.
    pub fn display_matches_selection(&self) -> bool {
        !self.selection.is_empty()
            && self.displayed.source == self.selection.source
            && self.displayed.session == self.selection.session
    }
}

/// Debounce before a settled cursor move attaches/switches its session+window.
/// Rapid navigation must NOT switch-client / select-window per step: each switch
/// makes the remote mux send a full-screen repaint, and a storm of repaints floods
/// the draw — the single-threaded loop then spends all its time redrawing, which IS
/// the freeze. Deferring the attach until the cursor settles keeps per-step redraws
/// to a cheap tree-only diff. The single source of this value; the cockpit's
/// host-event re-arm paths reference it so the two can never drift.
pub(crate) const ATTACH_DEBOUNCE_MS: u64 = 90;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Selection;
    use crate::model::{Action, Command, FocusTarget};
    use crate::proxy::app::Focus;
    use std::time::Duration;

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.displayed.is_empty());
        assert!(s.attach_deadline.is_none());
        assert!(!s.attach_pending);
        assert_eq!(s.last_saved_session, "");
        assert!(s.focus.is_tree_focused());
        assert!(s.popup.is_none());
        assert!(!s.is_modal_popup_open());
        assert!(!s.is_inputting());
        assert!(!s.menu_active());
        assert!(s.modal_kind().is_none());
    }

    fn sel(session: &str) -> Selection {
        Selection {
            source: "jup".into(),
            session: session.into(),
            window: None,
        }
    }

    #[test]
    fn apply_select_sets_selection_marks_pending_and_emits_no_command() {
        let mut s = State::default();
        let cmds = s.apply(Action::Select(sel("api")));
        assert_eq!(s.selection, sel("api"));
        assert!(s.attach_pending, "Select marks the attach pending");
        assert!(
            s.attach_deadline.is_none(),
            "Select does NOT arm the deadline — the trailing Tick does"
        );
        assert!(cmds.is_empty(), "Select emits no attach command");
    }

    #[test]
    fn apply_tick_arms_then_fires_one_attach_after_debounce() {
        let mut s = State::default();
        let t0 = Instant::now();
        s.apply(Action::Select(sel("api")));
        // Tick at t0 arms the deadline (no fire yet — now < deadline).
        let armed = s.apply(Action::Tick {
            now: t0,
            key_live: true,
            in_flight: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        // Tick at t0+90ms (deadline reached) with no intervening Select fires once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: true,
            in_flight: false,
        });
        assert_eq!(
            fired,
            vec![
                Command::PersistLastSession("jup/api".into()),
                Command::Attach(sel("api")),
            ],
            "the settled selection attaches exactly once"
        );
        assert!(
            s.attach_deadline.is_none(),
            "the deadline is cleared on fire"
        );
    }

    #[test]
    fn apply_select_between_ticks_rearms_so_rapid_nav_does_not_fire_early() {
        let mut s = State::default();
        let t0 = Instant::now();
        s.apply(Action::Select(sel("api")));
        s.apply(Action::Tick {
            now: t0,
            key_live: true,
            in_flight: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        // A Select 30ms later (rapid nav) re-marks pending; the next Tick re-arms the
        // deadline PAST the original, so the original deadline does not fire.
        s.apply(Action::Select(sel("db")));
        let rearm = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(30),
            key_live: true,
            in_flight: false,
        });
        assert!(
            rearm.is_empty(),
            "re-arming a moved selection does not fire"
        );
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(30 + 90)),
            "the deadline is pushed out by the re-arm"
        );
        // At the ORIGINAL deadline (t0+90) the now-later deadline (t0+120) has not
        // elapsed → no premature fire.
        let early = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: true,
            in_flight: false,
        });
        assert!(early.is_empty(), "no fire before the re-armed deadline");
        // Only at t0+120 does the trailing selection (db) attach, once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(120),
            key_live: true,
            in_flight: false,
        });
        assert_eq!(
            fired,
            vec![
                Command::PersistLastSession("jup/db".into()),
                Command::Attach(sel("db")),
            ],
            "only the trailing selection attaches"
        );
    }

    #[test]
    fn apply_tick_does_not_fire_when_already_displayed_and_live() {
        // should_attach gate: selection == displayed AND key_live AND not in_flight
        // ⇒ nothing to do (already persisted, so no persist command either).
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(), // already persisted → no persist command
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            key_live: true,
            in_flight: false,
        });
        assert!(
            cmds.is_empty(),
            "no attach when the selection is already the confirmed display and live"
        );
        assert!(
            s.attach_deadline.is_none(),
            "the elapsed deadline is cleared"
        );
    }

    #[test]
    fn apply_tick_fires_recovery_when_display_pty_gone() {
        // should_attach gate: selection == displayed but its PTY is not live ⇒ re-attach.
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(), // already persisted → no persist command
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
        });
        assert_eq!(
            cmds,
            vec![Command::Attach(sel("api"))],
            "a vanished display PTY re-attaches even with an unchanged selection"
        );
    }

    #[test]
    fn apply_tick_does_not_fire_attach_while_in_flight_but_still_persists() {
        // The attach is suppressed while one is in flight (no storm), but the settled
        // session is still recorded as last-selected — the persist is independent of
        // the attach gate.
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("db"),
            displayed: sel("api"),
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: true,
        });
        assert!(
            !cmds.iter().any(|c| matches!(c, Command::Attach(_))),
            "never spawn a second attach while one is already in flight"
        );
        assert_eq!(
            cmds,
            vec![Command::PersistLastSession("jup/db".into())],
            "the settled session is still persisted while the attach is suppressed"
        );
    }

    #[test]
    fn apply_tick_persists_second_session_of_same_host_while_first_attach_in_flight() {
        // Differential parity: settle on B → B attaches (its attach now in flight on
        // the shared host key) → settle on C of the SAME host → its Tick sees the key
        // still in flight, so the attach is suppressed, but C MUST still be persisted
        // as last-selected (else the next launch wrongly restores B).
        let mut s = State::default();
        let t0 = Instant::now();
        // Settle on B and let its attach fire (no in-flight yet, B differs from the
        // empty displayed).
        s.apply(Action::Select(sel("b")));
        s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
        }); // arms
        let b_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: false,
            in_flight: false,
        });
        assert_eq!(
            b_cmds,
            vec![
                Command::PersistLastSession("jup/b".into()),
                Command::Attach(sel("b")),
            ],
        );
        // Move to C of the same host while B's attach is still in flight.
        s.apply(Action::Select(sel("c")));
        s.apply(Action::Tick {
            now: t0 + Duration::from_millis(100),
            key_live: false,
            in_flight: true,
        }); // arms
        let c_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(190),
            key_live: false,
            in_flight: true, // first attach (B) still in flight on the shared host key
        });
        assert!(
            c_cmds.contains(&Command::PersistLastSession("jup/c".into())),
            "C must be persisted even though its attach is suppressed by in_flight: {c_cmds:?}"
        );
        assert!(
            !c_cmds.iter().any(|c| matches!(c, Command::Attach(_))),
            "C's attach is suppressed while B's attach is in flight (no storm): {c_cmds:?}"
        );
        assert_eq!(s.last_saved_session, "jup/c");
    }

    #[test]
    fn apply_tick_with_empty_selection_does_nothing() {
        let t0 = Instant::now();
        let mut s = State {
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
        });
        assert!(cmds.is_empty(), "empty selection never attaches");
        assert!(s.attach_deadline.is_none());
    }

    #[test]
    fn apply_focus_moves_focus_with_no_command() {
        let mut s = State::default();
        assert!(s.focus.is_tree_focused());
        assert!(s.apply(Action::Focus(FocusTarget::Terminal)).is_empty());
        assert_eq!(s.focus, Focus::Terminal);
        assert!(s.apply(Action::Focus(FocusTarget::Tree)).is_empty());
        assert_eq!(s.focus, Focus::Tree);
    }

    #[test]
    fn apply_switch_emits_select_address_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::Switch {
                address: "jup/db".into()
            }),
            vec![Command::SelectAddress("jup/db".into())]
        );
    }

    #[test]
    fn apply_rescan_emits_rescan_command() {
        let mut s = State::default();
        assert_eq!(s.apply(Action::Rescan), vec![Command::Rescan]);
    }

    #[test]
    fn apply_tree_width_emits_adjust_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::TreeWidth(-2)),
            vec![Command::AdjustTreeWidth(-2)]
        );
    }

    #[test]
    fn apply_toggle_auto_hide_emits_toggle_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::ToggleAutoHide),
            vec![Command::ToggleAutoHide]
        );
    }

    #[test]
    fn apply_quit_emits_quit_command() {
        let mut s = State::default();
        assert_eq!(s.apply(Action::Quit), vec![Command::Quit]);
    }
}
