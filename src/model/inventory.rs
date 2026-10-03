//! Inventory values shared by provisioning, state, and the nav row model.

use crate::session::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Blocked,
    Unreachable,
}

impl FailureKind {
    pub fn from_error(error: &str) -> Self {
        if crate::transport::diagnostic::requires_login(error) {
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

impl Group {
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
