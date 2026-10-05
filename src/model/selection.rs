//! [`Selection`] — the canonical display target (`source`/`session`), the
//! single source of truth the display reads. A pure domain value with no dependency on
//! the orchestration (`app`) layer, so `model`/`state`/`driver` no longer import upward
//! to reach it. Deriving a `Selection` from a ui target lives in `app` (it depends on the
//! ui `TerminalViewTarget`); the value itself lives here.

/// The canonical selection — the single source of truth the display reads. The
/// `Switcher` owns the tree + selection; the app commits the selection's target into
/// this struct, and the render, input routing, and spinner all key off it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub source: String,
    /// Empty ⇒ no terminal view (selection on a host/loading row).
    pub session: String,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.session.is_empty()
    }
}

/// One level of the host / source / session hierarchy, the thing a view screen is about
/// and a nav selection names. A host is a machine, a source is one mux on it, and a
/// session lives in a source, so every node but a host has exactly one parent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Host(String),
    Source(String),
    Session(crate::session::Address),
}

impl Node {
    /// The node one level up, `None` for a host.
    pub fn parent(&self) -> Option<Node> {
        match self {
            Node::Host(_) => None,
            Node::Source(source) => Some(Node::Host(crate::session::machine_of(source).into())),
            Node::Session(address) => Some(Node::Source(address.source.clone())),
        }
    }

    /// The host this node belongs to.
    pub fn machine(&self) -> &str {
        match self {
            Node::Host(machine) => machine,
            Node::Source(source) => crate::session::machine_of(source),
            Node::Session(address) => crate::session::machine_of(&address.source),
        }
    }

    /// The source this node belongs to, `None` for a host.
    pub fn source(&self) -> Option<&str> {
        match self {
            Node::Host(_) => None,
            Node::Source(source) => Some(source),
            Node::Session(address) => Some(&address.source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_walks_up_session_source_host() {
        let session = Node::Session(crate::session::Address::new("db:tmux", "pg"));
        let source = session.parent().unwrap();
        assert_eq!(source, Node::Source("db:tmux".into()));
        let host = source.parent().unwrap();
        assert_eq!(host, Node::Host("db".into()));
        assert_eq!(host.parent(), None);
        assert_eq!(session.machine(), "db");
        assert_eq!(source.source(), Some("db:tmux"));
        assert_eq!(host.source(), None);
    }

    #[test]
    fn selection_is_empty_only_without_a_session() {
        assert!(Selection::default().is_empty());
        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
        };
        assert!(!sel.is_empty());
    }
}
