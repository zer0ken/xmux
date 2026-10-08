//! `Hosts`: the runtime host registry - every host keyed by id, in display order
//! (local first). The single owner of each host's `Host`, and the one registry every
//! consumer reads: the event loop drives the `Host`s, and the off-loop operations, the
//! CLI, and the scan read the [`HostDefs`] it publishes, so no two consumers can
//! disagree about which hosts exist.

use std::collections::{HashMap, HashSet};

use crate::model::host_def::{HostDef, HostDefs, Runner};
use crate::model::{Host, Liveness};
use crate::mux::for_binary;
use crate::provision::config::Config;
use crate::session::LOCAL_MACHINE;
use crate::transport::Transport;

/// Every host, keyed by host id, in display order (local first). The single owner of
/// each host's `Host` for the app loop, so a host is present here or nowhere.
///
/// A machine whose muxes are xmux's to decide has no host until the machine itself answers
/// which muxes it serves, so it is also held as a MACHINE: its name and the transport that
/// reaches it. That transport is what probes the machine and asks it for its muxes, and
/// every host found on it is built from it.
///
/// Every change to the hosts republishes `hosts`, the [`HostDefs`] handed to the
/// work that runs off the event loop, so a host added here is operable everywhere and
/// no caller has a second registry to keep in step.
#[derive(Default)]
pub struct Hosts {
    order: Vec<String>,
    map: HashMap<String, Host>,
    auto: Vec<(String, Box<dyn Transport>)>,
    /// Explicit mux choices, used to distinguish config edits from missed probes.
    written_muxes: HashMap<String, Vec<String>>,
    hosts: HostDefs,
    credentials: crate::transport::auth::Credentials,
    remote_shells: crate::model::host_def::RemoteShells,
    /// The runner every published host runs its commands through; `None` is the real
    /// exec runner.
    runner: Option<std::sync::Arc<dyn Runner>>,
}

/// What one [`Hosts::reconcile`] changed: the host ids it added, the ids it dropped
/// because the fresh roster no longer names their machine, and the machines that joined
/// and left the roster. The loop acts on all four, so the registry, the nav, and the live
/// connections stay one answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RosterDelta {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub machines_added: Vec<String>,
    pub machines_removed: Vec<String>,
}

impl Hosts {
    /// An empty registry (same as `Default`; both pinned because tests call
    /// `Hosts::default()`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a host, keyed on `host.id()`, appending to display order
    /// on first insert only.
    pub fn insert(&mut self, host: Host) {
        self.insert_unpublished(host);
        self.publish();
    }

    fn insert_unpublished(&mut self, host: Host) {
        let id = host.id().to_string();
        if !self.map.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.map.insert(id, host);
    }

    /// Derives one [`HostDef`] from each runtime host, in display order, and publishes
    /// them. The definition is read off the `Host` itself (its id, its mux binary, and
    /// the construction data of its transport), so it cannot name a machine or a mux the
    /// loop does not drive.
    fn publish(&self) {
        let hosts = self
            .order
            .iter()
            .filter_map(|id| self.map.get(id))
            .map(|host| HostDef {
                alias: host.id().to_string(),
                binary: host.mux.bin().to_string(),
                kind: host.transport.machine_kind(),
                runner: self.runner.clone(),
                remote_shells: self.remote_shells.clone(),
                credentials: self.credentials.clone(),
            })
            .collect();
        self.hosts.replace(hosts);
    }

    /// The published hosts, shared with the work that runs off the event loop.
    pub fn defs(&self) -> HostDefs {
        self.hosts.clone()
    }

    /// The host answering as `id`, if this registry holds one.
    pub fn def(&self, id: &str) -> Option<HostDef> {
        self.hosts.get(id)
    }

    /// Every host, in display order.
    pub fn def_list(&self) -> Vec<HostDef> {
        self.hosts.list()
    }

    /// Shares the run's machine credential store with every transport this registry holds
    /// and every host it publishes.
    pub(crate) fn set_credentials(&mut self, credentials: crate::transport::auth::Credentials) {
        for host in self.map.values_mut() {
            host.transport.set_credentials(credentials.clone());
        }
        for (_, transport) in &mut self.auto {
            transport.set_credentials(credentials.clone());
        }
        self.credentials = credentials;
        self.publish();
    }

    /// Shares the record of each machine's shell family with every host this registry
    /// publishes, so an off-loop operation composes its command for the shell that reads
    /// it.
    pub(crate) fn set_remote_shells(
        &mut self,
        remote_shells: crate::model::host_def::RemoteShells,
    ) {
        self.remote_shells = remote_shells;
        self.publish();
    }

    #[cfg(test)]
    pub(crate) fn set_runner(&mut self, runner: std::sync::Arc<dyn Runner>) {
        self.runner = Some(runner);
        self.publish();
    }

    /// Holds a machine as an unresolved machine card until its mux list arrives.
    pub(crate) fn hold_unresolved(&mut self, machine: String, mut transport: Box<dyn Transport>) {
        if !self.serves_any(&machine) && !self.auto.iter().any(|(name, _)| *name == machine) {
            transport.set_credentials(self.credentials.clone());
            self.auto.push((machine, transport));
        }
    }

    /// Assembles the hosts for a config: this machine's hosts first (one per entry of the
    /// RESOLVED `local_muxes`, its socket from `$TMUX`), then each ssh machine in order,
    /// then each WSL distribution. WSL comes last so adding the implementation leaves every
    /// id an existing install already had in the position it had.
    /// A machine whose muxes are xmux's to decide is held by name and transport, with no
    /// host until it answers.
    /// `xmux_dir` seeds each ssh transport's ControlMaster socket path. OpenSSH expands
    /// its `%C` component from the connection.
    pub fn build(
        cfg: &Config,
        ssh_aliases: &[String],
        wsl_distros: &[String],
        os: &str,
        local_muxes: &[String],
        xmux_dir: &std::path::Path,
        local_socket: Option<String>,
    ) -> Hosts {
        let mut hosts = Hosts::default();

        // One host per (machine, mux): this machine contributes one for each mux it serves.
        let qualified = local_muxes.len() > 1;
        for bin in local_muxes {
            let id = crate::session::host_id(LOCAL_MACHINE, bin, qualified);
            hosts.insert_unpublished(host_for(
                LOCAL_MACHINE,
                bin,
                id,
                os,
                xmux_dir,
                local_socket.clone(),
            ));
        }

        for spec in cfg
            .host_specs(ssh_aliases)
            .into_iter()
            .chain(cfg.wsl_specs(wsl_distros))
        {
            if spec.alias == LOCAL_MACHINE {
                continue; // "local" is reserved for this machine's hosts.
            }
            hosts.insert_unpublished(host_for(
                &spec.alias,
                &spec.bin,
                spec.id,
                os,
                xmux_dir,
                local_socket.clone(),
            ));
        }
        for machine in cfg.auto_machines(ssh_aliases, wsl_distros) {
            let kind = crate::transport::kind_for(&machine, machine.clone(), os, xmux_dir, None);
            hosts.auto.push((machine, kind.transport()));
        }
        for machine in hosts
            .machines()
            .into_iter()
            .chain([LOCAL_MACHINE.to_owned()])
        {
            if !cfg.mux_is_auto(&machine) {
                hosts.written_muxes.insert(
                    machine.clone(),
                    hosts
                        .order
                        .iter()
                        .filter(|id| crate::session::machine_of(id) == machine)
                        .map(|id| hosts.map[id].mux.bin().to_owned())
                        .collect(),
                );
            }
        }
        hosts.publish();
        hosts
    }

    /// Reconciles this registry against a freshly built one, so a re-scan reflects a
    /// config edit, or a machine coming online, without a restart. Returns what changed.
    ///
    /// A surviving host keeps its LIVE `Host` untouched. The detected mux, the display
    /// tty, and the connection the loop drives all live on it, and replacing it would
    /// tear down a channel that has nothing wrong with it.
    ///
    /// A changed explicit mux list removes hosts it no longer names. Otherwise,
    /// removal is decided by machine: which muxes an automatic machine serves is
    /// answered by PROBING the machine, and building a registry probes nothing, so a
    /// host that async mux discovery added is absent from `fresh` while its machine is
    /// perfectly well named. Dropping by id would tear those cards down on every re-scan
    /// and re-find them a moment later.
    ///
    /// A MACHINE absent from `fresh` is therefore taken as gone, which holds only because
    /// what reaches here is already settled: a machine a probe offers is carried back into
    /// the roster before this sees it (see [`Env::carry_probed`]), so absence here is a
    /// record no longer naming it and never a probe that was too slow.
    ///
    /// [`Env::carry_probed`]: crate::provision::env::Env::carry_probed
    ///
    /// A surviving host keeps the display position it had and an added one appends, so a
    /// card the user is looking at does not move because another machine answered.
    ///
    /// A machine whose muxes are xmux's to decide survives on its NAME, and keeps the
    /// transport it had, which holds what its probe and login established.
    pub fn reconcile(&mut self, mut fresh: Hosts) -> RosterDelta {
        let before = self.machines();
        let transports: HashMap<_, _> = before
            .iter()
            .filter_map(|machine| {
                self.machine_transport(machine)
                    .filter(|transport| transport.is_remote())
                    .map(|transport| (machine.clone(), transport.clone_box()))
            })
            .collect();
        let mut machines: HashSet<&str> = fresh
            .order
            .iter()
            .map(|id| crate::session::machine_of(id))
            .chain(fresh.auto.iter().map(|(machine, _)| machine.as_str()))
            .collect();
        // This box always exists. The local machine's presence in `fresh` depends on a
        // probe (the resolved local mux list), and a probe result is a verdict on which
        // muxes are here, never on whether the machine exists - so it must not be able
        // to reap every local host on a re-scan where the probe failed to answer.
        machines.insert(LOCAL_MACHINE);
        let removed: Vec<String> = self
            .order
            .iter()
            .filter(|id| {
                let machine = crate::session::machine_of(id);
                !machines.contains(machine)
                    || fresh.written_muxes.get(machine).is_some_and(|bins| {
                        self.written_muxes.get(machine) != Some(bins)
                            && !bins.iter().any(|bin| bin == self.map[*id].mux.bin())
                    })
            })
            .cloned()
            .collect();
        let gone_auto: Vec<String> = self
            .auto
            .iter()
            .map(|(machine, _)| machine.clone())
            .filter(|machine| !fresh.auto.iter().any(|(m, _)| m == machine))
            .collect();
        drop(machines);
        self.written_muxes = std::mem::take(&mut fresh.written_muxes);
        self.auto
            .retain(|(machine, _)| !gone_auto.contains(machine));
        self.order.retain(|id| !removed.contains(id));
        for id in &removed {
            self.map.remove(id);
        }
        let mut added = Vec::new();
        for (machine, transport) in std::mem::take(&mut fresh.auto) {
            if self.auto.iter().any(|(m, _)| *m == machine) {
                continue;
            }
            self.auto.push((machine, transport));
        }
        for id in std::mem::take(&mut fresh.order) {
            if self.map.contains_key(&id) {
                continue;
            }
            if let Some(mut host) = fresh.map.remove(&id) {
                // A (machine, mux) pair is served by at most one id. Local ids are
                // qualified from how many muxes the probe reported, so a bare `local`
                // and a `local:psmux` can name the same pair across resolutions;
                // adding the second spelling would paint a duplicate card.
                if self.machine_serves(crate::session::machine_of(&id), host.mux.bin()) {
                    continue;
                }
                if let Some(transport) = transports.get(crate::session::machine_of(&id)) {
                    host.transport = transport.clone_as(&id);
                }
                self.insert_unpublished(host);
                added.push(id);
            }
        }
        self.publish();
        let after = self.machines();
        RosterDelta {
            added,
            removed,
            machines_added: after
                .iter()
                .filter(|m| !before.contains(m))
                .cloned()
                .collect(),
            machines_removed: before.into_iter().filter(|m| !after.contains(m)).collect(),
        }
    }

    /// Whether `machine` already serves a host running the mux binary `bin`. The
    /// discovery add path asks before adding, so a mux the machine was already
    /// configured to run is never duplicated under a second id.
    pub fn machine_serves(&self, machine: &str, bin: &str) -> bool {
        self.order.iter().any(|id| {
            crate::session::machine_of(id) == machine
                && self.map.get(id).is_some_and(|h| h.mux.bin() == bin)
        })
    }

    /// Whether `machine` serves any host at all.
    pub fn serves_any(&self, machine: &str) -> bool {
        self.order
            .iter()
            .any(|id| crate::session::machine_of(id) == machine)
    }

    /// Every machine, once each: the machines the hosts name, then the machines that serve
    /// no host yet.
    pub fn machines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let named = self
            .order
            .iter()
            .map(|id| crate::session::machine_of(id))
            .chain(self.auto.iter().map(|(machine, _)| machine.as_str()));
        for machine in named {
            if !out.iter().any(|m| m == machine) {
                out.push(machine.to_string());
            }
        }
        out
    }

    /// The transport that reaches `machine`: its own, when it is a machine whose muxes xmux
    /// asks for, and otherwise that of the first host it serves.
    pub fn machine_transport(&self, machine: &str) -> Option<&dyn Transport> {
        if let Some((_, t)) = self.auto.iter().find(|(m, _)| m == machine) {
            return Some(t.as_ref());
        }
        self.order
            .iter()
            .find(|id| crate::session::machine_of(id) == machine)
            .and_then(|id| self.map.get(id))
            .map(|h| h.transport.as_ref())
    }

    /// Applies `f` to every transport that reaches `machine`, so a fact the machine
    /// established (the shell family its probe read, the values a login authenticated
    /// with) holds for every command sent to it, and for every host found on it later.
    pub fn for_each_transport_of(&mut self, machine: &str, mut f: impl FnMut(&mut dyn Transport)) {
        for (m, t) in self.auto.iter_mut() {
            if m == machine {
                f(t.as_mut());
            }
        }
        for (id, host) in self.map.iter_mut() {
            if crate::session::machine_of(id) == machine {
                f(host.transport.as_mut());
            }
        }
    }

    /// A host for the mux binary `bin` that `machine` answered it serves, answering as
    /// the host `id`, reached exactly as the machine is reached now. `None` for a
    /// machine this registry does not reach or a name no kind owns.
    ///
    /// It is DETECTED already: the answer came from the mux's own identity probe, the
    /// same one detection would run again.
    pub fn discovered_host(&self, machine: &str, bin: &str, id: &str) -> Option<Host> {
        let transport = self.machine_transport(machine)?.clone_as(id);
        let mut host = Host::new(transport, for_binary(bin)?);
        host.detected = true;
        Some(host)
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.map.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.map.get_mut(id)
    }

    /// Host ids in display order (local first) - the render projection iterates these.
    pub fn ids(&self) -> &[String] {
        &self.order
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Host> {
        self.map.values_mut()
    }

    /// Routes one `HostEvent` (the metadata reader's output) to the host it names,
    /// folding Host-owned liveness state. The sessions carried by `Connected`/`Inventory`
    /// are folded into `model::Host.inventory` by the run loop (or via `Host::enumerate`);
    /// this sets liveness. An unknown host id is a no-op - there is no second registry to
    /// grow a ghost host.
    pub fn apply_host_event(&mut self, ev: &crate::link::HostEvent) {
        use crate::link::HostEvent::*;
        match ev {
            Connected { host, .. } | Inventory { host, .. } => {
                if let Some(h) = self.get_mut(host) {
                    h.liveness = Liveness::Live;
                }
            }
            Exited { host, .. } => {
                if let Some(h) = self.get_mut(host) {
                    h.clear_display_tty();
                    h.liveness = Liveness::Unreachable;
                }
            }
            // Change events drive refetch in the render projection; they touch no
            // Host-owned field here.
            Changed { .. } => {}
            // The tty-matched reap of xmux's own display attach is the supervisor's job (it
            // owns the registry + the recover-from-detach rearm); the Hosts map holds no
            // per-attach state to fold here. `ClientSessionChanged` is the same: the tty match
            // + display-belief sync + nav follow run in the supervisor's effect handler.
            // Setting whether the display client sizes its session is a command to the host,
            // also the supervisor's.
            ClientDetached { .. } | ClientSessionChanged { .. } | DisplaySessionClients { .. } => {}
            // The metadata channel's own session is a fact the state keeps for the client
            // count; no Host-owned field holds it.
            ControlSession { .. } => {}
            // The -CC `list-clients` probe resolved xmux's display-client tty (or None if
            // the display attach has not registered yet). Record it so a session switch is
            // an in-place `switch-client -c <tty>`; None clears any stale tty.
            DisplayTty { host, tty } => {
                if let Some(h) = self.get_mut(host) {
                    h.record_display_tty(tty.clone());
                }
            }
            // Poll-host data carriers (enumeration results), the detection probe, a
            // machine's mux-discovery answer, and a machine's reachability probe. Their
            // sessions/mux/host set are applied by the caller (apply_host_result /
            // apply_scan_result / the discovery-add and machine-probe effects); they fold
            // no Host-owned liveness here. A discovery or machine-probe answer names a
            // MACHINE, not a host in this map, so it could not route here anyway.
            Scanned { .. }
            | AuthObserved { .. }
            | SharedConnectionSeen { .. }
            | Sessions { .. }
            | MuxesFound { .. }
            | RosterResolved { .. }
            | RosterKept
            | StartupResolved { .. }
            | MachineProbed { .. } => {}
        }
    }
}

/// One [`Host`] for the mux binary `bin` on `machine`, answering as the host `id`.
/// The transport comes from [`crate::transport::kind_for`], the one place a machine's
/// construction data is assembled.
pub fn host_for(
    machine: &str,
    bin: &str,
    id: String,
    os: &str,
    xmux_dir: &std::path::Path,
    local_socket: Option<String>,
) -> Host {
    // The socket reaches the machine only when `bin` takes one. See
    // `crate::mux::server_socket_for`.
    let local_socket = crate::mux::server_socket_for(bin, local_socket);
    let kind = crate::transport::kind_for(machine, id, os, xmux_dir, local_socket);
    Host::new(
        kind.transport(),
        for_binary(bin).expect("a host's binary is a registry name"),
    )
}

#[cfg(test)]
mod tests {
    /// The resolved local mux list a test builds with: what `Config::default()` on a
    /// unix box resolves to, so these tests pin host ORDER and ids, not discovery.
    fn local() -> Vec<String> {
        vec!["tmux".to_string()]
    }

    use super::*;
    use crate::link::HostEvent;
    use crate::model::{Liveness, ServerModel};
    use crate::transport::Transport;

    #[test]
    fn default_and_new_are_empty() {
        assert!(Hosts::default().ids().is_empty());
        assert!(Hosts::new().ids().is_empty());
    }

    #[test]
    fn insert_keys_on_host_id_and_appends_order_once() {
        let mut hosts = Hosts::default();
        let local = Host::new(crate::transport::local(None), for_binary("tmux").unwrap());
        hosts.insert(local);
        assert_eq!(hosts.ids(), &["local".to_string()]);
        // Re-inserting the same id replaces in place, does not duplicate the order.
        let local2 = Host::new(crate::transport::local(None), for_binary("psmux").unwrap());
        hosts.insert(local2);
        assert_eq!(
            hosts.ids(),
            &["local".to_string()],
            "same id does not duplicate order"
        );
        assert_eq!(
            hosts.get("local").unwrap().mux.server_model(),
            ServerModel::PerSession,
            "psmux replaced tmux"
        );
    }

    /// A config in which every machine named writes `tmux` as its mux.
    fn tmux_on(aliases: &[&str]) -> Config {
        Config {
            machines: aliases
                .iter()
                .map(|a| crate::provision::config::MachineConfig {
                    ssh: a.to_string(),
                    mux: "tmux".into(),
                })
                .collect(),
            ..Config::default()
        }
    }

    /// A registry for the ssh aliases named, each writing `tmux`, on a box serving one
    /// local mux.
    fn built(aliases: &[&str]) -> Hosts {
        let cfg = tmux_on(aliases);
        let aliases: Vec<String> = aliases.iter().map(|a| a.to_string()).collect();
        Hosts::build(
            &cfg,
            &aliases,
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        )
    }

    #[test]
    fn reconcile_adds_the_machines_the_fresh_roster_names() {
        let mut hosts = built(&["prod"]);
        let delta = hosts.reconcile(built(&["prod", "stage"]));
        assert_eq!(delta.added, vec!["stage".to_string()]);
        assert!(delta.removed.is_empty());
        assert_eq!(
            hosts.ids(),
            &["local".to_string(), "prod".to_string(), "stage".to_string()],
            "an added host appends, so a card the user is looking at does not move"
        );
    }

    #[test]
    fn reconcile_drops_the_machines_the_fresh_roster_stopped_naming() {
        let mut hosts = built(&["prod", "stage"]);
        let delta = hosts.reconcile(built(&["prod"]));
        assert_eq!(delta.removed, vec!["stage".to_string()]);
        assert!(delta.added.is_empty());
        assert_eq!(hosts.ids(), &["local".to_string(), "prod".to_string()]);
        assert!(hosts.get("stage").is_none(), "dropped from the map too");
    }

    /// A registry for the ssh aliases named, where `tmux` is written only for the ones in
    /// `written`; every other alias is a machine whose muxes xmux decides.
    fn built_with_auto(aliases: &[&str], written: &[&str]) -> Hosts {
        let cfg = tmux_on(written);
        let aliases: Vec<String> = aliases.iter().map(|a| a.to_string()).collect();
        Hosts::build(
            &cfg,
            &aliases,
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        )
    }

    #[test]
    fn reconcile_names_the_machines_it_adds_and_drops_apart_from_their_hosts() {
        let mut hosts = built_with_auto(&["prod", "web"], &["prod"]);
        assert!(
            !hosts.ids().contains(&"web".to_string()),
            "a machine whose muxes are not known has no host"
        );
        let delta = hosts.reconcile(built_with_auto(&["prod", "db"], &["prod", "db"]));
        assert_eq!(
            delta.added,
            vec!["db".to_string()],
            "db's written mux is a host"
        );
        assert!(delta.removed.is_empty(), "web never had a host to drop");
        assert_eq!(delta.machines_added, vec!["db".to_string()]);
        assert_eq!(delta.machines_removed, vec!["web".to_string()]);

        let delta = hosts.reconcile(built_with_auto(&["prod", "db", "api"], &["prod", "db"]));
        assert!(delta.added.is_empty(), "api has no mux known, so no host");
        assert_eq!(delta.machines_added, vec!["api".to_string()]);
        assert!(delta.machines_removed.is_empty());
        assert!(hosts.machines().contains(&"api".to_string()));
    }

    #[test]
    fn reconcile_leaves_a_surviving_host_live() {
        // The detected mux, the display tty, and the connection the loop drives all live
        // on the `Host`. Replacing a survivor would tear down a channel with nothing
        // wrong with it, so the live object must come through untouched.
        let mut hosts = built(&["prod"]);
        hosts.get_mut("prod").unwrap().liveness = Liveness::Live;
        hosts.get_mut("prod").unwrap().detected = true;
        hosts.reconcile(built(&["prod", "stage"]));
        let prod = hosts.get("prod").unwrap();
        assert_eq!(prod.liveness, Liveness::Live);
        assert!(prod.detected, "a survivor is not rebuilt from config");
    }

    #[test]
    fn reconcile_keeps_a_discovered_mux_on_a_machine_that_survives() {
        // `prod:zellij` exists because a probe ANSWERED, not because config named it, so
        // a freshly built registry cannot contain it. Removing by id would tear the card
        // down on every re-scan and re-find it a moment later; removal is by MACHINE.
        let mut hosts = built(&["prod"]);
        hosts.insert(host_for(
            "prod",
            "zellij",
            "prod:zellij".to_string(),
            "linux",
            std::path::Path::new("/x"),
            None,
        ));
        let delta = hosts.reconcile(built(&["prod"]));
        assert!(
            delta.removed.is_empty(),
            "prod is still named, so nothing it serves is dropped: {delta:?}"
        );
        assert!(hosts.get("prod:zellij").is_some());
    }

    #[test]
    fn reconcile_drops_a_discovered_mux_when_its_machine_goes() {
        let mut hosts = built(&["prod"]);
        hosts.insert(host_for(
            "prod",
            "zellij",
            "prod:zellij".to_string(),
            "linux",
            std::path::Path::new("/x"),
            None,
        ));
        let mut delta = hosts.reconcile(built(&[]));
        delta.removed.sort();
        assert_eq!(
            delta.removed,
            vec!["prod".to_string(), "prod:zellij".to_string()],
            "the machine is gone, so every host it served goes with it"
        );
        assert_eq!(hosts.ids(), &["local".to_string()]);
    }

    #[test]
    fn a_machine_that_writes_no_mux_is_held_without_a_host() {
        let hosts = Hosts::build(
            &Config::default(),
            &["win".to_string()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        assert_eq!(
            hosts.ids(),
            &["local".to_string()],
            "no mux is assumed for it"
        );
        assert_eq!(hosts.machines(), vec!["local", "win"]);
        let t = hosts.machine_transport("win").expect("it is still reached");
        assert!(t.is_remote());
        assert_eq!(t.host_id(), "win");
    }

    #[test]
    fn reconcile_keeps_the_transport_a_machine_that_writes_no_mux_already_has() {
        // The transport holds what the machine's probe and login established, so a re-scan
        // that still names the machine must not swap in a fresh one.
        let build = || {
            Hosts::build(
                &Config::default(),
                &["win".to_string()],
                &[],
                "linux",
                &local(),
                std::path::Path::new("/x"),
                None,
            )
        };
        let mut hosts = build();
        hosts.for_each_transport_of("win", |t| {
            t.set_remote_shell(crate::transport::vocab::RemoteShell::Other)
        });
        let delta = hosts.reconcile(build());
        assert_eq!(delta, RosterDelta::default());
        assert_eq!(
            hosts.machine_transport("win").unwrap().remote_shell(),
            crate::transport::vocab::RemoteShell::Other
        );
    }

    #[test]
    fn reconcile_keeps_local_when_the_fresh_roster_fails_to_name_it() {
        // A roster resolution whose local mux probe answered nothing names no `local`
        // machine at all. That probe result is a verdict on which muxes are installed,
        // never on whether this machine exists, so the standing local hosts must survive
        // it instead of being reaped on the re-scan.
        let mut hosts = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &["psmux".to_string(), "zellij".to_string()],
            std::path::Path::new("/x"),
            None,
        );
        let no_local = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &[],
            std::path::Path::new("/x"),
            None,
        );
        let delta = hosts.reconcile(no_local);
        assert!(
            delta.removed.is_empty(),
            "local must not be reaped: {delta:?}"
        );
        assert!(hosts.get("local:psmux").is_some(), "local:psmux survives");
        assert!(hosts.get("local:zellij").is_some(), "local:zellij survives");
    }

    #[test]
    fn reconcile_does_not_add_a_second_spelling_of_a_served_local_pair() {
        // The local id is qualified from how many muxes the probe reported, so a partial
        // probe can name `local` (bare) for psmux while `local:psmux` already serves it.
        // The second spelling must not be added as a duplicate card.
        let mut hosts = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &["psmux".to_string(), "zellij".to_string()],
            std::path::Path::new("/x"),
            None,
        );
        let fresh = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &["psmux".to_string()],
            std::path::Path::new("/x"),
            None,
        );
        let delta = hosts.reconcile(fresh);
        assert!(delta.removed.is_empty(), "no host dropped: {delta:?}");
        assert!(
            delta.added.is_empty(),
            "no duplicate spelling added: {delta:?}"
        );
        assert!(
            hosts.get("local").is_none(),
            "the bare `local` id is not added"
        );
        assert!(hosts.get("local:psmux").is_some());
        assert!(hosts.get("local:zellij").is_some());
    }

    #[test]
    fn machine_serves_asks_by_machine_and_mux_not_by_id() {
        // The discovery add path asks this before adding, and it must see through the id
        // spelling: `prod` (bare) serves tmux just as `prod:tmux` would.
        let cfg = tmux_on(&["prod"]);
        let hosts = Hosts::build(
            &cfg,
            &["prod".to_string()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        assert!(
            hosts.machine_serves("prod", "tmux"),
            "the bare id serves it"
        );
        assert!(!hosts.machine_serves("prod", "zellij"));
        assert!(!hosts.machine_serves("other", "tmux"), "machine-scoped");
    }

    #[test]
    fn build_puts_local_first_then_ssh_hosts_in_order() {
        let cfg = tmux_on(&["prod", "db"]);
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let hosts = Hosts::build(
            &cfg,
            &aliases,
            &[],
            "linux",
            &local(),
            std::path::Path::new("/home/u/.xmux"),
            None,
        );
        assert_eq!(
            hosts.ids(),
            &["local".to_string(), "prod".to_string(), "db".to_string()]
        );
        assert!(!hosts.get("local").unwrap().transport.is_remote());
        let prod = hosts.get("prod").unwrap();
        assert!(prod.transport.is_remote());
        assert_eq!(prod.transport.host_id(), "prod");
    }

    #[test]
    fn build_local_socket_threads_into_the_transport() {
        let cfg = Config::default();
        let hosts = Hosts::build(
            &cfg,
            &[],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            Some("/tmp/tmux-1000/work".into()),
        );
        // The socket is observable as the `-S <socket>` the transport injects.
        let command = hosts
            .get("local")
            .unwrap()
            .transport
            .exec_argv(false, &["tmux".to_string(), "list-sessions".to_string()]);
        assert!(
            command
                .windows(2)
                .any(|w| w == ["-S".to_string(), "/tmp/tmux-1000/work".to_string()]),
            "socket threads into the transport as -S: {command:?}"
        );
    }

    #[test]
    fn get_mut_and_iter_mut_reach_every_host() {
        let cfg = tmux_on(&["prod"]);
        let mut hosts = Hosts::build(
            &cfg,
            &["prod".to_string()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        assert!(hosts.get_mut("prod").is_some());
        assert!(hosts.get_mut("absent").is_none());
        assert_eq!(hosts.iter_mut().count(), 2, "local + prod");
    }

    #[test]
    fn apply_exited_clears_tty_and_marks_unreachable() {
        let mut hosts = Hosts::build(
            &tmux_on(&["jup"]),
            &["jup".to_string()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        hosts
            .get_mut("jup")
            .unwrap()
            .record_display_tty(Some("/dev/pts/9".into()));
        hosts.apply_host_event(&HostEvent::Exited {
            host: "jup".into(),
            reason: None,
            detached: false,
        });
        let h = hosts.get("jup").unwrap();
        assert!(
            h.display_tty.0.is_none(),
            "death clears the tty so no switch-client targets it"
        );
        assert_eq!(h.liveness, Liveness::Unreachable);
    }

    #[test]
    fn apply_connected_marks_live() {
        let mut hosts = Hosts::build(
            &tmux_on(&["jup"]),
            &["jup".to_string()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        hosts.apply_host_event(&HostEvent::Connected {
            host: "jup".into(),
            sessions: vec![],
        });
        assert_eq!(hosts.get("jup").unwrap().liveness, Liveness::Live);
    }

    #[test]
    fn build_orders_local_then_ssh_aliases_then_config_only_machines() {
        // Local first, then ssh specs in config order (ssh-config aliases, then
        // config-only machines). The cards `State` is seeded with lead with these ids, and
        // the published hosts list them in the same order.
        // A config-only machine (declared in config.toml, not ssh-config) with a mux override.
        let mut cfg = tmux_on(&["prod", "db"]);
        cfg.machines.push(crate::provision::config::MachineConfig {
            ssh: "cfgonly".into(),
            mux: "psmux".into(),
        });
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let os = "linux";
        let dir = std::path::Path::new("/home/u/.xmux");
        let hosts = Hosts::build(&cfg, &aliases, &[], os, &local(), dir, None);
        let host_order: Vec<String> = hosts.def_list().into_iter().map(|s| s.alias).collect();
        assert_eq!(
            hosts.ids(),
            host_order.as_slice(),
            "the published hosts follow the registry's order"
        );
        assert_eq!(
            host_order,
            vec![
                "local".to_string(),
                "prod".to_string(),
                "db".to_string(),
                "cfgonly".to_string(),
            ],
            "local first, ssh-config aliases in order, then config-only machines"
        );
    }

    #[test]
    fn build_appends_wsl_distributions_after_the_ssh_hosts() {
        // The WSL implementation has to survive as a transport: the ids an existing install
        // had keep their positions, and the new ones follow.
        let mut cfg = tmux_on(&["prod"]);
        cfg.wsl.push(crate::provision::config::WslConfig {
            distro: "Ubuntu-24.04".into(),
            mux: "tmux".into(),
        });
        let aliases = vec!["prod".to_string()];
        let distros = vec!["wsl.Ubuntu-24.04".to_string()];
        let dir = std::path::Path::new("/x");
        let hosts = Hosts::build(&cfg, &aliases, &distros, "windows", &local(), dir, None);
        let src_order: Vec<String> = hosts.def_list().into_iter().map(|s| s.alias).collect();
        assert_eq!(
            src_order,
            vec![
                "local".to_string(),
                "prod".to_string(),
                "wsl.Ubuntu-24.04".to_string(),
            ]
        );
        assert_eq!(hosts.ids(), src_order.as_slice());
        let wsl = hosts
            .get("wsl.Ubuntu-24.04")
            .expect("the distribution's host");
        assert!(
            !wsl.transport.is_remote(),
            "a distro on this box is not remote"
        );
        assert!(wsl.transport.runs_through_shell());
        let command = wsl
            .transport
            .exec_argv(false, &["tmux".to_string(), "list-sessions".to_string()]);
        assert_eq!(command.program(), "wsl.exe");
    }

    #[test]
    fn apply_event_for_unknown_host_is_a_noop() {
        let mut hosts = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        // No "ghost" host: routing an event to an id not in the map changes nothing.
        hosts.apply_host_event(&HostEvent::Connected {
            host: "ghost".into(),
            sessions: vec![],
        });
        assert!(hosts.get("ghost").is_none());
    }
    #[test]
    fn a_local_zellij_host_lists_sessions_without_a_socket_flag() {
        // End to end on the registry side: the command the scan runs carries no `-S`, so
        // the listing reaches zellij's own argument parsing instead of dying before it.
        let h = host_for(
            crate::session::LOCAL_MACHINE,
            "zellij",
            "local:zellij".to_string(),
            "linux",
            std::path::Path::new("/tmp/xmux"),
            Some("/tmp/psmux-1/default".to_string()),
        );
        let cmd = h.list_sessions_command();
        assert!(
            !cmd.iter().any(|a| a == "-S"),
            "no socket flag reaches zellij: {cmd:?}"
        );

        // And the tmux implementation keeps addressing the server it was given.
        let t = host_for(
            crate::session::LOCAL_MACHINE,
            "psmux",
            "local:psmux".to_string(),
            "linux",
            std::path::Path::new("/tmp/xmux"),
            Some("/tmp/psmux-1/default".to_string()),
        );
        let cmd = t.list_sessions_command();
        assert!(
            cmd.windows(2)
                .any(|w| w[0] == "-S" && w[1] == "/tmp/psmux-1/default"),
            "psmux still targets its server: {cmd:?}"
        );
    }

    /// The aliases the registry publishes, in order.
    fn published(hosts: &Hosts) -> Vec<String> {
        hosts.defs().list().into_iter().map(|s| s.alias).collect()
    }

    #[test]
    fn a_host_found_at_runtime_is_published_to_every_holder_of_the_set() {
        // The set is handed to the off-loop operations once, at construction. A mux that
        // answers later is added to the registry alone, and it has to reach the set they
        // already hold, or it paints and scans but refuses every operation.
        let cfg = Config::default();
        let mut hosts = Hosts::build(
            &cfg,
            &["win".to_string()],
            &[],
            "windows",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        let held = hosts.defs();
        assert_eq!(published(&hosts), vec!["local".to_string()], "precondition");
        let host = hosts
            .discovered_host("win", "psmux", "win")
            .expect("the host the registry reaches");
        hosts.insert(host);
        let def = held.get("win").expect("the set handed out earlier has it");
        assert_eq!(def.binary, "psmux");
        assert!(matches!(
            def.kind,
            crate::transport::MachineKind::Ssh { ref id, ref alias, .. } if id == "win" && alias == "win"
        ));
        assert_eq!(
            def.host().transport.host_id(),
            hosts.get("win").unwrap().transport.host_id(),
            "the published host reaches the machine as the loop's host does"
        );
    }

    #[test]
    fn reconcile_publishes_what_it_adds_and_drops() {
        let mut hosts = built(&["prod", "stage"]);
        let held = hosts.defs();
        hosts.reconcile(built(&["prod", "db"]));
        assert_eq!(
            held.list().into_iter().map(|s| s.alias).collect::<Vec<_>>(),
            vec!["local".to_string(), "prod".to_string(), "db".to_string()]
        );
    }

    #[test]
    fn a_local_zellij_host_is_published_without_the_tmux_socket() {
        // The socket comes from `$TMUX`, which is set whenever xmux runs inside a mux -
        // the normal case. Handing it to zellij made every local zellij host fail its
        // listing on argument parsing, so it never reaches the machine at all.
        let sock = Some("/tmp/psmux-1/default".to_string());
        let hosts = Hosts::build(
            &Config::default(),
            &[],
            &[],
            "linux",
            &["psmux".to_string(), "zellij".to_string()],
            std::path::Path::new("/tmp/xmux"),
            sock.clone(),
        );
        let z = hosts.def("local:zellij").expect("the zellij host");
        assert_eq!(z.kind.local_socket(), None, "no socket reaches zellij");
        let p = hosts.def("local:psmux").expect("the psmux host");
        assert_eq!(p.kind.local_socket(), sock, "psmux targets its server");
    }
    #[test]
    fn changed_mux_config_replaces_the_old_host() {
        let mut hosts = built(&["prod"]);
        hosts.for_each_transport_of("prod", |transport| {
            transport.set_login(crate::transport::Login {
                address: Some("192.0.2.10".into()),
                port: Some(2222),
                user: Some("alice".into()),
            })
        });
        let mut cfg = tmux_on(&["prod"]);
        cfg.machines[0].mux = "zellij".into();
        let fresh = Hosts::build(
            &cfg,
            &["prod".into()],
            &[],
            "linux",
            &local(),
            std::path::Path::new("/x"),
            None,
        );
        let delta = hosts.reconcile(fresh);
        assert_eq!(hosts.get("prod").unwrap().mux.bin(), "zellij");
        assert_eq!(delta.removed, ["prod"]);
        assert_eq!(delta.added, ["prod"]);
        let argv = hosts
            .get("prod")
            .unwrap()
            .transport
            .raw_shell_argv("true")
            .unwrap();
        for option in ["HostName=192.0.2.10", "Port=2222", "User=alice"] {
            assert!(argv.iter().any(|arg| arg == option), "{argv:?}");
        }
    }

    #[test]
    fn shortened_mux_config_keeps_only_the_requested_pair() {
        let build = |mux| {
            let mut cfg = tmux_on(&["prod"]);
            cfg.machines[0].mux = mux;
            Hosts::build(
                &cfg,
                &["prod".into()],
                &[],
                "linux",
                &local(),
                std::path::Path::new("/x"),
                None,
            )
        };
        let mut hosts = build(vec!["tmux", "zellij"].into());
        let delta = hosts.reconcile(build("zellij".into()));
        assert_eq!(delta.removed, ["prod:tmux"]);
        assert!(hosts.machine_serves("prod", "zellij"));
        assert!(!hosts.machine_serves("prod", "tmux"));
        assert!(delta.added.is_empty());
    }
    #[test]
    fn changed_local_mux_config_uses_its_own_socket_policy() {
        let build = |bin: &str| {
            let mut cfg = Config::default();
            cfg.local.mux = bin.into();
            Hosts::build(
                &cfg,
                &[],
                &[],
                "linux",
                &[bin.into()],
                std::path::Path::new("/x"),
                Some("/tmp/tmux.sock".into()),
            )
        };
        let mut hosts = build("tmux");
        hosts.reconcile(build("zellij"));
        let host = hosts.get("local").unwrap();
        assert_eq!(host.mux.bin(), "zellij");
        assert_eq!(host.transport.machine_kind().local_socket(), None);
    }
}
