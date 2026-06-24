//! `Hosts`: every host keyed by id, in display order (local first). The one owner —
//! replaces `HostManager`'s clients map (host.rs:553) and `Env`'s srcs/by_alias
//! (env.rs:28/29), so a host cannot exist in one registry but not the other.

use std::collections::HashMap;

use crate::config::Config;
use crate::model::{for_binary, Host, Transport};
use crate::session::LOCAL_SOURCE;

/// Every host, keyed by host id, in display order (local first). The one owner —
/// replaces `HostManager`'s clients map (host.rs:553) and `Env`'s srcs/by_alias
/// (env.rs:28/29), so a host cannot exist in one registry but not the other.
#[derive(Default)]
pub struct Hosts {
    order: Vec<String>,
    map: HashMap<String, Host>,
}

impl Hosts {
    /// An empty registry (same as `Default`; both pinned because Phase 4 tests call
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

    /// Assembles the hosts for a config: the local host first (its mux from
    /// `Config::local_bin`, its socket from `$TMUX`), then each ssh host in order.
    /// Mirrors `source::build` (source.rs:460) but yields owning `Host`s. `xmux_dir`
    /// seeds each ssh transport's ControlMaster socket path (`cm-<alias>.sock`),
    /// exactly as `source::build` did.
    pub fn build(
        cfg: &Config,
        ssh_aliases: &[String],
        os: &str,
        xmux_dir: &std::path::Path,
        local_socket: Option<String>,
    ) -> Hosts {
        let mut hosts = Hosts::default();

        let local_bin = cfg.local_bin(os);
        hosts.insert(Host::new(
            Transport::Local { socket: local_socket },
            for_binary(&local_bin),
        ));

        for spec in cfg.host_specs(ssh_aliases) {
            if spec.alias == LOCAL_SOURCE {
                continue; // "local" is reserved for the local mux source.
            }
            let control_path = xmux_dir
                .join(format!("cm-{}.sock", spec.alias))
                .to_string_lossy()
                .into_owned();
            hosts.insert(Host::new(
                Transport::Ssh {
                    alias: spec.alias,
                    control_path,
                    os: os.to_string(),
                },
                for_binary(&spec.bin),
            ));
        }
        hosts
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.map.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.map.get_mut(id)
    }

    /// Host ids in display order (local first) — replaces `Ops::sources`
    /// (switcher.rs:121) and `Env.srcs` iteration for the render projection.
    pub fn ids(&self) -> &[String] {
        &self.order
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Host> {
        self.map.values_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServerModel;

    #[test]
    fn default_and_new_are_empty() {
        assert!(Hosts::default().ids().is_empty());
        assert!(Hosts::new().ids().is_empty());
    }

    #[test]
    fn insert_keys_on_host_id_and_appends_order_once() {
        let mut hosts = Hosts::default();
        let local = Host::new(Transport::Local { socket: None }, for_binary("tmux"));
        hosts.insert(local);
        assert_eq!(hosts.ids(), &["local".to_string()]);
        // Re-inserting the same id replaces in place, does not duplicate the order.
        let local2 = Host::new(Transport::Local { socket: None }, for_binary("psmux"));
        hosts.insert(local2);
        assert_eq!(hosts.ids(), &["local".to_string()], "same id does not duplicate order");
        assert_eq!(hosts.get("local").unwrap().mux.server_model(), ServerModel::PerSession, "psmux replaced tmux");
    }

    #[test]
    fn build_puts_local_first_then_ssh_hosts_in_order() {
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let hosts = Hosts::build(&cfg, &aliases, "linux", std::path::Path::new("/home/u/.xmux"), None);
        assert_eq!(hosts.ids(), &["local".to_string(), "prod".to_string(), "db".to_string()]);
        assert!(matches!(hosts.get("local").unwrap().transport, Transport::Local { .. }));
        match &hosts.get("prod").unwrap().transport {
            Transport::Ssh { alias, .. } => assert_eq!(alias, "prod"),
            _ => panic!("prod must be an ssh transport"),
        }
    }

    #[test]
    fn build_local_socket_threads_into_the_transport() {
        let cfg = Config::default();
        let hosts = Hosts::build(&cfg, &[], "linux", std::path::Path::new("/x"), Some("/tmp/tmux-1000/work".into()));
        match &hosts.get("local").unwrap().transport {
            Transport::Local { socket } => assert_eq!(socket.as_deref(), Some("/tmp/tmux-1000/work")),
            _ => panic!("local transport"),
        }
    }

    #[test]
    fn get_mut_and_iter_mut_reach_every_host() {
        let cfg = Config::default();
        let mut hosts = Hosts::build(&cfg, &["prod".to_string()], "linux", std::path::Path::new("/x"), None);
        assert!(hosts.get_mut("prod").is_some());
        assert!(hosts.get_mut("absent").is_none());
        assert_eq!(hosts.iter_mut().count(), 2, "local + prod");
    }
}
