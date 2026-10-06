//! The pure tree-model logic for the session switcher: a slice of [`Group`]s (one
//! per source) each carrying its sessions in name order. The functions here are
//! side-effect-free transforms over that model; the interactive ratatui
//! rendering is layered on top separately.

use std::borrow::Cow;
use std::collections::HashSet;

pub use crate::model::{add_session, sort_by_name, Group};
use crate::session::Session;
pub(crate) use crate::state::RowRef;

/// The one session a re-enumeration RENAMED, as `(from, to)`: exactly one name left the
/// list and exactly one name joined it. A listing carries names only, so a rename is
/// recognised by that shape alone; any other difference (sessions created or killed, or
/// several changed at once) is not read as a rename.
pub fn renamed_session(old: &[Session], new: &[Session]) -> Option<(String, String)> {
    let mut gone = old.iter().filter(|o| !new.iter().any(|n| n.name == o.name));
    let mut came = new.iter().filter(|n| !old.iter().any(|o| o.name == n.name));
    match (gone.next(), gone.next(), came.next(), came.next()) {
        (Some(from), None, Some(to), None) => Some((from.name.clone(), to.name.clone())),
        _ => None,
    }
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
            .filter(|s| fuzzy_match(pattern, &s.address().display()))
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

/// Returns groups with the session at `address` removed from its group. The
/// now-possibly-empty group is kept, since an empty reachable group is still a
/// valid create target. Inputs are not mutated.
pub fn remove_session(groups: &[Group], address: &crate::session::Address) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if let Some(j) = g
            .sessions
            .iter()
            .position(|s| s.source == address.source && s.name == address.session)
        {
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
pub fn rename_session(
    groups: &[Group],
    address: &crate::session::Address,
    new_name: &str,
) -> Vec<Group> {
    let mut out = groups.to_vec();
    for g in out.iter_mut() {
        if let Some(j) = g
            .sessions
            .iter()
            .position(|s| s.source == address.source && s.name == address.session)
        {
            g.sessions[j].name = new_name.to_string();
            return out;
        }
    }
    out
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
    /// Session and host-state cards take numbers and participate in card navigation.
    /// A section title opens its source's information screen by click or the info key.
    pub(crate) fn selectable(&self) -> bool {
        !matches!(self.reference, RowRef::Section { .. })
    }
}

/// The groups to render, in `groups` order - that order is authoritative (established
/// by the deterministic source order at rebuild via [`order_groups`], which a routine
/// poll reproduces exactly, so a poll never reshuffles the tree). An empty filter
/// borrows the input unchanged. A non-matching filter must not be a dead end (XM-01):
/// it falls back to header-only groups (every source, no sessions) so the hosts stay
/// visible. Inputs are not mutated.
pub(crate) fn visible_groups<'a>(groups: &'a [Group], filter: &str) -> Cow<'a, [Group]> {
    if filter.is_empty() {
        Cow::Borrowed(groups)
    } else {
        let filtered = filter_groups(groups, filter);
        if filtered.is_empty() {
            Cow::Owned(
                groups
                    .iter()
                    .map(|g| Group {
                        source: g.source.clone(),
                        err: g.err.clone(),
                        sessions: Vec::new(),
                    })
                    .collect(),
            )
        } else {
            Cow::Owned(filtered)
        }
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

/// The state word of a host that answered through at least one of its sources.
pub(crate) const HOST_REACHABLE: &str = "reachable";

/// The status word a host-state card and its screen share. The specific states precede
/// unreachable because authentication and listing failures carry their own words. The
/// reason stays on the screen rather than in this compact state name.
pub(crate) fn host_state_word(
    scanning: bool,
    blocked: bool,
    list_failed: bool,
    unreachable: bool,
) -> &'static str {
    if scanning {
        "scanning"
    } else if blocked {
        "login needed"
    } else if list_failed {
        "list failed"
    } else if unreachable {
        "unreachable"
    } else {
        "no sessions"
    }
}

/// The hosts that are down: every source of the host failed to connect (unreachable, or
/// refused until a login) and none is still waiting on an answer. A listing failure is not
/// one of them, since the host answered it.
pub(crate) fn down_machines(groups: &[Group], scanning: &HashSet<String>) -> HashSet<String> {
    let mut up = HashSet::new();
    let mut all = HashSet::new();
    for g in groups {
        let machine = crate::session::machine_of(&g.source).to_string();
        let failed = !scanning.contains(&g.source)
            && matches!(
                g.failure(),
                Some(crate::model::FailureKind::Blocked | crate::model::FailureKind::Unreachable)
            );
        if !failed {
            up.insert(machine.clone());
        }
        all.insert(machine);
    }
    all.retain(|machine| !up.contains(machine));
    all
}

/// Flattens the inventory into a flat list of navigation rows: a section title per
/// source that has a session to show, then one session card per session, emitted in
/// group order (the deterministic local→WSL→remote, name-sorted order `rebuild`
/// establishes, so a routine poll reproduces the same list). Sources with no session to
/// show get one host-state card each: reachable empty sources first, then sources whose
/// connection or inventory is unresolved. A host that is down gets one host card in place
/// of its sources' cards, where its first source's card would stand; its screen is the host
/// screen, and its sources are reached through that screen's links. The mux each row NAMES is resolved here through `mux_of_source`, so a
/// row cannot exist without it and two rows on one source cannot name their mux two
/// ways; colour is derived at render time from each row's [`RowRef`], so this stays
/// terminal-free. Inputs are not mutated.
pub(crate) fn flatten(
    groups: &[Group],
    scanning: &HashSet<String>,
    filter: &str,
    mux_of_source: &dyn Fn(&str) -> String,
) -> Vec<Row> {
    let down = down_machines(groups, scanning);
    let first_source = |machine: &str| {
        groups
            .iter()
            .find(|g| crate::session::machine_of(&g.source) == machine)
            .map(|g| g.source.clone())
            .unwrap_or_default()
    };
    let blocked_machine = |machine: &str| {
        groups.iter().any(|g| {
            crate::session::machine_of(&g.source) == machine
                && g.failure() == Some(crate::model::FailureKind::Blocked)
        })
    };
    let groups = visible_groups(groups, filter);
    let groups: &[Group] = &groups;
    let mut machine_cards: HashSet<&str> = HashSet::new();

    let mut rows = Vec::new();
    // 1. A section per source that has a session to show: outside numbered-card steps
    //    `{host}/{mux}` title, then one session card per session. A session created
    //    from one of these cards is its sibling - it joins this same section.
    for g in groups {
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
    // Host cards are grouped by connection state after the session cards.
    for connected in [true, false] {
        for g in groups {
            let is_scanning = scanning.contains(&g.source);
            let blocked = g.failure() == Some(crate::model::FailureKind::Blocked);
            let list_failed = g.failure() == Some(crate::model::FailureKind::ListFailed);
            let unreachable = g.err.is_some() && !list_failed;
            if connected != (g.err.is_none() && !is_scanning) {
                continue;
            }
            if g.err.is_none() && !g.sessions.is_empty() {
                continue;
            }
            let machine = crate::session::machine_of(&g.source);
            if down.contains(machine) {
                if machine_cards.insert(machine) {
                    rows.push(Row {
                        mux: String::new(),
                        reference: RowRef::Machine {
                            machine: machine.to_string(),
                            source: first_source(machine),
                            blocked: blocked_machine(machine),
                        },
                    });
                }
                continue;
            }
            // The mux a host-state card may CLAIM. A host still scanning or unreachable has
            // answered nothing, so its card claims no mux: it reads the host alone
            // (unreachable) or spins in the mux position (scanning).
            let mux_confirmed =
                crate::session::mux_may_be_named(&g.source, !is_scanning && !unreachable);
            rows.push(Row {
                mux: if mux_confirmed {
                    mux_of_source(&g.source)
                } else {
                    String::new()
                },
                reference: RowRef::Host {
                    source: g.source.clone(),
                    unreachable,
                    blocked,
                    list_failed,
                    scanning: is_scanning,
                },
            });
        }
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

    #[test]
    fn a_rename_is_one_name_gone_and_one_name_new() {
        let names = |ns: &[&str]| ns.iter().map(|n| sess("s", n)).collect::<Vec<_>>();
        assert_eq!(
            renamed_session(&names(&["a", "b"]), &names(&["b", "c"])),
            Some(("a".into(), "c".into()))
        );
        // A session made or killed, or two changed at once, is not a rename.
        assert_eq!(renamed_session(&names(&["a"]), &names(&["a", "b"])), None);
        assert_eq!(renamed_session(&names(&["a", "b"]), &names(&["a"])), None);
        assert_eq!(
            renamed_session(&names(&["a", "b"]), &names(&["c", "d"])),
            None
        );
        assert_eq!(renamed_session(&names(&["a"]), &names(&["a"])), None);
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
        let got = remove_session(&groups, &crate::session::Address::new("local", "web"));
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
        let got = remove_session(&groups, &crate::session::Address::new("local", "web"));
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
        let _ = remove_session(&groups, &crate::session::Address::new("local", "web"));
        assert_eq!(groups[0].sessions.len(), orig_len);
    }

    #[test]
    fn rename_session_keeps_position() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "alpha"), sess("local", "zeta")],
        }];
        let got = rename_session(
            &groups,
            &crate::session::Address::new("local", "alpha"),
            "zzz",
        );
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
        let got = rename_session(
            &groups,
            &crate::session::Address::new("local", "nonexistent"),
            "newname",
        );
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
        let _ = rename_session(
            &groups,
            &crate::session::Address::new("local", "web"),
            "renamed",
        );
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
            RowRef::Machine { .. } => "machine",
            RowRef::Session { .. } => "session",
        }
    }

    /// The session address a session card references ("" for a host card or a section).
    fn addr_of(r: &RowRef) -> String {
        match r {
            RowRef::Section { source, .. } => source.clone(),
            RowRef::Session { sess } => sess.address().display(),
            RowRef::Host { source, .. } => source.clone(),
            RowRef::Machine { machine, .. } => machine.clone(),
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
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
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
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
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
        let rows = flatten(&groups, &scanning, "", &mux_of_source);
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
    fn flatten_gives_an_empty_source_its_card_and_a_down_host_one_card() {
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
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host", "machine"]);
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
            RowRef::Machine { blocked: false, .. }
        ));
    }

    #[test]
    fn a_host_none_of_whose_sources_connected_is_one_card() {
        // Two muxes on one machine that refused both: one card for the machine, where its
        // first source's card would stand, naming that source for the login.
        let groups = vec![
            Group {
                source: "db:tmux".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
            Group {
                source: "db:zellij".into(),
                err: Some("logged out; log in again or re-scan".into()),
                sessions: vec![],
            },
        ];
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0].reference,
            RowRef::Machine { machine, source, blocked: true } if machine == "db" && source == "db:tmux"
        ));
    }

    #[test]
    fn a_host_with_an_answering_source_keeps_a_card_per_source() {
        // One mux answered, so the machine is up: the failing mux is a source card of its
        // own, and so is one still scanning.
        let groups = vec![
            Group {
                source: "db:tmux".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
            Group {
                source: "db:screen".into(),
                err: None,
                sessions: vec![],
            },
        ];
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(kinds, vec!["host", "host"]);
        let mut scanning = HashSet::new();
        scanning.insert("db:screen".to_string());
        let rows = flatten(&groups, &scanning, "", &mux_of_source);
        let kinds: Vec<&str> = rows.iter().map(|r| kind(&r.reference)).collect();
        assert_eq!(
            kinds,
            vec!["host", "host"],
            "a scanning source is not down yet"
        );
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
        let rows = flatten(&groups, &scanning, "", &mux_of_source);
        assert!(matches!(
            rows[0].reference,
            RowRef::Host { scanning: true, .. }
        ));
    }

    #[test]
    fn flatten_keeps_the_unreachable_card_when_the_filter_names_it() {
        // The filter names the disconnected host directly.
        let groups = sample_groups();
        let rows = flatten(&groups, &HashSet::new(), "dead", &mux_of_source);
        assert!(rows.iter().any(|r| matches!(
            &r.reference,
            RowRef::Machine { machine, .. } if machine == "deadhost"
        )));
    }

    #[test]
    fn flatten_keeps_disconnected_hosts_as_cards() {
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
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn flatten_marks_a_blocked_host_as_blocked() {
        let groups = vec![Group {
            source: "pwbox".into(),
            err: Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
            sessions: vec![],
        }];
        let rows = flatten(&groups, &HashSet::new(), "", &mux_of_source);
        match &rows[0].reference {
            RowRef::Machine { blocked, .. } => {
                assert!(*blocked, "a login answers it");
            }
            _ => panic!("expected the host's card, got another row"),
        }
    }

    #[test]
    fn host_state_word_names_the_login_state() {
        assert_eq!(host_state_word(true, false, false, false), "scanning");
        assert_eq!(host_state_word(false, true, false, true), "login needed");
        assert_eq!(host_state_word(false, false, true, false), "list failed");
        assert_eq!(host_state_word(false, false, false, true), "unreachable");
        assert_eq!(host_state_word(false, false, false, false), "no sessions");
    }
}
