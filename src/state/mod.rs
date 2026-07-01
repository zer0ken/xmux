//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the cockpit loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
use crate::app::cockpit::Selection;
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
    /// the single display truth, and the target of both rendering and input. The
    /// terminal view always shows THIS session's grid; on a switch it stays on the
    /// prior session until the new one is confirmed (stale-while-revalidate), then
    /// advances. Set only at confirmation (a synchronous in-place switch, or
    /// DisplayReady). Empty before the first confirmation → the view is blank.
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
    pub focus: crate::app::focus::Focus,
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
    /// [`ModalKind::Popup`]: crate::app::focus::ModalKind::Popup
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
    /// [`Focus`]: crate::app::focus::Focus
    pub(crate) fn modal_kind(&self) -> Option<crate::app::focus::ModalKind> {
        use crate::app::focus::ModalKind;
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
        use crate::model::{Action, Command, FocusTarget, MuxOp};
        use std::time::Duration;
        match action {
            Action::Switch { address } => vec![Command::SelectAddress(address)],
            Action::Focus(FocusTarget::Terminal) => {
                self.focus
                    .set_pane_focus(crate::app::focus::PaneFocus::Terminal);
                Vec::new()
            }
            Action::Focus(FocusTarget::Tree) => {
                self.focus
                    .set_pane_focus(crate::app::focus::PaneFocus::Tree);
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
            // Session-lifecycle intents are pure effect emitters: they fold into the
            // MuxOp the run loop runs off-loop. `apply` mutates no domain state — the
            // inventory change arrives later as the OpResult.
            Action::CreateSession { source, name } => {
                vec![Command::RunOp(MuxOp::Create { source, name })]
            }
            Action::NewWindow {
                source,
                session,
                name,
            } => vec![Command::RunOp(MuxOp::NewWindow {
                source,
                session,
                name,
            })],
            Action::SplitWindow {
                source,
                target,
                session,
                vertical,
            } => vec![Command::RunOp(MuxOp::SplitWindow {
                source,
                target,
                session,
                vertical,
            })],
            Action::RenameSession { sess, new_name } => {
                vec![Command::RunOp(MuxOp::Rename { sess, new_name })]
            }
            Action::KillSession { sess } => vec![Command::RunOp(MuxOp::Kill { sess })],
            Action::KillWindow {
                source,
                session,
                target,
            } => vec![Command::RunOp(MuxOp::KillWindow {
                source,
                session,
                target,
            })],
            Action::RenameWindow {
                source,
                session,
                target,
                new_name,
            } => vec![Command::RunOp(MuxOp::RenameWindow {
                source,
                session,
                target,
                new_name,
            })],
        }
    }

    /// The single event-driven mutation site: folds one backend [`HostEvent`] into the
    /// domain state and returns the backend follow-ups as [`EventEffect`]s. The mirror
    /// of [`apply`](State::apply) for the inbound (backend → state) direction — every
    /// `%`-notification, metadata reply, poll result, and reap routes through here, so
    /// State owns the event-driven mutations just as `apply` owns the intent-driven ones.
    ///
    /// `apply_event` performs only the mutations whose data is SELF-CONTAINED in the
    /// event (the active-window marker, a pane subtree, a poll enumeration, the
    /// unreachable mark) — driven through the switcher, which rebuilds the tree against
    /// `&mut State`. The follow-ups that need a backend handle the state layer must not
    /// hold (a host client's inventory lock, a control-mode probe, the attach registry,
    /// the detection dispatch) are returned as [`EventEffect`]s for the run loop — the
    /// sole executor — to carry out (the AGENTS rule: no IO/registry mutation here).
    ///
    /// `connected` (the run loop's once-connected set) enters as data, like the clock on
    /// `Tick`: an `Exited` of a once-connected host is a transient drop that keeps the
    /// last-known tree; otherwise it resolves the host's real state.
    ///
    /// [`HostEvent`]: crate::host::HostEvent
    /// [`EventEffect`]: crate::model::EventEffect
    pub fn apply_event(
        &mut self,
        ev: crate::host::HostEvent,
        switcher: &mut crate::ui::switcher::Switcher,
        connected: &mut std::collections::HashSet<String>,
    ) -> Vec<crate::model::EventEffect> {
        use crate::host::HostEvent;
        use crate::model::EventEffect;
        match ev {
            HostEvent::Connected { host } | HostEvent::Inventory { host } => {
                // The inventory lives behind the host client's lock the state layer
                // cannot reach; record the connected mark and let the loop apply it.
                connected.insert(host.clone());
                vec![EventEffect::ApplyInventory { host }]
            }
            HostEvent::Changed { host } => vec![EventEffect::Refetch { host }],
            HostEvent::ActiveWindowChanged {
                host,
                session_id,
                window_id: _,
            } => {
                // Probe the SPECIFIC session the payload names (its tmux id) — never a
                // guessed displayed session. The reply resolves the session name + new
                // active window index and drives the marker for THAT session.
                vec![EventEffect::ProbeActiveWindow {
                    host,
                    session_ref: session_id,
                }]
            }
            HostEvent::Focus {
                host,
                session,
                window,
            } => {
                // The active-window probe resolved — flip the bold+italic marker. A pure
                // state mutation: cursor follow is a loop-top concern, not here.
                switcher.set_active_window(&host, &session, window, self);
                Vec::new()
            }
            HostEvent::Exited { host, reason } => {
                // Mark the host unreachable in the tree (unless a transient drop of a
                // once-connected host), then reap its dead client.
                crate::app::cockpit::note_host_exited(switcher, self, connected, &host, reason);
                vec![EventEffect::ReapHost { host }]
            }
            HostEvent::ClientDetached { host, client } => {
                // The tty match against the host's recorded display tty + the registry
                // reap are loop-owned; forward the descriptor, mutate no State.
                vec![EventEffect::ReapDisplayAttach { host, client }]
            }
            HostEvent::DisplayTty { host, tty } => {
                // The tty lives on the Host (behind the loop's reach), so the state
                // layer forwards it as an effect for the loop to record.
                vec![EventEffect::RecordDisplayTty { host, tty }]
            }
            HostEvent::Scanned { source, detected } => {
                vec![EventEffect::DispatchScanned { source, detected }]
            }
            HostEvent::Sessions {
                source,
                sessions,
                err,
            } => {
                // Apply the poll enumeration to the tree. On a SUCCESSFUL enumeration
                // hand the sessions back so the loop drops any stale attach + syncs the
                // PTY set; a transient failure shows the error but keeps attachments.
                let had_err = err.is_some();
                switcher.apply_source_result(source.clone(), sessions.clone(), err, self);
                if had_err {
                    Vec::new()
                } else {
                    vec![EventEffect::SyncPollSessions { source, sessions }]
                }
            }
            HostEvent::Panes { address, panes } => {
                switcher.apply_panes(address, panes, self);
                Vec::new()
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
    use crate::app::cockpit::Selection;
    use crate::app::focus::Focus;
    use crate::model::{Action, Command, FocusTarget};
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

    // --- session-lifecycle intents fold into Command::RunOp ------------------
    // Each lifecycle Action is a pure intent → effect: apply mutates nothing and
    // returns the MuxOp descriptor the run loop runs off-loop. The OpResult
    // flow-back stays the existing channel path (5.4d-iii territory).

    fn a_sess(name: &str) -> crate::session::Session {
        crate::session::Session {
            source: "jup".into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn apply_create_session_emits_run_op_create() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::CreateSession {
                source: "jup".into(),
                name: "api".into(),
            }),
            vec![Command::RunOp(MuxOp::Create {
                source: "jup".into(),
                name: "api".into(),
            })]
        );
    }

    #[test]
    fn apply_new_window_emits_run_op_new_window() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::NewWindow {
                source: "jup".into(),
                session: "api".into(),
                name: "logs".into(),
            }),
            vec![Command::RunOp(MuxOp::NewWindow {
                source: "jup".into(),
                session: "api".into(),
                name: "logs".into(),
            })]
        );
    }

    #[test]
    fn apply_split_window_emits_run_op_split() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::SplitWindow {
                source: "jup".into(),
                target: "api:1".into(),
                session: "api".into(),
                vertical: false,
            }),
            vec![Command::RunOp(MuxOp::SplitWindow {
                source: "jup".into(),
                target: "api:1".into(),
                session: "api".into(),
                vertical: false,
            })]
        );
    }

    #[test]
    fn apply_rename_session_emits_run_op_rename() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::RenameSession {
                sess: a_sess("api"),
                new_name: "svc".into(),
            }),
            vec![Command::RunOp(MuxOp::Rename {
                sess: a_sess("api"),
                new_name: "svc".into(),
            })]
        );
    }

    #[test]
    fn apply_kill_session_emits_run_op_kill() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::KillSession {
                sess: a_sess("api")
            }),
            vec![Command::RunOp(MuxOp::Kill {
                sess: a_sess("api")
            })]
        );
    }

    #[test]
    fn apply_kill_window_emits_run_op_kill_window() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::KillWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:1".into(),
            }),
            vec![Command::RunOp(MuxOp::KillWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:1".into(),
            })]
        );
    }

    #[test]
    fn apply_rename_window_emits_run_op_rename_window() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::RenameWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:1".into(),
                new_name: "logs".into(),
            }),
            vec![Command::RunOp(MuxOp::RenameWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:1".into(),
                new_name: "logs".into(),
            })]
        );
    }

    #[test]
    fn apply_lifecycle_action_does_not_touch_selection_or_focus() {
        // Lifecycle intents are pure effect emitters: they leave domain state alone
        // (the OpResult that follows mutates the inventory, not apply).
        let mut s = State::default();
        let before_sel = s.selection.clone();
        s.apply(Action::KillSession {
            sess: a_sess("api"),
        });
        assert_eq!(
            s.selection, before_sel,
            "kill intent leaves selection alone"
        );
        assert!(s.focus.is_tree_focused(), "kill intent leaves focus alone");
        assert!(s.popup.is_none(), "kill intent leaves the popup alone");
    }

    // --- apply_event(HostEvent) -----------------------------------------------
    // State owns the EVENT-DRIVEN mutations: apply_event folds the self-contained
    // arms (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable
    // mark) into State directly, and returns the backend follow-ups (refetch /
    // probe / reap / sync / scan-dispatch) as EventEffects for the run loop to run.
    use crate::host::HostEvent;
    use crate::model::EventEffect;
    use crate::session::{Pane, Session, WindowPanes};
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use std::collections::HashSet;

    fn one_session_scan() -> Scan {
        let mut panes = HashMap::new();
        panes.insert(
            "jup/api".to_string(),
            vec![
                WindowPanes {
                    index: 0,
                    name: "w0".into(),
                    active: true,
                    panes: vec![Pane {
                        index: 0,
                        active: true,
                        command: "bash".into(),
                    }],
                },
                WindowPanes {
                    index: 1,
                    name: "w1".into(),
                    active: false,
                    panes: vec![Pane {
                        index: 0,
                        active: true,
                        command: "bash".into(),
                    }],
                },
            ],
        );
        Scan {
            groups: vec![Group {
                source: "jup".into(),
                err: None,
                sessions: vec![Session {
                    source: "jup".into(),
                    name: "api".into(),
                    windows: 2,
                    attached: false,
                    last_attached: 100,
                }],
            }],
            panes,
        }
    }

    fn with_switcher(scan: Scan) -> (State, Switcher) {
        let mut state = State::from_scan(scan);
        let sw = Switcher::new(&mut state);
        (state, sw)
    }

    #[test]
    fn apply_event_focus_moves_marker_and_emits_no_effect() {
        // Focus is self-contained (host/session/window in the payload): apply_event
        // flips the active-window marker in State and produces no backend effect.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        // window 0 is active in the scan; move the marker to window 1.
        let effects = state.apply_event(
            HostEvent::Focus {
                host: "jup".into(),
                session: "api".into(),
                window: 1,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            effects.is_empty(),
            "Focus is a pure state mutation — no backend effect"
        );
        let windows = state.panes.get("jup/api").unwrap();
        assert!(windows.iter().find(|w| w.index == 1).unwrap().active);
        assert!(!windows.iter().find(|w| w.index == 0).unwrap().active);
    }

    #[test]
    fn apply_event_panes_loads_subtree_and_emits_no_effect() {
        // Panes carries its data; apply_event applies it and marks the address loaded.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let new_panes = vec![WindowPanes {
            index: 0,
            name: "only".into(),
            active: true,
            panes: vec![Pane {
                index: 0,
                active: true,
                command: "zsh".into(),
            }],
        }];
        let effects = state.apply_event(
            HostEvent::Panes {
                address: "jup/db".into(),
                panes: new_panes,
            },
            &mut sw,
            &mut connected,
        );
        assert!(effects.is_empty(), "Panes is a pure state mutation");
        assert!(state.panes_loaded.contains("jup/db"));
        assert_eq!(state.panes.get("jup/db").unwrap().len(), 1);
    }

    #[test]
    fn apply_event_connected_marks_connected_and_emits_apply_inventory() {
        // Connected/Inventory's data lives behind the host client's lock, which State
        // cannot reach — so apply_event records the connected mark and hands the
        // inventory apply back to the loop as an effect.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Connected { host: "jup".into() },
            &mut sw,
            &mut connected,
        );
        assert!(connected.contains("jup"), "Connected records the host");
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host }] if host == "jup"),
            "Connected returns one ApplyInventory effect: {effects:?}"
        );
        // Inventory behaves identically (the arm is shared).
        let effects = state.apply_event(
            HostEvent::Inventory { host: "jup".into() },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host }] if host == "jup"),
        );
    }

    #[test]
    fn apply_event_changed_emits_refetch() {
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Changed { host: "jup".into() },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::Refetch { host }] if host == "jup"),
            "Changed returns one Refetch effect: {effects:?}"
        );
    }

    #[test]
    fn apply_event_active_window_changed_probes_the_payload_session() {
        // The payload-bearing ActiveWindowChanged must forward the notification's session
        // id INTO the probe effect — probing that SPECIFIC session, never a guessed
        // displayed one.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::ActiveWindowChanged {
                host: "jup".into(),
                session_id: "$7".into(),
                window_id: "@3".into(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::ProbeActiveWindow { host, session_ref }]
                    if host == "jup" && session_ref == "$7"
            ),
            "ActiveWindowChanged returns one payload-targeted ProbeActiveWindow: {effects:?}"
        );
    }

    #[test]
    fn apply_event_client_detached_emits_reap_display_attach_with_no_state_change() {
        // The tty match + reap need the host registry (loop-owned); apply_event only
        // forwards the descriptor and touches no State.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let before_groups = state.groups.len();
        let before_sessions = state.groups[0].sessions.len();
        let effects = state.apply_event(
            HostEvent::ClientDetached {
                host: "jup".into(),
                client: "/dev/pts/3".into(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::ReapDisplayAttach { host, client }]
                    if host == "jup" && client == "/dev/pts/3"
            ),
            "ClientDetached forwards a ReapDisplayAttach effect: {effects:?}"
        );
        // ClientDetached mutates no State (the tree group set is untouched).
        assert_eq!(state.groups.len(), before_groups);
        assert_eq!(state.groups[0].sessions.len(), before_sessions);
        assert!(state.popup.is_none());
    }

    #[test]
    fn apply_event_exited_marks_unreachable_and_emits_reap() {
        // A never-connected host exiting with a real failure marks the tree
        // unreachable (a State mutation) AND asks the loop to reap the client.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new(); // not connected → not a transient drop
        let effects = state.apply_event(
            HostEvent::Exited {
                host: "jup".into(),
                reason: Some("connection refused".into()),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),
            "Exited returns one ReapHost effect: {effects:?}"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert!(
            g.err.is_some(),
            "the host is marked unreachable in the tree"
        );
    }

    #[test]
    fn apply_event_exited_of_connected_host_keeps_tree_and_still_reaps() {
        // A transient drop of a once-connected host keeps its last-known tree (no
        // unreachable flash) but still reaps the dead client.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        connected.insert("jup".to_string());
        let effects = state.apply_event(
            HostEvent::Exited {
                host: "jup".into(),
                reason: None,
            },
            &mut sw,
            &mut connected,
        );
        assert!(matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),);
        assert!(
            !connected.contains("jup"),
            "the connected mark is cleared so a later failed reconnect resolves"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert!(
            g.err.is_none(),
            "a transient drop keeps the last-known tree"
        );
    }

    #[test]
    fn apply_event_sessions_applies_tree_and_emits_sync_on_success() {
        // A poll host's enumeration is self-contained: apply_event applies the
        // sessions to the tree and hands the sessions back for the stale-attach /
        // sync follow-up the loop owns.
        let mut state = State::from_sources(vec!["local".into()]);
        let mut sw = Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let sessions = vec![Session {
            source: "local".into(),
            name: "work".into(),
            windows: 1,
            attached: false,
            last_attached: 5,
        }];
        let effects = state.apply_event(
            HostEvent::Sessions {
                source: "local".into(),
                sessions: sessions.clone(),
                err: None,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            !state.scanning.contains("local"),
            "the enumerated source is no longer scanning"
        );
        let g = state.groups.iter().find(|g| g.source == "local").unwrap();
        assert_eq!(g.sessions.len(), 1, "the session is in the tree");
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::SyncPollSessions { source, sessions: s }]
                    if source == "local" && s.len() == 1
            ),
            "a successful enumeration syncs terminals: {effects:?}"
        );
    }

    #[test]
    fn apply_event_sessions_with_error_applies_tree_but_emits_no_sync() {
        // A transient enumeration failure shows the error in the tree but keeps
        // attachments (the keep-alive guarantee) — no sync effect.
        let mut state = State::from_sources(vec!["local".into()]);
        let mut sw = Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Sessions {
                source: "local".into(),
                sessions: Vec::new(),
                err: Some("poll failed".into()),
            },
            &mut sw,
            &mut connected,
        );
        let g = state.groups.iter().find(|g| g.source == "local").unwrap();
        assert_eq!(g.err.as_deref(), Some("poll failed"));
        assert!(
            effects.is_empty(),
            "a failed enumeration keeps attachments — no sync effect: {effects:?}"
        );
    }

    #[test]
    fn apply_event_scanned_emits_dispatch_carrying_the_detection() {
        // The detection box + the host-channel dispatch are loop-owned; apply_event
        // forwards the descriptor (no detection here = still undetected).
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Scanned {
                source: "jup".into(),
                detected: None,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::DispatchScanned { source, detected: None }] if source == "jup"
            ),
            "Scanned forwards a DispatchScanned effect: {effects:?}"
        );
    }
}
