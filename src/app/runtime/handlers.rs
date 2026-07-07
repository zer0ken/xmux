use super::*;

impl Runtime {
    /// Applies one [`HostEvent`]: [`State::apply_event`] folds the self-contained arms
    /// (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable mark) into
    /// `State` and returns the mux follow-ups it cannot perform; this executes them (it
    /// holds the host clients, the registry, and the display worker the state layer must
    /// not reach). Drained in a burst by `on_host_event`. Returns `true` when the caller
    /// should rearm `attach_deadline` + mark dirty (the matched-client detach-reap path).
    pub(super) fn handle_host_event(&mut self, ev: HostEvent) -> bool {
        let mut rearm = false;
        for effect in self
            .state
            .apply_event(ev, &mut self.switcher, &mut self.connected)
        {
            if self.run_event_effect(effect) {
                rearm = true;
            }
        }
        rearm
    }

    /// Carries out one [`EventEffect`](crate::model::EventEffect) `State::apply_event`
    /// returned — the mux I/O the state layer cannot perform (the single-owner inventory
    /// fold into `model::Host`, a control-mode probe, the attach registry, the detection
    /// dispatch). Returns `true` only for the matched-client display-attach reap, which
    /// asks the caller to rearm `attach_deadline` + mark `dirty` (the recover-from-detach
    /// path).
    pub(super) fn run_event_effect(&mut self, effect: crate::model::EventEffect) -> bool {
        use crate::model::EventEffect;
        // Split-borrow the world state into the loose names the arms below use, so this
        // body stays the loop's imperative effect executor without a per-line `self.`.
        let Self {
            mgr,
            hosts,
            registry,
            switcher,
            state,
            panes_requested,
            detecting,
            worker,
            driver_pty_tx: pty_tx,
            attach_seq,
            cols,
            body_rows: rows,
            tree_width,
            ..
        } = self;
        let (cols, rows, tree_width) = (*cols, *rows, *tree_width);
        match effect {
            EventEffect::ApplyInventory { host, sessions } => {
                // The reader carried the parsed sessions on the event. Fold them into the
                // single owner (`model::Host.inventory`), apply them to the tree, request
                // each session's panes, and sync the display PTY(s). Pane subtrees arrive
                // separately as `HostEvent::Panes` (applied purely by `apply_event`).
                if let Some(h) = hosts.get_mut(&host) {
                    h.inventory.sessions = sessions.clone();
                }
                // Act on the tree/terminals ONLY while the host still has a live client.
                // Per-host FIFO delivers this inventory before the host's `Exited`/reap, so
                // `mgr.get` is normally `Some` here; the gate is the backstop that keeps a
                // broken ordering from reviving a reaped host in the tree
                // (`apply_source_result`) or resyncing its dead terminals. (`ApplyInventory`
                // is emitted only for control-mode hosts, so a poll host is never gated out.)
                if mgr.get(&host).is_some() {
                    switcher.apply_source_result(host.clone(), sessions.clone(), None, state);
                    if let Some(client) = mgr.get(&host) {
                        request_session_panes(client, &sessions, panes_requested);
                    }
                    let n = sessions.len();
                    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                    tracing::info!(host, n, ?names, "sessions_applied");
                    // Sync this host's display terminal(s) (per-host for remote tmux).
                    let mut ctx = crate::driver::DriverCtx {
                        registry: &mut *registry,
                        hosts: &mut *hosts,
                        worker,
                        mgr,
                        pty_tx,
                        attach_seq: &mut *attach_seq,
                        cols,
                        body_rows: rows,
                        tree_width,
                    };
                    sync_source_terminals(&host, &sessions, &mut ctx);
                }
            }
            EventEffect::Refetch { host } => {
                // The server's session/window structure changed (a `%`-notification).
                // Refetch so the tree, panes, and PTY set resync (#5 tree view sync).
                refetch_host(mgr, panes_requested, &host);
            }
            EventEffect::ProbeActiveWindow { host, session_ref } => {
                // A session's ACTIVE WINDOW switched — the structure did NOT change, so do
                // NOT refetch the whole inventory: a full list-sessions + per-session
                // list-panes per change storms the single-threaded loop and freezes the UI
                // during rapid window navigation (each tree step issues select-window,
                // which echoes back as this notification). Probe ONLY the session the
                // notification names (its tmux id, `session_ref`) — never a guessed displayed
                // session; the reply (Focus) resolves the session name + new active window
                // and updates THAT session's marker without any refetch.
                if let Some(client) = mgr.get(&host) {
                    client.probe_active_window(&session_ref);
                }
            }
            EventEffect::ReapHost { host } => {
                mgr.reap(&host);
            }
            EventEffect::ReapDisplayAttach { host, client } => {
                // Reap our display attach ONLY when the detaching client is OUR display client
                // (matched against the in-memory Host.display_tty). An unrelated client's detach
                // can never match, so it is structurally inert — no blanket reap.
                let Some(h) = hosts.get(&host) else {
                    return false;
                };
                if !h.matches_display_tty(&client) {
                    return false;
                }
                let key = host_selection_key(h); // Shared ⇒ key == host id
                registry.remove(&key);
                if let Some(h) = hosts.get_mut(&host) {
                    h.display.clear(&key); // forget the shown session + any in-flight spawn
                    h.display_tty = crate::model::DisplayTty(None); // the dead client's tty is gone
                }
                return true; // rearm recovery
            }
            EventEffect::DispatchScanned { source, detected } => {
                // A detection probe resolved: (re)identify the mux, then dispatch the
                // now-detected host onto its metadata channel (control client or poll task).
                detecting.remove(&source);
                apply_scan_result(hosts, &source, detected);
                let (vc, vr) = terminal_view_size(cols, rows, tree_width);
                dispatch_detected_host(mgr, hosts, &source, vc, vr);
            }
            EventEffect::SyncPollSessions { source, sessions } => {
                // A poll host's SUCCESSFUL enumeration (the tree group is already applied).
                // The `poll enum` debug line is logged UNCONDITIONALLY at the producer
                // (`run_poll`), where `err` is in hand — `apply_event` drops the error path
                // before reaching here, so logging here would only ever see successes.
                // PerSession psmux: a session whose registry .port disappeared is dead even
                // if its PTY has not EOF'd. Drop the stale attach so it cannot show a dead grid.
                if let Some(h) = hosts.get(&source) {
                    for s in &sessions {
                        if !h.session_is_live(&s.name) {
                            // The host-keyed display attachment (one per-host PTY, reattached).
                            registry.remove(&host_selection_key(h));
                        }
                    }
                }
                let mut ctx = crate::driver::DriverCtx {
                    registry: &mut *registry,
                    hosts: &mut *hosts,
                    worker,
                    mgr,
                    pty_tx,
                    attach_seq: &mut *attach_seq,
                    cols,
                    body_rows: rows,
                    tree_width,
                };
                sync_source_terminals(&source, &sessions, &mut ctx);
            }
            EventEffect::RecordDisplayTty { host, tty } => {
                // The -CC `list-clients` probe resolved xmux's display-client tty. Record it
                // on the Host so a session switch is an in-place `switch-client -c <tty>`;
                // `None` (only the control client attached so far) clears any stale tty.
                if let Some(h) = hosts.get_mut(&host) {
                    if tty.is_some() {
                        tracing::debug!(host, ?tty, "display_tty_recorded");
                    }
                    h.record_display_tty(tty);
                }
            }
        }
        false
    }
}

impl Runtime {
    /// Builds the world state from `env` and returns it alongside the loop's receiver
    /// halves ([`LoopIo`]). Pure construction — it starts NO probes (the startup scan
    /// is kicked from `run_app`), so a headless unit test can build a `Runtime`.
    pub(super) fn new(env: Arc<Env>) -> (Runtime, LoopIo) {
        let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let (cols, body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row
                                                                    // Restore the natural tree width the user last set; clamp a stale out-of-range
                                                                    // value, fall back to the default when none is saved.
        let tree_width_natural = adjust_tree_width(
            crate::prefs::load_tree_width(&env.xmux_dir).unwrap_or(crate::ui::switcher::TREE_WIDTH),
            0,
        );
        let tree_width = tree_width_natural;
        let auto_hide_tree = crate::prefs::load_auto_hide_tree(&env.xmux_dir)
            .unwrap_or_else(|| env.cfg.ui_auto_hide_tree());

        // The control-mode metadata clients: one per remote host.
        let (host_tx, host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mgr = HostManager::new(host_tx);
        // The live PTY attachments: one real attached mux client per session.
        let (pty_tx, pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let driver_pty_tx = pty_tx.clone();
        let worker = DisplayWorker::new(pty_tx);
        let registry = AttachRegistry::new();

        // Host model: the single runtime registry, keyed by id (local first, then each
        // ssh alias in config order), built from the config-assembly products on `Env`.
        let host_os = std::env::consts::OS;
        let hosts = crate::model::Hosts::build(
            &env.cfg,
            &env.ssh_aliases,
            host_os,
            &env.xmux_dir,
            env.local_socket.clone(),
        );

        // The app's runtime state (single source of truth), seeded from the host ids;
        // events stream the tree in.
        let mut state = crate::state::State::from_sources(hosts.ids().to_vec());
        let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
        // Feed the switcher the ssh config so an unreachable host's info panel can show
        // its Host/Match stanza. Read once; a missing file just yields no stanza.
        state.chrome.set_ssh_config_text(
            std::fs::read_to_string(crate::env::ssh_config_path()).unwrap_or_default(),
        );
        // View border colours: the config baseline (explicit overrides + stock fallback),
        // applied before any host is displayed and whenever no mux answers. Once a host is
        // displayed its live pane-*-border-style is queried and re-resolved (on_border_styles).
        state
            .chrome
            .set_view_border_colors(crate::ui::switcher::ViewBorderColors::resolve(
                "",
                "",
                &env.cfg.ui.view_active_border_style,
                &env.cfg.ui.view_border_style,
                &env.cfg.ui.view_border_hover_style,
            ));
        // The help modal must show the prefix the user configured, not a literal.
        state.chrome.set_ui_prefix(env.ui_prefix.clone());

        // The live mutate ops (create/rename/kill) — NOT tree probing.
        let ops = env.ops();
        let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
        let term_input = crate::display::input::TermInput::new(prefix);
        let tree_decoder = crate::display::decode::KeyDecoder::new();
        let (op_tx, op_rx) = tokio::sync::mpsc::unbounded_channel();
        let (border_tx, border_rx) = tokio::sync::mpsc::unbounded_channel();

        let rt = Runtime {
            env,
            ops,
            hosts,
            mgr,
            registry,
            worker,
            switcher,
            state,
            // Off-loop attach sequence. The in-flight set / reaped-ids / which session
            // each display shows live on each `host.display` (HostDisplay).
            attach_seq: 0,
            driver_pty_tx,
            op_tx,
            cols,
            body_rows,
            tree_width,
            tree_width_natural,
            auto_hide_tree,
            mouse_state: MouseState::default(),
            term_input,
            tree_decoder,
            prefix,
            connected: HashSet::new(),
            panes_requested: HashSet::new(),
            detecting: HashSet::new(),
            // The draw hot path's observability (per-key grid fingerprints + slow-step
            // probe), owned off the draw block so it does nothing but lock → render.
            draw_observer: DrawObserver::default(),
            spinner_start: std::time::Instant::now(),
            dirty: true,
            last_draw: std::time::Instant::now() - std::time::Duration::from_millis(FRAME_MS),
            width_dirty: false,
            width_flush_at: None,
            border_tx,
            border_cache: HashMap::new(),
            border_inflight: HashSet::new(),
            border_applied: None,
        };
        (
            rt,
            LoopIo {
                host_rx,
                pty_rx,
                op_rx,
                border_rx,
            },
        )
    }

    /// Folds a detached border-style query result: re-resolve the view border colours
    /// (mux value < config override < stock default), cache them per source, clear the
    /// in-flight mark, and apply to the chrome only if that source is still displayed.
    pub(super) fn on_border_styles(&mut self, src: String, ra: String, ri: String) {
        let c = crate::ui::switcher::ViewBorderColors::resolve(
            &ra,
            &ri,
            &self.env.cfg.ui.view_active_border_style,
            &self.env.cfg.ui.view_border_style,
            &self.env.cfg.ui.view_border_hover_style,
        );
        self.border_inflight.remove(&src);
        self.border_cache.insert(src.clone(), c);
        if self.state.displayed.source == src {
            self.state.chrome.set_view_border_colors(c);
            self.border_applied = Some(src);
        }
    }

    /// The loop top: advance the spinner, reconcile the modal/tree-width, run the `r`
    /// reattach-kick, follow the active window in terminal focus, sync the selection,
    /// drive one debounce beat (the settled attach), flush the debounced width persist,
    /// then draw the gated frame. `term` is the loop-local ratatui terminal.
    pub(super) fn prepare_and_draw(&mut self, term: &mut Term) {
        use std::time::Duration;
        // Advance the spinner from wall-clock so it animates regardless of which arm fired.
        self.state
            .chrome
            .set_spinner_frame(spinner_frame_at(self.spinner_start.elapsed()));
        self.state
            .chrome
            .set_view_border_hovered(self.mouse_state.hovered_view_border);
        // Derive the modal dimension of focus from the open-modal kind (single owner of
        // the modal/view reconciliation).
        let modal_kind = self.state.modal_kind();
        self.state.focus.sync_modal(modal_kind);
        // The single owner of the effective tree width: reconcile it to the focus + the
        // hide setting + any natural-width change. On a change, resize the PTYs so the
        // mux reflows, and mark dirty.
        let want_tree_width = reconciled_tree_width(
            self.state.focus.is_terminal_focused(),
            self.auto_hide_tree,
            self.tree_width_natural,
        );
        if want_tree_width != self.tree_width {
            // Crossing the hidden sentinel (0) flips the column TOPOLOGY; a stale wide-char
            // cell at the new boundary can survive ratatui's diff, so force a full repaint.
            let crossed_hidden = (want_tree_width == 0) != (self.tree_width == 0);
            self.tree_width = want_tree_width;
            let (vc, vr) = terminal_view_size(self.cols, self.body_rows, self.tree_width);
            self.registry.resize_all(vc, vr);
            self.mgr.resize_all(vc, vr);
            if crossed_hidden {
                if let Err(e) = term.clear() {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
            }
            self.dirty = true;
        }
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::display::term::ensure_mouse_capture();
        // An `r` re-scan also re-attaches the CURRENT display: tear the (possibly dead)
        // attachment down and clear its latch so the attach below re-creates a fresh
        // client for the viewed session.
        if self.switcher.take_reattach_kick() && !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            self.registry.remove(&key);
            if let Some(h) = self.hosts.get_mut(&self.state.selection.source) {
                h.display.clear(&key); // drop the prior latch so the re-attach is fresh
            }
            self.state.apply(crate::model::Action::ClearDisplay); // nothing confirmed → blank view
            self.state.apply(crate::model::Action::RearmAttachNow {
                now: std::time::Instant::now(),
            });
        }
        // In terminal focus the tree selection tracks the displayed session's active
        // window (idempotent, so calling it each iteration is cheap).
        if self.state.focus.is_terminal_focused() {
            self.switcher.select_active_window(&mut self.state);
        }
        if sync_selection_from_switcher(&mut self.state, &self.switcher) {
            // The selection moved → the tree needs a redraw. The attach is NOT issued
            // here; the Tick below arms the debounce, re-armed on every move.
            self.dirty = true;
        }
        // Drive one debounce beat. The clock + the registry/host attach facts enter as
        // DATA on the Tick; State::apply owns the arm/fire decision.
        {
            let (key_live, in_flight) =
                selection_attach_facts(&self.registry, &self.hosts, &self.state.selection);
            let cmds = self.state.apply(crate::model::Action::Tick {
                now: std::time::Instant::now(),
                key_live,
                in_flight,
            });
            for cmd in cmds {
                match cmd {
                    crate::model::Command::PersistLastSession(addr) => {
                        crate::prefs::save_last_session(&self.env.xmux_dir, &addr);
                    }
                    crate::model::Command::Attach(sel) => {
                        let t = std::time::Instant::now();
                        // select_attach picks the host's driver and hands it the intent.
                        let shown = select_attach(
                            &sel,
                            &mut crate::driver::DriverCtx {
                                registry: &mut self.registry,
                                hosts: &mut self.hosts,
                                worker: &self.worker,
                                mgr: &self.mgr,
                                pty_tx: &self.driver_pty_tx,
                                attach_seq: &mut self.attach_seq,
                                cols: self.cols,
                                body_rows: self.body_rows,
                                tree_width: self.tree_width,
                            },
                        );
                        if shown {
                            // Advance the display truth synchronously ONLY for a confirmed
                            // in-place path: a live grid for the key exists AND no reattach
                            // is in flight. A pending reattach KEEPS the prior session's grid
                            // (stale-while-revalidate) until DisplayReady swaps it in.
                            let k = display_key(&self.hosts, &sel);
                            let reattach_pending = self
                                .hosts
                                .get(&sel.source)
                                .is_some_and(|h| h.display.in_flight_contains(&k));
                            if self.registry.contains(&k) && !reattach_pending {
                                self.state
                                    .apply(crate::model::Action::ConfirmDisplay(sel.clone()));
                            }
                        }
                        DrawObserver::slow_step("select_attach", t);
                        self.dirty = true;
                        let key = display_key(&self.hosts, &sel);
                        let session = &sel.session;
                        tracing::debug!(key, session, "selection");
                    }
                    // The settled-selection Tick never returns the synchronous key/ctl-only
                    // commands or a session-lifecycle RunOp.
                    crate::model::Command::SelectAddress(_)
                    | crate::model::Command::Rescan
                    | crate::model::Command::AdjustTreeWidth(_)
                    | crate::model::Command::ToggleAutoHide
                    | crate::model::Command::RunOp(_)
                    | crate::model::Command::Quit => {}
                }
            }
        }

        // Flush the debounced tree-width persist once the resize burst settles.
        if self.width_dirty
            && self
                .width_flush_at
                .is_some_and(|d| std::time::Instant::now() >= d)
        {
            crate::prefs::save_tree_width(&self.env.xmux_dir, self.tree_width_natural);
            self.width_dirty = false;
            self.width_flush_at = None;
        }

        // Match the view border colours to the displayed host's live mux server. The
        // resolved colours are cached per source (border-style is server-global and rarely
        // changes), so a displayed host is queried at most once; a cache hit applies
        // synchronously, a miss spawns one detached query whose result folds via
        // on_border_styles. The chrome keeps the config baseline until the result lands.
        {
            let src = self.state.displayed.source.clone();
            if !src.is_empty() && self.border_applied.as_deref() != Some(&src) {
                if let Some(&c) = self.border_cache.get(&src) {
                    self.state.chrome.set_view_border_colors(c);
                    self.border_applied = Some(src);
                } else if self.border_inflight.insert(src.clone()) {
                    let ops = self.ops.clone();
                    let tx = self.border_tx.clone();
                    tokio::spawn(async move {
                        let (ra, ri) = ops.border_styles(&src).await.unwrap_or_default();
                        let _ = tx.send((src, ra, ri));
                    });
                }
            }
        }

        // Draw the split (tree + selected session's live grid). GATED — redraw only when
        // something changed AND at most once per frame, so rapid navigation / a busy PTY
        // cannot flood the terminal.
        if self.dirty && self.last_draw.elapsed() >= Duration::from_millis(FRAME_MS) {
            // Render the CONFIRMED display truth (`displayed`), not the selection: the prior
            // session stays on screen until the new one is ready (stale-while-revalidate).
            let grid_arc = current_grid(
                &self.state.displayed,
                &crate::driver::DriverCtx {
                    registry: &mut self.registry,
                    hosts: &mut self.hosts,
                    worker: &self.worker,
                    mgr: &self.mgr,
                    pty_tx: &self.driver_pty_tx,
                    attach_seq: &mut self.attach_seq,
                    cols: self.cols,
                    body_rows: self.body_rows,
                    tree_width: self.tree_width,
                },
            );
            let terminal_focused = self.state.focus.is_terminal_focused();
            // The view border glyph reflects auto-hide-tree mode (║ on, │ off).
            self.state.chrome.set_auto_hide(self.auto_hide_tree);
            let t_draw = std::time::Instant::now();
            if let Err(e) = match &grid_arc {
                Some(g) => {
                    let t_lock = std::time::Instant::now();
                    let guard = g.lock().ok();
                    DrawObserver::slow_step("grid_lock", t_lock);
                    // Compute the grid fingerprint under the same lock used for rendering;
                    // the observer emits display_grid_changed only on a real content change.
                    if let Some(grid) = guard.as_deref() {
                        let addr = display_key(&self.hosts, &self.state.displayed);
                        let session = &self.state.displayed.session;
                        let fp = grid.fingerprint();
                        match self.draw_observer.observe(&addr, session, fp) {
                            FpOutcome::Unchanged => {}
                            FpOutcome::Steady => {
                                tracing::trace!(addr = %addr, session = %session, fp, "display_grid_changed");
                            }
                            FpOutcome::Switched => {
                                tracing::info!(addr = %addr, session = %session, fp, "display_grid_changed");
                            }
                        }
                    }
                    // Split-borrow so the draw closure captures only these fields, not all
                    // of `self` (the fingerprint block's borrows have ended above).
                    let switcher = &mut self.switcher;
                    let state = &self.state;
                    let tree_width = self.tree_width;
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        switcher.render(f, guard.as_deref(), terminal_focused, tree_width, state);
                        DrawObserver::slow_step("render", t_render);
                    })
                }
                None => {
                    let switcher = &mut self.switcher;
                    let state = &self.state;
                    let tree_width = self.tree_width;
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        switcher.render(f, None, terminal_focused, tree_width, state);
                        DrawObserver::slow_step("render", t_render);
                    })
                }
            } {
                tracing::warn!(error = %e, "term_draw_failed");
            }
            DrawObserver::slow_step("draw", t_draw);
            // The grids are now on screen — clear every attachment's output-coalescing flag.
            self.registry.clear_all_pending();
            self.dirty = false;
            self.last_draw = std::time::Instant::now();
        }
    }

    /// The `host_rx` arm: apply one host event, then drain a burst (bounded) so a `%`-event
    /// flood coalesces into one redraw. Re-arms the attach debounce on the detach-reap path.
    pub(super) fn on_host_event(
        &mut self,
        ev: HostEvent,
        host_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    ) {
        let t = std::time::Instant::now();
        if self.handle_host_event(ev) {
            self.state.apply(crate::model::Action::RearmAttach {
                now: std::time::Instant::now(),
            });
            self.dirty = true;
        }
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match host_rx.try_recv() {
                Ok(ev) => {
                    if self.handle_host_event(ev) {
                        self.state.apply(crate::model::Action::RearmAttach {
                            now: std::time::Instant::now(),
                        });
                        self.dirty = true;
                    }
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        DrawObserver::slow_step("host_drain", t);
    }

    /// The `pty_rx` arm: a kept attachment fed its grid or hit EOF (reap). Detach-to-recover
    /// re-attaches the VIEWED session if its client exits; a background session is just reaped.
    pub(super) fn on_pty_event(
        &mut self,
        ev: PtyEvent,
        pty_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    ) {
        // Capture the viewed attach id BEFORE any reap removes it; a background session
        // dropping (tree focus, or a non-displayed attach) is just reaped.
        let displayed_attach_id = (self.state.focus.is_terminal_focused()
            && !self.state.selection.is_empty())
        .then(|| {
            self.registry
                .get(&display_key(&self.hosts, &self.state.selection))
                .map(|a| a.id())
        })
        .flatten();
        let mut detached = false;
        match ev {
            PtyEvent::Exited { id } => {
                if Some(id) == displayed_attach_id {
                    detached = true;
                }
                clear_display_tty_for_attach(&mut self.hosts, &self.registry, id);
                if !self.registry.reap(id) {
                    // pre-Ready Exited: registry has no id yet. Attribute to the owning host
                    // via pending so its Ready tears down instead of inserting a dead pane.
                    self.hosts
                        .iter_mut()
                        .any(|h| h.display.mark_reaped_if_pending(id));
                }
            }
            PtyEvent::DisplayTty { id, tty } => {
                record_display_tty(&mut self.hosts, &self.registry, id, tty)
            }
            PtyEvent::Output { .. } => {}
        }
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match pty_rx.try_recv() {
                Ok(PtyEvent::Exited { id }) => {
                    if Some(id) == displayed_attach_id {
                        detached = true;
                    }
                    clear_display_tty_for_attach(&mut self.hosts, &self.registry, id);
                    if !self.registry.reap(id) {
                        self.hosts
                            .iter_mut()
                            .any(|h| h.display.mark_reaped_if_pending(id));
                    }
                    budget -= 1;
                }
                Ok(PtyEvent::Output { .. }) => {
                    budget -= 1;
                }
                Ok(PtyEvent::DisplayTty { id, tty }) => {
                    record_display_tty(&mut self.hosts, &self.registry, id, tty);
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        if detached {
            // The viewed session's client detached/exited — recover by re-attaching it
            // (reaped above, so the loop-top attach re-fires once its PTY is gone).
            self.state.apply(crate::model::Action::RearmAttach {
                now: std::time::Instant::now(),
            });
            self.dirty = true;
        }
    }

    /// The worker `Ready`/`Failed` arm. `HostDisplay` owns the reap/install/stale DECISION;
    /// the loop performs the registry install/teardown it alone can.
    pub(super) fn on_display_event(&mut self, ev: DisplayEvent) {
        match ev {
            DisplayEvent::Ready {
                seq,
                key,
                attachment,
            } => {
                let hid = host_of_key(&key).to_string();
                let id = attachment.id();
                let outcome = match self.hosts.get_mut(&hid) {
                    Some(h) => {
                        tracing::info!(key, seq, id, "attach_ready");
                        Some(h.display.resolve_ready(&key, seq, id))
                    }
                    None => None,
                };
                match outcome {
                    Some(crate::model::ReadyOutcome::Install { shown }) => {
                        // Swap: tear down the stale attachment held under this key (the prior
                        // session, kept on screen until now) and install the fresh one.
                        self.registry.remove(&key);
                        self.registry.insert(&key, attachment);
                        self.state
                            .apply(crate::model::Action::ConfirmDisplay(Selection {
                                source: hid.clone(),
                                session: shown,
                                window: None,
                            }));
                    }
                    // Reaped-race, stale seq, or unknown host: tear the fresh attachment down
                    // (resolve_ready already cleared the bookkeeping for the first two).
                    Some(_) | None => attachment.teardown(),
                }
            }
            DisplayEvent::Failed { seq, key, message } => {
                let hid = host_of_key(&key).to_string();
                if let Some(h) = self.hosts.get_mut(&hid) {
                    h.display.resolve_failed(&key, seq);
                }
                tracing::warn!(key, error = %message, "attach_failed");
            }
        }
    }

    /// The `stdin_rx` arm: route a raw read (mouse/keys) through the input core. Returns
    /// whether the app should quit.
    pub(super) fn on_stdin(&mut self, bytes: &[u8]) -> bool {
        use std::time::Duration;
        // Clone the selection so &mut state can be threaded alongside it (the ForwardToMux
        // path reads the selection for display_key/registry input).
        let selection = self.state.selection.clone();
        let outcome = self.handle_stdin_bytes(bytes, &selection);
        if outcome.dirty {
            self.dirty = true;
        }
        if outcome.width_changed {
            self.width_dirty = true;
            self.width_flush_at =
                Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
        }
        outcome.quit
    }

    /// The control-socket arm: headless op/status/dump/key/bytes. Returns whether to quit.
    pub(super) fn on_ctl_command(&mut self, cmd: crate::ui::run::Cmd, term: &mut Term) -> bool {
        use crate::ui::run::{dump_screen, Cmd};
        use std::time::Duration;
        match cmd {
            Cmd::Op(action) => {
                // dispatch_action spawns any RunOp off-loop itself; its OpResult folds back
                // through op_tx as usual.
                let (quit_op, wc) = dispatch_action(
                    action,
                    &mut self.switcher,
                    &mut self.state,
                    &mut self.tree_width_natural,
                    &mut self.auto_hide_tree,
                    &self.env.xmux_dir,
                    (&self.ops, &self.op_tx),
                );
                if wc {
                    self.width_dirty = true;
                    self.width_flush_at =
                        Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                }
                if quit_op {
                    return true;
                }
                // A Switch/Focus may need the selection's host connected.
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.switcher,
                    self.cols,
                    self.body_rows,
                    self.tree_width,
                );
                if sync_selection_from_switcher(&mut self.state, &self.switcher) {
                    self.dirty = true;
                }
            }
            Cmd::Status(reply) => {
                let _ = reply.send(status_line(
                    &self.switcher,
                    self.state.focus.view_is_tree(),
                    &self_cwd(),
                    &self_tty(),
                ));
            }
            Cmd::Dump(reply) => {
                let sz = term.size().unwrap_or(ratatui::layout::Size {
                    width: 80,
                    height: 24,
                });
                let grid_arc = current_grid(
                    &self.state.displayed,
                    &crate::driver::DriverCtx {
                        registry: &mut self.registry,
                        hosts: &mut self.hosts,
                        worker: &self.worker,
                        mgr: &self.mgr,
                        pty_tx: &self.driver_pty_tx,
                        attach_seq: &mut self.attach_seq,
                        cols: self.cols,
                        body_rows: self.body_rows,
                        tree_width: self.tree_width,
                    },
                );
                let dump = match &grid_arc {
                    Some(g) => {
                        let guard = g.lock().ok();
                        dump_screen(
                            &mut self.switcher,
                            guard.as_deref(),
                            sz.width,
                            sz.height,
                            &self.state,
                        )
                    }
                    None => dump_screen(&mut self.switcher, None, sz.width, sz.height, &self.state),
                };
                let _ = reply.send(dump);
            }
            Cmd::RawKey(k) => {
                // Route the FULL command batch through the single dispatcher (RunOp spawns
                // off-loop, its OpResult folding back through op_tx).
                let cmds = self.switcher.handle_key(k, &mut self.state);
                let (quit_key, wc) = dispatch_commands(
                    cmds,
                    &mut self.switcher,
                    &mut self.state,
                    &mut self.tree_width_natural,
                    &mut self.auto_hide_tree,
                    &self.env.xmux_dir,
                    (&self.ops, &self.op_tx),
                );
                if wc {
                    self.width_dirty = true;
                    self.width_flush_at =
                        Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                }
                if quit_key {
                    return true;
                }
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.switcher,
                    self.cols,
                    self.body_rows,
                    self.tree_width,
                );
                if sync_selection_from_switcher(&mut self.state, &self.switcher) {
                    self.dirty = true;
                }
            }
            Cmd::RawBytes(bytes) => {
                if !bytes.is_empty() {
                    // Inject into the VISIBLE session (`displayed`), matching the interactive
                    // keystroke path.
                    if let Some(host) = self.hosts.get(&self.state.displayed.source) {
                        let mut driver = crate::driver::driver_for(host);
                        let ctx = crate::driver::DriverCtx {
                            registry: &mut self.registry,
                            hosts: &mut self.hosts,
                            worker: &self.worker,
                            mgr: &self.mgr,
                            pty_tx: &self.driver_pty_tx,
                            attach_seq: &mut self.attach_seq,
                            cols: self.cols,
                            body_rows: self.body_rows,
                            tree_width: self.tree_width,
                        };
                        driver.input(&self.state.displayed, bytes, &ctx);
                    }
                }
            }
        }
        false
    }

    /// The op-result arm: fold a finished mutate op back into the tree/state.
    pub(super) fn on_op_result(&mut self, result: crate::ui::switcher::OpResult) {
        self.switcher.apply_op_result(result, &mut self.state);
    }

    /// The animation-tick arm: detect a console resize (push the new size to PTYs +
    /// control clients, force a full repaint) and refresh the connecting-spinner set.
    pub(super) fn on_tick(&mut self, term: &mut Term) {
        // Resize detection: poll the console size (an ioctl, not a stdin read).
        if let Ok((c, r)) = ratatui::crossterm::terminal::size() {
            if (c, r) != (self.cols, self.body_rows + 1) {
                let body = r.saturating_sub(1);
                self.cols = c;
                self.body_rows = body;
                let (vc, vr) = terminal_view_size(c, body, self.tree_width);
                self.registry.resize_all(vc, vr);
                self.mgr.resize_all(vc, vr);
                let _ = term.autoresize();
                // A console resize reflows the existing cells; force a full repaint.
                if let Err(e) = term.clear() {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
                self.dirty = true;
            }
        }
        // Spinner set = the selected session if its PTY is still connecting.
        let mut sp = HashSet::new();
        if !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.state.selection.source)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if in_flight_for_key || self.registry.connecting(&key) {
                sp.insert(self.state.selection.address());
            }
        }
        self.state.chrome.set_spinner(sp);
    }

    /// The reconnect-sweep arm: re-ensure died metadata channels, re-detect undetected
    /// hosts, re-warm dropped control-host PTYs, capture display ttys, and re-attach the
    /// selected session if its display terminal dropped. The sole automatic retry path.
    pub(super) fn on_reconnect(&mut self) {
        let (vc, vr) = terminal_view_size(self.cols, self.body_rows, self.tree_width);
        // Snapshot the ids so the loops can re-borrow `hosts` (incl. &mut) without holding
        // the `ids()` borrow across the body.
        let ids: Vec<String> = self.hosts.ids().to_vec();
        // Self-heal sweep: a DETECTED host re-ensures its metadata channel; an UNDETECTED
        // one retries detection.
        for id in &ids {
            let detected = self.hosts.get(id).map(|h| h.detected).unwrap_or(false);
            if detected {
                if let Some(host) = self.hosts.get(id) {
                    let _ = self.mgr.ensure(id, host, vc, vr);
                }
            } else {
                scan_or_dispatch_host(&mut self.mgr, &self.hosts, &mut self.detecting, id, vc, vr);
            }
        }
        // Re-warm each control host's dropped per-host PTY via its driver (ENSURE-ONLY;
        // a host with no known sessions yet is skipped rather than reaping a live PTY).
        for id in &ids {
            if self.mgr.get(id).is_none() {
                continue;
            }
            let inventory = match self.hosts.get(id) {
                Some(h) => h.inventory.sessions.clone(),
                None => continue,
            };
            if inventory.is_empty() {
                continue;
            }
            let mut ctx = crate::driver::DriverCtx {
                registry: &mut self.registry,
                hosts: &mut self.hosts,
                worker: &self.worker,
                mgr: &self.mgr,
                pty_tx: &self.driver_pty_tx,
                attach_seq: &mut self.attach_seq,
                cols: self.cols,
                body_rows: self.body_rows,
                tree_width: self.tree_width,
            };
            sync_source_terminals(id, &inventory, &mut ctx);
        }
        // Capture the display-client tty for any shared host whose display attach is live
        // but whose tty is not yet known (retried each sweep).
        for id in &ids {
            let Some(h) = self.hosts.get(id) else {
                continue;
            };
            if h.display_tty.0.is_some() {
                continue;
            }
            if self.registry.contains(&host_selection_key(h)) {
                if let Some(client) = self.mgr.get(id) {
                    client.capture_display_tty();
                }
            }
        }
        // Re-attach the selected session's display terminal if it dropped.
        if !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.state.selection.source)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if !self.registry.contains(&key) && !in_flight_for_key {
                let mut ctx = crate::driver::DriverCtx {
                    registry: &mut self.registry,
                    hosts: &mut self.hosts,
                    worker: &self.worker,
                    mgr: &self.mgr,
                    pty_tx: &self.driver_pty_tx,
                    attach_seq: &mut self.attach_seq,
                    cols: self.cols,
                    body_rows: self.body_rows,
                    tree_width: self.tree_width,
                };
                select_attach(&self.state.selection, &mut ctx);
            }
        }
    }
}
