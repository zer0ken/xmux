//! Inventory values shared by provisioning, state, and the nav row model.

use crate::session::Session;

/// How a source's failure classifies. Whether ssh refused for a reason a login can answer
/// is the transport's ssh diagnostic, read here once; presentation filtering and row
/// construction consume this typed result and never classify a failure themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Blocked,
    ListFailed,
    Unreachable,
}

impl FailureKind {
    pub fn from_error(error: &str) -> Self {
        if error.starts_with("invalid ") && error.contains(" session listing:") {
            Self::ListFailed
        } else if error.starts_with("logged out;")
            || crate::transport::diagnostic::requires_login(error)
        {
            Self::Blocked
        } else {
            Self::Unreachable
        }
    }
}

/// The sessions of one source. A non-`None` `err` means the host was
/// unreachable, in which case `sessions` carries no meaning.
#[derive(Debug, Clone)]
pub struct Group {
    pub source: String,
    pub err: Option<String>,
    pub sessions: Vec<Session>,
}

/// Returns groups with `session` placed in the group whose source matches its source,
/// replacing any existing session of the same name in place (dedup by name) or, when
/// new, appending it at the group's end. It does NOT sort here: a session created
/// mid-session is placed by the next rebuild's deterministic order, not by this
/// mutation. If no group has the source, a new group is appended. Inputs are not
/// mutated.
pub fn add_session(groups: &[Group], session: Session) -> Vec<Group> {
    let mut out = groups.to_vec();
    for group in &mut out {
        if group.source != session.source {
            continue;
        }
        let mut replaced = false;
        for existing in &mut group.sessions {
            if existing.name == session.name {
                *existing = session.clone();
                replaced = true;
            }
        }
        if !replaced {
            group.sessions.push(session);
        }
        return out;
    }
    out.push(Group {
        source: session.source.clone(),
        err: None,
        sessions: vec![session],
    });
    out
}

impl Group {
    pub fn failure(&self) -> Option<FailureKind> {
        self.err.as_deref().map(FailureKind::from_error)
    }
}

/// One machine on the roster, the level its hosts belong to. It holds what the machine
/// answered as a whole, which is what its card and screen state while no host of it is
/// known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    /// Why the machine itself could not be asked: its reachability probe or its mux
    /// discovery failed.
    pub err: Option<String>,
    /// The machine answered that it serves no mux xmux supports, so it has nothing to
    /// show.
    pub muxless: bool,
}

impl Machine {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn failure(&self) -> Option<FailureKind> {
        self.err.as_deref().map(FailureKind::from_error)
    }
}

/// Orders sessions in place by name ascending. The sort is stable so sessions
/// with equal names keep their original relative order.
pub fn sort_by_name(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(source: &str, name: &str) -> Session {
        Session {
            source: source.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn sort_by_name_orders() {
        let mut sessions = vec![
            session("local", "beta"),
            session("local", "alpha"),
            session("local", "gamma"),
            session("local", "delta"),
        ];
        sort_by_name(&mut sessions);
        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "delta", "gamma"]);
    }

    #[test]
    fn sort_by_name_stable_for_equal_names() {
        let mut sessions = vec![session("h1", "x"), session("h2", "x"), session("h3", "x")];
        sort_by_name(&mut sessions);
        let sources: Vec<&str> = sessions.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(sources, vec!["h1", "h2", "h3"]);
    }

    #[test]
    fn group_classifies_failures_for_domain_callers() {
        let group = |err: &str| Group {
            source: "prod".into(),
            err: Some(err.into()),
            sessions: Vec::new(),
        };

        assert_eq!(
            group("dev@prod: Permission denied (publickey,password).").failure(),
            Some(FailureKind::Blocked)
        );
        assert_eq!(
            group("ssh: connect to host prod port 22: Connection refused").failure(),
            Some(FailureKind::Unreachable)
        );
        assert_eq!(
            format!(
                "{:?}",
                group("invalid tuios session listing: expected value at line 1 column 1").failure()
            ),
            "Some(ListFailed)"
        );
        assert_eq!(
            Group {
                source: "prod".into(),
                err: None,
                sessions: Vec::new(),
            }
            .failure(),
            None
        );
    }
}
