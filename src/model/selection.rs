//! [`Selection`] — the canonical display target (`host`/`session`), the
//! single source of truth the display reads. A pure domain value with no dependency on
//! the orchestration (`app`) layer, so `model`/`state`/`driver` no longer import upward
//! to reach it. Deriving a `Selection` from a ui target lives in `app` (it depends on the
//! ui `TerminalViewTarget`); the value itself lives here.

/// The canonical selection — the single source of truth the display reads. The
/// `Switcher` owns the tree + selection; the app commits the selection's target into
/// this struct, and the render, input routing, and spinner all key off it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub host: String,
    /// Empty ⇒ no terminal view (selection on a host/loading row).
    pub session: String,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.session.is_empty()
    }
}

/// One level of the machine / host / session hierarchy, the thing a view screen is about
/// and a nav selection names. A host is one mux on a machine, and a
/// session lives in a host, so every node but a machine has exactly one parent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Machine(String),
    Host(String),
    Session(crate::session::Address),
}

impl Node {
    /// The node one level up, `None` for a machine.
    pub fn parent(&self) -> Option<Node> {
        match self {
            Node::Machine(_) => None,
            Node::Host(host) => Some(Node::Machine(crate::session::machine_of(host).into())),
            Node::Session(address) => Some(Node::Host(address.host.clone())),
        }
    }

    /// The machine this node belongs to.
    pub fn machine(&self) -> &str {
        match self {
            Node::Machine(machine) => machine,
            Node::Host(host) => crate::session::machine_of(host),
            Node::Session(address) => crate::session::machine_of(&address.host),
        }
    }

    /// The host this node belongs to, `None` for a machine.
    pub fn host(&self) -> Option<&str> {
        match self {
            Node::Machine(_) => None,
            Node::Host(host) => Some(host),
            Node::Session(address) => Some(&address.host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_walks_up_session_host_machine() {
        let session = Node::Session(crate::session::Address::new("db:tmux", "pg"));
        let host = session.parent().unwrap();
        assert_eq!(host, Node::Host("db:tmux".into()));
        let machine = host.parent().unwrap();
        assert_eq!(machine, Node::Machine("db".into()));
        assert_eq!(machine.parent(), None);
        assert_eq!(session.machine(), "db");
        assert_eq!(host.host(), Some("db:tmux"));
        assert_eq!(machine.host(), None);
    }

    #[test]
    fn selection_is_empty_only_without_a_session() {
        assert!(Selection::default().is_empty());
        let sel = Selection {
            host: "jup".into(),
            session: "api".into(),
        };
        assert!(!sel.is_empty());
    }
}
