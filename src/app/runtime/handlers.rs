use super::*;

impl Runtime {
    /// Applies one [`HostEvent`] through the application update transition, then runs
    /// its ordered effects through the unified executor with the host clients,
    /// registry, and display worker that own the required capabilities.
    /// Drained in a burst by `on_host_event`. Returns `true` when the caller should
    /// rearm `attach_deadline` and mark dirty for a matched-client detach reap.
    pub(super) fn handle_host_event(&mut self, mut ev: HostEvent) -> bool {
        if let HostEvent::AuthObserved {
            machine,
            credential_generation,
            ..
        } = &ev
        {
            if *credential_generation != self.env.credentials().generation(machine) {
                return false;
            }
        }
        if let HostEvent::MachineProbed {
            machine,
            credential_held,
            current_credential_generation,
            ..
        } = &mut ev
        {
            *credential_held = self.env.credentials().contains(machine);
            *current_credential_generation = self.env.credentials().generation(machine);
        }
        let effects = update(
            &mut self.model,
            Msg::HostEvent {
                event: ev,
                logged_in: self.env.credentials().machines(),
            },
        );
        let (_, _, rearm) = self.execute_effects(effects);
        rearm
    }

    /// Performs the host-specific I/O carried by one nested event effect. The unified
    /// effect executor delegates this capability work and places any returned follow-up
    /// effects back on its ordered work queue.
    pub(super) fn perform_host_effect(
        &mut self,
        effect: crate::model::EventEffect,
    ) -> (bool, Vec<Effect>) {
        use crate::model::EventEffect;
        let nav = self.model.nav_size();
        // Split-borrow the world state into the loose names the arms below use, so this
        // body stays readable without a per-line `self.`.
        let Self {
            env,
            mgr,
            hosts,
            scan_pool,
            registry,
            model,
            worker,
            driver_pty_tx: pty_tx,
            attach_seq,
            cols,
            body_rows: rows,
            ..
        } = self;
        let (cols, rows) = (*cols, *rows);
        // The nav's live size as one value, read once for this effect: the width the user
        // set, the width on screen, the band height, the attachment side, and whether it
        // is collapsed. Every geometry below is cut from it, so none re-derives a part.
        let mut followups = Vec::new();
        match effect {
            EventEffect::MarkConnected { .. }
            | EventEffect::ApplyHostResult { .. }
            | EventEffect::ApplyPollResult { .. }
            | EventEffect::NoteHostExited { .. } => {
                unreachable!("state event effects are applied before runtime effects")
            }
            EventEffect::ApplyInventory { host, sessions } => {
                // The reader carried the parsed sessions on the event. Fold them into the
                // single owner (`model::Host.inventory`), apply them to the nav, and sync
                // the display PTY(s).
                if let Some(h) = hosts.get_mut(&host) {
                    h.inventory.sessions = sessions.clone();
                }
                // Act on the nav/terminals ONLY while the host still has a live client.
                // Per-host FIFO delivers this inventory before the host's `Exited`/reap, so
                // `mgr.get` is normally `Some` here; the gate is the backstop that keeps a
                // broken ordering from reviving a reaped host in the nav
                // (`apply_host_result`) or resyncing its dead terminals. (`ApplyInventory`
                // is emitted only for control-mode hosts, so a poll host is never gated out.)
                let live = mgr.get(&host).is_some();
                followups = update(
                    model,
                    Msg::ApplyInventory {
                        host: host.clone(),
                        sessions: sessions.clone(),
                        live,
                    },
                );
                if live {
                    let n = sessions.len();
                    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                    tracing::info!(host, n, ?names, "sessions_applied");
                    // Keep the display rename ahead of terminal reconciliation. The update
                    // result already contains that rename, so append sync behind it on the
                    // unified executor's ordered follow-up queue.
                    followups.push(Effect::Event(EventEffect::SyncInventorySessions {
                        host,
                        sessions,
                    }));
                }
            }
            EventEffect::CheckSharedConnection { machine } => {
                if let Some(transport) = hosts.machine_transport(&machine) {
                    spawn_shared_connection_check(machine, transport.clone_box(), mgr.events());
                }
            }
            EventEffect::Refetch { host } => {
                // The server's session/window structure changed (a `%`-notification).
                // Refetch so the nav and PTY set resync (#5 nav view sync).
                refetch_host(mgr, &host);
            }
            EventEffect::ReapHost { host } => {
                mgr.reap(&host);
            }
            EventEffect::DisconnectMachine { machine } => {
                let host_ids: Vec<String> = hosts
                    .ids()
                    .iter()
                    .filter(|host_id| crate::session::machine_of(host_id) == machine)
                    .cloned()
                    .collect();
                for host_id in &host_ids {
                    mgr.reap(host_id);
                    if let Some(host) = hosts.get_mut(host_id) {
                        host.clear_display_tty();
                        host.liveness = crate::model::Liveness::Unreachable;
                        host.display = Default::default();
                    }
                }
                for key in registry.addresses() {
                    if crate::session::machine_of(host_of_key(&key)) == machine {
                        registry.remove(&key);
                    }
                }
            }
            EventEffect::ReopenHost { host } => {
                // The detached channel is reaped, so this opens exactly one new one. tmux
                // attaches it to another of the host's sessions. With none left, the new
                // stream ends with tmux's own word that it has nothing to serve (its "no
                // sessions" error, or the client's "no server running" complaint through
                // the tty), and that exit settles the card as an empty host.
                tracing::info!(host, "control_reopen_after_detach");
                let (vc, vr) = terminal_view_size(cols, rows, nav);
                dispatch_detected_host(mgr, hosts, &host, vc, vr);
            }
            EventEffect::ReapDisplayAttach { host, client } => {
                // Reap our display attach ONLY when the detaching client is OUR display client
                // (matched against the in-memory Host.display_tty). An unrelated client's detach
                // can never match, so it is structurally inert - no blanket reap.
                let Some(h) = hosts.get(&host) else {
                    return (false, Vec::new());
                };
                if !h.matches_display_tty(&client) {
                    return (false, Vec::new());
                }
                let key = host_selection_key(h); // Shared ⇒ key == host id
                registry.remove(&key);
                if let Some(h) = hosts.get_mut(&host) {
                    h.display.clear(&key); // forget the shown session + any in-flight spawn
                    h.clear_display_tty(); // the dead client's tty is gone
                }
                return (true, Vec::new()); // rearm recovery
            }
            EventEffect::FollowDisplaySession {
                host,
                client,
                session,
            } => {
                // Follow ONLY when the switched client is OUR display attach (matched against
                // Host.display_tty). A third party's own client (e.g. the user's separate tmux
                // client on a real server) can never match, so it is structurally inert - the
                // nav never chases someone else's switch.
                let Some(h) = hosts.get_mut(&host) else {
                    return (false, Vec::new());
                };
                if !h.matches_display_tty(&client) {
                    // With no tty known yet, the report cannot be judged. A remote attach
                    // records its tty before it execs the mux client, and the mux reports
                    // that client the moment it attaches, so the record exists by now:
                    // read it, and keep the report until the reply says whose it was.
                    if h.display_tty.0.is_none() && h.transport.runs_through_shell() {
                        let key = host_selection_key(h);
                        if let Some(attachment) = registry.get(&key) {
                            h.note_reported_session(&client, &session);
                            if let Some(control) = mgr.get(&host) {
                                let tty_key = crate::mux::display_tty_key(
                                    &key,
                                    &self.instance_name,
                                    attachment.id(),
                                );
                                control.capture_display_tty(&tty_key);
                            }
                        }
                    }
                    return (false, Vec::new());
                }
                let h = &*h;
                // xmux's own display PTY was moved to `session` by the mux itself (e.g.
                // the user's prefix+s). RECORD IT AND NOTHING ELSE: this is where a mux
                // with a control channel makes known where its client actually is, and
                // what follows from the client and the selection naming different
                // sessions is one comparison the loop makes continuously, in one place
                // for every mux (`display_astray` / `follow_selection_to_display`). The
                // record also keeps the next show() from dispatching a switch-client to a
                // session the client is already on.
                let key = host_selection_key(h); // Shared ⇒ key == host id
                if let Some(h) = hosts.get_mut(&host) {
                    h.display.set_shows(&key, &session);
                }
                tracing::info!(
                    host = %host,
                    session = %session,
                    client = %client,
                    "display_client_session_changed"
                );
            }
            EventEffect::AddDiscoveredHosts { machine, muxes } => {
                if model.state.invalid_auth.contains(&machine) {
                    return (false, Vec::new());
                }
                // A machine answered which muxes it has. Every one it does not already
                // serve becomes a host of its own, RIGHT NOW: the card appears scanning
                // and streams its sessions in like any other.
                //
                // A machine that serves no host yet names its hosts the way a written
                // list would: one mux takes the bare machine name, and several are each
                // qualified. A machine
                // that already serves a host adds each new one qualified (`prod:zellij`),
                // and the one already served keeps the id it was painted with, because
                // that id is what the frozen order, the persisted selection, and anything
                // the user typed are keyed to - renaming it mid-run would break all three.
                let (vc, vr) = terminal_view_size(cols, rows, nav);
                let first = !hosts.serves_any(&machine);
                let muxes = match muxes {
                    Ok(muxes) => muxes,
                    // The machine could not be asked at all, which says nothing about what
                    // it serves. A machine standing as its own card keeps it and shows the
                    // failure there; one that serves hosts has them report for it.
                    Err(reason) => {
                        tracing::warn!(machine = %machine, error = %reason, "mux discovery failed");
                        if first {
                            let effects = update(
                                model,
                                Msg::ApplyMachineResult {
                                    machine,
                                    err: Some(reason),
                                },
                            );
                            debug_assert!(effects.is_empty());
                        }
                        return (false, Vec::new());
                    }
                };
                let found: Vec<String> = muxes
                    .into_iter()
                    .filter(|bin| !hosts.machine_serves(&machine, bin))
                    .collect();
                let specs: Vec<(String, String)> = if first {
                    crate::provision::config::host_specs_for(&machine, &found)
                        .into_iter()
                        .map(|spec| (spec.bin, spec.id))
                        .collect()
                } else {
                    found
                        .into_iter()
                        .map(|bin| {
                            let id = crate::session::host_id(&machine, &bin, true);
                            (bin, id)
                        })
                        .collect()
                };
                let mut added = Vec::new();
                for (bin, id) in specs {
                    if hosts.get(&id).is_some() {
                        continue;
                    }
                    let Some(host) = hosts.discovered_host(&machine, &bin, &id) else {
                        continue;
                    };
                    tracing::info!(machine = %machine, mux = %bin, host_id = %id, "mux discovered");
                    hosts.insert(host);
                    added.push(id);
                }
                if !added.is_empty() {
                    let effects = update(model, Msg::SetHostReach(reach_map(env, hosts)));
                    debug_assert!(effects.is_empty());
                    // Every host the machine answered joins at once, so the card the
                    // machine stood on gives way to all of them in one rebuild; each
                    // one's first listing is now in flight.
                    let effects = update(
                        model,
                        Msg::AddHosts {
                            hosts: added.clone(),
                        },
                    );
                    debug_assert!(effects.is_empty());
                    for id in &added {
                        scan_or_dispatch_host(mgr, hosts, model, id, vc, vr, scan_pool);
                    }
                } else if first {
                    // Nothing answered that xmux supports, so the machine has nothing to
                    // show and its card goes.
                    let effects = update(model, Msg::SettleMuxless { machine });
                    debug_assert!(effects.is_empty());
                }
            }
            EventEffect::ApplyRoster {
                roster,
                startup,
                rescan,
            } => {
                let launching = startup.is_some();
                if let Some(startup) = startup {
                    env.credentials().set_force_askpass(startup.force_askpass);
                    model.switcher.set_own_session(startup.own_session);
                }
                // A roster resolution completed. The host registry and the nav have to
                // agree about which machines exist, so both are reconciled from this ONE
                // answer. Which makes this the one place to settle what the answer even
                // is: a machine only a PROBE offers is carried back in before anything
                // reads the roster, so a probe that was too slow cannot reap a card.
                let mut roster = roster;
                env.carry_probed(&mut roster);
                // What offered each machine, refreshed with the roster: a machine added by this
                // resolution has to be able to name the provider that offered it, exactly
                // as one present since launch can.
                let providers = roster
                    .roster_providers
                    .iter()
                    .map(|(machine, provider)| (machine.clone(), provider.label().to_owned()))
                    .collect();
                let login_defaults = roster.login_defaults.clone();
                let ssh_stanzas = roster.ssh_stanzas.clone();
                env.replace_roster(*roster);
                let delta = hosts.reconcile(env.hosts());
                let held = env.credentials().machines();
                let effects = update(
                    model,
                    Msg::SetRosterFacts {
                        providers,
                        login_defaults,
                        ssh_stanzas,
                        held_credentials: held,
                        host_reach: reach_map(env, hosts),
                    },
                );
                debug_assert!(effects.is_empty());
                for id in &delta.removed {
                    tracing::info!(host_id = %id, "roster dropped a host");
                    // Everything this host held: its metadata channel, the live PTY
                    // attachments showing its sessions, and its card. A card left behind
                    // would paint a session nothing can reach any more.
                    mgr.reap(id);
                    for address in registry.addresses() {
                        if address == *id {
                            registry.remove(&address);
                        }
                    }
                    let effects = update(
                        model,
                        Msg::RemoveHost {
                            host: id.clone(),
                            clear_tracking: true,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                for machine in &delta.machines_removed {
                    tracing::info!(machine = %machine, "roster dropped a machine");
                    let effects = update(
                        model,
                        Msg::RemoveMachine {
                            machine: machine.clone(),
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                for id in &delta.added {
                    tracing::info!(host_id = %id, "roster offered a new host");
                    let effects = update(
                        model,
                        Msg::AddHost {
                            host: id.clone(),
                            scanning: launching,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                // A machine joins after its hosts, so one that has a host does not
                // stand on the nav by itself even for a moment.
                for machine in &delta.machines_added {
                    tracing::info!(machine = %machine, "roster offered a new machine");
                    let effects = update(
                        model,
                        Msg::AddMachine {
                            machine: machine.clone(),
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                if launching {
                    let effects = update(model, Msg::LaunchRosterApplied);
                    debug_assert!(effects.is_empty());
                    probe_machines(hosts, mgr.events(), scan_pool, false, None);
                    return (false, Vec::new());
                }
                // Probe each ADDED machine's reachability (deduped by machine): a machine
                // the roster just named turns into a connected card that streams its
                // sessions, or a locked/unreachable one, exactly as at launch. The machines
                // that were already standing keep the channels they hold; the concurrent
                // re-probe of all machines that the re-scan already started reclassifies
                // those, so nothing is probed twice for one re-scan.
                let mut probed: HashSet<&str> = HashSet::new();
                let added = delta
                    .added
                    .iter()
                    .map(|id| crate::session::machine_of(id))
                    .chain(delta.machines_added.iter().map(String::as_str));
                for machine in added {
                    if probed.insert(machine) {
                        probe_machine(machine, hosts, mgr.events(), scan_pool, false, 0);
                    }
                }
                // The nav now holds what this roster added and dropped, so a re-scan that
                // asked for it may report.
                if rescan {
                    let effects = update(model, Msg::RescanRosterApplied);
                    debug_assert!(effects.is_empty());
                }
            }
            EventEffect::DispatchScanned {
                host: host_id,
                detected,
                ..
            } => {
                if model
                    .state
                    .invalid_auth
                    .contains(crate::session::machine_of(&host_id))
                {
                    let effects = update(
                        model,
                        Msg::DetectionFinished {
                            host: host_id.clone(),
                        },
                    );
                    debug_assert!(effects.is_empty());
                    return (false, Vec::new());
                }
                // A detection probe resolved: (re)identify the mux, then dispatch the
                // now-detected host onto its metadata channel (control client or poll task).
                // A probe that could not identify one has ALREADY settled the card as
                // unreachable in update (when it was still scanning), so opening a
                // doomed control child would just die and overwrite that reason with a
                // bare "connection closed". The reconnect sweep retries detection.
                let effects = update(
                    model,
                    Msg::DetectionFinished {
                        host: host_id.clone(),
                    },
                );
                debug_assert!(effects.is_empty());
                apply_scan_result(hosts, &host_id, detected);
                if hosts.get(&host_id).is_some_and(|h| h.detected) {
                    let (vc, vr) = terminal_view_size(cols, rows, nav);
                    dispatch_detected_host(mgr, hosts, &host_id, vc, vr);
                }
            }
            EventEffect::MachineConnected {
                machine,
                shell,
                rescan,
            } => {
                // Record the shell family the probe read on every host this machine
                // serves, before a channel opens: the attach shape and the in-place
                // switch are composed for a shell family, so the first command must
                // already know which one answered.
                if let Some(shell) = shell {
                    env.record_remote_shell(&machine, shell);
                    hosts.for_each_transport_of(&machine, |t| t.set_remote_shell(shell));
                }
                // The machine's reachability probe connected: resolve every host it
                // serves onto its metadata channel (a re-scan re-enumerates a live one; a
                // launch detects then ensures it), and, when the machine left its mux list
                // to xmux, ask which muxes it serves so the ones nobody wrote down appear.
                let (vc, vr) = terminal_view_size(cols, rows, nav);
                let host_ids: Vec<String> = hosts
                    .ids()
                    .iter()
                    .filter(|id| crate::session::machine_of(id) == machine)
                    .cloned()
                    .collect();
                for host_id in &host_ids {
                    let detected = hosts.get(host_id).is_some_and(|h| h.detected);
                    if detected {
                        if rescan {
                            if let Some(host) = hosts.get(host_id) {
                                mgr.rescan(host_id, host, vc, vr);
                            }
                        } else {
                            dispatch_detected_host(mgr, hosts, host_id, vc, vr);
                        }
                    } else {
                        scan_or_dispatch_host(mgr, hosts, model, host_id, vc, vr, scan_pool);
                    }
                }
                // Mux discovery is a machine-level question, asked once per connect and
                // only when the machine left its list to xmux. The startup roster already
                // resolved this box's muxes, so it is never re-probed here.
                if !crate::session::is_local_host(&machine)
                    && env.roster().cfg.mux_is_auto(&machine)
                {
                    if let Some(transport) = hosts.machine_transport(&machine) {
                        spawn_mux_discovery(
                            machine,
                            transport.clone_box(),
                            mgr.events(),
                            scan_pool.clone(),
                        );
                    }
                }
            }
            EventEffect::RenameDisplayed {
                host: host_id,
                from,
                to,
            } => {
                if let Some(h) = hosts.get_mut(&host_id) {
                    h.display.rename_session(&from, &to);
                }
            }
            EventEffect::SyncInventorySessions {
                host: host_id,
                sessions,
            } => {
                // Sync this host's display terminal(s) (per-host for remote tmux).
                let mut ctx = crate::driver::DriverCtx {
                    registry: &mut *registry,
                    hosts: &mut *hosts,
                    instance_name: &self.instance_name,
                    mgr,
                    worker,
                    pty_tx,
                    attach_seq: &mut *attach_seq,
                    viewport: terminal_view_size(cols, rows, nav),
                };
                sync_host_terminals(&host_id, &sessions, &mut ctx);
            }
            EventEffect::SyncPollSessions {
                host: host_id,
                sessions,
            } => {
                // A poll host's SUCCESSFUL enumeration (the nav group is already applied).
                // The enumeration is logged at the producer (`run_poll`), where `err` is in
                // hand - update drops the error path before reaching here, so logging
                // here would only ever see successes.
                // PerSession psmux: a session whose registry .port disappeared is dead even
                // if its PTY has not EOF'd. Drop the stale attach so it cannot show a dead grid.
                if let Some(h) = hosts.get(&host_id) {
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
                    instance_name: &self.instance_name,
                    mgr,
                    worker,
                    pty_tx,
                    attach_seq: &mut *attach_seq,
                    viewport: terminal_view_size(cols, rows, nav),
                };
                sync_host_terminals(&host_id, &sessions, &mut ctx);
            }
            EventEffect::RecordDisplayTty { host, tty } => {
                // The -CC `list-clients` probe resolved xmux's display-client tty. Record it
                // on the Host so a session switch is an in-place `switch-client -c <tty>`;
                // `None` (only the control client attached so far) clears any stale tty.
                if let Some(h) = hosts.get_mut(&host) {
                    if tty.is_some() {
                        tracing::info!(host, ?tty, "display_tty_recorded");
                    }
                    h.record_display_tty(tty);
                    // A move the mux reported before the tty was known lands now.
                    if let Some(session) = h.take_reported_session() {
                        let key = host_selection_key(h);
                        h.display.set_shows(&key, &session);
                        tracing::info!(host, session, "display_client_session_changed");
                    }
                }
            }
        }
        (false, followups)
    }
}

/// Detects a config-file change and, on a real change, reloads the `[ui]` section.
/// The redraw cadence stats the file rather than watching it, so no watch dependency is
/// needed, and a malformed edit keeps the previous settings. Only the `[ui]` presentation
/// settings reload live: re-scanning hosts is the `rescan` key's job, and a config edit
/// must not reset the user's sessions.
/// Returns `Some(ui)` only when the file genuinely changed since the last sight;
/// the first sight just records a baseline and a missing/currently-unwritable file is
/// ignored, so an editor mid-save never blanks the UI. Pure - it touches no global
/// state, which is what lets a test drive it with a temp file.
pub(super) fn poll_ui_config(
    last: &mut Option<std::time::SystemTime>,
    path: &std::path::Path,
) -> Option<Result<crate::provision::config::UiConfig, String>> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if mtime == *last {
        return None;
    }
    let prev = *last;
    *last = mtime;
    // First sight = baseline (the startup apply already ran); a missing file = an
    // editor mid-save or a deletion. Record the state and wait for a real change.
    if prev.is_none() || mtime.is_none() {
        return None;
    }
    match crate::provision::config::load(path) {
        Ok(config) => Some(Ok(config.ui)),
        Err(error) => {
            tracing::warn!(%error, "config_reload_failed");
            Some(Err(error.to_string()))
        }
    }
}

impl Runtime {
    /// Builds the world state from `env` and returns it alongside the loop's receiver
    /// halves ([`LoopIo`]). Pure construction - it starts NO probes (the startup scan
    /// is kicked from `run_app`), so a headless unit test can build a `Runtime`.
    pub(super) fn new(env: Arc<Env>) -> (Runtime, LoopIo) {
        let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let (cols, body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row
                                                                    // Restore the natural nav width the user last set; clamp a stale out-of-range
                                                                    // value, fall back to the default when none is saved.
        let nav_width_natural = adjust_nav_width(
            crate::app::prefs::load_nav_width(&env.xmux_dir)
                .unwrap_or(crate::ui::switcher::NAV_WIDTH),
            0,
            &env.ui_prefix,
        );
        let nav_collapsed = crate::app::prefs::load_nav_collapsed(&env.xmux_dir);
        let nav_width = if nav_collapsed {
            crate::ui::switcher::collapsed_nav_width(&env.ui_prefix)
        } else {
            nav_width_natural
        };
        // Restore the band-layout nav height (0 = auto ~40%); a stale value is clamped at
        // render time by compute_regions, so no clamp is needed here.
        let nav_height = crate::app::prefs::load_nav_height(&env.xmux_dir).unwrap_or(0);
        // The runtime host registry, keyed by id (local first, then each ssh alias in
        // config order), built from the roster on `Env`. Every host shares the
        // environment's machine credential store before it can spawn, including one
        // discovered or reconciled after a login, so a held password reaches each command
        // that host runs. Nothing replaces the roster during construction, so the read
        // below sees the same answer the registry was built from.
        let mut hosts = env.hosts();
        // One read of the roster for the whole construction, so every product below is
        // built from ONE answer about which machines exist.
        let roster = env.roster();
        let nav_default = roster.cfg.ui.nav_position();
        let max_fps = roster.cfg.ui.max_fps;
        // The discovery pool capacity: the configured value, clamped to [1, MAX].
        let scan_concurrency = roster
            .cfg
            .discovery
            .scan_concurrency
            .clamp(1, crate::provision::config::SCAN_CONCURRENCY_MAX);
        let nav_position_pinned = crate::app::prefs::load_nav_position(&env.xmux_dir);
        // The initial position: a pinned side wins, else the [ui] default. Resolved once
        // here so the first frame and the first PTY sizing already split the screen the
        // way the pin and default say; the loop-top reconcile re-resolves it every frame
        // from the same inputs.
        let nav_position = nav_position_pinned.unwrap_or(nav_default);
        let auto_hide_nav = crate::app::prefs::load_auto_hide_nav(&env.xmux_dir)
            .unwrap_or_else(|| roster.cfg.ui_auto_hide_nav());

        // The control-mode metadata clients: one per remote host.
        let (host_tx, host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mgr = HostManager::new(host_tx);
        // The live PTY attachments: one real attached mux client per session.
        let (pty_tx, pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let driver_pty_tx = pty_tx.clone();
        let worker = DisplayWorker::new(pty_tx);
        let registry = AttachRegistry::new();

        if env.startup_pending && !hosts.serves_any(crate::session::LOCAL_MACHINE) {
            hosts.hold_unresolved(
                crate::session::LOCAL_MACHINE.to_string(),
                crate::transport::local(None),
            );
        }

        // The app's runtime state (single source of truth), seeded from the host ids;
        // events stream the nav in.
        let mut state = crate::state::State::from_roster(hosts.ids().to_vec(), hosts.machines());
        state.recorded_logins = crate::app::prefs::load_ssh_logins(&env.xmux_dir);
        let saved_logins = state.recorded_logins.clone();
        let mut switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
        // The one session the terminal view refuses: the one xmux is running in. Named
        // once here, because the environment that names it cannot change under a run.
        switcher.set_own_session(env.own_session.clone());
        // Nothing is chosen at launch, so the terminal view lands on the landing screen
        // rather than on whichever session answers first.
        switcher.open_landing();
        switcher.set_renumbering(roster.cfg.ui.renumbering, &mut state);
        // The launch roster can add hosts after the first hosts answer, so the card
        // numbers stay open until it is in.
        if env.startup_pending {
            switcher.hold_numbers(true, &state);
        }
        // [ui] notifications: whether results show as toasts; the history keeps them either
        // way.
        state.notify.set_toasts_enabled(roster.cfg.ui.notifications);
        state.chrome.braille_animation = roster.cfg.ui.braille_animation;
        // And what offered each machine, so an unreachable one can name the provider that
        // put it on the roster. Reduced to words here: the screen prints them and
        // nothing branches on which provider it was.
        state.chrome.set_roster_providers(
            roster
                .roster_providers
                .iter()
                .map(|(machine, p)| (machine.clone(), p.label().to_string()))
                .collect(),
        );
        // And what the login pane starts from: the address a provider knew for each machine,
        // and this machine's own account name. Both are what ssh would have used, so a
        // pane that opens on a failure opens showing what just failed.
        state
            .chrome
            .set_login_defaults(roster.login_defaults.clone(), roster.ssh_stanzas.clone());
        // And how each host is REACHED, so an unreachable one states what was asked of
        // it and over what, not only that it failed. Resolved to words here for the same
        // reason the providers are: the screen prints them and nothing branches on them.
        state.chrome.set_host_reach(reach_map(&env, &hosts));
        // Where the whole history of dispatched commands is written, so the screen can name
        // the file instead of leaving the user to know about it.
        state.chrome.set_log_path(
            crate::logging::log_files(&env.xmux_dir)
                .display()
                .to_string(),
        );
        // The palette is ANSI-16 slots and attributes throughout, so it needs nothing from
        // the terminal but the theme the terminal already has: no colour query, no probing,
        // no fallback to guess at. The colours from outside those slots are the ones the
        // user names in `[ui]` role keys and `[ui] selection-style`.
        let palette = crate::ui::palette::resolve_output(
            &roster.cfg.ui.theme,
            crate::ui::chrome::palette_overrides(&roster.cfg.ui),
        );
        state.chrome.apply_palette(&roster.cfg.ui, &palette);
        switcher.set_palette(palette);
        // The help modal must show the prefix the user configured, not a literal.
        state.chrome.set_ui_prefix(env.ui_prefix.clone());
        state.chrome.first_key_seen = crate::app::prefs::first_key_help_seen(&env.xmux_dir);
        drop(roster);

        // The live mutate ops (create/rename/kill) - NOT nav probing. They resolve each
        // host through the registry's published set.
        let ops = env.ops(hosts.defs());
        let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
        let term_input = crate::display::input::TermInput::new(prefix);
        let nav_decoder = crate::display::decode::KeyDecoder::new();
        let (op_tx, op_rx) = tokio::sync::mpsc::unbounded_channel();

        let model = AppModel {
            switcher,
            render_plan: crate::ui::switcher::RenderPlan::default(),
            state,
            nav_width,
            nav_width_natural,
            nav_collapsed,
            nav_height,
            nav_position,
            nav_position_pinned,
            nav_default,
            max_fps,
            applied_nav_height: u16::MAX,
            applied_nav_collapsed: !nav_collapsed,
            auto_hide_nav,
            nav_was_focused: true,
            mouse_state: MouseState::default(),
            connected: HashSet::new(),
            detecting: HashSet::new(),
            config_last_mtime: None,
            width_dirty: false,
            width_flush_at: None,
            rescan: None,
            logout: None,
            running_logins: Vec::new(),
            saved_logins,
        };
        let initial_frame_interval = frame_interval(model.max_fps);
        let rt = Runtime {
            env,
            // Replaced in `run_app` once the free name is resolved (that needs a dial,
            // so it cannot happen in this synchronous constructor).
            instance_name: String::new(),
            ops,
            hosts,
            mgr,
            scan_pool: Arc::new(tokio::sync::Semaphore::new(scan_concurrency)),
            registry,
            worker,
            model,
            // Off-loop attach sequence. The in-flight set / reaped-ids / which session
            // each display shows live on each `host.display` (HostDisplay).
            attach_seq: 0,
            driver_pty_tx,
            op_tx,
            key_gates: Default::default(),
            cols,
            body_rows,
            term_input,
            nav_decoder,
            paste: Default::default(),
            window_focused: true,
            child_focus: None,
            keyboard_pushed: false,
            keyboard_flags: 0,
            prefix,
            // The draw hot path's observability (per-key grid fingerprints + slow-step
            // probe), owned off the draw block so it does nothing but lock → render.
            draw_observer: DrawObserver::default(),
            images: Default::default(),
            spinner_start: std::time::Instant::now(),
            login_probes: 0,
            dirty: true,
            last_draw: std::time::Instant::now() - initial_frame_interval,
            rescan_pending: false,
            display_probe: DisplayProbe::default(),
            held_input: None,
            passthrough: Vec::new(),
            title: None,
            #[cfg(test)]
            discovery_runs: 0,
            #[cfg(test)]
            machine_rescans: Vec::new(),
            // The live config watch records a baseline on its first frame tick, so the
            // startup settings are not re-applied. `None` means no baseline yet.
        };
        (
            rt,
            LoopIo {
                host_rx,
                pty_rx,
                op_rx,
            },
        )
    }

    /// The loop top: advance the spinner, reconcile the modal/nav-width, run the `r`
    /// reattach-kick, hold the nav selection and the display client to one session, sync
    /// the selection, drive one debounce beat (the settled attach), flush the debounced
    /// width persist, then draw the gated frame. `term` is the loop-local ratatui
    /// terminal.
    /// The nav's live size, in one place: the width the user set, the width on screen
    /// (0 while auto-hide has taken it), the band height the user set, the side the nav is
    /// attached to, and the collapsed state. Every geometry the loop computes reads this instead of picking
    /// fields out of `self`, so a resize while xmux runs cannot reach one consumer and
    /// miss another.
    pub(super) fn nav_size(&self) -> crate::ui::switcher::NavSize {
        self.model.nav_size()
    }

    /// Generic over the backend so the headless tests drive the same loop-top reconcile
    /// against a `TestBackend` that the live loop drives against stdout.
    pub(super) fn prepare_and_draw<B: ratatui::backend::Backend>(
        &mut self,
        term: &mut ratatui::Terminal<B>,
    ) {
        // Advance the spinner from wall-clock so it animates regardless of which arm fired.
        let spinner_frame = spinner_frame_at(self.spinner_start.elapsed());
        let view_border_hovered = self.model.mouse_state.hovered_view_border;
        // The repeat window lapses on the clock, not on an event, so compare before
        // storing: a bar that just went idle must repaint even though nothing arrived.
        let prefix_active = self.prefix_active();
        if self.model.state.chrome.armed != prefix_active {
            self.dirty = true;
        }
        let collapsed_before = self.model.nav_collapsed;
        let effects = update(
            &mut self.model,
            Msg::SyncFrame {
                spinner_frame,
                animation_ms: self.spinner_start.elapsed().as_millis() as u64,
                view_border_hovered,
                prefix_active,
            },
        );
        let _ = self.execute_effects(effects);
        if collapsed_before != self.model.nav_collapsed {
            self.dirty = true;
        }
        // The single owner of the effective nav width: reconcile it to the focus + the
        // hide setting + any natural-width change. On a change, resize the PTYs so the
        // mux reflows, and mark dirty.
        let shown = crate::ui::switcher::NavSize {
            width: if self.model.nav_collapsed {
                crate::ui::switcher::collapsed_nav_width(&self.env.ui_prefix)
            } else {
                self.model.nav_width_natural
            },
            ..self.model.nav_size()
        };
        let crowds = crate::ui::switcher::nav_crowds_terminal(
            ratatui::layout::Rect::new(0, 0, self.cols, self.body_rows.saturating_add(1)),
            shown,
        );
        let want_nav_width = reconciled_nav_width(
            self.model.state.focus.is_terminal_focused(),
            self.model.auto_hide_nav,
            crowds,
            prefix_active,
            self.model.nav_width_natural,
            self.model.nav_collapsed,
            &self.env.ui_prefix,
        );
        // The nav's attachment side is resolved here too, every frame: a pinned side
        // wins, else the [ui] default. The nav never moves on its own.
        let want_position = self
            .model
            .nav_position_pinned
            .unwrap_or(self.model.nav_default);
        // Resize when ANY dimension of the split moved: the width (focus / hide / prefix
        // Ctrl-←/→ in a column), the band height (border drag / resize keys), or the side the
        // nav is attached to. All change the mux terminal region, so all must resize the
        // PTYs or the grid mismatches the draw.
        if want_nav_width != self.model.nav_width
            || self.model.nav_height != self.model.applied_nav_height
            || self.model.nav_collapsed != self.model.applied_nav_collapsed
            || want_position != self.model.nav_position
        {
            // Crossing the hidden sentinel (0) flips the column TOPOLOGY; a stale wide-char
            // cell at the new boundary can survive ratatui's diff, so force a full repaint.
            // A position change moves the border to the opposite side of the screen and
            // gets the same treatment; the selection and the focus stay as they are.
            let crossed_hidden = (want_nav_width == 0) != (self.model.nav_width == 0);
            let crossed_position = want_position != self.model.nav_position;
            let effects = update(
                &mut self.model,
                Msg::ReconcileNav {
                    width: want_nav_width,
                    position: want_position,
                },
            );
            debug_assert!(effects.is_empty());
            let (vc, vr) = terminal_view_size(self.cols, self.body_rows, self.nav_size());
            self.registry.resize_all(vc, vr);
            if crossed_hidden || crossed_position {
                if let Err(e) = clear_screen(term) {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
                self.images.forget();
            }
            self.dirty = true;
        }
        // The key list and the help modal name the arrow pair the CURRENT placement
        // makes active, so they read the resolved position every frame.
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::display::term::ensure_mouse_capture();
        // An `R` re-scan also re-attaches the CURRENT display: tear the (possibly dead)
        // attachment down and clear its latch so the attach below re-creates a fresh
        // client for the viewed session.
        let effects = update(
            &mut self.model,
            Msg::ConsumeReattach {
                now: std::time::Instant::now(),
            },
        );
        let _ = self.execute_effects(effects);
        // The two regions must name ONE session. In terminal focus the user is driving
        // the mux, so the selection goes to the client; in nav focus the selection stands
        // and the beat below carries the client back to it.
        if self.follow_selection_to_display() {
            self.dirty = true;
        }
        if sync_selection_from_switcher(&mut self.model) {
            // The selection moved → the nav needs a redraw. The attach is NOT issued
            // here; the beat below arms the debounce, re-armed on every move.
            self.dirty = true;
        }
        self.drive_attach_beat(std::time::Instant::now());
        if self.sync_display_clients() {
            self.dirty = true;
        }

        // Flush the debounced nav-width persist once the resize burst settles.
        let effects = update(
            &mut self.model,
            Msg::FlushWidth {
                now: std::time::Instant::now(),
                force: false,
            },
        );
        let _ = self.execute_effects(effects);

        // Draw the split (nav + selected session's live grid). GATED - redraw only when
        // something changed AND at most once per frame, so rapid navigation / a busy PTY
        // cannot flood the terminal.
        if self.dirty && self.last_draw.elapsed() >= frame_interval(self.model.max_fps) {
            // Render the CONFIRMED display truth (`displayed`), not the selection: the prior
            // session stays on screen until the fresh one paints (stale-while-revalidate).
            let nav = self.nav_size();
            let grid_arc = current_grid(
                &self.model.state.displayed,
                &crate::driver::DriverCtx {
                    registry: &mut self.registry,
                    hosts: &mut self.hosts,
                    instance_name: &self.instance_name,
                    mgr: &self.mgr,
                    worker: &self.worker,
                    pty_tx: &self.driver_pty_tx,
                    attach_seq: &mut self.attach_seq,
                    viewport: (0, 0),
                },
            );
            let terminal_focused = self.model.state.focus.is_terminal_focused();
            // The view border glyph reflects auto-hide-nav mode (║ on, │ off).
            let t_draw = std::time::Instant::now();
            let previous_plan = self.model.render_plan.clone();
            let mut next_plan = None;
            let draw_result = match &grid_arc {
                Some(g) => {
                    let t_lock = std::time::Instant::now();
                    let guard = g.lock().ok();
                    DrawObserver::slow_step("grid_lock", t_lock);
                    // Compute the grid fingerprint under the same lock used for rendering;
                    // the observer emits display_grid_changed only on a real content change.
                    if let Some(grid) = guard.as_deref() {
                        let addr = display_key(&self.hosts, &self.model.state.displayed);
                        let session = &self.model.state.displayed.session;
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
                    let switcher = &self.model.switcher;
                    let state = &self.model.state;
                    let drawn = term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        let plan = switcher.layout(f.area(), nav, state, &previous_plan);
                        switcher.render(f, guard.as_deref(), terminal_focused, state, &plan);
                        next_plan = Some(plan);
                        DrawObserver::slow_step("render", t_render);
                    });
                    drawn.map(|frame| {
                        Self::paint_images(&mut self.images, frame.buffer, guard.as_deref())
                    })
                }
                None => {
                    let nav = self.nav_size();
                    let switcher = &self.model.switcher;
                    let state = &self.model.state;
                    let drawn = term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        let plan = switcher.layout(f.area(), nav, state, &previous_plan);
                        switcher.render(f, None, terminal_focused, state, &plan);
                        next_plan = Some(plan);
                        DrawObserver::slow_step("render", t_render);
                    });
                    drawn.map(|frame| Self::paint_images(&mut self.images, frame.buffer, None))
                }
            };
            if let Err(e) = draw_result {
                tracing::warn!(error = %e, "term_draw_failed");
            }
            // The plan is kept even when the flush fails: its scroll offsets are where the
            // next frame continues from.
            if let Some(plan) = next_plan {
                let effects = update(&mut self.model, Msg::SetRenderPlan(plan));
                debug_assert!(effects.is_empty());
            }
            DrawObserver::slow_step("draw", t_draw);
            // The grids are now on screen - clear every attachment's output-coalescing flag.
            self.registry.clear_all_pending();
            self.dirty = false;
            self.last_draw = std::time::Instant::now();
        }
    }

    /// The `host_rx` arm: apply one host event, then drain a burst (bounded) so a `%`-event
    /// flood coalesces into one redraw. A reaped display attach only repaints; it opens
    /// nothing, because a client that detached is not a reason to connect again.
    pub(super) fn on_host_event(
        &mut self,
        ev: HostEvent,
        host_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    ) {
        let t = std::time::Instant::now();
        if self.handle_host_event(ev) {
            self.dirty = true;
        }
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match host_rx.try_recv() {
                Ok(ev) => {
                    if self.handle_host_event(ev) {
                        self.dirty = true;
                    }
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        DrawObserver::slow_step("host_drain", t);
    }

    /// Draws the sixel image pieces a completed frame shows onto the outer terminal,
    /// right after ratatui flushed the frame, so the write never lands mid-frame.
    fn paint_images(
        painter: &mut crate::display::image::paint::Painter,
        frame: &ratatui::buffer::Buffer,
        grid: Option<&crate::display::grid::Grid>,
    ) {
        let Some(cell_px) = crate::display::image::sixel_cell_px() else {
            return;
        };
        let bytes = painter.paint(frame, |id| grid?.image(id), cell_px);
        if !bytes.is_empty() {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&bytes);
            let _ = out.flush();
        }
    }

    /// Writes the sequences a child asked the terminal above xmux for (an OSC 52
    /// clipboard write, a bell, a desktop notification) on xmux's own stdout. The loop
    /// calls it strictly between ratatui frames: ratatui owns stdout, and a write from
    /// anywhere else (the pump thread) would land mid-frame. Re-emitting the escape,
    /// not calling a clipboard or notification API, is what keeps this working when
    /// xmux itself runs over ssh.
    pub(super) fn flush_passthrough(&mut self) {
        if self.passthrough.is_empty() {
            return;
        }
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&self.passthrough);
        let _ = out.flush();
        self.passthrough.clear();
    }

    /// Queues the terminal's title for the session on screen: the OSC 0 or OSC 2 title
    /// its client set, or `xmux` once a title xmux wrote is no longer backed by the
    /// session on screen. Until a client sets a title, xmux leaves the terminal's own
    /// title alone, and the terminal guard restores it on exit.
    pub(super) fn sync_title(&mut self) {
        let displayed = &self.model.state.displayed;
        let title = (!displayed.is_empty())
            .then(|| self.registry.grid(&display_key(&self.hosts, displayed)))
            .flatten()
            .and_then(|grid| grid.lock().ok()?.title().map(str::to_string));
        if self.title.is_none() && title.is_none() {
            return;
        }
        if self.title.as_ref() == Some(&title) {
            return;
        }
        let shown = title.as_deref().unwrap_or(OWN_TITLE);
        self.passthrough
            .extend_from_slice(format!("]2;{shown}").as_bytes());
        self.title = Some(title);
    }

    /// A bell or a notification from attachment `id`. Every one reaches the terminal
    /// above xmux, the way tmux's `bell-action any` passes a bell from any window, so
    /// the user is told wherever they are looking. One from a session whose grid is not
    /// on screen also marks that session's card, so the user can tell which one asked.
    fn on_alert(&mut self, id: u64, alert: crate::display::grid::Alert) {
        self.passthrough.extend_from_slice(alert.bytes());
        let Some(key) = self
            .registry
            .address_of_id(id)
            .filter(|key| self.registry.get(key).is_some_and(|a| a.id() == id))
        else {
            return;
        };
        let host = host_of_key(&key);
        let Some(session) = self
            .hosts
            .get(host)
            .and_then(|h| h.display.shows(&key))
            .map(str::to_string)
        else {
            return;
        };
        let displayed = &self.model.state.displayed;
        if displayed.host == host
            && displayed.session == session
            && display_key(&self.hosts, displayed) == key
        {
            return;
        }
        let text = match alert {
            crate::display::grid::Alert::Bell => None,
            crate::display::grid::Alert::Notify { text, .. } => Some(text),
        };
        let effects = update(
            &mut self.model,
            Msg::SessionAlert {
                address: crate::session::Address::new(host, session),
                text,
            },
        );
        debug_assert!(effects.is_empty());
        self.dirty = true;
    }

    /// The `pty_rx` arm: a kept attachment fed its grid or hit EOF (reap). Detach-to-recover
    /// re-attaches the VIEWED session if its client exits; a background session is just reaped.
    pub(super) fn on_pty_event(
        &mut self,
        ev: PtyEvent,
        pty_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    ) {
        let mut detached = self.handle_one_pty_event(ev);
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match pty_rx.try_recv() {
                Ok(ev) => {
                    detached |= self.handle_one_pty_event(ev);
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        if detached {
            // The viewed session's client detached or exited. The view keeps the last
            // frame it drew and NOTHING re-attaches: the re-attach would be a fresh
            // connection raised by the death of the connection before it, and when the
            // session is gone every attempt dies the same way, so the chain does not stop
            // on its own. The user recovers the pane by selecting its card again or
            // re-scanning. The one exception, a client its mux dropped right after it
            // attached, was answered where the exit was applied and is bounded to once
            // per selection. Repaint so the pane shows what it is now.
            self.dirty = true;
        }
    }

    /// Tells the model how many of xmux's display attachments are live on each session,
    /// which the mux counts among that session's clients. An attachment counts while the
    /// registry holds it, installed or parked until it paints, and stops counting once its
    /// client ends. Returns whether the count changed.
    pub(super) fn sync_display_clients(&mut self) -> bool {
        let mut clients: std::collections::HashMap<crate::session::Address, u32> =
            std::collections::HashMap::new();
        for id in self.hosts.ids() {
            let Some(host) = self.hosts.get(id) else {
                continue;
            };
            for (key, session, parked) in host.display.attachments() {
                let live = if parked {
                    self.registry.contains_pending(key)
                } else {
                    self.registry.contains(key)
                };
                if live {
                    *clients
                        .entry(crate::session::Address::new(id, session))
                        .or_default() += 1;
                }
            }
        }
        if clients == self.model.state.display_clients {
            return false;
        }
        let effects = update(&mut self.model, Msg::DisplayClients(clients));
        debug_assert!(effects.is_empty());
        true
    }

    /// Applies one PTY event. Returns whether the displayed attachment exited.
    fn handle_one_pty_event(&mut self, ev: PtyEvent) -> bool {
        if let PtyEvent::Exited { id } = &ev {
            self.promote_pending_exit(*id);
        }
        // Capture the viewed attach id after a pending exit is promoted but before reap
        // removes it. A background attachment dropping is just reaped.
        let displayed_attach_id = (self.model.state.focus.is_terminal_focused()
            && !self.model.state.selection.is_empty())
        .then(|| {
            self.registry
                .get(&display_key(&self.hosts, &self.model.state.selection))
                .map(|a| a.id())
        })
        .flatten();
        match ev {
            PtyEvent::Exited { id } => {
                let now = std::time::Instant::now();
                let ended_early = self.registry.address_of_id(id).is_some_and(|key| {
                    self.attach_is_for_selection(&key)
                        && self
                            .hosts
                            .get(host_of_key(&key))
                            .is_some_and(|h| h.display.ended_early(&key, id, now))
                });
                if let Some(address) = self.registry.address_of_id(id) {
                    let effects = update(
                        &mut self.model,
                        Msg::DisplayAuth {
                            host: address,
                            method: None,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                // Read before the reap: the reap drops the grid the reason is written on.
                let last = last_pane_line(&self.registry, id);
                clear_display_tty_for_attach(&mut self.hosts, &self.registry, id);
                if !self.registry.reap(id) {
                    // pre-Ready Exited: registry has no id yet. Attribute to the owning host
                    // via pending so its Ready tears down instead of inserting a dead pane.
                    self.hosts
                        .iter_mut()
                        .any(|h| h.display.mark_reaped_if_pending(id));
                    tracing::info!(id, established = false, last = %last, "attach_exited");
                } else {
                    // An attachment that had been serving and is now gone. The loop reads
                    // its absence as a client to replace, so a pane that keeps dying is a
                    // reattach that keeps firing; saying so here is what separates that
                    // from a reattach decision gone wrong.
                    tracing::info!(id, established = true, last = %last, "attach_exited");
                    if ended_early {
                        self.on_selection_attach_lost(id, true, now);
                    }
                }
                Some(id) == displayed_attach_id
            }
            PtyEvent::DisplayTty { id, tty } => {
                record_display_tty(&mut self.hosts, &self.registry, id, tty);
                false
            }
            PtyEvent::AuthObserved { id, method } => {
                if let Some(address) = self.registry.address_of_id(id).filter(|key| {
                    self.registry
                        .get(key)
                        .is_some_and(|attachment| attachment.id() == id)
                }) {
                    let effects = update(
                        &mut self.model,
                        Msg::DisplayAuth {
                            host: address,
                            method: Some(method),
                        },
                    );
                    debug_assert!(effects.is_empty());
                    self.dirty = true;
                }
                false
            }
            PtyEvent::Output { id } => {
                self.note_pending_output(id);
                false
            }
            PtyEvent::Osc52 { seq } => {
                self.passthrough.extend_from_slice(&seq);
                false
            }
            PtyEvent::Alert { id, alert } => {
                self.on_alert(id, alert);
                false
            }
            PtyEvent::DisplayClientSession { id, at } => {
                self.display_probe.in_flight = false;
                if at.is_some_and(|at| self.record_display_client(id, at)) {
                    self.dirty = true;
                }
                false
            }
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
                let output_times = attachment.output_times();
                let hold_for_paint = self.registry.contains(&key);
                let current_for_selection = self.attach_is_for_selection(&key)
                    && self
                        .hosts
                        .get(&hid)
                        .is_some_and(|h| h.display.reply_is_current(&key, seq));
                let outcome = match self.hosts.get_mut(&hid) {
                    Some(h) => {
                        tracing::info!(key, seq, id, "attach_ready");
                        Some(h.display.resolve_ready(
                            &key,
                            seq,
                            id,
                            hold_for_paint,
                            output_times,
                            std::time::Instant::now(),
                        ))
                    }
                    None => None,
                };
                match outcome {
                    Some(crate::model::ReadyOutcome::Install { shown }) => {
                        self.install_attachment(key, attachment, shown);
                    }
                    Some(crate::model::ReadyOutcome::Hold { shown, replaced }) => {
                        let registry_replaced = self.registry.park_pending(&key, attachment);
                        debug_assert_eq!(replaced, registry_replaced);
                        tracing::info!(key, id, session = shown, "attach_waiting_for_paint");
                    }
                    // The selection's own client ended before its Ready: it never carried
                    // the display, so it is answered like a start that failed.
                    Some(crate::model::ReadyOutcome::TearDownReaped) if current_for_selection => {
                        attachment.teardown();
                        self.on_selection_attach_lost(id, false, std::time::Instant::now());
                    }
                    // Reaped-race, stale seq, or unknown host: tear the fresh attachment down
                    // (resolve_ready already cleared the bookkeeping for the first two).
                    Some(_) | None => attachment.teardown(),
                }
            }
            DisplayEvent::Failed { seq, key, message } => {
                let hid = host_of_key(&key).to_string();
                let for_selection = self.attach_is_for_selection(&key);
                let current = self
                    .hosts
                    .get_mut(&hid)
                    .is_some_and(|h| h.display.resolve_failed(&key, seq));
                tracing::warn!(key, error = %message, "attach_failed");
                // A start that failed is not a reason to start again: the selection waits
                // for the user to ask for it, as a display that died does.
                if current && for_selection {
                    let effects = update(
                        &mut self.model,
                        Msg::Action(crate::model::Action::AttachFailed),
                    );
                    debug_assert!(effects.is_empty());
                }
            }
        }
    }

    /// Whether `key` is the key the selection renders through and its attach request
    /// was made for the selected session, so what happens to that attach happens to the
    /// selection.
    fn attach_is_for_selection(&self, key: &str) -> bool {
        let selection = &self.model.state.selection;
        !selection.is_empty()
            && display_key(&self.hosts, selection) == key
            && self
                .hosts
                .get(&selection.host)
                .is_some_and(|h| h.display.shows(key) == Some(selection.session.as_str()))
    }

    /// Answers the selection's attachment `id` ending before it confirmed the display
    /// (`confirmed` false) or within [`EARLY_END`](crate::model::EARLY_END) after it did.
    ///
    /// On a mux that drops fresh clients the session is most likely still there, so the
    /// selection is attached once more, and the state bounds that to once per selection.
    /// Any other mux ended the client for a reason of its own: an unconfirmed attach then
    /// waits for the user, and a confirmed one is the ordinary ended display.
    fn on_selection_attach_lost(&mut self, id: u64, confirmed: bool, now: std::time::Instant) {
        let drops = self
            .hosts
            .get(&self.model.state.selection.host)
            .is_some_and(|h| h.mux.drops_fresh_client());
        let action = if drops {
            tracing::info!(id, confirmed, "attach_dropped_early");
            crate::model::Action::FreshClientDropped { now }
        } else if !confirmed {
            crate::model::Action::AttachFailed
        } else {
            return;
        };
        let effects = update(&mut self.model, Msg::Action(action));
        debug_assert!(effects.is_empty());
    }

    /// Installs one attachment whose display gate has opened and confirms it only when
    /// its key is the one the current selection renders through. An attachment for any
    /// other key installs and stays warm without claiming the terminal view, so a host
    /// warming a PTY on its own inventory cannot move the view to a machine nobody
    /// selected.
    fn install_attachment(
        &mut self,
        key: String,
        mut attachment: crate::display::attachment::Attachment,
        shown: String,
    ) {
        let selected_key = display_key(&self.hosts, &self.model.state.selection);
        let hid = host_of_key(&key).to_string();
        let attach_id = attachment.id();
        let child_tty = attachment.child_tty().map(str::to_string);
        let effects = update(
            &mut self.model,
            Msg::DisplayAuth {
                host: hid.clone(),
                method: None,
            },
        );
        debug_assert!(effects.is_empty());
        attachment.watch_auth(self.driver_pty_tx.clone());
        self.registry.remove(&key);
        self.registry.insert(&key, attachment);
        // The display may have opened, or be riding, a shared connection other than the
        // one the machine was last seen on.
        let machine = crate::session::machine_of(&hid).to_owned();
        if let Some(transport) = self.hosts.machine_transport(&machine) {
            spawn_shared_connection_check(machine, transport.clone_box(), self.mgr.events());
        }

        if let Some(h) = self.hosts.get_mut(&hid) {
            if let Some(tty) = child_tty.filter(|_| !h.transport.runs_through_shell()) {
                tracing::info!(host = %hid, tty, "display_tty_from_pty");
                h.record_display_tty(Some(tty));
            }
            if h.display_tty.0.is_none() && h.transport.runs_through_shell() {
                if let Some(client) = self.mgr.get(&hid) {
                    let tty_key = crate::mux::display_tty_key(&hid, &self.instance_name, attach_id);
                    client.capture_display_tty(&tty_key);
                }
            }
        }

        if key == selected_key {
            let effects = update(
                &mut self.model,
                Msg::Action(crate::model::Action::ConfirmDisplay(Selection {
                    host: hid,
                    session: shown,
                })),
            );
            debug_assert!(effects.is_empty());
        }
    }

    /// Promotes one parked attachment after its paint gate opens.
    fn promote_pending(&mut self, pending: crate::model::PendingInstall) -> bool {
        let Some(attachment) = self.registry.take_pending(&pending.key) else {
            return false;
        };
        if attachment.id() != pending.id {
            tracing::warn!(
                key = %pending.key,
                expected = pending.id,
                actual = attachment.id(),
                "pending_attachment_id_mismatch"
            );
            attachment.teardown();
            return false;
        }
        tracing::info!(key = %pending.key, id = pending.id, "attach_painted");
        self.install_attachment(pending.key, attachment, pending.shown);
        true
    }

    /// Advances every parked attachment whose settle, hard, or no-output cap elapsed.
    pub(super) fn promote_due_pending(&mut self, now: std::time::Instant) -> bool {
        let mut due = Vec::new();
        for host in self.hosts.iter_mut() {
            due.extend(host.display.take_due_pending(now));
        }
        let mut promoted = false;
        for pending in due {
            promoted |= self.promote_pending(pending);
        }
        promoted
    }

    /// Records output timing for a parked attachment without coupling PTY mechanics to
    /// a mux kind. Output that has not yet left anything visible on the grid records
    /// nothing, so bytes such as a clear-screen or terminal queries never open the
    /// paint gate on an empty frame.
    pub(super) fn note_pending_output(&mut self, id: u64) {
        let Some(key) = self.registry.pending_address_of_id(id) else {
            return;
        };
        let Some(output_at) = self.registry.pending_last_output(id) else {
            return;
        };
        if let Some(host) = self.hosts.get_mut(host_of_key(&key)) {
            host.display.note_pending_output(id, output_at);
        }
    }

    /// Promotes a parked attachment immediately before applying the normal installed
    /// attachment exit path. This retires the stale session and leaves the fresh grid,
    /// even when blank, as the exited session's final frame.
    fn promote_pending_exit(&mut self, id: u64) {
        let pending = self
            .registry
            .pending_address_of_id(id)
            .and_then(|key| self.hosts.get_mut(host_of_key(&key)))
            .and_then(|host| host.display.take_pending_exit(id));
        if let Some(pending) = pending {
            self.promote_pending(pending);
        }
    }

    /// The `stdin_rx` arm: route a raw read (mouse/keys) through the input core. Returns
    /// whether the app should quit.
    pub(super) fn on_stdin(&mut self, bytes: &[u8]) -> bool {
        use std::time::Duration;
        // A paste is routed apart from the keys around it, in the order read.
        let mut outcome = crate::app::input::StdinOutcome::default();
        for segment in self.paste.feed(bytes) {
            let part = match segment {
                crate::display::paste::Segment::Keys(keys) => {
                    // Clone the selection so &mut state can be threaded alongside it (the
                    // ForwardToMux path reads the selection for display_key/registry input).
                    let selection = self.model.state.selection.clone();
                    self.handle_stdin_bytes(&keys, &selection)
                }
                crate::display::paste::Segment::Paste(text) => self.handle_paste(text),
            };
            outcome.dirty |= part.dirty;
            outcome.width_changed |= part.width_changed;
            outcome.quit |= part.quit;
            if outcome.quit {
                break;
            }
        }
        if outcome.dirty {
            self.dirty = true;
        }
        if outcome.width_changed {
            let effects = update(
                &mut self.model,
                Msg::MarkWidthDirty {
                    flush_at: std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS),
                },
            );
            debug_assert!(effects.is_empty());
        }
        outcome.quit
    }

    /// The control-socket arm: headless op/status/dump/key/bytes. Returns whether to quit.
    pub(super) fn on_ctl_command(
        &mut self,
        cmd: crate::app::control::Cmd,
        term: &mut Term,
    ) -> bool {
        use crate::app::control::Cmd;
        use crate::ui::run::dump_screen;
        use std::time::Duration;
        match cmd {
            Cmd::Op(action, reply) => {
                // The ctl reply: `switch` answers by the address resolution against the
                // current inventory (the same lookup the selection move performs); the
                // other verbs have no synchronous outcome and answer ok. Sent before
                // the quit check so a `quit` still acknowledges. The loop owns the
                // state, so the reply is computed here, not in the dispatch task - the
                // task only awaits it.
                let resp = match &action {
                    crate::model::Action::Switch(address) => {
                        match self.model.state.resolve_switch_address(address) {
                            Ok(()) => "ok".to_string(),
                            Err(problem) => format!("err: {problem}"),
                        }
                    }
                    _ => "ok".to_string(),
                };
                // dispatch_action spawns any RunOp off-loop itself; its OpResult folds back
                // through op_tx as usual.
                let (quit_op, wc) = self.dispatch_action(action);
                let _ = reply.send(resp);
                if wc {
                    let effects = update(
                        &mut self.model,
                        Msg::MarkWidthDirty {
                            flush_at: std::time::Instant::now()
                                + Duration::from_millis(WIDTH_FLUSH_MS),
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                if quit_op {
                    return true;
                }
                // A Switch/Focus may need the selection's host connected.
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.model.switcher,
                    self.cols,
                    self.body_rows,
                    self.model.nav_width,
                );
                if sync_selection_from_switcher(&mut self.model) {
                    self.dirty = true;
                }
            }
            Cmd::Status(reply) => {
                let _ = reply.send(status_line(
                    &self.model.switcher,
                    &self.model.state,
                    &self.instance_name,
                    self.model.state.focus.view_is_nav(),
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
                    &self.model.state.displayed,
                    &crate::driver::DriverCtx {
                        registry: &mut self.registry,
                        hosts: &mut self.hosts,
                        instance_name: &self.instance_name,
                        mgr: &self.mgr,
                        worker: &self.worker,
                        pty_tx: &self.driver_pty_tx,
                        attach_seq: &mut self.attach_seq,
                        viewport: (0, 0),
                    },
                );
                let dump = match &grid_arc {
                    Some(g) => {
                        let guard = g.lock().ok();
                        dump_screen(
                            &self.model.switcher,
                            guard.as_deref(),
                            sz.width,
                            sz.height,
                            &self.model.state,
                            &self.model.render_plan,
                        )
                    }
                    None => dump_screen(
                        &self.model.switcher,
                        None,
                        sz.width,
                        sz.height,
                        &self.model.state,
                        &self.model.render_plan,
                    ),
                };
                let _ = reply.send(dump);
            }
            Cmd::RawKey(k) => {
                // Route the FULL command batch through the single dispatcher (RunOp spawns
                // off-loop, its OpResult folding back through op_tx).
                let effects = update(&mut self.model, Msg::Key(k));
                let (quit_key, wc, _) = self.execute_effects(effects);
                if wc {
                    let effects = update(
                        &mut self.model,
                        Msg::MarkWidthDirty {
                            flush_at: std::time::Instant::now()
                                + Duration::from_millis(WIDTH_FLUSH_MS),
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                if quit_key {
                    return true;
                }
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.model.switcher,
                    self.cols,
                    self.body_rows,
                    self.model.nav_width,
                );
                if sync_selection_from_switcher(&mut self.model) {
                    self.dirty = true;
                }
            }
            Cmd::RawBytes(bytes) => {
                if !bytes.is_empty() {
                    // A BLOCKED host has no PTY: its login pane owns the keys, exactly as the
                    // interactive terminal-focus path routes them (see `input.rs`). So the
                    // ctl raw surface drives the pane the same way a keyboard does, down to
                    // a running login taking no input but the Esc that ends it.
                    let login_running = self.model.state.login_run.as_ref().is_some_and(|l| {
                        self.model.switcher.current_host().as_deref() == Some(&l.host)
                    });
                    if login_running {
                        if bytes.as_slice() == b"\x1b" {
                            let effects = update(&mut self.model, Msg::CancelRunningLogin);
                            let _ = self.execute_effects(effects);
                        }
                        self.dirty = true;
                    } else if self.model.switcher.login_pane_shown(&self.model.state) {
                        if let Some(host) = self.model.switcher.current_host() {
                            let effects = update(&mut self.model, Msg::FeedLogin { host, bytes });
                            let _ = self.execute_effects(effects);
                            self.dirty = true;
                        }
                    } else {
                        // The same route as the interactive keystroke path.
                        self.forward_input(bytes);
                    }
                }
            }
        }
        self.flush_rescan();
        false
    }

    /// Whether a prefix interaction is live, in EITHER focus. The hint bar and the
    /// auto-hide nav show ask the same question, so every form of "live" is OR'd here
    /// rather than making the chrome know about focus or about which command ran.
    ///
    /// A prefix is consumed when its FUNCTION ENDS, not when its command key arrives,
    /// so "live" spans three shapes:
    /// - READY, awaiting the command key (the two focus paths' armed latches).
    /// - INPUTTING: a command that opened an input row owns the prefix until Enter or
    ///   Esc closes the row.
    /// - REPEATING: a resize command opened the bare-Ctrl-arrow repeat window; the
    ///   function is still running until the window lapses.
    ///
    /// Ready also clears on a focus switch or a mouse action (canceled).
    pub(super) fn prefix_active(&self) -> bool {
        self.model.mouse_state.nav_armed
            || self.term_input.is_armed()
            || self.model.state.is_inputting()
            || self
                .model
                .mouse_state
                .repeat_until
                .is_some_and(|d| std::time::Instant::now() < d)
    }

    /// The op-result arm: fold a finished create back into the nav/state. A successful
    /// login returns the host it was for; only THAT machine's reach changed (locked →
    /// connected), so re-probe just it - over what the login left behind - instead of the
    /// whole roster. The re-probe is what turns the pane back into the host's sessions.
    pub(super) fn on_op_result(&mut self, result: crate::ui::switcher::OpResult) {
        if let crate::ui::switcher::OpResult::SshFactsRead { machine, facts } = &result {
            self.env.set_ssh_facts(machine, facts);
        }
        let effects = update(
            &mut self.model,
            Msg::OpResult {
                result,
                logged_in: self.env.credentials().machines(),
            },
        );
        let _ = self.execute_effects(effects);
    }

    /// Reads what ssh config says about `machine` off the loop, and answers through the
    /// op channel. Only a machine reached over ssh has any: a local machine or a WSL
    /// distribution is not looked up in ssh config.
    pub(super) fn read_ssh_facts(&self, machine: String) {
        if !self
            .hosts
            .machine_transport(&machine)
            .is_some_and(|transport| transport.is_remote())
        {
            return;
        }
        let address = self
            .env
            .with_roster(|roster| roster.machine_addresses.get(&machine).cloned());
        let tx = self.op_tx.clone();
        tokio::spawn(async move {
            let facts = crate::provision::env::read_ssh_facts(machine.clone(), address).await;
            let _ = tx.send(crate::ui::switcher::OpResult::SshFactsRead { machine, facts });
        });
    }

    /// Drives one debounce beat: folds the clock and the runtime attach facts into
    /// [`Action::Tick`](crate::model::Action::Tick) and carries out the effects it
    /// returns. `State::apply` owns the arm/fire decision; the facts it needs (the
    /// registry, the host bookkeeping, where the display client actually is) live out
    /// here, so the loop reads them just before the beat and passes them as DATA.
    ///
    /// `now` is injected rather than read here, the same way `apply` takes it, so a
    /// caller can drive the debounce across its whole span.
    pub(super) fn drive_attach_beat(&mut self, now: std::time::Instant) {
        let in_flight = selection_attach_in_flight(&self.hosts, &self.model.state.selection);
        let _ = self.dispatch_action(crate::model::Action::Tick {
            now,
            in_flight,
            display_astray: display_astray(&self.model.state, &self.hosts),
        });
    }

    /// Moves the NAV SELECTION to the session the display client is on, while the
    /// terminal holds the focus. Returns true when the selection moved.
    ///
    /// The user is driving the mux, so a session change the mux made is theirs and the
    /// nav is what yields: the selection goes to the client, and the ordinary reconcile
    /// then has nothing to do, because the client is already where the selection names.
    /// The other direction ([`display_astray`]) belongs to nav focus, where the selection
    /// is the user's own and the client is what comes back. One of the two regions moves,
    /// never both, so the pair cannot chase each other.
    ///
    /// NOTHING IS REMEMBERED WHEN THE MOVE CANNOT LAND. A session created moments ago has
    /// no card yet and the selection has nowhere to go; the client is still on it, and
    /// this runs again on the next pass, so the move lands on the first pass after the
    /// enumeration that brings the card in. The client itself is the record, which is why
    /// there is none here to go stale, to be retried, or to be cancelled.
    ///
    /// WHICH OF THE TWO MOVED FIRST is the one thing the difference alone cannot say, and
    /// an attach still owed for the selection is what says it. While one is owed, the
    /// difference is a selection that moved and a display that has not been carried to it
    /// yet - a ctl `switch` while the terminal holds the focus is exactly that - and
    /// answering it here would drag the selection back to where the display still is and
    /// undo the switch that was just asked for. Every driver records the session it is
    /// showing as it acts, so the debt is settled in the same breath as the display, and
    /// a difference outliving the debt is the mux's own doing.
    pub(super) fn follow_selection_to_display(&mut self) -> bool {
        if !self.model.state.focus.is_terminal_focused() || self.model.state.selection.is_empty() {
            return false;
        }
        let in_flight = selection_attach_in_flight(&self.hosts, &self.model.state.selection);
        if self.model.state.attach_pending
            || self.model.state.attach_deadline.is_some()
            || in_flight
        {
            return false;
        }
        let Some(shown) = display_session(&self.hosts, &self.model.state.selection.host) else {
            return false;
        };
        if shown == self.model.state.selection.session {
            return false;
        }
        let addr = crate::session::Address::new(&self.model.state.selection.host, shown);
        let before = self.model.switcher.terminal_view_target();
        let effects = update(&mut self.model, Msg::FollowDisplay(addr));
        debug_assert!(effects.is_empty());
        before != self.model.switcher.terminal_view_target()
    }

    /// Reads xmux's own display client for the session it is on and records it, for a mux
    /// that reports its client switches nowhere else. Returns true when the record moved.
    ///
    /// A mux with a control channel pushes a client-switch notification and the record is
    /// written off that event. A mux without one moves its client INSIDE the client
    /// process, so nothing is pushed and nothing can be asked: the client's own live state
    /// is the only source of truth, and it has to be looked at. This is that look, on the
    /// animation beat, which is fast enough that the nav moves with the screen and bounded
    /// so a busy PTY cannot turn it into a per-output-chunk probe. What FOLLOWS from the
    /// record and the selection naming different sessions is one comparison the loop makes
    /// continuously, the same for every mux, and none of it is decided here.
    ///
    /// Mux-blind, in both directions. Which muxes have a source of truth to read is answered by
    /// each mux (a mux with none reports nothing to read, and this returns immediately),
    /// and what to do with the answer is shared by all of them. So a mux added later joins
    /// by answering, not by being named here.
    ///
    /// The read is REFUSED while a reattach is in flight for the display key. The stale
    /// client is deliberately kept on screen until the fresh one paints, and it is still
    /// sitting on the session the selection just left - reading it then would report the
    /// old session as where the display is and send the reconcile chasing a client that is
    /// already on its way somewhere else. A switch the mux pushes over a control channel
    /// is a fresh fact rather than a re-reading of the stale client, so it is recorded
    /// even then.
    ///
    /// THE READ RUNS ON THE LOOP, and it is the one process read that may. The rule it
    /// stands against bans work whose duration ANOTHER PARTY sets: a spawn, a pipe, a PTY
    /// close, each of which waits on something that may never answer. This read waits on
    /// nobody. It asks the OS about a process and copies that process's memory, so its
    /// cost is set by the size of an environment block, which the read itself caps at
    /// 64 KiB. It is timed over 500 repeats against a real ConPTY child in a release build
    /// (the cost test lives beside the read): the mean lands in the low TENS OF
    /// MICROSECONDS and the worst repeat a few times that. Those two figures are what the
    /// machine, the size of the child's environment block, and scheduler noise decide, so
    /// no single run's number is a constant to hold anything to - re-run the test to get
    /// this machine's. What holds across runs and machines is the ORDER: microseconds
    /// against a 120 ms animation beat, three to four orders of magnitude apart, so even
    /// the worst repeat costs well under a percent of the beat it runs on and of the
    /// 33 ms frame budget. That is far below any cadence the loop can perceive, and
    /// cheaper than moving it off the loop, which would cost a thread hop and a channel
    /// round trip per beat to save nothing. The test guards the conclusion rather than the
    /// figure: it fails only when a read costs a visible share of a beat.
    pub(super) fn observe_display_session(&mut self) -> bool {
        if self.model.state.selection.is_empty() {
            return false;
        }
        let id = self.model.state.selection.host.clone();
        let Some(host) = self.hosts.get(&id) else {
            return false;
        };
        let key = host_selection_key(host);
        if host.display.in_flight_contains(&key) || host.display.pending_paint_contains(&key) {
            return false;
        }
        let Some(session) = crate::driver::live_client_session(host, &self.registry) else {
            return false;
        };
        if host.display.shows(&key) == Some(session.as_str()) {
            return false;
        }
        if let Some(h) = self.hosts.get_mut(&id) {
            h.display.set_shows(&key, &session);
        }
        tracing::info!(
            host = %id,
            session = %session,
            "display_client_session_changed"
        );
        true
    }

    /// Starts the host-side query for where xmux's own display client is, for a host
    /// whose client cannot be read on this machine (see
    /// [`crate::driver::display_client_probe`]). The answer comes back as a
    /// `PtyEvent::DisplayClientSession` and is recorded by
    /// [`record_display_client_session`](Self::record_display_client_session).
    ///
    /// The query runs off the loop, because its duration is the host's to set. One runs
    /// at a time and the next waits [`DISPLAY_PROBE_EVERY`], so a host that answers
    /// slowly is asked less often rather than more. No query starts while a reattach is
    /// in flight for the display key, for the same reason the local read refuses one.
    pub(super) fn start_display_probe(&mut self, now: std::time::Instant) {
        if self.display_probe.in_flight
            || self.display_probe.next.is_some_and(|next| now < next)
            || self.model.state.selection.is_empty()
        {
            return;
        }
        let Some(host) = self.hosts.get(&self.model.state.selection.host) else {
            return;
        };
        let key = host_selection_key(host);
        if host.display.in_flight_contains(&key) || host.display.pending_paint_contains(&key) {
            return;
        }
        let Some((id, command)) =
            crate::driver::display_client_probe(host, &self.registry, &self.instance_name)
        else {
            return;
        };
        let mux = host.mux.clone_box();
        let events = self.driver_pty_tx.clone();
        self.display_probe.next = Some(now + DISPLAY_PROBE_EVERY);
        self.display_probe.in_flight = true;
        tokio::spawn(async move {
            use crate::model::host_def::Runner;
            let at = match crate::model::host_def::ExecRunner.run_spec(&command).await {
                Ok(out) => mux.parse_display_client(&String::from_utf8_lossy(&out)),
                Err(error) => {
                    tracing::debug!(id, error = %error, "display_client_query_failed");
                    None
                }
            };
            let _ = events.send(PtyEvent::DisplayClientSession { id, at });
        });
    }

    /// Records where a host-side query found attachment `id`'s mux client, as
    /// [`observe_display_session`](Self::observe_display_session) records a local read.
    /// Returns true when the record or the nav moved.
    ///
    /// The answer is about the attachment the query was started for, and it is recorded
    /// only while that attachment is still the live one for its key and no reattach is
    /// in flight or waiting to paint there. A reattach the nav started after the query
    /// leaves the old client on the old session until it is torn down, so its answer
    /// would name a session the display is leaving.
    ///
    /// A client on another of the host's sessions is recorded as what the display shows,
    /// and the comparison the loop makes on every pass decides which side follows. One
    /// answer is held back: the FIRST about a fresh attachment, naming a session other
    /// than the one it was attached for, while the nav holds the focus. Such a client
    /// started where the mux put it, not where the user moved it, and carrying it back
    /// would reattach into the same start again. The nav names the session it is on
    /// instead, until the client moves.
    ///
    /// A client somewhere no card covers leaves the selection and the record where they
    /// are, since the attachment is still the session xmux opened, and the nav names the
    /// place on that session's card instead.
    pub(super) fn record_display_client(&mut self, id: u64, at: crate::mux::ClientAt) -> bool {
        let Some(key) = self
            .registry
            .address_of_id(id)
            .filter(|key| self.registry.get(key).is_some_and(|a| a.id() == id))
        else {
            return false;
        };
        let host_id = host_of_key(&key).to_string();
        let Some(host) = self.hosts.get_mut(&host_id) else {
            return false;
        };
        if host.display.in_flight_contains(&key) || host.display.pending_paint_contains(&key) {
            return false;
        }
        let Some(shown) = host.display.shows(&key).map(str::to_string) else {
            return false;
        };
        let fresh = self.display_probe.answered != Some(id);
        self.display_probe.answered = Some(id);
        let place = match at {
            crate::mux::ClientAt::Session(session) => {
                if session == shown {
                    self.display_probe.held = None;
                    return self.clear_display_away();
                }
                let nav_focused = !self.model.state.focus.is_terminal_focused();
                if fresh && nav_focused {
                    self.display_probe.held = Some((id, session.clone()));
                }
                if self.display_probe.held.as_ref() != Some(&(id, session.clone())) {
                    self.display_probe.held = None;
                    if let Some(h) = self.hosts.get_mut(&host_id) {
                        h.display.set_shows(&key, &session);
                    }
                    tracing::info!(
                        host = %host_id,
                        session = %session,
                        "display_client_session_changed"
                    );
                    self.clear_display_away();
                    return true;
                }
                session
            }
            crate::mux::ClientAt::Away(label) => {
                self.display_probe.held = None;
                label
            }
        };
        let address = crate::session::Address::new(&host_id, shown);
        if self
            .display_probe
            .away
            .as_ref()
            .is_some_and(|(was, at, label)| *was == id && *at == address && *label == place)
        {
            return false;
        }
        tracing::info!(host = %host_id, place = %place, "display_client_away");
        self.display_probe.away = Some((id, address.clone(), place.clone()));
        let effects = update(&mut self.model, Msg::DisplayAway(Some((address, place))));
        debug_assert!(effects.is_empty());
        true
    }

    /// Takes back the nav's note of a display client away from its card. Returns true
    /// when there was one.
    fn clear_display_away(&mut self) -> bool {
        if self.display_probe.away.take().is_none() {
            return false;
        }
        let effects = update(&mut self.model, Msg::DisplayAway(None));
        debug_assert!(effects.is_empty());
        true
    }

    /// Clears the nav's note of a display client away from its card once the attachment
    /// it is about is no longer the live one for its key. Returns true when it cleared.
    pub(super) fn drop_stale_display_away(&mut self) -> bool {
        let Some((id, _, _)) = self.display_probe.away else {
            return false;
        };
        let live = self
            .registry
            .address_of_id(id)
            .is_some_and(|key| self.registry.get(&key).is_some_and(|a| a.id() == id));
        !live && self.clear_display_away()
    }

    /// The animation-tick arm: detect a console resize (push the new size to PTYs +
    /// control clients, force a full repaint), read xmux's own display client for a
    /// mux-side session change, and refresh the connecting-spinner set.
    pub(super) fn on_tick(&mut self, term: &mut Term) {
        let auth_effects = update(
            &mut self.model,
            Msg::CredentialInventory {
                held: self.env.credentials().machines(),
            },
        );
        if !auth_effects.is_empty() {
            self.execute_effects(auth_effects);
            self.dirty = true;
        }
        if self.promote_due_pending(std::time::Instant::now()) {
            self.dirty = true;
        }
        // Resize detection: poll the console size (an ioctl, not a stdin read).
        if let Ok((c, r)) = ratatui::crossterm::terminal::size() {
            if (c, r) != (self.cols, self.body_rows + 1) {
                let body = r.saturating_sub(1);
                self.cols = c;
                self.body_rows = body;
                let (vc, vr) = terminal_view_size(c, body, self.nav_size());
                self.registry.resize_all(vc, vr);
                let _ = term.autoresize();
                // A console resize reflows the existing cells; force a full repaint.
                if let Err(e) = clear_screen(term) {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
                self.images.forget();
                self.dirty = true;
            }
        }
        if self.observe_display_session() {
            self.dirty = true;
        }
        if self.drop_stale_display_away() {
            self.dirty = true;
        }
        self.start_display_probe(std::time::Instant::now());
        // Spinner set = the selected session if its PTY is still connecting.
        let mut sp = HashSet::new();
        if !self.model.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.model.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.model.state.selection.host)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if in_flight_for_key || self.registry.connecting(&key) {
                sp.insert(
                    crate::session::Address::new(
                        &self.model.state.selection.host,
                        &self.model.state.selection.session,
                    )
                    .display(),
                );
            }
        }
        let effects = update(
            &mut self.model,
            Msg::Tick {
                now: std::time::Instant::now(),
                spinner: sp,
            },
        );
        debug_assert!(effects.is_empty());
        // A toast that left, one still counting down its remaining time, or the open
        // history's ages moving is a change on screen the tick is the only wake for.
        if self.model.state.notify.repaint {
            self.dirty = true;
        }
    }

    /// Live config reload, called on the redraw cadence. When [`poll_ui_config`] sees
    /// the file change it re-applies the `[ui]` presentation settings - theme /
    /// selection-style (the palette) and the hint-bar / view-border styles - so a
    /// config edit lands without restarting. Returns true when something was
    /// re-applied so the loop marks the frame dirty.
    ///
    /// A malformed edit keeps the current settings (and logs) rather than blanking the
    /// UI; the roster and hosts are left alone, because re-scanning hosts is the
    /// `rescan` key's job and a config edit must not reset the user's sessions. The
    /// prefix is input-side and deliberately not re-applied (it needs the key-decoder
    /// rebuild, which is not worth it on a setting that changes rarely).
    pub(super) fn on_config_check(&mut self) -> bool {
        let mut mtime = self.model.config_last_mtime;
        let ui = poll_ui_config(&mut mtime, &crate::provision::env::config_path());
        let Some(ui) = ui else {
            let effects = update(&mut self.model, Msg::ConfigObserved { mtime, ui: None });
            debug_assert!(effects.is_empty());
            return false;
        };
        let ui = match ui {
            Ok(ui) => ui,
            Err(error) => {
                let effects = update(&mut self.model, Msg::ConfigObserved { mtime, ui: None });
                debug_assert!(effects.is_empty());
                let effects = update(&mut self.model, Msg::ConfigError(error));
                debug_assert!(effects.is_empty());
                return true;
            }
        };
        let palette = crate::ui::palette::resolve_output(
            &ui.theme,
            crate::ui::chrome::palette_overrides(&ui),
        );
        let effects = update(
            &mut self.model,
            Msg::ConfigObserved {
                mtime,
                ui: Some(Box::new((ui, palette))),
            },
        );
        debug_assert!(effects.is_empty());
        true
    }
}

/// The last line the attachment `id`'s pane holds, or a placeholder when there is none.
///
/// Read BEFORE the attachment is reaped: the reap drops the grid, and the grid is the only
/// place the child's own account of why it stopped exists.
fn last_pane_line(registry: &crate::display::registry::AttachRegistry, id: u64) -> String {
    let Some(addr) = registry.address_of_id(id) else {
        return "(no pane)".to_string();
    };
    let Some(grid) = registry.grid(&addr) else {
        return "(no grid)".to_string();
    };
    // ssh's closing transfer report follows whatever ended the session.
    let line = grid
        .lock()
        .ok()
        .and_then(|g| g.last_line_except(crate::transport::diagnostic::is_verbose_report_line));
    line.map(|l| crate::driver::escape_controls(&l))
        .unwrap_or_else(|| "(blank)".to_string())
}

/// How xmux reaches every card: each host, and each machine that serves no host yet.
/// A machine's entry names the machine and its reachability probe, and no mux, because
/// none has answered for it.
fn reach_map(
    env: &Env,
    hosts: &crate::model::Hosts,
) -> std::collections::HashMap<String, crate::state::HostReach> {
    let defs = hosts.def_list();
    let mut reach: std::collections::HashMap<String, crate::state::HostReach> = defs
        .iter()
        .map(|s| (s.alias.clone(), host_reach(s)))
        .collect();
    let roster = env.roster();
    for machine in roster
        .cfg
        .auto_machines(&roster.ssh_aliases, &roster.wsl_distros)
    {
        if defs
            .iter()
            .any(|s| crate::session::machine_of(&s.alias) == machine)
        {
            continue;
        }
        let kind = crate::transport::kind_for(
            &machine,
            machine.clone(),
            std::env::consts::OS,
            &env.xmux_dir,
            None,
        );
        let addressed = kind.addressed_as();
        let socket = kind.socket_path();
        let ssh = kind.clone().transport().is_remote();
        let probe = kind
            .transport()
            .raw_shell_argv(crate::transport::vocab::SHELL_PROBE)
            .map(|argv| crate::driver::shell_line(&argv))
            .unwrap_or_default();
        reach.insert(
            machine,
            crate::state::HostReach {
                ssh,
                probe,
                machine: addressed,
                socket,
                ..Default::default()
            },
        );
    }
    reach
}

/// How xmux reaches `s`, reduced to the words the unreachable screen prints.
///
/// The reduction happens HERE, at the wiring, for the reason the roster providers are
/// reduced here: the screen prints these and branches on none of them, so the UI layer
/// never learns what a machine kind or a mux binary is. Each field comes from the one
/// place that owns it - the machine describes its own addressing, the host composes its
/// own listing command - rather than being re-derived from a host id.
pub(super) fn host_reach(s: &crate::model::host_def::HostDef) -> crate::state::HostReach {
    crate::state::HostReach {
        ssh: s.kind.clone().transport().is_remote(),
        probe: crate::driver::shell_line(&s.host().list_sessions_command()),
        machine: s.kind.addressed_as(),
        mux: s.binary.clone(),
        kind: crate::mux::for_binary(&s.binary)
            .map(|m| m.kind().to_string())
            .unwrap_or_else(|| s.binary.clone()),
        socket: s.kind.socket_path(),
        refresh: match s.host().mux.event_source() {
            crate::model::EventSource::Control => "live".into(),
            crate::model::EventSource::Poll if s.kind.clone().transport().reuses_connection() => {
                "every 3 s over held connection".into()
            }
            crate::model::EventSource::Poll => "on request".into(),
        },
    }
}
