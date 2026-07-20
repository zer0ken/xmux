//! The control-mode reader: the line state machine that turns a `-CC` child's
//! stdout into `HostEvent`s, correlating reply blocks via the in-flight FIFO.

use std::sync::atomic::Ordering;

use crate::mux::{parse_panes, parse_sessions};
use crate::mux::{ControlProtocol, Line, Notif};

use super::{HostEvent, InFlight, PendingReply, ReaderState};

/// Runs the line state machine over `lines` (an `Iterator<Item=String>` of stdout
/// lines, already split on `\n`), driving `state`, `in_flight`, and emitting events
/// via `emit`. Returns when the iterator ends (child EOF). Pure over its inputs so
/// a test feeds canned bytes; the real reader wraps a `BufRead`.
pub fn run_reader<E: FnMut(HostEvent)>(
    host: &str,
    proto: &dyn ControlProtocol,
    lines: impl Iterator<Item = String>,
    state: &ReaderState,
    in_flight: &InFlight,
    mut emit: E,
) {
    // num, kind, body — the open `%begin` block, if any.
    let mut block: Option<(u64, PendingReply, Vec<String>)> = None;
    // The last %error block's text, so a never-connected exit carries a meaningful
    // reason (notably "no sessions" / "no server running" → reachable-but-empty).
    let mut last_error: Option<String> = None;
    for line in lines {
        // The entry DCS `\x1bP1000p` ([research §1]) introduces control mode. It may
        // arrive on its own line or glued to the first `%begin`. Strip it; a lone DCS
        // becomes empty (Body, ignored), a glued line classifies its remainder as
        // `%begin`. Correlation does NOT depend on it: blocks are matched by the
        // `%begin` flags bit (see below), not by a fragile startup-banner heuristic.
        let line = line
            .strip_prefix("\x1bP1000p")
            .map(str::to_string)
            .unwrap_or(line);
        // Inside a block, only the matching %end/%error closes it; everything
        // else is body (notifications never appear inside a block).
        if let Some((num, _, _)) = block.as_ref() {
            let num = *num;
            let (close, is_err) = match proto.classify(&line) {
                Line::End { num: n } if n == num => (true, false),
                Line::Error { num: n } if n == num => (true, true),
                _ => (false, false),
            };
            if close {
                let (_, kind, body) = block.take().unwrap();
                // Remember an error block's text ("no sessions" / "no server running"
                // / …) so a control client that dies before connecting carries it —
                // the app then tells a reachable-but-empty mux from a dead host.
                if is_err {
                    let t = body.join(" ").trim().to_string();
                    if !t.is_empty() {
                        last_error = Some(t);
                    }
                }
                resolve_block(host, kind, &body, state, proto, &mut emit);
            } else {
                // Re-borrow only to push; the `as_ref` borrow above has ended.
                block.as_mut().unwrap().2.push(line);
            }
            continue;
        }
        match proto.classify(&line) {
            Line::Begin { num, control } => {
                // A block replying to a command WE sent (flags bit 0 set) pops the
                // correlation FIFO; a spontaneous block (startup banner, another
                // client's command, a hook — flags bit 0 clear) consumes ZERO FIFO
                // entries, so it can never shift our replies. This is robust across
                // tmux versions (3.4 emits a separate flags=0 banner; 3.5a glues the
                // DCS to the first flags=1 reply and trails a flags=0 block).
                let kind = if control {
                    in_flight
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(PendingReply::Ignore)
                } else {
                    PendingReply::Ignore
                };
                block = Some((num, kind, Vec::new()));
            }
            // %output is the per-pane PIXEL stream; the per-session PTY attachments
            // own pixels now, and the control client runs with `refresh-client -f
            // no-output`, so it should not arrive. If an older mux that lacks the
            // flag sends it anyway, discard it (just note the channel is live) — the
            // control client is metadata-only.
            Line::Output { .. } | Line::ExtendedOutput { .. } => clear_connecting(state),
            Line::Notification(n) => dispatch_notif(host, proto, n, &last_error, &mut emit),
            // Stray frame/body outside a block.
            Line::End { .. } | Line::Error { .. } | Line::Body(_) => {}
        }
    }
    // Iterator ended = child stdout EOF.
    emit(HostEvent::Exited {
        host: host.to_string(),
        reason: last_error,
    });
}

/// Resolves a closed `%begin…%end` block by parsing its body and carrying the result
/// on a `HostEvent` — the loop folds it into `model::Host.inventory` (the single
/// owner); the reader holds no inventory. `proto` supplies the mux-specific parse of a
/// `list-clients` body (the display-client tty), so the reader names no tmux wire detail.
fn resolve_block<E: FnMut(HostEvent)>(
    host: &str,
    kind: PendingReply,
    body: &[String],
    state: &ReaderState,
    proto: &dyn ControlProtocol,
    emit: &mut E,
) {
    match kind {
        PendingReply::ListSessions => {
            let out = body.join("\n");
            let sessions = parse_sessions(host, &out);
            clear_connecting(state);
            emit(HostEvent::Connected {
                host: host.to_string(),
                sessions: sessions.clone(),
            });
            emit(HostEvent::Inventory {
                host: host.to_string(),
                sessions,
            });
        }
        PendingReply::ListPanes { address } => {
            let out = body.join("\n");
            let panes = parse_panes(&out);
            // Carry the subtree on the same `Panes` event the poll path uses — the loop
            // applies it purely, no shared inventory to write.
            emit(HostEvent::Panes { address, panes });
        }
        PendingReply::ActiveWindow => {
            // `display-message -p '#{session_name}\t#{window_index}'` prints one line:
            // `<name>\t<index>`. The probe targeted a session id, so the RESOLVED name
            // comes back in the reply. Emit Focus for that session so the app follows
            // the selection (#2). A missing/garbled body (no `name\tindex`) yields no event.
            if let Some((session, window)) = body.iter().find_map(|l| {
                let (name, idx) = l.split_once('\t')?;
                let idx = idx.trim().parse::<i64>().ok()?;
                Some((name.to_string(), idx))
            }) {
                emit(HostEvent::Focus {
                    host: host.to_string(),
                    session,
                    window,
                });
            }
        }
        PendingReply::DisplayClientTty => {
            // A `list-clients` body: the mux protocol parses out the display attach's tty
            // — the reader names no wire detail.
            emit(HostEvent::DisplayTty {
                host: host.to_string(),
                tty: proto.parse_display_client_tty(body),
            });
        }
        PendingReply::Ignore => {}
    }
}

/// Maps one notification to the app event it triggers and emits it. The policy
/// table lives behind the mux's [`ControlProtocol::notif_event`] (a tmux protocol
/// detail); this thin wrapper just forwards the event when there is one.
fn dispatch_notif<E: FnMut(HostEvent)>(
    host: &str,
    proto: &dyn ControlProtocol,
    notif: Notif<'_>,
    last_error: &Option<String>,
    emit: &mut E,
) {
    if let Some(event) = proto.notif_event(host, notif, last_error) {
        emit(event);
    }
}

/// Marks the host as connected once any wire activity proves the channel is live.
fn clear_connecting(state: &ReaderState) {
    state.connecting.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_control_proto;
    use crate::session::Session;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Builds a `ReaderState` with `connecting = true`. The control client is
    /// metadata-only now (no grid); the parsed inventory rides on `HostEvent`s.
    fn test_state(_cols: u16, _rows: u16) -> ReaderState {
        ReaderState {
            connecting: Arc::new(AtomicBool::new(true)),
        }
    }

    /// The sessions the reader carried on its first `Inventory` event — the parsed
    /// list-sessions result the loop folds into `model::Host.inventory`.
    fn carried_sessions(events: &[HostEvent]) -> Vec<Session> {
        events
            .iter()
            .find_map(|e| match e {
                HostEvent::Inventory { sessions, .. } => Some(sessions.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn reader_resolves_list_sessions_block_into_inventory() {
        // The reader holds no shared inventory: the parsed sessions ride on the
        // Connected + Inventory events for the loop to fold into `model::Host`.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        // Both Connected and Inventory carry the parsed session.
        let carried = |e: &HostEvent| match e {
            HostEvent::Connected { sessions, .. } | HostEvent::Inventory { sessions, .. } => {
                Some(sessions.clone())
            }
            _ => None,
        };
        let connected = events
            .iter()
            .find_map(|e| {
                matches!(e, HostEvent::Connected { .. })
                    .then(|| carried(e))
                    .flatten()
            })
            .expect("a Connected event carrying sessions");
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].name, "api");
        assert_eq!(connected[0].source, "jupiter06");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, HostEvent::Inventory { sessions, .. } if sessions.len() == 1)),
            "an Inventory event carries the same session"
        );
        assert!(!state.connecting.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn reader_structure_notifications_emit_changed() {
        // A `%`-notification that the server's session/window STRUCTURE changed
        // (added, closed, renamed, or the set of sessions) must emit Changed: it
        // carries only an id, so the app refetches (re-list-sessions +
        // re-list-panes) to resync the tree + active-window markers (#5).
        for line in [
            "%window-add @4",
            "%window-close @4",
            "%window-renamed @4 logs",
            "%sessions-changed",
        ] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
                "{line:?} must emit Changed"
            );
        }
    }

    #[test]
    fn reader_unlinked_window_notifications_emit_changed() {
        // A window added/closed/renamed in a session OTHER than the control client's
        // OWN attached session arrives as `%unlinked-window-*` (tmux sends the plain
        // `%window-*` form only for the client's current session). The displayed
        // session is usually NOT the control client's session, so without handling
        // these the tree view misses real-time window add/delete there. They must emit
        // Changed exactly like their linked counterparts so the app refetches.
        for line in [
            "%unlinked-window-add @4",
            "%unlinked-window-close @4",
            "%unlinked-window-renamed @4 logs",
        ] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
                "{line:?} must emit Changed"
            );
        }
    }

    #[test]
    fn session_window_changed_emits_active_window_changed_with_payload() {
        // A session's ACTIVE WINDOW switched (`%session-window-changed $id @win`):
        // emit ActiveWindowChanged CARRYING the notification's session id + window id,
        // so the app probes THAT SPECIFIC session (not a guessed displayed one)
        // and follows the tree selection to it (#2). It must NOT collapse to a blanket
        // Changed (which only refetches and would leave the selection behind), and it must
        // NOT drop the payload to a host-only event (which forces the guess).
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            test_control_proto(),
            vec!["%session-window-changed $0 @1".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::ActiveWindowChanged { host, session_id, window_id }
                    if host == "jupiter06" && session_id == "$0" && window_id == "@1"
            )),
            "%session-window-changed must emit ActiveWindowChanged with the $id/@win payload"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, HostEvent::Changed { .. })),
            "%session-window-changed must not collapse to a blanket Changed"
        );
    }

    #[test]
    fn client_session_changed_emits_event_with_client_and_session() {
        // `%client-session-changed <client> $id <name>`: another client's attached session
        // switched. Emit ClientSessionChanged carrying the client tty + the new session name
        // so the supervisor can match the tty against Host.display_tty and follow the nav
        // selection when it is xmux's OWN display attach. It must NOT collapse to a blanket
        // Changed (which refetches and leaves the selection behind).
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter00",
            test_control_proto(),
            vec!["%client-session-changed /dev/pts/3 $2 work".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::ClientSessionChanged { host, client, session }
                    if host == "jupiter00" && client == "/dev/pts/3" && session == "work"
            )),
            "%client-session-changed must emit ClientSessionChanged with the client tty + session"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, HostEvent::Changed { .. })),
            "%client-session-changed must not collapse to a blanket Changed"
        );
    }

    #[test]
    fn reader_resolves_active_window_block_into_focus() {
        // The active-window probe (`display-message -p '#{session_name}\t#{window_index}'`)
        // returns a single line: `<name>\t<index>`. Resolving its block emits Focus
        // carrying the RESOLVED session name + parsed window index (the probe targeted a
        // session id, so the name comes back in the reply — not the correlator), so the
        // app moves the tree selection to that window row of the correct session.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActiveWindow);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "api\t2".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::Focus { host, session, window }
                    if host == "jupiter06" && session == "api" && *window == 2
            )),
            "an active-window block resolves to Focus"
        );
    }

    #[test]
    fn active_window_query_line_quotes_and_escapes() {
        // The probe targets a session (by name or `$id`) and prints its active window's
        // session name + index so the reply resolves BOTH. The format braces are escaped
        // (so `#{session_name}`/`#{window_index}` reach tmux literally) and a target with
        // spaces is quoted for the control-mode parser.
        let proto = test_control_proto();
        // A session id target (`$0`) is quoted by the control-mode target quoter (the `$`
        // is outside the bare-safe set); tmux strips the single-quotes and resolves `$0`
        // as the session id.
        assert_eq!(
            proto.active_window_line("$0"),
            "display-message -p -t '$0' '#{session_name}\t#{window_index}'\n"
        );
        assert_eq!(
            proto.active_window_line("my proj"),
            "display-message -p -t 'my proj' '#{session_name}\t#{window_index}'\n"
        );
    }

    #[test]
    fn reader_session_changed_and_pane_changed_are_inert() {
        // `%session-changed` (the metadata client's own attach) and
        // `%window-pane-changed` do not affect the tree view, so they must NOT
        // trigger a Changed refetch. (run_reader always emits a trailing Exited on
        // EOF, so assert specifically that no Changed was emitted.)
        for line in ["%session-changed $1 api", "%window-pane-changed @1 %2"] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { .. })),
                "{line:?} must not trigger a refetch"
            );
        }
    }

    #[test]
    fn client_detached_emits_host_scoped_event_with_client() {
        let mut events = Vec::new();
        dispatch_notif(
            "jupiter06",
            test_control_proto(),
            Notif::ClientDetached {
                client: "/dev/pts/3",
            },
            &Some("ignored".into()),
            &mut |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::ClientDetached { host, client }
                    if host == "jupiter06" && client == "/dev/pts/3"
            )),
            "%client-detached emits a host-scoped ClientDetached carrying the client tty"
        );
        assert!(
            !events.iter().any(|e| matches!(e, HostEvent::Exited { .. })),
            "it must NOT reap the host (no Exited) — that is the supervisor's tty-matched job"
        );
    }

    /// The `-CC` entry DCS `\x1bP1000p` introduces control mode, and tmux 3.3.6/3.4
    /// emit a flags=0 startup banner block BEFORE the first command reply. The
    /// banner (flags=0) must consume zero FIFO entries; the real list-sessions reply
    /// (flags=1) pops `ListSessions`. Proven in BOTH framings: the DCS on its own
    /// line, and the DCS glued to the banner `%begin`.
    #[test]
    fn reader_startup_banner_keeps_fifo_lockstep() {
        // SEPARATE-line framing: a lone DCS line, the flags=0 banner, then the
        // flags=1 list-sessions reply.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight
                .lock()
                .unwrap()
                .push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p".to_string(),
                "%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                lines,
                &state,
                &in_flight,
                |e| events.push(e),
            );
            let sessions = carried_sessions(&events);
            assert_eq!(
                sessions.len(),
                1,
                "flags=0 banner stole the ListSessions entry"
            );
            assert_eq!(sessions[0].name, "api");
        }
        // GLUED framing: the DCS prefixed onto the flags=0 banner `%begin` line.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight
                .lock()
                .unwrap()
                .push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                lines,
                &state,
                &in_flight,
                |e| events.push(e),
            );
            let sessions = carried_sessions(&events);
            assert_eq!(
                sessions.len(),
                1,
                "glued flags=0 banner stole the ListSessions entry"
            );
            assert_eq!(sessions[0].name, "api");
        }
    }

    #[test]
    fn reader_uses_begin_flags_to_correlate_not_a_banner_heuristic() {
        // A %begin block replying to a command WE sent carries flags=1; a
        // spontaneous block (startup banner, another client's command, a hook) is
        // flags=0. The reader pops the correlation FIFO only for flags=1, so a
        // spontaneous block never shifts the replies. tmux 3.5a glues the entry DCS
        // to the FIRST real reply (flags=1) and emits a trailing spontaneous block
        // (flags=0); a single-banner-skip approach would mis-skip the real reply,
        // desync the FIFO, and resolve list-sessions empty.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let lines = vec![
            "\x1bP1000p%begin 1 10 1".to_string(), // DCS glued to the first reply
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 10 1".to_string(),
            "%begin 1 11 0".to_string(), // spontaneous: must NOT consume a correlator
            "%end 1 11 0".to_string(),
        ]
        .into_iter();
        let mut events = Vec::new();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        let sessions = carried_sessions(&events);
        assert_eq!(
            sessions.len(),
            1,
            "list-sessions resolved against the flags=1 block"
        );
        assert_eq!(sessions[0].name, "api");
    }

    #[test]
    fn reader_spontaneous_block_does_not_steal_a_pending_reply() {
        // A flags=0 block arriving BEFORE our command reply (another client ran a
        // command first) must not consume our queued correlator.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let lines = vec![
            "%begin 1 5 0".to_string(), // spontaneous, flags=0
            "noise".to_string(),
            "%end 1 5 0".to_string(),
            "%begin 1 6 1".to_string(), // our list-sessions reply, flags=1
            "3\t1\t1700000000\twork".to_string(),
            "%end 1 6 1".to_string(),
        ]
        .into_iter();
        let mut events = Vec::new();
        run_reader("h", test_control_proto(), lines, &state, &in_flight, |e| {
            events.push(e)
        });
        assert_eq!(carried_sessions(&events).len(), 1);
    }

    #[test]
    fn reader_exit_emits_exited() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            test_control_proto(),
            vec!["%exit too far behind".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(events.iter().any(|e| matches!(
            e,
            HostEvent::Exited { reason: Some(r), .. } if r == "too far behind"
        )));
    }

    #[test]
    fn reader_no_sessions_error_makes_exit_carry_the_reason() {
        // An empty / no-server mux: `tmux -CC attach` emits a "no sessions" %error
        // block then a bare %exit. The reader must fold the error body into the Exited
        // reason so the app can tell "reachable but empty" from "dead host".
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 1 0".to_string(),
            "no sessions".to_string(),
            "%error 1 1 0".to_string(),
            "%exit".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::Exited { reason: Some(r), .. } if r.contains("no sessions")
            )),
            "the exit reason carries the no-sessions error"
        );
    }

    #[test]
    fn reader_resolves_list_panes_block_into_inventory() {
        // A session's window/pane subtree must arrive via an explicit list-panes
        // query (correlated to the session's `source/name` address); otherwise the
        // session stays on "loading…" forever.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListPanes {
                address: "jupiter00/if".into(),
            });
        let mut events = Vec::new();
        // PANE_FORMAT: window_index, window_active, pane_index, pane_active,
        // pane_current_command, window_name.
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "0\t1\t0\t1\tbash\tmain".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        // The subtree rides on a `Panes` event keyed by the session address — the same
        // carrier the poll path uses; the loop applies it purely (no shared inventory).
        let panes = events
            .iter()
            .find_map(|e| match e {
                HostEvent::Panes { address, panes } if address == "jupiter00/if" => {
                    Some(panes.clone())
                }
                _ => None,
            })
            .expect("a Panes event under the session address");
        assert_eq!(panes.len(), 1, "one window parsed");
    }

    #[test]
    fn reader_resolves_display_tty_block_into_event() {
        // A list-clients block resolves to the NON-control client's tty (xmux's display
        // attach), ignoring the -CC metadata client regardless of line order.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::DisplayClientTty);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "/dev/pts/7 control-mode".to_string(),
            "/dev/pts/3 active-pane".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::DisplayTty { host, tty: Some(t) } if host == "jupiter00" && t == "/dev/pts/3"
            )),
            "a list-clients block resolves to the non-control client's tty"
        );
    }
}
