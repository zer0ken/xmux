//! `Hosts`: the app loop's runtime host registry — every host keyed by id, in display
//! order (local first). The single owner of each machine's `Host`, so display and host
//! management cannot disagree about which machines exist.

use std::collections::HashMap;

use crate::config::Config;
use crate::model::{Host, Liveness};
use crate::mux::for_binary;
use crate::session::LOCAL_SOURCE;

/// Every host, keyed by host id, in display order (local first). The single owner of
/// each machine's `Host` for the app loop, so a host is present here or nowhere.
#[derive(Default)]
pub struct Hosts {
    order: Vec<String>,
    map: HashMap<String, Host>,
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
        let id = host.id().to_string();
        if !self.map.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.map.insert(id, host);
    }

    /// Assembles the hosts for a config: this box's hosts first (one per entry of the
    /// RESOLVED `local_muxes`, its socket from `$TMUX`), then each ssh host in order,
    /// then each WSL distribution. Mirrors `source::build` but yields owning `Host`s.
    /// `xmux_dir` seeds each ssh transport's ControlMaster socket path
    /// (`cm-<alias>.sock`), exactly as `source::build` does.
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

        // One host per (machine, mux): this box contributes one for each mux it serves.
        // The list is the one `Env` resolved, so these ids match the source ids.
        let qualified = local_muxes.len() > 1;
        for bin in local_muxes {
            let id = crate::session::source_id(LOCAL_SOURCE, bin, qualified);
            hosts.insert(host_for(
                LOCAL_SOURCE,
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
            if spec.alias == LOCAL_SOURCE {
                continue; // "local" is reserved for this box's sources.
            }
            hosts.insert(host_for(
                &spec.alias,
                &spec.bin,
                spec.id,
                os,
                xmux_dir,
                local_socket.clone(),
            ));
        }
        hosts
    }

    /// Whether `machine` already serves a source running the mux binary `bin`. The
    /// discovery add path asks before adding, so a mux the machine was already
    /// configured (or assumed) to run is never duplicated under a second id.
    pub fn machine_serves(&self, machine: &str, bin: &str) -> bool {
        self.order.iter().any(|id| {
            crate::session::machine_of(id) == machine
                && self.map.get(id).is_some_and(|h| h.mux.bin() == bin)
        })
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.map.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.map.get_mut(id)
    }

    /// Host ids in display order (local first) — the render projection iterates these.
    pub fn ids(&self) -> &[String] {
        &self.order
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Host> {
        self.map.values_mut()
    }

    /// Routes one `HostEvent` (the metadata reader's output) to the host it names,
    /// folding Host-owned liveness state. The sessions carried by `Connected`/`Inventory`
    /// are folded into `model::Host.inventory` by the run loop (or via `Host::enumerate`);
    /// this sets liveness. An unknown host id is a no-op — there is no second registry to
    /// grow a ghost host.
    pub fn apply_host_event(&mut self, ev: &crate::host::HostEvent) {
        use crate::host::HostEvent::*;
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
            // Change/window/focus events drive refetch + selection follow in the render
            // projection; they touch no Host-owned field here.
            Changed { .. } | ActiveWindowChanged { .. } | Focus { .. } => {}
            // The tty-matched reap of xmux's own display attach is the supervisor's job (it
            // owns the registry + the recover-from-detach rearm); the Hosts map holds no
            // per-attach state to fold here. `ClientSessionChanged` is the same: the tty match
            // + display-belief sync + nav follow run in the supervisor's effect handler.
            ClientDetached { .. } | ClientSessionChanged { .. } => {}
            // The -CC `list-clients` probe resolved xmux's display-client tty (or None if
            // the display attach has not registered yet). Record it so a session switch is
            // an in-place `switch-client -c <tty>`; None clears any stale tty.
            DisplayTty { host, tty } => {
                if let Some(h) = self.get_mut(host) {
                    h.record_display_tty(tty.clone());
                }
            }
            // Poll-host data carriers (enumeration results), the detection probe, and a
            // machine's mux-discovery answer. Their sessions/mux/source set are applied by
            // the caller (apply_source_result / apply_scan_result / the discovery-add
            // effect); they fold no Host-owned liveness here. A discovery answer names a
            // MACHINE, not a host in this map, so it could not route here anyway.
            Scanned { .. } | Sessions { .. } | Panes { .. } | MuxesFound { .. } => {}
        }
    }
}

/// One [`Host`] for the mux binary `bin` on `machine`, answering as the source `id`.
/// The transport comes from [`crate::machine::kind_for`], the one place a machine's
/// construction data is assembled, so this host and the matching `Source` agree.
pub fn host_for(
    machine: &str,
    bin: &str,
    id: String,
    os: &str,
    xmux_dir: &std::path::Path,
    local_socket: Option<String>,
) -> Host {
    let kind = crate::machine::kind_for(machine, id, os, xmux_dir, local_socket);
    Host::new(kind.transport(), for_binary(bin))
}

#[cfg(test)]
mod tests {
    /// The resolved local mux list a test builds with: what `Config::default()` on a
    /// unix box resolves to, so these tests pin host ORDER and ids, not discovery.
    fn local() -> Vec<String> {
        vec!["tmux".to_string()]
    }

    use super::*;
    use crate::host::HostEvent;
    use crate::machine::Transport;
    use crate::model::{Liveness, ServerModel};

    #[test]
    fn default_and_new_are_empty() {
        assert!(Hosts::default().ids().is_empty());
        assert!(Hosts::new().ids().is_empty());
    }

    #[test]
    fn insert_keys_on_host_id_and_appends_order_once() {
        let mut hosts = Hosts::default();
        let local = Host::new(crate::machine::local(None), for_binary("tmux"));
        hosts.insert(local);
        assert_eq!(hosts.ids(), &["local".to_string()]);
        // Re-inserting the same id replaces in place, does not duplicate the order.
        let local2 = Host::new(crate::machine::local(None), for_binary("psmux"));
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

    #[test]
    fn machine_serves_asks_by_machine_and_mux_not_by_id() {
        // The discovery add path asks this before adding, and it must see through the id
        // spelling: `prod` (bare) serves tmux just as `prod:tmux` would.
        let cfg = Config::default();
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
        let cfg = Config::default();
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
        let (_n, args) = hosts
            .get("local")
            .unwrap()
            .transport
            .exec_argv(false, &["tmux".to_string(), "list-sessions".to_string()]);
        assert!(
            args.windows(2)
                .any(|w| w == ["-S".to_string(), "/tmp/tmux-1000/work".to_string()]),
            "socket threads into the transport as -S: {args:?}"
        );
    }

    #[test]
    fn get_mut_and_iter_mut_reach_every_host() {
        let cfg = Config::default();
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
            &Config::default(),
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
            &Config::default(),
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
    fn build_ids_match_source_build_order_for_multi_host_config() {
        // The single runtime registry's projection (`Hosts::ids`) must list the SAME
        // hosts in the SAME order as the `source::build` list it replaces: local first,
        // then ssh specs in config order (ssh-config aliases, then config-only hosts).
        // Seeding `State` from `hosts.ids()` is therefore byte-identical to the retired
        // `env.srcs` seed — a reordered or dropped host would be a live regression.
        // A config-only host (declared in config.toml, not ssh-config) with a mux override.
        let cfg = Config {
            hosts: vec![crate::config::HostConfig {
                ssh: "cfgonly".into(),
                mux: "psmux".into(),
            }],
            ..Config::default()
        };
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let os = "linux";
        let dir = std::path::Path::new("/home/u/.xmux");
        let hosts = Hosts::build(&cfg, &aliases, &[], os, &local(), dir, None);
        let srcs = crate::source::build(&cfg, &aliases, &[], os, &local(), dir, None);
        let src_order: Vec<String> = srcs.iter().map(|s| s.alias.clone()).collect();
        assert_eq!(
            hosts.ids(),
            src_order.as_slice(),
            "the host registry projection must equal the source-derived order"
        );
        assert_eq!(
            src_order,
            vec![
                "local".to_string(),
                "prod".to_string(),
                "db".to_string(),
                "cfgonly".to_string(),
            ],
            "local first, ssh-config aliases in order, then config-only hosts"
        );
    }

    #[test]
    fn build_appends_wsl_distributions_after_the_ssh_hosts() {
        // The registry projection and the source list must agree on the WSL family too,
        // and the family has to survive as a transport: the ids an existing install had
        // keep their positions, and the new ones follow.
        let cfg = Config::default();
        let aliases = vec!["prod".to_string()];
        let distros = vec!["wsl.Ubuntu-24.04".to_string()];
        let dir = std::path::Path::new("/x");
        let hosts = Hosts::build(&cfg, &aliases, &distros, "windows", &local(), dir, None);
        let srcs = crate::source::build(&cfg, &aliases, &distros, "windows", &local(), dir, None);
        let src_order: Vec<String> = srcs.iter().map(|s| s.alias.clone()).collect();
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
        let (name, _args) = wsl
            .transport
            .exec_argv(false, &["tmux".to_string(), "list-sessions".to_string()]);
        assert_eq!(name, "wsl.exe");
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
}
