use super::*;

impl Runtime {
    /// Applies one [`HostEvent`] through the application update transition, then runs
    /// its ordered effects through the unified executor with the host clients,
    /// registry, and display worker that own the required capabilities.
    /// Drained in a burst by `on_host_event`. Returns `true` when the caller should
    /// rearm `attach_deadline` and mark dirty for a matched-client detach reap.
    pub(super) fn handle_host_event(&mut self, mut ev: HostEvent) -> bool {
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

    /// Performs the source-specific I/O carried by one nested event effect. The unified
    /// effect executor delegates this capability work and places any returned follow-up
    /// effects back on its ordered work queue.
    pub(super) fn perform_source_effect(
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
            | EventEffect::ApplySourceResult { .. }
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
                // (`apply_source_result`) or resyncing its dead terminals. (`ApplyInventory`
                // is emitted only for control-mode hosts, so a poll host is never gated out.)
                let live = mgr.get(&host).is_some();
                followups = update(
                    model,
                    Msg::ApplyInventory {
                        source: host.clone(),
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
                        source: host,
                        sessions,
                    }));
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
                    h.display_tty = crate::model::DisplayTty(None); // the dead client's tty is gone
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
                // nav never chases someone else's switch. The display tty is captured right
                // after attach (the record snippet echoes it on the first pump read, before the
                // user can drive the client), so a real prefix+s always matches; only a switch
                // in the sub-capture window would be missed, and the next nav move self-heals it.
                let Some(h) = hosts.get(&host) else {
                    return (false, Vec::new());
                };
                if !h.matches_display_tty(&client) {
                    return (false, Vec::new());
                }
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
            EventEffect::AddDiscoveredSources { machine, muxes } => {
                // A machine answered which muxes it has. Every one it does not already
                // serve becomes a source of its own, RIGHT NOW: the card appears scanning
                // and streams its sessions in like any other.
                //
                // A machine that serves no source yet names its sources the way a written
                // list would: one mux takes the bare machine name, which is the card the
                // machine has been showing, and several are each qualified. A machine
                // that already serves a source adds each new one qualified (`prod:zellij`),
                // and the one already served keeps the id it was painted with, because
                // that id is what the frozen order, the persisted selection, and anything
                // the user typed are keyed to - renaming it mid-run would break all three.
                let (vc, vr) = terminal_view_size(cols, rows, nav);
                let first = !hosts.serves_any(&machine);
                let muxes = match muxes {
                    Ok(muxes) => muxes,
                    // The machine could not be asked at all, which says nothing about what
                    // it serves. A machine standing as its own card keeps it and shows the
                    // failure there; one that serves sources has them report for it.
                    Err(reason) => {
                        tracing::warn!(machine = %machine, error = %reason, "mux discovery failed");
                        if first {
                            let effects = update(
                                model,
                                Msg::ApplySourceResult {
                                    source: machine,
                                    sessions: Vec::new(),
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
                            let id = crate::session::source_id(&machine, &bin, true);
                            (bin, id)
                        })
                        .collect()
                };
                // The card that stood for the machine goes when no source takes its name:
                // nothing answered, so there is nothing to show, or several muxes did and
                // each has a card of its own.
                if first && !specs.iter().any(|(_, id)| *id == machine) {
                    let effects = update(
                        model,
                        Msg::RemoveSource {
                            source: machine.clone(),
                            clear_tracking: false,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                for (bin, id) in specs {
                    if hosts.get(&id).is_some() {
                        continue;
                    }
                    let Some(host) = hosts.discovered_host(&machine, &bin, &id) else {
                        continue;
                    };
                    tracing::info!(machine = %machine, mux = %bin, source = %id, "mux discovered");
                    hosts.insert(host);
                    // The loop drives the `Host`; the OFF-LOOP ops (create a session, read
                    // panes, read border styles) resolve a source by id through `Env`. Both
                    // have to learn the source, or it paints and scans but refuses every
                    // operation with `unknown source`.
                    env.add_source(crate::model::source::for_machine_mux(
                        &machine,
                        &bin,
                        id.clone(),
                        std::env::consts::OS,
                        &env.xmux_dir,
                        env.local_socket.clone(),
                    ));
                    let effects = update(model, Msg::SetSourceReach(reach_map(env)));
                    debug_assert!(effects.is_empty());
                    // A source that takes the card the machine stood as inherits that card,
                    // whatever it last showed; its own first listing is now in flight.
                    let effects = update(
                        model,
                        Msg::AddSource {
                            source: id.clone(),
                            scanning: true,
                        },
                    );
                    debug_assert!(effects.is_empty());
                    scan_or_dispatch_host(mgr, hosts, model, &id, vc, vr, scan_pool);
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
                // A roster resolution completed. Three registries have to agree about
                // which machines exist, so all three are reconciled from this ONE answer:
                // the host registry the loop drives, the source list the off-loop ops
                // resolve against, and the nav. Which makes this the one place to settle
                // what the answer even is: a machine only a PROBE offers is carried back
                // in before anything reads the roster, so a probe that was too slow
                // cannot reap a card through all three at once.
                let mut roster = roster;
                env.carry_probed(&mut roster);
                let mut fresh = crate::model::Hosts::build(
                    &roster.cfg,
                    &roster.ssh_aliases,
                    &roster.wsl_distros,
                    std::env::consts::OS,
                    &roster.local_muxes,
                    &env.xmux_dir,
                    env.local_socket.clone(),
                );
                fresh.set_credentials(env.credentials());
                // What offered each host, refreshed with the roster: a host added by this
                // resolution has to be able to name the provider that offered it, exactly
                // as one present since launch can.
                let providers = roster
                    .roster_providers
                    .iter()
                    .map(|(host, provider)| (host.clone(), provider.label().to_owned()))
                    .collect();
                let login_defaults = roster.login_defaults.clone();
                let ssh_stanzas = roster.ssh_stanzas.clone();
                env.replace_roster(*roster);
                let held = env.credentials().machines();
                let effects = update(
                    model,
                    Msg::SetRosterFacts {
                        providers,
                        login_defaults,
                        ssh_stanzas,
                        held_credentials: held,
                        source_reach: reach_map(env),
                    },
                );
                debug_assert!(effects.is_empty());
                let delta = hosts.reconcile(fresh);
                for id in &delta.removed {
                    tracing::info!(source = %id, "roster dropped a source");
                    // Everything this source held: its metadata channel, the live PTY
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
                        Msg::RemoveSource {
                            source: id.clone(),
                            clear_tracking: true,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                for id in &delta.added {
                    tracing::info!(source = %id, "roster offered a new source");
                    let effects = update(
                        model,
                        Msg::AddSource {
                            source: id.clone(),
                            scanning: launching,
                        },
                    );
                    debug_assert!(effects.is_empty());
                }
                if launching {
                    probe_machines(hosts, mgr.events(), scan_pool, false);
                    return (false, Vec::new());
                }
                // Probe each ADDED machine's reachability (deduped by machine): a machine
                // the roster just named turns into a connected card that streams its
                // sessions, or a locked/unreachable one, exactly as at launch. The machines
                // that were already standing keep the channels they hold; the concurrent
                // re-probe of all machines that the re-scan already started reclassifies
                // those, so nothing is probed twice for one re-scan.
                let mut probed: HashSet<&str> = HashSet::new();
                for id in &delta.added {
                    let machine = crate::session::machine_of(id);
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
                source, detected, ..
            } => {
                // A detection probe resolved: (re)identify the mux, then dispatch the
                // now-detected host onto its metadata channel (control client or poll task).
                // A probe that could not identify one has ALREADY settled the card as
                // unreachable in update (when it was still scanning), so opening a
                // doomed control child would just die and overwrite that reason with a
                // bare "connection closed". The reconnect sweep retries detection.
                let effects = update(
                    model,
                    Msg::DetectionFinished {
                        source: source.clone(),
                    },
                );
                debug_assert!(effects.is_empty());
                apply_scan_result(hosts, &source, detected);
                if hosts.get(&source).is_some_and(|h| h.detected) {
                    let (vc, vr) = terminal_view_size(cols, rows, nav);
                    dispatch_detected_host(mgr, hosts, &source, vc, vr);
                }
            }
            EventEffect::MachineConnected {
                machine,
                shell,
                rescan,
            } => {
                // Record the shell family the probe read on every source this machine
                // serves, before a channel opens: the attach shape and the in-place
                // switch are composed for a shell family, so the first command must
                // already know which one answered.
                if let Some(shell) = shell {
                    env.record_remote_shell(&machine, shell);
                    hosts.for_each_transport_of(&machine, |t| t.set_remote_shell(shell));
                }
                // The machine's reachability probe connected: resolve every source it
                // serves onto its metadata channel (a re-scan re-enumerates a live one; a
                // launch detects then ensures it), and, when the machine left its mux list
                // to xmux, ask which muxes it serves so the ones nobody wrote down appear.
                let (vc, vr) = terminal_view_size(cols, rows, nav);
                let sources: Vec<String> = hosts
                    .ids()
                    .iter()
                    .filter(|id| crate::session::machine_of(id) == machine)
                    .cloned()
                    .collect();
                for source in &sources {
                    let detected = hosts.get(source).is_some_and(|h| h.detected);
                    if detected {
                        if rescan {
                            if let Some(host) = hosts.get(source) {
                                mgr.rescan(source, host, vc, vr);
                            }
                        } else {
                            dispatch_detected_host(mgr, hosts, source, vc, vr);
                        }
                    } else {
                        scan_or_dispatch_host(mgr, hosts, model, source, vc, vr, scan_pool);
                    }
                }
                // Mux discovery is a machine-level question, asked once per connect and
                // only when the machine left its list to xmux. The startup roster already
                // resolved this box's muxes, so it is never re-probed here.
                if !crate::session::is_local_source(&machine)
                    && env.roster().cfg.mux_is_auto(&machine)
                {
                    if let Some(transport) = hosts.host_transport(&machine) {
                        spawn_mux_discovery(
                            machine,
                            transport.clone_box(),
                            mgr.events(),
                            scan_pool.clone(),
                        );
                    }
                }
            }
            EventEffect::RenameDisplayed { source, from, to } => {
                if let Some(h) = hosts.get_mut(&source) {
                    h.display.rename_session(&from, &to);
                }
            }
            EventEffect::SyncInventorySessions { source, sessions } => {
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
                sync_source_terminals(&source, &sessions, &mut ctx);
            }
            EventEffect::SyncPollSessions { source, sessions } => {
                // A poll host's SUCCESSFUL enumeration (the nav group is already applied).
                // The enumeration is logged at the producer (`run_poll`), where `err` is in
                // hand - update drops the error path before reaching here, so logging
                // here would only ever see successes.
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
                    instance_name: &self.instance_name,
                    mgr,
                    worker,
                    pty_tx,
                    attach_seq: &mut *attach_seq,
                    viewport: terminal_view_size(cols, rows, nav),
                };
                sync_source_terminals(&source, &sessions, &mut ctx);
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
                }
            }
        }
        (false, followups)
    }
}

/// Detects a config-file change and, on a real change, reloads the `[ui]` section.
/// Returns `Some(ui)` only when the file genuinely changed since the last sight;
/// the first sight just records a baseline and a missing/currently-unwritable file is
/// ignored, so an editor mid-save never blanks the UI. Pure - it touches no global
/// state, which is what lets a test drive it with a temp file.
pub(super) fn poll_ui_config(
    last: &mut Option<std::time::SystemTime>,
    path: &std::path::Path,
) -> Option<crate::provision::config::UiConfig> {
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
    crate::provision::config::load(path).ok().map(|c| c.ui)
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
        // One read of the roster for the whole construction, so every product below is
        // built from ONE answer about which machines exist.
        let roster = env.roster();
        let nav_default = roster.cfg.ui.nav_position();
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

        // Host model: the single runtime registry, keyed by id (local first, then each
        // ssh alias in config order), built from the config-assembly products on `Env`.
        let host_os = std::env::consts::OS;
        let mut hosts = crate::model::Hosts::build(
            &roster.cfg,
            &roster.ssh_aliases,
            &roster.wsl_distros,
            host_os,
            &roster.local_muxes,
            &env.xmux_dir,
            env.local_socket.clone(),
        );
        if env.startup_pending && !hosts.serves_any(crate::session::LOCAL_SOURCE) {
            hosts.hold_unresolved(
                crate::session::LOCAL_SOURCE.to_string(),
                crate::transport::local(None),
            );
        }
        hosts.set_credentials(env.credentials());

        // The app's runtime state (single source of truth), seeded from the host ids;
        // events stream the nav in.
        let mut state = crate::state::State::from_sources(hosts.card_ids());
        let mut switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
        // The one session the terminal view refuses: the one xmux is running in. Named
        // once here, because the environment that names it cannot change under a run.
        switcher.set_own_session(env.own_session.clone());
        // [ui] hide-unreachable: the nav drops the settled unreachable hosts' cards. The
        // filter naming one brings its card, and its unreachable screen, back.
        switcher.set_hide_unreachable(roster.cfg.ui_hide_unreachable(), &mut state);
        // [ui] notifications: whether results show as toasts; the history keeps them either
        // way.
        state.notify.set_toasts_enabled(roster.cfg.ui.notifications);
        // And what offered each host, so an unreachable one can name the provider that
        // put it on the roster. Reduced to words here: the screen prints them and
        // nothing branches on which provider it was.
        state.chrome.set_roster_providers(
            roster
                .roster_providers
                .iter()
                .map(|(host, p)| (host.clone(), p.label().to_string()))
                .collect(),
        );
        // And what the login pane starts from: the address a provider knew for each host,
        // and this machine's own account name. Both are what ssh would have used, so a
        // pane that opens on a failure opens showing what just failed.
        state
            .chrome
            .set_login_defaults(roster.login_defaults.clone(), roster.ssh_stanzas.clone());
        // And how each source is REACHED, so an unreachable one states what was asked of
        // it and over what, not only that it failed. Resolved to words here for the same
        // reason the providers are: the screen prints them and nothing branches on them.
        state.chrome.set_source_reach(reach_map(&env));
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
        let palette = crate::ui::palette::resolve(
            &roster.cfg.ui.theme,
            crate::ui::chrome::palette_overrides(&roster.cfg.ui),
        );
        state.chrome.apply_palette(&roster.cfg.ui, &palette);
        switcher.set_palette(palette);
        // The help modal must show the prefix the user configured, not a literal.
        state.chrome.set_ui_prefix(env.ui_prefix.clone());
        drop(roster);

        // The live mutate ops (create/rename/kill) - NOT nav probing.
        let ops = env.ops();
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
        };
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
            cols,
            body_rows,
            term_input,
            nav_decoder,
            prefix,
            // The draw hot path's observability (per-key grid fingerprints + slow-step
            // probe), owned off the draw block so it does nothing but lock → render.
            draw_observer: DrawObserver::default(),
            spinner_start: std::time::Instant::now(),
            login_probes: 0,
            dirty: true,
            last_draw: std::time::Instant::now() - std::time::Duration::from_millis(FRAME_MS),
            rescan_pending: false,
            #[cfg(test)]
            discovery_runs: 0,
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
        use std::time::Duration;
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
        let want_nav_width = reconciled_nav_width(
            self.model.state.focus.is_terminal_focused(),
            self.model.auto_hide_nav,
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
        // h·l in a column), the band height (border drag / resize keys), or the side the
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
            // gets the same treatment.
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
            }
            self.dirty = true;
        }
        // The cheatsheet and the help modal name the arrow pair the CURRENT placement
        // makes active, so they read the resolved position every frame.
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::display::term::ensure_mouse_capture();
        // An `r` re-scan also re-attaches the CURRENT display: tear the (possibly dead)
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
        if self.dirty && self.last_draw.elapsed() >= Duration::from_millis(FRAME_MS) {
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
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        let plan = switcher.layout(f.area(), nav, state, &previous_plan);
                        switcher.render(f, guard.as_deref(), terminal_focused, state, &plan);
                        next_plan = Some(plan);
                        DrawObserver::slow_step("render", t_render);
                    })
                }
                None => {
                    let nav = self.nav_size();
                    let switcher = &self.model.switcher;
                    let state = &self.model.state;
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        let plan = switcher.layout(f.area(), nav, state, &previous_plan);
                        switcher.render(f, None, terminal_focused, state, &plan);
                        next_plan = Some(plan);
                        DrawObserver::slow_step("render", t_render);
                    })
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

    /// Re-emits an OSC 52 clipboard sequence on xmux's own stdout so the terminal
    /// above it sets the clipboard. Called from [`Runtime::on_pty_event`], which the
    /// `select!` loop runs strictly between ratatui frames - ratatui owns stdout and a
    /// write from anywhere else (the pump thread) would land mid-frame. Re-emitting
    /// the escape, not calling a clipboard API, is what keeps this working when xmux
    /// itself runs over ssh.
    fn emit_osc52(seq: &[u8]) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(seq);
        let _ = out.flush();
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
            // re-scanning. Repaint so the pane shows what it is now.
            self.dirty = true;
        }
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
                }
                Some(id) == displayed_attach_id
            }
            PtyEvent::DisplayTty { id, tty } => {
                record_display_tty(&mut self.hosts, &self.registry, id, tty);
                false
            }
            PtyEvent::Output { id } => {
                self.note_pending_output(id);
                false
            }
            PtyEvent::Osc52 { seq } => {
                Self::emit_osc52(&seq);
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

    /// Installs one attachment whose display gate has opened and confirms it only when
    /// its key is the one the current selection renders through.
    fn install_attachment(
        &mut self,
        key: String,
        attachment: crate::display::attachment::Attachment,
        shown: String,
    ) {
        let selected_key = display_key(&self.hosts, &self.model.state.selection);
        let hid = host_of_key(&key).to_string();
        let attach_id = attachment.id();
        let child_tty = attachment.child_tty().map(str::to_string);
        self.registry.remove(&key);
        self.registry.insert(&key, attachment);

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
                    source: hid,
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
        // Clone the selection so &mut state can be threaded alongside it (the ForwardToMux
        // path reads the selection for display_key/registry input).
        let selection = self.model.state.selection.clone();
        let outcome = self.handle_stdin_bytes(bytes, &selection);
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
                        self.model.switcher.current_source().as_deref() == Some(&l.source)
                    });
                    if login_running {
                        if bytes.as_slice() == b"\x1b" {
                            let effects = update(&mut self.model, Msg::CancelRunningLogin);
                            let _ = self.execute_effects(effects);
                        }
                        self.dirty = true;
                    } else if self.model.switcher.current_host_blocked() {
                        if let Some(source) = self.model.switcher.current_source() {
                            let effects = update(&mut self.model, Msg::FeedLogin { source, bytes });
                            let _ = self.execute_effects(effects);
                            self.dirty = true;
                        }
                    } else {
                        let Some(host) = self.hosts.get(&self.model.state.selection.source) else {
                            return false;
                        };
                        // Follow the selected destination, matching the interactive
                        // keystroke path. While its fresh client is paint-pending, the
                        // registry routes input there instead of into the stale frame.
                        let mut driver = crate::driver::driver_for(host);
                        let ctx = crate::driver::DriverCtx {
                            registry: &mut self.registry,
                            hosts: &mut self.hosts,
                            instance_name: &self.instance_name,
                            mgr: &self.mgr,
                            worker: &self.worker,
                            pty_tx: &self.driver_pty_tx,
                            attach_seq: &mut self.attach_seq,
                            viewport: (0, 0),
                        };
                        driver.input(&self.model.state.selection, bytes, &ctx);
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
    /// login returns the source it was for; only THAT machine's reach changed (locked →
    /// connected), so re-probe just it - over what the login left behind - instead of the
    /// whole roster. The re-probe is what turns the pane back into the host's sessions.
    pub(super) fn on_op_result(&mut self, result: crate::ui::switcher::OpResult) {
        let effects = update(
            &mut self.model,
            Msg::OpResult {
                result,
                logged_in: self.env.credentials().machines(),
            },
        );
        let _ = self.execute_effects(effects);
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
        let Some(shown) = display_session(&self.hosts, &self.model.state.selection.source) else {
            return false;
        };
        if shown == self.model.state.selection.session {
            return false;
        }
        let addr = crate::session::Address::new(&self.model.state.selection.source, shown);
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
    /// already on its way somewhere else.
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
        let source = self.model.state.selection.source.clone();
        let Some(host) = self.hosts.get(&source) else {
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
        if let Some(h) = self.hosts.get_mut(&source) {
            h.display.set_shows(&key, &session);
        }
        tracing::info!(
            host = %source,
            session = %session,
            "display_client_session_changed"
        );
        true
    }

    /// The animation-tick arm: detect a console resize (push the new size to PTYs +
    /// control clients, force a full repaint), read xmux's own display client for a
    /// mux-side session change, and refresh the connecting-spinner set.
    pub(super) fn on_tick(&mut self, term: &mut Term) {
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
                self.dirty = true;
            }
        }
        if self.observe_display_session() {
            self.dirty = true;
        }
        // A flash outlives the moment it was about, so it comes down on its own for a
        // user who pressed nothing. The tick is where that is noticed, because it is the
        // one wake that happens without the user doing anything.
        let had_flash = !self.model.state.chrome.flash.is_empty();
        // Spinner set = the selected session if its PTY is still connecting.
        let mut sp = HashSet::new();
        if !self.model.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.model.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.model.state.selection.source)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if in_flight_for_key || self.registry.connecting(&key) {
                sp.insert(
                    crate::session::Address::new(
                        &self.model.state.selection.source,
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
        if had_flash && self.model.state.chrome.flash.is_empty() {
            self.dirty = true;
        }
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
    /// UI; the roster and hosts are left alone, because re-scanning sources is the
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
        let palette =
            crate::ui::palette::resolve(&ui.theme, crate::ui::chrome::palette_overrides(&ui));
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
    let line = grid.lock().ok().and_then(|g| g.last_line());
    line.map(|l| crate::driver::escape_controls(&l))
        .unwrap_or_else(|| "(blank)".to_string())
}

/// How xmux reaches every card: each source, and each host that serves no source yet.
/// A host's entry names the machine and its reachability probe, and no mux, because
/// none has answered for it.
fn reach_map(env: &Env) -> std::collections::HashMap<String, crate::state::SourceReach> {
    let sources = env.source_list();
    let mut reach: std::collections::HashMap<String, crate::state::SourceReach> = sources
        .iter()
        .map(|s| (s.alias.clone(), source_reach(s)))
        .collect();
    let roster = env.roster();
    for machine in roster
        .cfg
        .auto_hosts(&roster.ssh_aliases, &roster.wsl_distros)
    {
        if sources
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
        let probe = kind
            .transport()
            .raw_shell_argv(crate::transport::vocab::SHELL_PROBE)
            .map(|argv| crate::driver::shell_line(&argv))
            .unwrap_or_default();
        reach.insert(
            machine,
            crate::state::SourceReach {
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
/// own listing command - rather than being re-derived from a source id.
pub(super) fn source_reach(s: &crate::model::source::Source) -> crate::state::SourceReach {
    crate::state::SourceReach {
        probe: crate::driver::shell_line(&s.host().list_sessions_command()),
        machine: s.kind.addressed_as(),
        mux: s.binary.clone(),
        kind: crate::mux::for_binary(&s.binary)
            .map(|m| m.kind().to_string())
            .unwrap_or_else(|| s.binary.clone()),
        socket: s.kind.socket_path(),
    }
}
