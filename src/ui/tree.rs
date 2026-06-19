//! The pure tree-model logic for the session switcher: a slice of [`Group`]s (one
//! per source) each carrying its sessions ordered by recency. The functions here
//! are side-effect-free transforms over that model; the interactive ratatui
//! rendering is layered on top separately.

use crate::session::Session;

/// The sessions of one source. A non-`None` `err` means the host was
/// unreachable, in which case `sessions` carries no meaning.
#[derive(Debug, Clone)]
pub struct Group {
    pub source: String,
    pub err: Option<String>,
    pub sessions: Vec<Session>,
}

/// Orders sessions in place with the most recently attached first
/// (`last_attached` descending), breaking ties by name ascending. The sort is
/// stable so sessions equal on both keys keep their original relative order.
pub fn sort_by_recency(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| {
        b.last_attached
            .cmp(&a.last_attached)
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Reports whether `pattern` is a case-insensitive subsequence of `s`: every
/// char of `pattern` appears in `s` in order, not necessarily contiguously. An
/// empty pattern always matches.
pub fn fuzzy_match(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    if p.is_empty() {
        return true;
    }
    let mut i = 0;
    for c in s.to_lowercase().chars() {
        if c == p[i] {
            i += 1;
            if i == p.len() {
                return true;
            }
        }
    }
    false
}

/// Keeps the groups whose source matches `pattern` or that have at least one
/// matching session, preserving group order. An empty pattern returns the input
/// unchanged. A reachable group whose source matches keeps all its sessions;
/// otherwise only the sessions whose address matches are kept. An unreachable
/// group (`err` set) is kept only when its source matches, since its sessions
/// carry no meaning. Inputs are never mutated.
pub fn filter_groups(groups: &[Group], pattern: &str) -> Vec<Group> {
    if pattern.is_empty() {
        return groups.to_vec();
    }
    let mut out = Vec::new();
    for g in groups {
        let source_match = fuzzy_match(pattern, &g.source);
        if g.err.is_some() {
            if source_match {
                out.push(g.clone());
            }
            continue;
        }
        if source_match {
            out.push(Group {
                source: g.source.clone(),
                err: None,
                sessions: g.sessions.clone(),
            });
            continue;
        }
        let kept: Vec<Session> = g
            .sessions
            .iter()
            .filter(|s| fuzzy_match(pattern, &s.address()))
            .cloned()
            .collect();
        if !kept.is_empty() {
            out.push(Group {
                source: g.source.clone(),
                err: None,
                sessions: kept,
            });
        }
    }
    out
}

/// Returns groups with `s` placed in the group whose source matches `s.source`,
/// replacing any existing session of the same name (dedup by name) and re-sorting
/// that group by recency. If no group has the source, a new group is appended.
/// Inputs are not mutated.
pub fn add_session(groups: &[Group], s: Session) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if g.source != s.source {
            continue;
        }
        let mut sessions = Vec::with_capacity(g.sessions.len() + 1);
        let mut replaced = false;
        for existing in &g.sessions {
            if existing.name == s.name {
                sessions.push(s.clone());
                replaced = true;
            } else {
                sessions.push(existing.clone());
            }
        }
        if !replaced {
            sessions.push(s.clone());
        }
        sort_by_recency(&mut sessions);
        g.sessions = sessions;
        return out;
    }
    out.push(Group {
        source: s.source.clone(),
        err: None,
        sessions: vec![s],
    });
    out
}

/// Returns groups with the session at `address` removed from its group. The
/// now-possibly-empty group is kept, since an empty reachable group is still a
/// valid create target. Inputs are not mutated.
pub fn remove_session(groups: &[Group], address: &str) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if let Some(j) = g.sessions.iter().position(|s| s.address() == address) {
            g.sessions.remove(j);
            return out;
        }
    }
    out
}

/// Orders host groups for display: the local source(s) pinned first (original
/// relative order), then remote groups by their most-recent session's
/// `last_attached` descending (a group with no sessions sorts last; ties by
/// source name ascending). Inputs are not mutated.
pub fn order_groups(groups: &[Group]) -> Vec<Group> {
    use crate::session::LOCAL_SOURCE;
    let mut local: Vec<Group> = Vec::new();
    let mut remote: Vec<Group> = Vec::new();
    for g in groups {
        if g.source == LOCAL_SOURCE {
            local.push(g.clone());
        } else {
            remote.push(g.clone());
        }
    }
    remote.sort_by(|a, b| {
        let am = a.sessions.iter().map(|s| s.last_attached).max();
        let bm = b.sessions.iter().map(|s| s.last_attached).max();
        // Some(max) before None; higher max first; ties by source name asc.
        bm.cmp(&am).then_with(|| a.source.cmp(&b.source))
    });
    local.into_iter().chain(remote).collect()
}

/// Returns groups with the session at `address` renamed to `new_name` and its
/// group re-sorted by recency. It is a no-op if no session matches. Inputs are
/// not mutated.
pub fn rename_session(groups: &[Group], address: &str, new_name: &str) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if let Some(j) = g.sessions.iter().position(|s| s.address() == address) {
            g.sessions[j].name = new_name.to_string();
            sort_by_recency(&mut g.sessions);
            return out;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(source: &str, name: &str, last: i64) -> Session {
        Session {
            source: source.into(),
            name: name.into(),
            last_attached: last,
            ..Default::default()
        }
    }

    fn sample_groups() -> Vec<Group> {
        vec![
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![
                    sess("jupiter00", "inference", 0),
                    sess("jupiter00", "training", 0),
                ],
            },
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "web", 0), sess("local", "db", 0)],
            },
            Group {
                source: "deadhost".into(),
                err: Some("dial: connection refused".into()),
                sessions: vec![sess("deadhost", "ghost", 0)],
            },
        ]
    }

    #[test]
    fn sort_by_recency_orders() {
        let mut in_ = vec![
            sess("local", "beta", 100),
            sess("local", "alpha", 200),
            sess("local", "gamma", 100),
            sess("local", "delta", 0),
        ];
        sort_by_recency(&mut in_);
        let names: Vec<&str> = in_.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma", "delta"]);
    }

    #[test]
    fn sort_by_recency_stable_for_equal_keys() {
        let mut in_ = vec![
            sess("h1", "x", 50),
            sess("h2", "x", 50),
            sess("h3", "x", 50),
        ];
        sort_by_recency(&mut in_);
        let srcs: Vec<&str> = in_.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(srcs, vec!["h1", "h2", "h3"]);
    }

    #[test]
    fn fuzzy_match_cases() {
        let cases: &[(&str, &str, bool)] = &[
            ("if", "jupiter00/inference", true),
            ("xyz", "abc", false),
            ("", "anything", true),
            ("", "", true),
            ("abc", "abc", true),
            ("abc", "a-b-c", true),
            ("cba", "abc", false),
            ("ABC", "xaybzc", true),
            ("abc", "XAYBZC", true),
            ("abcd", "abc", false),
            ("local", "local/web", true),
            ("web", "local/web", true),
        ];
        for &(pattern, s, want) in cases {
            assert_eq!(
                fuzzy_match(pattern, s),
                want,
                "fuzzy_match({pattern:?}, {s:?})"
            );
        }
    }

    #[test]
    fn filter_groups_empty_pattern_passthrough() {
        let in_ = sample_groups();
        let got = filter_groups(&in_, "");
        assert_eq!(got.len(), in_.len());
        for i in 0..in_.len() {
            assert_eq!(got[i].source, in_[i].source);
            assert_eq!(got[i].sessions.len(), in_[i].sessions.len());
        }
    }

    #[test]
    fn filter_groups_source_match_keeps_all_sessions() {
        let got = filter_groups(&sample_groups(), "jptr");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "jupiter00");
        assert_eq!(got[0].sessions.len(), 2);
    }

    #[test]
    fn filter_groups_session_only_match() {
        let got = filter_groups(&sample_groups(), "jupiter00/inference");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "jupiter00");
        assert_eq!(got[0].sessions.len(), 1);
        assert_eq!(got[0].sessions[0].name, "inference");
    }

    #[test]
    fn filter_groups_unreachable_kept_only_on_source_match() {
        let got = filter_groups(&sample_groups(), "dead");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "deadhost");
        assert!(got[0].err.is_some());

        let got2 = filter_groups(&sample_groups(), "ghost");
        assert!(got2.iter().all(|g| g.source != "deadhost"));
    }

    #[test]
    fn filter_groups_preserves_order() {
        let got = filter_groups(&sample_groups(), "e");
        let order: Vec<&str> = got.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(order, vec!["jupiter00", "local", "deadhost"]);
    }

    #[test]
    fn filter_groups_does_not_mutate_input() {
        let in_ = sample_groups();
        let orig_len = in_[0].sessions.len();
        let orig_first = in_[0].sessions[0].name.clone();
        let _ = filter_groups(&in_, "jupiter00/inference");
        assert_eq!(in_[0].sessions.len(), orig_len);
        assert_eq!(in_[0].sessions[0].name, orig_first);
    }

    #[test]
    fn add_session_new_group() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0)],
        }];
        let got = add_session(&groups, sess("remote", "build", 0));
        assert_eq!(got.len(), 2);
        let last = got.last().unwrap();
        assert_eq!(last.source, "remote");
        assert_eq!(last.sessions.len(), 1);
        assert_eq!(last.sessions[0].name, "build");
    }

    #[test]
    fn add_session_append_to_existing_and_resort() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 50)],
        }];
        let got = add_session(&groups, sess("local", "db", 100));
        assert_eq!(got.len(), 1);
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "db");
        assert_eq!(s[1].name, "web");
    }

    #[test]
    fn add_session_dedup_by_name_replaces() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![
                Session {
                    source: "local".into(),
                    name: "web".into(),
                    windows: 1,
                    last_attached: 10,
                    ..Default::default()
                },
                sess("local", "db", 5),
            ],
        }];
        let got = add_session(
            &groups,
            Session {
                source: "local".into(),
                name: "web".into(),
                windows: 9,
                last_attached: 100,
                ..Default::default()
            },
        );
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        let web = s.iter().find(|x| x.name == "web").expect("web present");
        assert_eq!(web.windows, 9);
        assert_eq!(web.last_attached, 100);
        assert_eq!(s[0].name, "web");
    }

    #[test]
    fn add_session_does_not_mutate_input() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0)],
        }];
        let orig_len = groups[0].sessions.len();
        let _ = add_session(&groups, sess("local", "db", 0));
        assert_eq!(groups[0].sessions.len(), orig_len);
    }

    #[test]
    fn remove_session_drops_session() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0), sess("local", "db", 0)],
        }];
        let got = remove_session(&groups, "local/web");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].sessions.len(), 1);
        assert_eq!(got[0].sessions[0].name, "db");
    }

    #[test]
    fn remove_session_keeps_empty_group() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0)],
        }];
        let got = remove_session(&groups, "local/web");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "local");
        assert!(got[0].sessions.is_empty());
    }

    #[test]
    fn remove_session_does_not_mutate_input() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0), sess("local", "db", 0)],
        }];
        let orig_len = groups[0].sessions.len();
        let _ = remove_session(&groups, "local/web");
        assert_eq!(groups[0].sessions.len(), orig_len);
    }

    #[test]
    fn rename_session_renames_and_resorts() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "alpha", 100), sess("local", "zeta", 100)],
        }];
        let got = rename_session(&groups, "local/alpha", "zzz");
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "zeta");
        assert_eq!(s[1].name, "zzz");
    }

    #[test]
    fn rename_session_no_op_when_missing() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0)],
        }];
        let got = rename_session(&groups, "local/nonexistent", "newname");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].sessions.len(), 1);
        assert_eq!(got[0].sessions[0].name, "web");
    }

    #[test]
    fn rename_session_does_not_mutate_input() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 0)],
        }];
        let _ = rename_session(&groups, "local/web", "renamed");
        assert_eq!(groups[0].sessions[0].name, "web");
    }

    #[test]
    fn order_groups_pins_local_then_remote_by_recency() {
        let groups = vec![
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![sess("jupiter00", "a", 100)],
            },
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "w", 50)],
            },
            Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![sess("jupiter06", "b", 300)],
            },
            Group {
                source: "deadhost".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ];
        let out = order_groups(&groups);
        let order: Vec<&str> = out.iter().map(|g| g.source.as_str()).collect();
        // local first; then remotes by max last_attached desc (jupiter06=300,
        // jupiter00=100); the empty/unreachable deadhost (no sessions) sorts last.
        assert_eq!(order, vec!["local", "jupiter06", "jupiter00", "deadhost"]);
    }

    #[test]
    fn order_groups_does_not_mutate_input() {
        let groups = sample_groups();
        let first = groups[0].source.clone();
        let _ = order_groups(&groups);
        assert_eq!(groups[0].source, first);
    }
}
