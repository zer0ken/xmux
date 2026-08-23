//! The pure tree-model logic for the session switcher: a slice of [`Group`]s (one
//! per source) each carrying its sessions ordered by recency. The functions here
//! are side-effect-free transforms over that model; the interactive ratatui
//! rendering is layered on top separately.

use std::collections::{HashMap, HashSet};

use crate::session::{Session, WindowPanes};

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
/// replacing any existing session of the same name in place (dedup by name) or, when
/// new, appending it at the group's end. It does NOT re-sort: a session created
/// mid-session must not reshuffle the frozen tree order - ordering is applied only at
/// scan time (see [`sort_by_recency`] / [`reorder_preserving`]). If no group has the
/// source, a new group is appended. Inputs are not mutated.
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

/// Reorders `incoming` to preserve the display order established in `existing`:
/// sessions present in both keep `existing`'s relative order (carrying the fresh
/// `incoming` data), sessions new since then are appended in `incoming` order, and
/// sessions absent from `incoming` are dropped. Used on a routine poll so the tree
/// stays put under the user - recency ordering ([`sort_by_recency`]) is applied only
/// at scan time (launch / re-scan), never on a live poll whose `last_attached` values
/// xmux's own pre-attaching would otherwise churn.
pub fn reorder_preserving(mut incoming: Vec<Session>, existing: &[Session]) -> Vec<Session> {
    let rank: std::collections::HashMap<String, usize> = existing
        .iter()
        .enumerate()
        .map(|(i, s)| (s.address(), i))
        .collect();
    // Stable sort: existing sessions land at their prior rank; new ones (rank
    // usize::MAX) keep their incoming relative order after them.
    incoming.sort_by_cached_key(|s| rank.get(&s.address()).copied().unwrap_or(usize::MAX));
    incoming
}

/// Orders host groups for display: the local source(s) pinned first (original
/// relative order), then remote groups by their most-recent session's
/// `last_attached` descending (a group with no sessions sorts last; ties by
/// source name ascending). Inputs are not mutated.
pub fn order_groups(groups: &[Group]) -> Vec<Group> {
    let mut local: Vec<Group> = Vec::new();
    let mut remote: Vec<Group> = Vec::new();
    for g in groups {
        if crate::session::is_local_source(&g.source) {
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

/// Returns groups with the session at `address` renamed to `new_name`, kept at its
/// current position (a rename is not a recency event, so it never reorders the tree
/// under the user). It is a no-op if no session matches. Inputs are not mutated.
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
/// card attaches to that session (the mux lands on its active window), a loading
/// card likewise, a host-state card selects the host (so its host screen shows).
#[derive(Clone)]
pub(crate) enum RowRef {
    /// A session card: a `{host}/{mux}` context line over a `{session}/{window}`
    /// detail line (the focused window, in its own mux's notation).
    Session { sess: Session },
    /// A host with no session to show (scanning / unreachable / empty) - the only
    /// host-level entry, sunk to the bottom of the list. `scanning` is the in-flight
    /// state: the card's unresolved level shows a spinner instead of a status word.
    Host {
        source: String,
        unreachable: bool,
        scanning: bool,
    },
    /// A session whose panes are still in flight: its card shows a spinner until
    /// the focused window's name resolves. Attaches to the session all the same.
    Loading { sess: Session },
}

/// One navigation card: a context line over a detail line (or the detail line
/// alone when collapsed under the previous card's identical context). The context
/// line is derived at render time from the card's [`RowRef`] - `{host}/{mux}` for
/// a session/loading card, `{host}` for a host-state card - as is colour, so this
/// model stays terminal-free (no `ratatui` dependency) and unit-testable without
/// a backend.
pub(crate) struct Row {
    /// The detail line's variable part: the focused (active) window's label, as the
    /// session's own mux writes it, for a session card (the renderer prefixes the
    /// session name), the SETTLED host state (⚠ unreachable / no sessions) for a
    /// host-state card. Empty on anything still in flight - a loading card and a
    /// scanning host card alike - because a spinner renders in place of the level that
    /// has not resolved.
    pub(crate) line2: String,
    pub(crate) reference: RowRef,
}

impl Row {
    /// Every card is a selectable target.
    pub(crate) fn selectable(&self) -> bool {
        true
    }
}

/// The groups to render, in `groups` order - that order is authoritative (established
/// by source recency at scan time via [`order_groups`], then frozen so a routine poll
/// never reshuffles the tree). An empty filter returns the input unchanged. A non-matching
/// filter must not be a dead end (XM-01): it falls back to header-only groups (every
/// source, no sessions) so the hosts stay visible. Inputs are not mutated.
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
pub(crate) fn target_for(reference: &RowRef, groups: &[Group], filter: &str) -> (String, String) {
    match reference {
        RowRef::Host { source, .. } => match groups
            .iter()
            .find(|g| &g.source == source)
            .and_then(|g| first_visible_session(g, filter))
        {
            Some(sess) => (sess.source, sess.name),
            None => (String::new(), String::new()),
        },
        RowRef::Session { sess } | RowRef::Loading { sess } => {
            (sess.source.clone(), sess.name.clone())
        }
    }
}

/// Pushes a session's card: line2 carries its focused (active) window, written the way
/// the session's own mux writes it, or a loading card stands in while the panes are in
/// flight.
fn push_session_card(
    rows: &mut Vec<Row>,
    sess: &Session,
    panes: &HashMap<String, Vec<WindowPanes>>,
    panes_loaded: &HashSet<String>,
) {
    let addr = sess.address();
    if panes_loaded.contains(&addr) {
        if let Some(windows) = panes.get(&addr) {
            // A READ window list that is EMPTY is an answer, not a wait: the mux was
            // asked and reported nothing it could name. The card drops its window row
            // rather than spinning forever on a spinner that no later sweep resolves.
            let line2 = match windows
                .iter()
                .find(|w| w.active)
                .or_else(|| windows.first())
            {
                Some(focused) => crate::mux::window_label(&sess.mux, focused.index, &focused.name),
                None => String::new(),
            };
            rows.push(Row {
                line2,
                reference: RowRef::Session { sess: sess.clone() },
            });
            return;
        }
    }
    rows.push(Row {
        line2: String::new(),
        reference: RowRef::Loading { sess: sess.clone() },
    });
}

/// The status word a SETTLED host reads, whether it is read on the host's nav card or on
/// its host screen. One source for both, so the card and the screen a user reaches from
/// it can never name the same state two ways.
pub(crate) fn host_state_word(unreachable: bool) -> &'static str {
    if unreachable {
        "⚠ unreachable"
    } else {
        "no sessions"
    }
}

/// Flattens the inventory into a flat list of navigation cards: one session card per
/// session (or a loading card while its panes are in flight), emitted in the frozen
/// MRU `order` (session addresses) so a routine poll never reshuffles the list under
/// the user; sessions absent from `order` (just appeared) follow in group order.
/// Hosts with no session to show (scanning / unreachable / empty) get one host-state
/// card each, sunk to the bottom. Colour and the context line are derived at render
/// time from each card's [`RowRef`], so this stays terminal-free. Inputs are not
/// mutated.
pub(crate) fn flatten(
    groups: &[Group],
    panes: &HashMap<String, Vec<WindowPanes>>,
    panes_loaded: &HashSet<String>,
    scanning: &HashSet<String>,
    filter: &str,
    order: &[String],
) -> Vec<Row> {
    let groups = visible_groups(groups, filter);
    // Reachable sessions indexed by address, so the frozen order can emit them directly.
    let mut by_addr: HashMap<String, &Session> = HashMap::new();
    for g in &groups {
        if g.err.is_some() {
            continue;
        }
        for sess in &g.sessions {
            by_addr.insert(sess.address(), sess);
        }
    }

    let mut rows = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    // 1. Session / loading cards in the frozen MRU order.
    for addr in order {
        if let Some(sess) = by_addr.get(addr) {
            push_session_card(&mut rows, sess, panes, panes_loaded);
            emitted.insert(addr.clone());
        }
    }
    // 2. Sessions that appeared since the order was frozen, in group order.
    for g in &groups {
        if g.err.is_some() {
            continue;
        }
        for sess in &g.sessions {
            if emitted.insert(sess.address()) {
                push_session_card(&mut rows, sess, panes, panes_loaded);
            }
        }
    }
    // 3. Host-state cards for hosts with no session to show - sunk to the bottom.
    for g in &groups {
        let is_scanning = scanning.contains(&g.source);
        let unreachable = g.err.is_some();
        if !unreachable && !g.sessions.is_empty() {
            continue;
        }
        // A scanning card carries no status WORD: the spinner in its unresolved level
        // is the status, so the card does not say the same thing twice. A settled card
        // carries the status word and nothing else: WHY a host is unreachable is the
        // screen's to state, where the message fits whole, while a card is only as wide
        // as the nav and would carry a cut-down copy of it.
        let line2 = if is_scanning {
            String::new()
        } else {
            host_state_word(unreachable).to_string()
        };
        rows.push(Row {
            line2,
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
    use crate::session::Pane;

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
    fn add_session_appends_new_at_end() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "web", 50)],
        }];
        // db is more recent (100 > 50) but a mid-session create must NOT reshuffle:
        // the new session appends after the existing web.
        let got = add_session(&groups, sess("local", "db", 100));
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
    fn rename_session_keeps_position() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "alpha", 100), sess("local", "zeta", 100)],
        }];
        let got = rename_session(&groups, "local/alpha", "zzz");
        let s = &got[0].sessions;
        assert_eq!(s.len(), 2);
        // Renamed in place: alpha's slot (index 0) now holds zzz; no re-sort, even
        // though by-name order would otherwise put zzz after zeta.
        assert_eq!(s[0].name, "zzz");
        assert_eq!(s[1].name, "zeta");
    }

    #[test]
    fn reorder_preserving_keeps_existing_order() {
        // Established display order is b, a. A poll arrives recency-sorted (a, b) with
        // a bumped - the poll must NOT re-sort; the b, a order holds.
        let existing = vec![sess("h", "b", 0), sess("h", "a", 0)];
        let incoming = vec![sess("h", "a", 999), sess("h", "b", 500)];
        let out = reorder_preserving(incoming, &existing);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a"]);
    }

    #[test]
    fn reorder_preserving_appends_new_sessions() {
        let existing = vec![sess("h", "b", 0), sess("h", "a", 0)];
        let incoming = vec![sess("h", "a", 0), sess("h", "b", 0), sess("h", "c", 0)];
        let out = reorder_preserving(incoming, &existing);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b", "a", "c"],
            "a session new since the scan appends last"
        );
    }

    #[test]
    fn reorder_preserving_multiple_new_keep_incoming_order() {
        let existing = vec![sess("h", "a", 0)];
        let incoming = vec![sess("h", "a", 0), sess("h", "z", 0), sess("h", "m", 0)];
        let out = reorder_preserving(incoming, &existing);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "z", "m"],
            "several new sessions keep their incoming order after the existing ones"
        );
    }

    #[test]
    fn reorder_preserving_drops_missing_sessions() {
        let existing = vec![sess("h", "b", 0), sess("h", "a", 0)];
        let incoming = vec![sess("h", "b", 0)];
        let out = reorder_preserving(incoming, &existing);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["b"], "a session gone from the poll is dropped");
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
    fn every_mux_on_this_box_pins_ahead_of_every_remote() {
        // Two muxes on this box are two sources with QUALIFIED ids. Both are local and
        // both must stay ahead of the remotes: a comparison against the bare "local"
        // would sink them in among the ssh hosts.
        let groups = vec![
            Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![sess("jupiter06", "b", 300)],
            },
            Group {
                source: "local:zellij".into(),
                err: None,
                sessions: vec![sess("local:zellij", "z", 10)],
            },
            Group {
                source: "local:psmux".into(),
                err: None,
                sessions: vec![sess("local:psmux", "p", 20)],
            },
        ];
        let out = order_groups(&groups);
        let order: Vec<&str> = out.iter().map(|g| g.source.as_str()).collect();
        assert_eq!(
            order,
            vec!["local:zellij", "local:psmux", "jupiter06"],
            "both local sources first, in their original relative order"
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
            RowRef::Host { .. } => "host",
            RowRef::Session { .. } => "session",
            RowRef::Loading { .. } => "loading",
        }
    }

    /// The session address a session/loading card references ("" for a host card).
    fn addr_of(r: &RowRef) -> String {
        match r {
            RowRef::Session { sess } | RowRef::Loading { sess } => sess.address(),
            RowRef::Host { source, .. } => source.clone(),
        }
    }

    fn win(index: i64, name: &str, active: bool, panes: Vec<Pane>) -> WindowPanes {
        WindowPanes {
            index,
            name: name.into(),
            active,
            panes,
        }
    }

    fn pane(index: i64, active: bool, command: &str) -> Pane {
        Pane {
            index,
            active,
            command: command.into(),
        }
    }

    /// One group with one loaded session `jup/api` carrying two windows, each one pane.
    fn loaded_fixture() -> (
        Vec<Group>,
        HashMap<String, Vec<WindowPanes>>,
        HashSet<String>,
    ) {
        let groups = vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 0)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "jup/api".to_string(),
            vec![
                win(0, "w0", true, vec![pane(0, true, "bash")]),
                win(1, "w1", false, vec![pane(0, false, "vim")]),
            ],
        );
        let mut loaded = HashSet::new();
        loaded.insert("jup/api".to_string());
        (groups, panes, loaded)
    }

    #[test]
    fn flatten_emits_a_card_per_session() {
        // A flat list: ONE card per session, line2 the focused (active) window's
        // `{index}:{name}`. No host rows (the host has sessions to show).
        let (groups, panes, loaded) = loaded_fixture();
        let rows = flatten(&groups, &panes, &loaded, &HashSet::new(), "", &[]);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["session"]);
        assert_eq!(addr_of(&rows[0].reference), "jup/api");
        assert_eq!(
            rows[0].line2, "0:w0",
            "line2 is the ACTIVE window's index:name"
        );
    }

    #[test]
    fn flatten_line2_falls_back_to_the_first_window() {
        // No window flagged active (a mux gap): the first window's name stands in
        // rather than dropping the card or spinning forever.
        let (groups, mut panes, loaded) = loaded_fixture();
        for w in panes.get_mut("jup/api").unwrap() {
            w.active = false;
        }
        let rows = flatten(&groups, &panes, &loaded, &HashSet::new(), "", &[]);
        assert_eq!(rows[0].line2, "0:w0");
    }

    #[test]
    fn flatten_loading_card_when_panes_unloaded() {
        let groups = vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 0)],
        }];
        // panes_loaded does not contain the address → one loading card for the session.
        let rows = flatten(
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            "",
            &[],
        );
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["loading"]);
        assert_eq!(addr_of(&rows[0].reference), "jup/api");
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
        let rows = flatten(
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &scanning,
            "",
            &[],
        );
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host"]);
        assert_eq!(addr_of(&rows[0].reference), "jup");
        // No status WORD while it scans: the card is marked in flight, and the render
        // turns a spinner in the level that has not resolved.
        assert!(matches!(
            rows[0].reference,
            RowRef::Host {
                scanning: true,
                unreachable: false,
                ..
            }
        ));
        assert_eq!(rows[0].line2, "");
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
        let rows = flatten(
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            "",
            &[],
        );
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host", "host"]);
        assert_eq!(addr_of(&rows[0].reference), "empty");
        assert_eq!(rows[0].line2, "no sessions");
        assert_eq!(addr_of(&rows[1].reference), "dead");
        assert_eq!(rows[1].line2, "⚠ unreachable");
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
    fn an_unreachable_card_states_the_state_not_the_reason() {
        // A card names WHAT a host is, the screen names WHY: a diagnostic wraps the
        // failure in its own context and runs past the nav's width, so a card carrying
        // it would carry a cut-down copy of what the screen already states whole.
        let groups = vec![Group {
            source: "kyla".into(),
            err: Some(
                "command failed (exit 255): ssh: connect to host kyla port 22: Connection timed out"
                    .into(),
            ),
            sessions: vec![],
        }];
        let rows = flatten(
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            "",
            &[],
        );
        assert_eq!(rows[0].line2, host_state_word(true));
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
        let rows = flatten(
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &scanning,
            "",
            &[],
        );
        // No word at all, and the card is marked in flight: the render turns a spinner
        // in the level it is waiting on, which is what says "doing it now".
        assert_eq!(rows[0].line2, "");
        assert!(matches!(
            rows[0].reference,
            RowRef::Host { scanning: true, .. }
        ));
    }

    #[test]
    fn flatten_follows_the_frozen_order() {
        // Two loaded single-window sessions; the frozen order lists the second before
        // the first, and the session cards come out in that order (no re-sort).
        let groups = vec![Group {
            source: "h".into(),
            err: None,
            sessions: vec![sess("h", "a", 0), sess("h", "b", 0)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "h/a".to_string(),
            vec![win(0, "wa", true, vec![pane(0, true, "sh")])],
        );
        panes.insert(
            "h/b".to_string(),
            vec![win(0, "wb", true, vec![pane(0, true, "sh")])],
        );
        let mut loaded = HashSet::new();
        loaded.insert("h/a".to_string());
        loaded.insert("h/b".to_string());
        let order = vec!["h/b".to_string(), "h/a".to_string()];
        let rows = flatten(&groups, &panes, &loaded, &HashSet::new(), "", &order);
        let addrs: Vec<String> = rows.iter().map(|r| addr_of(&r.reference)).collect();
        assert_eq!(addrs, vec!["h/b", "h/a"]);
    }

    #[test]
    fn first_visible_session_respects_filter() {
        let g = Group {
            source: "jup".into(),
            err: None,
            sessions: vec![sess("jup", "api", 0), sess("jup", "web", 0)],
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
            sessions: vec![sess("jup", "api", 0)],
        };
        assert!(first_visible_session(&dead, "").is_none());
    }
}
