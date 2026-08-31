//! The pure tree-model logic for the session switcher: a slice of [`Group`]s (one
//! per source) each carrying its sessions in name order. The functions here are
//! side-effect-free transforms over that model; the interactive ratatui
//! rendering is layered on top separately.

use std::collections::HashSet;

use crate::session::Session;

/// The sessions of one source. A non-`None` `err` means the host was
/// unreachable, in which case `sessions` carries no meaning.
#[derive(Debug, Clone)]
pub struct Group {
    pub source: String,
    pub err: Option<String>,
    pub sessions: Vec<Session>,
}

/// Orders sessions in place by name ascending. The sort is stable so sessions
/// with equal names keep their original relative order.
pub fn sort_by_name(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
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
/// replacing any existing session of the same name in place (dedup by name) or, when
/// new, appending it at the group's end. It does NOT sort here: a session created
/// mid-session is placed by the next rebuild's deterministic order, not by this
/// mutation. If no group has the source, a new group is appended. Inputs are not
/// mutated.
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

/// Orders host groups for display: local sources first, then WSL distros, then
/// remote hosts, each tier by source name ascending. Inputs are not mutated.
pub fn order_groups(groups: &[Group]) -> Vec<Group> {
    let mut out = groups.to_vec();
    out.sort_by(|a, b| {
        source_tier(&a.source)
            .cmp(&source_tier(&b.source))
            .then_with(|| a.source.cmp(&b.source))
    });
    out
}

/// The display tier of a source: local (0) before WSL (1) before remote (2).
/// A WSL machine is a distro on this machine, neither this machine's own mux scope nor an
/// ssh host, so it gets its own tier between them.
fn source_tier(source: &str) -> u8 {
    let machine = crate::session::machine_of(source);
    if machine == crate::session::LOCAL_SOURCE {
        0
    } else if crate::session::wsl_distro_of(machine).is_some() {
        1
    } else {
        2
    }
}

/// Returns groups with the session at `address` renamed to `new_name`, kept at its
/// current position; the next rebuild's deterministic order places it. It is a no-op
/// if no session matches. Inputs are not mutated.
pub fn rename_session(groups: &[Group], address: &str, new_name: &str) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if let Some(j) = g.sessions.iter().position(|s| s.address() == address) {
            g.sessions[j].name = new_name.to_string();
            return out;
        }
    }
    out
}

/// What a navigation card references. Every card is a selectable target: a session
/// card attaches to that session (the mux lands on its active window),
/// a host-state card selects the host (so its host screen shows). A section title is
/// not a card: it names the group under it and cannot take the selection.
#[derive(Clone)]
pub(crate) enum RowRef {
    /// A host/mux SECTION TITLE: the non-selectable header row a group of sibling
    /// session cards hangs under. It carries `{host}/{mux}` and is never numbered or
    /// selectable - the numbers below it are the sessions'. `n` on one of those
    /// sessions creates a sibling in the same section.
    Section { source: String },
    /// A session card: the session name on a single detail line. Every session card
    /// carries its session name; the focused window it used to name is gone from the
    /// card, and the `{host}/{mux}` it used to carry now lives on the section title
    /// above it.
    Session { sess: Session },
    /// A host with no session to show (scanning / unreachable / empty) - the only
    /// host-level entry, sunk to the bottom of the list. `scanning` is the in-flight
    /// state: the card's unresolved level shows a spinner instead of a settled mux.
    Host {
        source: String,
        unreachable: bool,
        scanning: bool,
    },
}

/// One navigation row: a session card is a single line carrying the session name,
/// a section title is the `{host}/{mux}` header above a group of them, and a
/// host-state card is the host's own row. The context is derived at render time from
/// the row's [`RowRef`] - `{host}/{mux}` for a section title, the session name for a
/// session card, `{host}` for a host-state card - as is colour, so this model stays
/// terminal-free (no `ratatui` dependency) and unit-testable without a backend.
pub(crate) struct Row {
    /// The mux the row NAMES, resolved once here so every row on one source
    /// names its mux the same way: the kind the enumeration stamped on the session, or the
    /// source's own mux where no session carries one (a host-state card, a session created
    /// since the last enumeration). Empty only while nothing knows it yet, which is the
    /// state a card turns a spinner for.
    pub(crate) mux: String,
    pub(crate) reference: RowRef,
}

impl Row {
    /// A section title is not a card: it names the group under it, and the selection
    /// cannot land on it. Every card (session, host state) is a selectable target.
    pub(crate) fn selectable(&self) -> bool {
        !matches!(self.reference, RowRef::Section { .. })
    }
}

/// The groups the nav may render when unreachable hosts are hidden (`[ui]
/// hide-unreachable`): every reachable group, plus an unreachable group only while
/// the filter names it - the named card is the one entry to that host's unreachable
/// screen, and an empty filter hides every unreachable group. A host still scanning
/// is not unreachable (its card turns the spinner), so it is never hidden, whatever
/// stale error it carries. Inputs are not mutated.
pub(crate) fn drop_hidden_unreachable(
    groups: &[Group],
    scanning: &HashSet<String>,
    filter: &str,
) -> Vec<Group> {
    groups
        .iter()
        .filter(|g| {
            g.err.is_none()
                || scanning.contains(&g.source)
                || (!filter.is_empty() && fuzzy_match(filter, &g.source))
        })
        .cloned()
        .collect()
}

/// The groups to render, in `groups` order - that order is authoritative (established
/// by the deterministic source order at rebuild via [`order_groups`], which a routine
/// poll reproduces exactly, so a poll never reshuffles the tree). An empty filter
/// returns the input unchanged. A non-matching filter must not be a dead end (XM-01):
/// it falls back to header-only groups (every source, no sessions) so the hosts stay
/// visible. Inputs are not mutated.
pub(crate) fn visible_groups(groups: &[Group], filter: &str) -> Vec<Group> {
    if filter.is_empty() {
        groups.to_vec()
    } else {
        let filtered = filter_groups(groups, filter);
        if filtered.is_empty() {
            groups
                .iter()
                .map(|g| Group {
                    source: g.source.clone(),
                    err: g.err.clone(),
                    sessions: Vec::new(),
                })
                .collect()
        } else {
            filtered
        }
    }
}

/// The group's first VISIBLE session under `filter`: the first session when the filter
/// is empty or the source itself matches (all sessions are kept), otherwise the first
/// session whose address matches. An unreachable group (`err` set) yields `None`, since
/// its sessions carry no meaning. Mirrors [`filter_groups`] for a single group without
/// cloning every host's sessions - used on the navigation hot path.
pub(crate) fn first_visible_session(group: &Group, filter: &str) -> Option<Session> {
    if group.err.is_some() {
        return None;
    }
    if filter.is_empty() || fuzzy_match(filter, &group.source) {
        group.sessions.first().cloned()
    } else {
        group
            .sessions
            .iter()
            .find(|s| fuzzy_match(filter, &s.address()))
            .cloned()
    }
}

/// The (source, target) an active-pane attach on `reference` would land on. `target`
/// empty ⇒ no terminal view (a host with no visible session). Pure over the inventory.
/// A section title is never the selection, so it targets nothing; the arm exists to
/// keep the match total.
pub(crate) fn target_for(reference: &RowRef, groups: &[Group], filter: &str) -> (String, String) {
    match reference {
        RowRef::Host { source, .. } | RowRef::Section { source, .. } => match groups
            .iter()
            .find(|g| &g.source == source)
            .and_then(|g| first_visible_session(g, filter))
        {
            Some(sess) => (sess.source, sess.name),
            None => (String::new(), String::new()),
        },
        RowRef::Session { sess } => (sess.source.clone(), sess.name.clone()),
    }
}

/// Pushes a session's card. Every session gets one card naming its session; the
/// focused window a card used to name has left the card, so there is no pane state
/// to wait on and no loading stand-in.
fn push_session_card(rows: &mut Vec<Row>, sess: &Session, mux_of_source: &dyn Fn(&str) -> String) {
    let mux = if sess.mux.is_empty() {
        mux_of_source(&sess.source)
    } else {
        sess.mux.clone()
    };
    rows.push(Row {
        mux,
        reference: RowRef::Session { sess: sess.clone() },
    });
}

/// The status word a SETTLED host reads on its host screen. One source for the
/// unreachable and the empty states, so the screen a user reaches from a card can
/// never name the same state two ways. The card itself no longer prints this word:
/// an unreachable card carries the `⚠` mark on its host row, and a reachable empty
/// host reads as the host row alone, so the word is the screen's alone.
pub(crate) fn host_state_word(unreachable: bool) -> &'static str {
    if unreachable {
        "⚠ unreachable"
    } else {
        "no sessions"
    }
}

/// Flattens the inventory into a flat list of navigation rows: a section title per
/// source that has a session to show, then one session card per session, emitted in
/// group order (the deterministic local→WSL→remote, name-sorted order `rebuild`
/// establishes, so a routine poll reproduces the same list). Hosts with no session to
/// show (scanning / unreachable / empty) get one host-state card each, sunk to the
/// bottom band. The mux each row NAMES is resolved here through `mux_of_source`, so a
/// row cannot exist without it and two rows on one source cannot name their mux two
/// ways; colour is derived at render time from each row's [`RowRef`], so this stays
/// terminal-free. With `hide_unreachable`, the unreachable hosts are pruned before the
/// filter runs, so the no-match fallback cannot resurrect a host the filter does not
/// name. Inputs are not mutated.
pub(crate) fn flatten(
    groups: &[Group],
    scanning: &HashSet<String>,
    filter: &str,
    hide_unreachable: bool,
    mux_of_source: &dyn Fn(&str) -> String,
) -> Vec<Row> {
    let groups = if hide_unreachable {
        drop_hidden_unreachable(groups, scanning, filter)
    } else {
        groups.to_vec()
    };
    let groups = visible_groups(&groups, filter);

    let mut rows = Vec::new();
    // 1. A section per source that has a session to show: the non-selectable
    //    `{host}/{mux}` title, then one session card per session. A session created
    //    from one of these cards is its sibling - it joins this same section.
    for g in &groups {
        if g.err.is_some() || g.sessions.is_empty() {
            continue;
        }
        rows.push(Row {
            mux: mux_of_source(&g.source),
            reference: RowRef::Section {
                source: g.source.clone(),
            },
        });
        for sess in &g.sessions {
            push_session_card(&mut rows, sess, mux_of_source);
        }
    }
    // 2. Host-state cards for hosts with no session to show - sunk to the bottom band.
    for g in &groups {
        let is_scanning = scanning.contains(&g.source);
        let unreachable = g.err.is_some();
        if !unreachable && !g.sessions.is_empty() {
            continue;
        }
        // The mux a host-state card may CLAIM. A settled reachable host's enumeration
        // answered through its mux, so that mux is a confirmed fact; a source id that
        // names its own mux was resolved from what the machine actually serves, so it
        // is confirmed too. A bare id's mux is only a config assumption, which no
        // answer has confirmed: while the host scans or is unreachable the card claims
        // no mux - it reads the host alone (unreachable) or spins in the mux position
        // (scanning).
        let mux_confirmed =
            (!is_scanning && !unreachable) || !crate::session::mux_of(&g.source).is_empty();
        rows.push(Row {
            mux: if mux_confirmed {
                mux_of_source(&g.source)
            } else {
                String::new()
            },
            reference: RowRef::Host {
                source: g.source.clone(),
                unreachable,
                scanning: is_scanning,
            },
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the app's resolver answers with: the source's own mux. A test source id
    /// carries its mux when its machine serves several and nothing else knows one.
    fn mux_of_source(source: &str) -> String {
        crate::session::mux_of(source).to_string()
    }

    fn sess(source: &str, name: &str) -> Session {
        Session {
            source: source.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    fn sample_groups() -> Vec<Group> {
        vec![
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![
                    sess("jupiter00", "inference"),
                    sess("jupiter00", "training"),
                ],
            },
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "web"), sess("local", "db")],
            },
            Group {
                source: "deadhost".into(),
                err: Some("dial: connection refused".into()),
                sessions: vec![sess("deadhost", "ghost")],
            },
        ]
    }

    #[test]
    fn sort_by_name_orders() {
        let mut in_ = vec![
            sess("local", "beta"),
            sess("local", "alpha"),
            sess("local", "gamma"),
            sess("local", "delta"),
        ];
        sort_by_name(&mut in_);
        let names: Vec<&str> = in_.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "delta", "gamma"]);
    }

    #[test]
    fn sort_by_name_stable_for_equal_names() {
        let mut in_ = vec![sess("h1", "x"), sess("h2", "x"), sess("h3", "x")];
        sort_by_name(&mut in_);
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
            sessions: vec![sess("local", "web")],
        }];
        let got = add_session(&groups, sess("remote", "build"));
        assert_eq!(got.len(), 2);
        let last = got.last().unwrap();
        assert_eq!(last.source, "remote");
        assert_eq!(last.sessions.len(), 1);
        assert_eq!(last.sessions[0].name, "build");
    }

    #[test]
    fn add_session_appends_new_at_end() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web")],
        }];
        // A mid-session create does not sort here - it appends, and the next rebuild's
        // deterministic order places it.
        let got = add_session(&groups, sess("local", "db"));
        assert_eq!(got.len(), 1);
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "web");
        assert_eq!(s[1].name, "db");
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
                    ..Default::default()
                },
                sess("local", "db"),
            ],
        }];
        let got = add_session(
            &groups,
            Session {
                source: "local".into(),
                name: "web".into(),
                windows: 9,
                ..Default::default()
            },
        );
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        let web = s.iter().find(|x| x.name == "web").expect("web present");
        assert_eq!(web.windows, 9);
        assert_eq!(s[0].name, "web");
    }

    #[test]
    fn add_session_does_not_mutate_input() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web")],
        }];
        let orig_len = groups[0].sessions.len();
        let _ = add_session(&groups, sess("local", "db"));
        assert_eq!(groups[0].sessions.len(), orig_len);
    }

    #[test]
    fn remove_session_drops_session() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web"), sess("local", "db")],
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
            sessions: vec![sess("local", "web")],
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
            sessions: vec![sess("local", "web"), sess("local", "db")],
        }];
        let orig_len = groups[0].sessions.len();
        let _ = remove_session(&groups, "local/web");
        assert_eq!(groups[0].sessions.len(), orig_len);
    }

    #[test]
    fn rename_session_keeps_position() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "alpha"), sess("local", "zeta")],
        }];
        let got = rename_session(&groups, "local/alpha", "zzz");
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        // Renamed in place: alpha's slot (index 0) now holds zzz; this mutation does not
        // sort, and the next rebuild's deterministic order places the renamed session.
        assert_eq!(s[0].name, "zzz");
        assert_eq!(s[1].name, "zeta");
    }

    #[test]
    fn rename_session_no_op_when_missing() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web")],
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
            sessions: vec![sess("local", "web")],
        }];
        let _ = rename_session(&groups, "local/web", "renamed");
        assert_eq!(groups[0].sessions[0].name, "web");
    }

    #[test]
    fn order_groups_local_then_wsl_then_remote_by_name() {
        let groups = vec![
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![sess("jupiter00", "a")],
            },
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "w")],
            },
            Group {
                source: "wsl.Debian".into(),
                err: None,
                sessions: vec![sess("wsl.Debian", "d")],
            },
            Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![sess("jupiter06", "b")],
            },
            Group {
                source: "deadhost".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ];
        let out = order_groups(&groups);
        let order: Vec<&str> = out.iter().map(|g| g.source.as_str()).collect();
        // local first, then WSL, then remotes by name; each tier by source name
        // ascending. deadhost's unreachable state does not sink it.
        assert_eq!(
            order,
            vec!["local", "wsl.Debian", "deadhost", "jupiter00", "jupiter06"]
        );
    }

    #[test]
    fn every_mux_on_this_box_pins_ahead_of_every_remote() {
        // Two muxes on this machine are two sources with QUALIFIED ids. Both are local and
        // both must stay ahead of the remotes: a comparison against the bare "local"
        // would sink them in among the ssh hosts.
        let groups = vec![
            Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![sess("jupiter06", "b")],
            },
            Group {
                source: "local:zellij".into(),
                err: None,
                sessions: vec![sess("local:zellij", "z")],
            },
            Group {
                source: "local:psmux".into(),
                err: None,
                sessions: vec![sess("local:psmux", "p")],
            },
        ];
        let out = order_groups(&groups);
        let order: Vec<&str> = out.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(
            order,
            vec!["local:psmux", "local:zellij", "jupiter06"],
            "both local sources first, by source name"
        );
    }

    #[test]
    fn order_groups_does_not_mutate_input() {
        let groups = sample_groups();
        let first = groups[0].source.clone();
        let _ = order_groups(&groups);
        assert_eq!(groups[0].source, first);
    }

    fn kind(r: &RowRef) -> &'static str {
        match r {
            RowRef::Section { .. } => "section",
            RowRef::Host { .. } => "host",
            RowRef::Session { .. } => "session",
        }
    }

    /// The session address a session card references ("" for a host card or a section).
    fn addr_of(r: &RowRef) -> String {
        match r {
            RowRef::Section { source, .. } => source.clone(),
            RowRef::Session { sess } => sess.address(),
            RowRef::Host { source, .. } => source.clone(),
        }
    }

    #[test]
    fn flatten_emits_a_section_then_a_card_per_session() {
        // A source with one session: a SECTION title, then one session card. No host
        // row (the host has sessions to show).
        let groups = vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api")],
        }];
        let rows = flatten(&groups, &HashSet::new(), "", false, &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["section", "session"]);
        assert_eq!(addr_of(&rows[1].reference), "jup/api");
        assert!(matches!(
            rows[0].reference,
            RowRef::Section { ref source } if source == "jup"
        ));
    }

    #[test]
    fn flatten_emits_sessions_in_group_order_under_one_section() {
        // Two sessions: the section title, then the session cards in the group's order
        // (the deterministic order `rebuild` establishes).
        let groups = vec![Group {
            source: "h".into(),
            err: None,
            sessions: vec![sess("h", "a"), sess("h", "b")],
        }];
        let rows = flatten(&groups, &HashSet::new(), "", false, &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["section", "session", "session"]);
        let addrs: Vec<String> = rows.iter().map(|r| addr_of(&r.reference)).collect();
        assert_eq!(addrs, vec!["h", "h/a", "h/b"]);
    }

    #[test]
    fn flatten_scanning_host_gets_a_host_state_card() {
        let groups = vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![],
        }];
        let mut scanning = HashSet::new();
        scanning.insert("jup".to_string());
        let rows = flatten(&groups, &scanning, "", false, &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host"]);
        assert_eq!(addr_of(&rows[0].reference), "jup");
        // The card is marked in flight: the render turns a spinner in the level that
        // has not resolved.
        assert!(matches!(
            rows[0].reference,
            RowRef::Host {
                scanning: true,
                unreachable: false,
                ..
            }
        ));
    }

    #[test]
    fn flatten_empty_and_unreachable_hosts_get_host_state_cards() {
        let groups = vec![
            Group {
                source: "empty".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                source: "dead".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ];
        let rows = flatten(&groups, &HashSet::new(), "", false, &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host", "host"]);
        assert_eq!(addr_of(&rows[0].reference), "empty");
        assert!(matches!(
            rows[0].reference,
            RowRef::Host {
                unreachable: false,
                scanning: false,
                ..
            }
        ));
        assert_eq!(addr_of(&rows[1].reference), "dead");
        assert!(matches!(
            rows[1].reference,
            RowRef::Host {
                unreachable: true,
                scanning: false,
                ..
            }
        ));
    }

    #[test]
    fn a_scanning_host_is_not_yet_a_failure() {
        // A host still being scanned has no reason to show, even carrying a stale one
        // from the last sweep: the card says what it is doing now.
        let groups = vec![Group {
            source: "kyla".into(),
            err: Some("ssh: Connection timed out".into()),
            sessions: vec![],
        }];
        let mut scanning = HashSet::new();
        scanning.insert("kyla".to_string());
        let rows = flatten(&groups, &scanning, "", false, &mux_of_source);
        assert!(matches!(
            rows[0].reference,
            RowRef::Host { scanning: true, .. }
        ));
    }

    #[test]
    fn first_visible_session_respects_filter() {
        let g = Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api"), sess("jup", "web")],
        };
        // Empty filter → the first session.
        assert_eq!(first_visible_session(&g, "").unwrap().name, "api");
        // Source match → the first session (all sessions kept).
        assert_eq!(first_visible_session(&g, "jup").unwrap().name, "api");
        // Session-only match → the first matching session.
        assert_eq!(first_visible_session(&g, "web").unwrap().name, "web");
        // Unreachable host → None (its sessions carry no meaning).
        let dead = Group {
            source: "jup".into(),
            err: Some("refused".into()),
            sessions: vec![sess("jup", "api")],
        };
        assert!(first_visible_session(&dead, "").is_none());
    }

    fn drop_hidden_setup() -> Vec<Group> {
        vec![
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "web")],
            },
            Group {
                source: "empty".into(),
                err: None,
                sessions: vec![],
            },
            Group {
                source: "deadhost".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ]
    }

    #[test]
    fn drop_hidden_unreachable_keeps_reachable_and_drops_settled_failures() {
        // An empty filter hides the settled unreachable host; the reachable hosts (one
        // with sessions, one empty) keep their groups.
        let got = drop_hidden_unreachable(&drop_hidden_setup(), &HashSet::new(), "");
        let sources: Vec<&str> = got.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(sources, vec!["local", "empty"]);
    }

    #[test]
    fn drop_hidden_unreachable_never_hides_a_scanning_host() {
        // A host still scanning is not unreachable yet: whatever stale error it carries,
        // its group stays, consistent with the render's spinner state.
        let mut scanning = HashSet::new();
        scanning.insert("deadhost".to_string());
        let got = drop_hidden_unreachable(&drop_hidden_setup(), &scanning, "");
        let sources: Vec<&str> = got.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(sources, vec!["local", "empty", "deadhost"]);
    }

    #[test]
    fn drop_hidden_unreachable_filter_naming_the_host_keeps_its_group() {
        // The filter naming the host keeps its card: it is the one entry to that host's
        // unreachable screen.
        let got = drop_hidden_unreachable(&drop_hidden_setup(), &HashSet::new(), "dead");
        let sources: Vec<&str> = got.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(sources, vec!["local", "empty", "deadhost"]);
    }

    #[test]
    fn drop_hidden_unreachable_does_not_mutate_input() {
        let groups = drop_hidden_setup();
        let orig_len = groups.len();
        let _ = drop_hidden_unreachable(&groups, &HashSet::new(), "");
        assert_eq!(groups.len(), orig_len);
        assert_eq!(groups[2].source, "deadhost");
        assert!(groups[2].err.is_some());
    }

    #[test]
    fn flatten_hides_unreachable_hosts_when_asked() {
        // With hiding on and an empty filter, the rows are the local section and card
        // and the empty reachable host's card; the unreachable host takes no row.
        let groups = drop_hidden_setup();
        let rows = flatten(&groups, &HashSet::new(), "", true, &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["section", "session", "host"]);
        assert!(!rows
            .iter()
            .any(|r| addr_of(&r.reference).contains("deadhost")));
    }

    #[test]
    fn flatten_keeps_the_unreachable_card_when_the_filter_names_it() {
        // The filter naming the hidden host brings its card back, unreachable as ever.
        let groups = drop_hidden_setup();
        let rows = flatten(&groups, &HashSet::new(), "dead", true, &mux_of_source);
        assert!(rows.iter().any(|r| matches!(
            &r.reference,
            RowRef::Host { source, unreachable: true, .. } if source == "deadhost"
        )));
    }

    #[test]
    fn flatten_no_match_fallback_does_not_resurrect_a_hidden_host() {
        // The prune runs before the filter, so the no-match fallback (header-only
        // groups for every remaining host) cannot bring the hidden host back.
        let groups = drop_hidden_setup();
        let rows = flatten(&groups, &HashSet::new(), "zzz", true, &mux_of_source);
        assert!(!rows
            .iter()
            .any(|r| addr_of(&r.reference).contains("deadhost")));
        // The hosts the filter does not name keep their fallback cards.
        assert!(rows.iter().any(|r| addr_of(&r.reference) == "local"));
        assert!(rows.iter().any(|r| addr_of(&r.reference) == "empty"));
    }

    #[test]
    fn flatten_hiding_every_host_leaves_no_rows() {
        // Every host unreachable and hiding on: the nav holds no row at all, and
        // nothing panics.
        let groups = vec![
            Group {
                source: "deadhost".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
            Group {
                source: "other".into(),
                err: Some("timed out".into()),
                sessions: vec![],
            },
        ];
        let rows = flatten(&groups, &HashSet::new(), "", true, &mux_of_source);
        assert!(rows.is_empty());
    }
}
