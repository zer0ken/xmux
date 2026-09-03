//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the app loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
use crate::model::Selection;
use crate::ui::tree::Group;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The app's canonical runtime state.
#[derive(Default)]
pub struct State {
    /// Inventory - hosts → sessions (all reachable). The single
    /// source of truth every component reads, instead of reaching into the tree.
    // ponytail: flat fields, not an Inventory sub-struct - bundle them if a reader
    // ever needs the whole group at once.
    pub groups: Vec<Group>,
    /// Sources whose `list-sessions` has not yet returned (host shows scanning…).
    pub scanning: HashSet<String>,
    /// How many times in a row each source has failed to enumerate, reset to zero the
    /// moment it answers. Written at the single result-apply site and read only to be
    /// SHOWN: the unreachable screen states it, because one failed sweep and a host that
    /// has not answered since launch are different problems behind the same message.
    pub failure_runs: HashMap<String, u32>,
    /// In-memory unlock secrets per source (id + password), kept for the run only:
    /// reused when the same host relocks, cleared when the host dies (not locks),
    /// and never rendered or serialized. `dump`/`status`/logs carry none of it.
    pub(crate) secrets: HashMap<String, StoredSecret>,
    /// Active fuzzy-filter text (drives the visible tree + the hint_bar).
    pub filter: String,
    /// What the tree selection points at - the session/window to show.
    pub selection: Selection,
    /// The address whose content is confirmed live in the on-screen terminal view -
    /// the single display truth, and the target of both rendering and input. The
    /// terminal view always shows THIS session's grid; on a switch it stays on the
    /// prior session until the new one is confirmed (stale-while-revalidate), then
    /// advances. Set only at confirmation (a synchronous in-place switch, or
    /// DisplayReady). Empty before the first confirmation → the view is blank.
    pub displayed: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// A selection moved and has not yet armed its debounce deadline. The next
    /// [`Action::Tick`] (re)arms `attach_deadline` from this - re-armed on EVERY
    /// pending selection so rapid navigation coalesces into one trailing attach
    /// instead of a per-step storm of switch-client repaints (the freeze).
    pub attach_pending: bool,
    /// How many dead-display recovery attaches have fired for the current selection
    /// without the display being confirmed again. Each recovery fire spends one and
    /// the recovery stands down at the limit, so a session that is gone cannot turn
    /// the EOF of every failed attach into the next attempt; a selection move, a
    /// cleared display, or a re-confirmed display starts a new count.
    pub attach_retries: u32,
    /// The session address last persisted as the user's last-selected, so it is
    /// not rewritten on every window step within the same session.
    pub last_saved_session: String,
    /// The app's focus state machine - which pane keys go to and whether a
    /// modal is open. The single source of truth for focus.
    pub focus: crate::app::focus::Focus,
    /// The single open modal, if any (help / inline input / kill confirm / context
    /// menu). One Option - not four independent fields - so the modals' mutual
    /// exclusion is structural: opening one drops whatever was open, and two can
    /// never coexist. The switcher owns the modal behavior and the transient popup
    /// geometry (drag offset / drawn rect); this owns which modal is open + its content.
    pub(crate) modal: Option<crate::ui::modal::Modal>,
    /// The switcher's chrome view-state: the tree|terminal view border, the tree-column
    /// hint bar (help / status / wrapped flash), and the host screens,
    /// plus their inputs (flash, spinner set + frame, auto-hide/hover cues, view border
    /// colours, ssh-config text, prefix). Owned here (the [`Modal`](crate::ui::modal::Modal)
    /// precedent) and fed by the app each frame; the switcher's `render` reads it off
    /// `&state`.
    pub(crate) chrome: crate::ui::chrome::Chrome,
}

/// The run's in-memory unlock secret for one source: the id and password the user
/// submitted. Kept only while the run lasts and only for a host still locked;
/// never serialized, logged, or rendered (the password field draws masked).
#[derive(Clone)]
pub(crate) struct StoredSecret {
    pub(crate) user: String,
    pub(crate) secret: String,
}

impl State {
    /// The stored secret for `source`, if one is kept for the run.
    pub(crate) fn secret_for(&self, source: &str) -> Option<&StoredSecret> {
        self.secrets.get(source)
    }

    /// The username an unlock starts with: the stored secret's user, else empty.
    /// The user always confirms the id before it is used (nothing is guessed).
    pub(crate) fn current_unlock_user(&self, source: &str) -> String {
        self.secrets
            .get(source)
            .map(|s| s.user.clone())
            .unwrap_or_default()
    }

    /// Drops the secret of every host that is no longer LOCKED (a host that died is
    /// not waiting on a password). Called after a result applies a group's `err`, the
    /// single place failure text lands.
    pub(crate) fn forget_unlocked_secrets(&mut self) {
        self.secrets.retain(|source, _| {
            self.groups
                .iter()
                .find(|g| &g.source == source)
                .is_some_and(|g| {
                    g.err.as_deref().is_some_and(crate::mux::is_locked)
                })
        });
    }

    /// True while a modal owns the screen (the help popup or the inline input) is
    /// open. These drive [`ModalKind::Popup`]; the context
    /// menu is separate (pointer-anchored).
    ///
    /// [`ModalKind::Popup`]: crate::app::focus::ModalKind::Popup
    pub fn is_modal_popup_open(&self) -> bool {
        crate::ui::modal::is_popup_open(&self.modal)
    }

    /// True while an inline input (filter / rename / new) is open. The app
    /// routes every key to the switcher then, with no focus-switch hijack.
    pub fn is_inputting(&self) -> bool {
        crate::ui::modal::is_inputting(&self.modal)
    }

    /// Which kind of modal is open - the focus machine derives its modal dimension
    /// from this each loop-top, so [`Focus`] can never mirror-and-desync from the
    /// open popup. A centered popup and the context menu are mutually exclusive.
    ///
    /// [`Focus`]: crate::app::focus::Focus
    pub(crate) fn modal_kind(&self) -> Option<crate::app::focus::ModalKind> {
        crate::ui::modal::modal_kind(&self.modal)
    }

    /// Builds the inventory from a complete snapshot: every host is resolved
    /// (reachable or unreachable per its `err`) and every session is present. Other
    /// state fields stay default.
    pub fn from_scan(scan: crate::ui::switcher::Scan) -> State {
        State {
            groups: scan.groups,
            ..State::default()
        }
    }

    /// Seeds the inventory from the resolved source list alone - no probing - so
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

    /// Resolves a `<source>/<session>` switch address against the current inventory -
    /// the set the nav shows. `Ok` when a session with exactly that address is listed;
    /// `Err` names which half is missing (an absent source, or a present source with no
    /// matching session). The answer the ctl `switch` verb replies with: resolution,
    /// not attach success, which is async and confirms later.
    pub fn resolve_switch_address(&self, address: &str) -> Result<(), String> {
        let target = crate::session::parse_target(address)?;
        let found = self
            .groups
            .iter()
            .any(|g| g.sessions.iter().any(|s| s.address() == address));
        if found {
            return Ok(());
        }
        if self.groups.iter().any(|g| g.source == target.source) {
            Err(format!(
                "no such session {:?} on source {:?}",
                target.name, target.source
            ))
        } else {
            Err(format!("no such source {:?}", target.source))
        }
    }

    /// The single domain-mutation site. Folds one [`Action`] into the state and
    /// returns the side effects to run as [`Command`]s. `apply` touches only `State`;
    /// every external effect (switcher selection move, attach, prefs persist, quit) is
    /// returned for the run loop to dispatch, so the intent → state → effect flow has
    /// exactly one mutation point.
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
                    .set_view_focus(crate::app::focus::ViewFocus::Terminal);
                Vec::new()
            }
            Action::Focus(FocusTarget::Nav) => {
                self.focus.set_view_focus(crate::app::focus::ViewFocus::Nav);
                Vec::new()
            }
            Action::FocusToggle => {
                self.focus.toggle();
                Vec::new()
            }
            Action::ConfirmDisplay(sel) => {
                self.displayed = sel;
                self.attach_retries = 0;
                Vec::new()
            }
            Action::ClearDisplay => {
                self.displayed = Selection::default();
                self.attach_retries = 0;
                Vec::new()
            }
            Action::RearmAttach { now } => {
                self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                Vec::new()
            }
            Action::RearmAttachNow { now } => {
                self.attach_deadline = Some(now);
                Vec::new()
            }
            Action::Rescan => vec![Command::Rescan],
            Action::NavWidth(d) => vec![Command::AdjustNavWidth(d)],
            Action::ToggleAutoHide => vec![Command::ToggleAutoHide],
            Action::Quit => vec![Command::Quit],
            Action::Select(target) => {
                // Mark the attach pending; do NOT arm the deadline or attach here.
                // The trailing Tick arms the debounce, so rapid navigation coalesces.
                self.selection = target;
                self.attach_pending = true;
                self.attach_retries = 0;
                Vec::new()
            }
            Action::Tick {
                now,
                key_live,
                in_flight,
                display_astray,
            } => {
                // RE-ARM on every pending selection: a fresh Select between ticks
                // pushes the deadline out, so only the trailing selection attaches
                // (one switch, not a per-step storm - the freeze fix). Re-arming and
                // firing are mutually exclusive on a tick: a just-armed deadline is
                // always in the future, so the elapsed check below cannot fire it.
                if self.attach_pending {
                    self.attach_pending = false;
                    self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    return Vec::new();
                }
                // ARM ON THE CONDITION, not on a change. A display sitting away from the
                // selection is a state, so it is answered for as long as it lasts and
                // nothing has to be remembered from the moment it began: a selection that
                // never moved again would arm nothing if only its MOVE could. The ways it
                // can sit away are one condition here, because the gate below fires on
                // them: the client left for another session of the selected host
                // (`display_astray`), the confirmed display is another session
                // altogether, or the display PTY is gone while the selection stands
                // (an EOF'd display attachment - the detach of the mirrored client).
                // The last of these arms only while the recovery could actually fire
                // (the inventory still lists the session and the retry budget lasts),
                // so a dead session cannot arm-and-refuse forever. Armed only with the
                // debounce idle, so a navigation burst still coalesces into one
                // trailing attach, and only with nothing in flight, so the attach
                // already carrying the display there is not restarted under itself.
                let recovery_possible =
                    self.selection_is_listed() && self.attach_retries < ATTACH_RECOVERY_LIMIT;
                let display_needs_carry = display_astray
                    || self.selection != self.displayed
                    || (!key_live && recovery_possible);
                if display_needs_carry
                    && !self.selection.is_empty()
                    && self.attach_deadline.is_none()
                    && !in_flight
                {
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
                // Persist the settled session as last-selected - INDEPENDENT of the
                // attach gate, so it records even when the attach is suppressed (e.g.
                // an in-flight attach on the same shared host while the selection moves to
                // another of its sessions). Only on an address change, so stepping
                // between windows of one session does not rewrite it.
                let addr = self.selection.address();
                if addr != self.last_saved_session {
                    self.last_saved_session = addr.clone();
                    cmds.push(Command::PersistLastSession(addr));
                }
                // Fire the attach only when the gate holds (selection differs from the
                // confirmed display, its PTY is gone, or the display is astray) and
                // nothing is in flight - the freeze invariant depends on this gate, so
                // it stays exactly as is. A fire that recovers a dead display PTY while
                // the selection stands (the detach of the mirrored client) spends one
                // of the recovery budget's retries; the gate itself refuses to fire
                // that leg past the budget or for a session the inventory dropped.
                if self.should_attach(key_live, in_flight, display_astray) {
                    if self.selection == self.displayed && !key_live {
                        self.attach_retries += 1;
                    }
                    cmds.push(Command::Attach(self.selection.clone()));
                }
                cmds
            }
            // The session-lifecycle intent is a pure effect emitter: it folds into the
            // MuxOp the run loop runs off-loop. `apply` mutates no domain state - the
            // inventory change arrives later as the OpResult.
            Action::CreateSession { source, name } => {
                vec![Command::RunOp(MuxOp::Create { source, name })]
            }
            // The unlock stores the in-memory secret (the run's reuse) and hands the
            // worker its id+password in the same command - the secret never sits in a
            // durable place, and the worker consumes it straight from the command.
            Action::Unlock {
                source,
                user,
                password,
            } => {
                self.secrets.insert(
                    source.clone(),
                    StoredSecret {
                        user: user.clone(),
                        secret: password.clone(),
                    },
                );
                vec![Command::RunUnlock {
                    source,
                    user,
                    password,
                }]
            }
        }
    }

    /// The single event-driven mutation site: folds one mux [`HostEvent`] into the
    /// domain state and returns the mux follow-ups as [`EventEffect`]s. The mirror
    /// of [`apply`](State::apply) for the inbound (mux → state) direction - every
    /// `%`-notification, metadata reply, poll result, and reap routes through here, so
    /// State owns the event-driven mutations just as `apply` owns the intent-driven ones.
    ///
    /// `apply_event` performs only the mutations whose data is SELF-CONTAINED in the
    /// event (a poll enumeration, the
    /// unreachable mark) - driven through the switcher, which rebuilds the tree against
    /// `&mut State`. The follow-ups that need a mux handle the state layer must not
    /// hold (the single-owner inventory fold into `model::Host`, a control-mode probe,
    /// the attach registry, the detection dispatch) are returned as [`EventEffect`]s for
    /// the run loop - the sole executor - to carry out (the AGENTS rule: no IO/registry
    /// mutation here).
    ///
    /// `connected` (the run loop's once-connected set) enters as data, like the clock on
    /// `Tick`: an `Exited` of a once-connected host is a transient drop that keeps the
    /// last-known tree; otherwise it resolves the host's real state.
    ///
    /// [`HostEvent`]: crate::link::HostEvent
    /// [`EventEffect`]: crate::model::EventEffect
    pub fn apply_event(
        &mut self,
        ev: crate::link::HostEvent,
        switcher: &mut crate::ui::switcher::Switcher,
        connected: &mut std::collections::HashSet<String>,
    ) -> Vec<crate::model::EventEffect> {
        use crate::link::HostEvent;
        use crate::model::EventEffect;
        match ev {
            HostEvent::Connected { host, sessions } | HostEvent::Inventory { host, sessions } => {
                // The reader carries the parsed sessions on the event; record the
                // connected mark and hand the sessions to the loop, which folds them
                // into `model::Host.inventory` (the single owner) and applies the tree.
                connected.insert(host.clone());
                vec![EventEffect::ApplyInventory { host, sessions }]
            }
            HostEvent::Changed { host } => vec![EventEffect::Refetch { host }],
            HostEvent::Exited { host, reason } => {
                // Mark the host unreachable in the tree (unless a transient drop of a
                // once-connected host), then reap its dead client.
                crate::app::runtime::note_host_exited(switcher, self, connected, &host, reason);
                vec![EventEffect::ReapHost { host }]
            }
            HostEvent::ClientDetached { host, client } => {
                // The tty match against the host's recorded display tty + the registry
                // reap are loop-owned; forward the descriptor, mutate no State.
                vec![EventEffect::ReapDisplayAttach { host, client }]
            }
            HostEvent::ClientSessionChanged {
                host,
                client,
                session,
            } => {
                // The tty match against the host's recorded display tty, the display-belief
                // sync, and the nav follow are all loop-owned (the tty lives on `Host`);
                // forward the descriptor, mutate no State here.
                vec![EventEffect::FollowDisplaySession {
                    host,
                    client,
                    session,
                }]
            }
            HostEvent::DisplayTty { host, tty } => {
                // The tty lives on the Host (behind the loop's reach), so the state
                // layer forwards it as an effect for the loop to record.
                vec![EventEffect::RecordDisplayTty { host, tty }]
            }
            HostEvent::MuxesFound { machine, muxes } => {
                // Nothing to fold: which muxes the machine ALREADY serves lives in the
                // host registry, so the whole decision is the loop's.
                vec![EventEffect::AddDiscoveredSources { machine, muxes }]
            }
            HostEvent::RosterResolved { roster } => {
                // Nothing to fold: which machines the registries already hold is the
                // loop's to know, so the whole decision is the loop's.
                vec![EventEffect::ApplyRoster { roster }]
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
        }
    }

    /// Whether to (re)issue an attach for the settled selection. Fire when the
    /// selection differs from what is confirmed on screen, when its display PTY is
    /// gone (`!key_live` - the attachment exited or was reaped, whether while the
    /// selection was elsewhere or while it stood on the displayed session: the
    /// detach of the mirrored client), or when the display client sits on another
    /// session (`display_astray`) - but never while an attach for the key is already
    /// in flight, so the async-attach window cannot spawn a storm of duplicates.
    /// The clock and these runtime facts enter as data on the Tick, never read here
    /// directly.
    ///
    /// The astray leg is why the two regions cannot settle on different sessions. The
    /// other two legs compare the selection against xmux's OWN record of what it put on
    /// screen, which a session change the mux made never touches: the selection and the
    /// confirmed display agree, the PTY is alive, and the client is somewhere else
    /// entirely. Only a fact about where the client actually is can say so.
    ///
    /// The dead-display leg is bounded: a display PTY that is gone while the
    /// selection stands is recovered only while the inventory still lists the
    /// session and the per-selection retry budget lasts. Once the enumeration has
    /// dropped the session there is nothing to attach to, and a session that is
    /// gone makes every re-attach EOF in turn - the budget is what keeps that from
    /// becoming an endless reap-and-reattach chain.
    pub(crate) fn should_attach(
        &self,
        key_live: bool,
        in_flight: bool,
        display_astray: bool,
    ) -> bool {
        let recovery = self.selection == self.displayed && !key_live;
        let owed = self.selection != self.displayed || !key_live || display_astray;
        if !owed || in_flight {
            return false;
        }
        if recovery {
            self.selection_is_listed() && self.attach_retries < ATTACH_RECOVERY_LIMIT
        } else {
            true
        }
    }

    /// Whether the selection's session is still listed by its source's inventory
    /// (the nav tree the enumerations stream into). Pure - it reads State's own
    /// inventory, and nothing else. An unlisted session has nothing to attach to:
    /// its card is gone, and the selection is on its way elsewhere.
    fn selection_is_listed(&self) -> bool {
        self.groups
            .iter()
            .find(|g| g.source == self.selection.source)
            .is_some_and(|g| g.sessions.iter().any(|s| s.name == self.selection.session))
    }

    /// Folds a completed [`MuxOp`](crate::model::MuxOp)'s [`OpResult`] into the
    /// inventory - the single owner of `groups` - and returns an
    /// [`OpFollow`] telling the switcher how to rebuild its rows (and, for a create,
    /// which session to reselect). State owns the domain mutation here just as
    /// [`apply`](State::apply) / [`apply_event`](State::apply_event) own the intent- and
    /// event-driven ones; the row rebuild + cursor restore stay in the switcher. A
    /// `Failed` op mutates no inventory - its message is returned to flash.
    ///
    /// [`OpResult`]: crate::ui::ops::OpResult
    /// [`OpFollow`]: crate::ui::ops::OpFollow
    pub(crate) fn fold_op_result(
        &mut self,
        result: crate::ui::ops::OpResult,
    ) -> crate::ui::ops::OpFollow {
        use crate::ui::ops::{OpFollow, OpResult};
        use crate::ui::tree;
        match result {
            OpResult::Created { session, .. } => {
                let addr = session.address();
                self.groups = tree::add_session(&self.groups, session);
                OpFollow::Reselect(addr)
            }
            OpResult::Failed { message } => OpFollow::Flash(message),
        }
    }

    /// Flashes a transient message in the tree-column hint bar (an error or notice).
    /// The next tree key clears it (the switcher's `handle_key` clear path), so the
    /// normal help/status hint bar returns. Delegates to the chrome's flash API.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.chrome.flash(msg);
    }
}

/// Debounce before a settled selection move attaches/switches its session+window.
/// Rapid navigation must NOT switch-client / select-window per step: each switch
/// makes the remote mux send a full-screen repaint, and a storm of repaints floods
/// the draw - the single-threaded loop then spends all its time redrawing, which IS
/// the freeze. Deferring the attach until the selection settles keeps per-step redraws
/// to a cheap tree-only diff. The single source of this value: both `apply`'s `Tick`
/// re-arm and its [`Action::RearmAttach`](crate::model::Action::RearmAttach) recovery
/// re-arm read it, so the two arming paths can never drift.
pub(crate) const ATTACH_DEBOUNCE_MS: u64 = 90;

/// How many dead-display recovery attaches may fire for one selection before the
/// recovery stands down until a selection move, a cleared display, or a re-confirmed
/// display refills it. The recovery chain - a dead display PTY re-armed, attached,
/// EOF'd, re-armed again - would otherwise run without end for a session that no
/// longer exists, so the budget bounds it while the enumeration catches up.
pub(crate) const ATTACH_RECOVERY_LIMIT: u32 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::focus::Focus;
    use crate::model::{Action, Command, FocusTarget, Selection};
    use std::time::Duration;

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.displayed.is_empty());
        assert!(s.attach_deadline.is_none());
        assert!(!s.attach_pending);
        assert_eq!(s.last_saved_session, "");
        assert!(s.focus.is_nav_focused());
        assert!(s.modal.is_none());
        assert!(!s.is_modal_popup_open());
        assert!(!s.is_inputting());
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
            "Select does NOT arm the deadline - the trailing Tick does"
        );
        assert!(cmds.is_empty(), "Select emits no attach command");
    }

    #[test]
    fn apply_tick_arms_then_fires_one_attach_after_debounce() {
        let mut s = State::default();
        let t0 = Instant::now();
        s.apply(Action::Select(sel("api")));
        // Tick at t0 arms the deadline (no fire yet - now < deadline).
        let armed = s.apply(Action::Tick {
            now: t0,
            key_live: true,
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        // Tick at t0+90ms (deadline reached) with no intervening Select fires once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: true,
            in_flight: false,
            display_astray: false,
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
            display_astray: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        // A Select 30ms later (rapid nav) re-marks pending; the next Tick re-arms the
        // deadline PAST the original, so the original deadline does not fire.
        s.apply(Action::Select(sel("db")));
        let rearm = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(30),
            key_live: true,
            in_flight: false,
            display_astray: false,
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
            display_astray: false,
        });
        assert!(early.is_empty(), "no fire before the re-armed deadline");
        // Only at t0+120 does the trailing selection (db) attach, once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(120),
            key_live: true,
            in_flight: false,
            display_astray: false,
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
            display_astray: false,
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
    fn apply_tick_arms_when_the_display_sits_on_another_session_with_no_select() {
        // The display can move while the selection stands still: an attach lands for a
        // session nobody chose, or the mux carries the client away. `should_attach` fires
        // on that difference, so the ARMING answers the same condition - if only a
        // `Select` could arm, the gate would sit true with no deadline and the two
        // regions would stay split until the next selection move.
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("api"),
            displayed: sel("db"),
            last_saved_session: "jup/api".into(), // already persisted → no persist command
            ..State::default()
        };
        let armed = s.apply(Action::Tick {
            now: t0,
            key_live: true,
            in_flight: false,
            display_astray: false,
        });
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(90)),
            "the difference alone arms the debounce"
        );
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: true,
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![Command::Attach(sel("api"))],
            "the attach carries the display back to the selection"
        );
    }

    #[test]
    fn apply_tick_arms_nothing_while_nothing_is_selected() {
        // Until the scan puts a card under the cursor there is nowhere to carry the
        // display to, so an empty selection differing from whatever is displayed arms
        // nothing - it would only arm and clear a deadline every beat.
        let t0 = Instant::now();
        let mut s = State {
            displayed: sel("db"),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert!(cmds.is_empty(), "an empty selection attaches nothing");
        assert!(
            s.attach_deadline.is_none(),
            "an empty selection arms nothing"
        );
    }

    #[test]
    fn apply_tick_fires_recovery_when_display_pty_gone() {
        // should_attach gate: selection == displayed but its PTY is not live ⇒ re-attach.
        // The recovery leg also asks the inventory to still list the session, so the
        // state is built from a scan that lists it.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
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
            display_astray: false,
        });
        assert_eq!(
            cmds,
            vec![Command::Attach(sel("api"))],
            "a vanished display PTY re-attaches even with an unchanged selection"
        );
    }

    #[test]
    fn apply_tick_arms_when_the_display_pty_dies_under_an_unchanged_selection() {
        // The mirrored client detached: the display PTY is gone while the selection and
        // the confirmed display still name one session. The arm mirrors the gate's fire
        // condition, so this state arms the debounce by itself - in either focus, with
        // no selection move and no rearm event - and the elapsed deadline then fires the
        // recovery attach.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(), // already persisted → no persist command
            ..State::default()
        };
        let armed = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(90)),
            "the dead display arms the debounce on its own"
        );
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![Command::Attach(sel("api"))],
            "the dead display re-attaches the still-selected session"
        );
    }

    #[test]
    fn the_dead_display_recovery_does_not_fire_for_a_session_the_inventory_dropped() {
        // Once the enumeration no longer lists the session there is nothing to attach
        // to; re-firing would only spawn one doomed attach after another.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(),
            ..State::default()
        };
        s.groups[0].sessions.clear(); // the enumeration dropped the session
        s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert!(
            s.attach_deadline.is_none(),
            "a recovery that cannot fire does not arm either"
        );
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert!(fired.is_empty(), "no attach for a dropped session");
    }

    #[test]
    fn the_dead_display_recovery_stands_down_after_its_retry_budget() {
        // A session that is gone makes every re-attach EOF in turn; each EOF would
        // otherwise re-arm the next attach. The recovery leg spends one retry per fire
        // and stops at the limit.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(),
            ..State::default()
        };
        let mut now = t0;
        let mut fires = 0usize;
        for _ in 0..(ATTACH_RECOVERY_LIMIT as usize + 2) {
            s.apply(Action::Tick {
                now,
                key_live: false,
                in_flight: false,
                display_astray: false,
            }); // arms
            now += Duration::from_millis(90);
            let cmds = s.apply(Action::Tick {
                now,
                key_live: false,
                in_flight: false,
                display_astray: false,
            }); // fires
            fires += cmds
                .iter()
                .filter(|c| matches!(c, Command::Attach(_)))
                .count();
            now += Duration::from_millis(90);
        }
        assert_eq!(
            fires, ATTACH_RECOVERY_LIMIT as usize,
            "the recovery fires at most the limit times, then stands down"
        );
    }

    #[test]
    fn a_selection_move_or_a_reconfirmed_display_refills_the_recovery_budget() {
        // The budget is per selection epoch: moving the selection, or the display being
        // confirmed again, starts a new count - so a later detach of the (new) display
        // recovers again.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: "jup/api".into(),
            ..State::default()
        };
        s.attach_retries = ATTACH_RECOVERY_LIMIT; // exhausted
        let refused = s.apply(Action::Tick {
            now: t0,
            key_live: false,
            in_flight: false,
            display_astray: false,
        });
        assert!(
            refused.is_empty(),
            "an exhausted budget attaches nothing on its own"
        );
        s.apply(Action::ConfirmDisplay(sel("api")));
        assert_eq!(
            s.attach_retries, 0,
            "a re-confirmed display refills the budget"
        );
        s.attach_retries = ATTACH_RECOVERY_LIMIT;
        s.apply(Action::Select(sel("db")));
        assert_eq!(s.attach_retries, 0, "a selection move refills the budget");
        s.attach_retries = ATTACH_RECOVERY_LIMIT;
        s.apply(Action::ClearDisplay);
        assert_eq!(s.attach_retries, 0, "a cleared display refills the budget");
    }

    #[test]
    fn apply_tick_does_not_fire_attach_while_in_flight_but_still_persists() {
        // The attach is suppressed while one is in flight (no storm), but the settled
        // session is still recorded as last-selected - the persist is independent of
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
            display_astray: false,
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
            display_astray: false,
        }); // arms
        let b_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            key_live: false,
            in_flight: false,
            display_astray: false,
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
            display_astray: false,
        }); // arms
        let c_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(190),
            key_live: false,
            in_flight: true, // first attach (B) still in flight on the shared host key
            display_astray: false,
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
            display_astray: false,
        });
        assert!(cmds.is_empty(), "empty selection never attaches");
        assert!(s.attach_deadline.is_none());
    }

    #[test]
    fn apply_rearm_attach_arms_the_debounce_deadline() {
        // The recovery re-arm (host-event detach-reap, pty-detach recover): apply owns
        // the debounce arithmetic that Tick owns, so the two arming paths cannot drift.
        let mut s = State::default();
        let t0 = Instant::now();
        let cmds = s.apply(Action::RearmAttach { now: t0 });
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(90)),
            "RearmAttach arms the deadline one debounce out"
        );
        assert!(cmds.is_empty(), "RearmAttach emits no command");
    }

    #[test]
    fn apply_rearm_attach_now_arms_an_immediate_deadline() {
        // The `r` reattach-kick fires ASAP: it sets the deadline to `now` so the SAME
        // loop iteration's trailing Tick sees it elapsed and re-attaches immediately.
        let mut s = State::default();
        let t0 = Instant::now();
        let cmds = s.apply(Action::RearmAttachNow { now: t0 });
        assert_eq!(
            s.attach_deadline,
            Some(t0),
            "RearmAttachNow arms an already-elapsed (immediate) deadline"
        );
        assert!(cmds.is_empty(), "RearmAttachNow emits no command");
    }

    #[test]
    fn apply_focus_moves_focus_with_no_command() {
        let mut s = State::default();
        assert!(s.focus.is_nav_focused());
        assert!(s.apply(Action::Focus(FocusTarget::Terminal)).is_empty());
        assert_eq!(s.focus, Focus::Terminal);
        assert!(s.apply(Action::Focus(FocusTarget::Nav)).is_empty());
        assert_eq!(s.focus, Focus::Nav);
    }

    #[test]
    fn apply_focus_toggle_flips_the_view_and_delegates_to_focus_toggle() {
        use crate::app::focus::ViewFocus;
        let mut s = State::default(); // Tree
        assert!(
            s.apply(Action::FocusToggle).is_empty(),
            "FocusToggle emits no command"
        );
        assert_eq!(s.focus, Focus::Terminal, "toggle flips Tree → Terminal");
        s.apply(Action::FocusToggle);
        assert_eq!(s.focus, Focus::Nav, "toggle flips back Terminal → Tree");
        // During a modal, toggle flips the carried prior and keeps the modal open.
        s.focus = Focus::Popup {
            prior: ViewFocus::Nav,
        };
        s.apply(Action::FocusToggle);
        assert_eq!(
            s.focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            },
            "toggle during a modal flips prior, the modal stays open"
        );
    }

    #[test]
    fn apply_confirm_display_sets_displayed() {
        // ConfirmDisplay advances the display truth to the given selection - the
        // in-place-attach / DisplayReady confirmation, folded at the single site.
        let mut s = State::default();
        assert!(s.displayed.is_empty());
        let cmds = s.apply(Action::ConfirmDisplay(sel("api")));
        assert_eq!(
            s.displayed,
            sel("api"),
            "ConfirmDisplay sets the display truth"
        );
        assert!(cmds.is_empty(), "ConfirmDisplay emits no command");
    }

    #[test]
    fn apply_clear_display_empties_displayed() {
        // ClearDisplay blanks the display truth - the reattach-kick path (nothing
        // confirmed yet → blank view until the fresh attach lands).
        let mut s = State {
            displayed: sel("api"),
            ..State::default()
        };
        assert!(!s.displayed.is_empty());
        let cmds = s.apply(Action::ClearDisplay);
        assert!(
            s.displayed.is_empty(),
            "ClearDisplay blanks the display truth"
        );
        assert!(cmds.is_empty(), "ClearDisplay emits no command");
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
    fn resolve_switch_address_reports_which_half_is_missing() {
        let s = State::from_scan(Scan {
            groups: vec![
                Group {
                    source: "jup".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "jup".into(),
                        name: "api".into(),
                        ..Default::default()
                    }],
                },
                Group {
                    source: "local:psmux".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "local:psmux".into(),
                        name: "swtarget".into(),
                        ..Default::default()
                    }],
                },
            ],
        });
        // A session the inventory lists resolves.
        assert_eq!(s.resolve_switch_address("jup/api"), Ok(()));
        assert_eq!(
            s.resolve_switch_address("local:psmux/swtarget"),
            Ok(()),
            "the qualified source address the nav actually uses resolves"
        );
        // A session missing under an existing source is a session error.
        let err = s.resolve_switch_address("jup/nope").unwrap_err();
        assert!(err.starts_with("no such session"), "{err}");
        // A source that does not exist is a source error - the issue's `local/swtarget`
        // when the real source is `local:psmux`, and a wholly unknown host.
        let err = s.resolve_switch_address("local/swtarget").unwrap_err();
        assert!(err.starts_with("no such source"), "{err}");
        let err = s
            .resolve_switch_address("nosuchhost/nosuchsession")
            .unwrap_err();
        assert!(err.starts_with("no such source"), "{err}");
        // An address with no `/` is invalid, not a missing source.
        assert!(s.resolve_switch_address("noslash").is_err());
    }

    #[test]
    fn apply_rescan_emits_rescan_command() {
        let mut s = State::default();
        assert_eq!(s.apply(Action::Rescan), vec![Command::Rescan]);
    }

    #[test]
    fn apply_nav_width_emits_adjust_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::NavWidth(-2)),
            vec![Command::AdjustNavWidth(-2)]
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
    // returns the MuxOp descriptor the run loop runs off-loop. The OpResult flows
    // back over the op channel.

    fn a_sess(name: &str) -> crate::session::Session {
        crate::session::Session {
            source: "jup".into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn unlock_action_stores_the_secret_and_emits_the_run_command() {
        let mut state = crate::state::State::default();
        let cmds = state.apply(crate::model::Action::Unlock {
            source: "pwbox".into(),
            user: "alice".into(),
            password: "hunter2".into(),
        });
        assert!(matches!(
            &cmds[..],
            [crate::model::Command::RunUnlock { user, password, .. }]
                if user == "alice" && password == "hunter2"
        ));
        assert_eq!(
            state
                .secret_for("pwbox")
                .map(|s| (s.user.clone(), s.secret.clone())),
            Some(("alice".to_string(), "hunter2".to_string()))
        );
        assert_eq!(state.current_unlock_user("pwbox"), "alice");
    }

    #[test]
    fn a_host_that_becomes_unreachable_forgets_its_secret() {
        let mut state = crate::state::State::default();
        state.apply(crate::model::Action::Unlock {
            source: "pwbox".into(),
            user: "alice".into(),
            password: "hunter2".into(),
        });
        // A non-locked failure (dead host) clears the secret; a locked failure keeps it.
        state.groups = vec![crate::ui::tree::Group {
            source: "pwbox".into(),
            err: Some("ssh: connect to host x: Connection refused".into()),
            sessions: vec![],
        }];
        state.forget_unlocked_secrets();
        assert!(
            state.secret_for("pwbox").is_none(),
            "unreachable forgets the secret"
        );
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
    fn apply_lifecycle_action_does_not_touch_selection_or_focus() {
        // The lifecycle intent is a pure effect emitter: it leaves domain state alone
        // (the OpResult that follows mutates the inventory, not apply).
        let mut s = State::default();
        let before_sel = s.selection.clone();
        s.apply(Action::CreateSession {
            source: "jup".into(),
            name: "api".into(),
        });
        assert_eq!(
            s.selection, before_sel,
            "create intent leaves selection alone"
        );
        assert!(s.focus.is_nav_focused(), "create intent leaves focus alone");
        assert!(s.modal.is_none(), "create intent leaves the popup alone");
    }

    // --- apply_event(HostEvent) -----------------------------------------------
    // State owns the EVENT-DRIVEN mutations: apply_event folds the self-contained
    // arms (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable
    // mark) into State directly, and returns the mux follow-ups (refetch /
    // probe / reap / sync / scan-dispatch) as EventEffects for the run loop to run.
    use crate::link::HostEvent;
    use crate::model::EventEffect;
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use std::collections::HashSet;

    fn one_session_scan() -> Scan {
        Scan {
            groups: vec![Group {
                source: "jup".into(),
                err: None,
                sessions: vec![Session {
                    source: "jup".into(),
                    name: "api".into(),
                    mux: "tmux".into(),
                    windows: 2,
                    attached: false,
                }],
            }],
        }
    }

    fn with_switcher(scan: Scan) -> (State, Switcher) {
        let mut state = State::from_scan(scan);
        let sw = Switcher::new(&mut state);
        (state, sw)
    }

    #[test]
    fn apply_event_connected_marks_connected_and_emits_apply_inventory() {
        // The reader carries the parsed sessions on Connected/Inventory; apply_event
        // records the connected mark and hands the sessions to the loop as an effect
        // (which folds them into `model::Host.inventory` - the single owner).
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let sessions = vec![crate::session::Session {
            source: "jup".into(),
            name: "api".into(),
            ..Default::default()
        }];
        let effects = state.apply_event(
            HostEvent::Connected {
                host: "jup".into(),
                sessions: sessions.clone(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(connected.contains("jup"), "Connected records the host");
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host, sessions }] if host == "jup" && sessions.len() == 1),
            "Connected carries its sessions into one ApplyInventory effect: {effects:?}"
        );
        // Inventory behaves identically (the arm is shared).
        let effects = state.apply_event(
            HostEvent::Inventory {
                host: "jup".into(),
                sessions,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host, sessions }] if host == "jup" && sessions.len() == 1),
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
        assert!(state.modal.is_none());
    }

    #[test]
    fn apply_event_client_session_changed_forwards_follow_effect_with_no_state_change() {
        // The tty match against Host.display_tty, the display-belief sync, and the nav
        // follow all need loop-owned state; apply_event only forwards the descriptor and
        // touches no State (the selection follow happens in the loop, gated on the match).
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let before_groups = state.groups.len();
        let before_sessions = state.groups[0].sessions.len();
        let effects = state.apply_event(
            HostEvent::ClientSessionChanged {
                host: "jup".into(),
                client: "/dev/pts/3".into(),
                session: "db".into(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::FollowDisplaySession { host, client, session }]
                    if host == "jup" && client == "/dev/pts/3" && session == "db"
            ),
            "ClientSessionChanged forwards a FollowDisplaySession effect: {effects:?}"
        );
        // apply_event mutates no State (the tree group set is untouched); the tty match +
        // selection follow are loop-owned.
        assert_eq!(state.groups.len(), before_groups);
        assert_eq!(state.groups[0].sessions.len(), before_sessions);
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
            mux: "tmux".into(),
            windows: 1,
            attached: false,
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
        // attachments (the keep-alive guarantee) - no sync effect.
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
            "a failed enumeration keeps attachments - no sync effect: {effects:?}"
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

    #[test]
    fn muxes_found_forwards_the_add_to_the_loop() {
        // Which muxes a machine ALREADY serves lives in the host registry, which this
        // layer does not hold, so the whole decision is forwarded rather than folded.
        let mut state = State::from_sources(vec!["prod".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let before = state.groups.len();
        let effects = state.apply_event(
            HostEvent::MuxesFound {
                machine: "prod".into(),
                muxes: vec!["tmux".into(), "zellij".into()],
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                &effects[..],
                [EventEffect::AddDiscoveredSources { machine, muxes }]
                    if machine == "prod" && muxes == &["tmux".to_string(), "zellij".to_string()]
            ),
            "{effects:?}"
        );
        assert_eq!(state.groups.len(), before, "and folds nothing itself");
    }

    // --- fold_op_result: State owns the op-result inventory mutation ----------
    // A completed MuxOp's OpResult folds its inventory change (groups) into State;
    // the returned OpFollow tells the switcher only how to rebuild the rows + move
    // the cursor. State owns the domain mutation.

    #[test]
    fn fold_op_result_failed_flashes_and_leaves_inventory_untouched() {
        use crate::ui::ops::{OpFollow, OpResult};
        let mut s = State::default();
        s.fold_op_result(OpResult::Created {
            session: a_sess("api"),
        });
        let before_groups = s.groups.len();
        let follow = s.fold_op_result(OpResult::Failed {
            message: "create failed: boom".into(),
        });
        assert_eq!(s.groups.len(), before_groups, "a failure mutates no groups");
        assert!(
            matches!(follow, OpFollow::Flash(m) if m == "create failed: boom"),
            "a failure carries its message to the switcher's flash"
        );
    }

    // --- chrome ownership: State owns the chrome view-state -------------------

    #[test]
    fn flash_sets_message_and_key_clears_it() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut state, mut sw) = with_switcher(one_session_scan());
        state.flash("boom");
        assert_eq!(
            state.chrome.flash, "boom",
            "State::flash sets the chrome flash"
        );
        // A navigation key clears the flash (the switcher's handle_key clear path).
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
        assert!(
            state.chrome.flash.is_empty(),
            "a key clears the flash so the normal hint bar returns"
        );
    }
}
