//! zellij's CLI output shapes, as pure functions over raw stdout.
//!
//! zellij shares no output format with tmux: its session listing is a human line
//! (`<name> [Created <age> ago] <suffix>`) and its tab listing is JSON. Both parsers
//! live here so the `Mux` impl stays argv-and-policy only, and both are total: a line
//! or record that does not fit is skipped rather than poisoning the list.

use crate::session::{Pane, Session, WindowPanes};

/// The literal `zellij list-sessions -n` puts between a session's name and its age.
/// Splitting on it is what lets a name containing spaces survive: zellij forbids only
/// `/` in a session name, so a space is legal and a whitespace split would truncate.
const CREATED_MARKER: &str = " [Created ";

/// The literal that closes the age field.
const AGE_SUFFIX: &str = " ago]";

/// The suffix zellij marks a dead-but-resurrectable session with.
const EXITED_MARKER: &str = "EXITED";

/// The suffix zellij marks the session the LISTING COMMAND ITSELF ran inside with.
/// It is the only attachment zellij's listing reports; a session with other clients
/// attached is indistinguishable from an idle one.
const CURRENT_MARKER: &str = "(current)";

/// Parses `zellij list-sessions -n` into sessions tagged with `source`.
///
/// Each line is `<name> [Created <age> ago] <suffix>`. `last_attached` is
/// `now - age`, because zellij reports a session's CREATION age and no attach time
/// at all: creation is the only instant available to order the list by, and the
/// nav's recency sort needs the same epoch scale tmux reports.
///
/// A session marked `EXITED` is SKIPPED. zellij keeps a resurrectable record of a
/// session after its server is gone and lists it alongside the live ones, so
/// including it would offer a row with nothing running behind it: attaching would
/// resurrect the session rather than show it. `now` is the caller's clock reading,
/// injected so this stays pure.
pub fn parse_sessions(source: &str, out: &str, now: i64) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in out.split('\n') {
        let ln = ln.strip_suffix('\r').unwrap_or(ln);
        let Some((name, rest)) = ln.split_once(CREATED_MARKER) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let Some((age, suffix)) = rest.split_once(AGE_SUFFIX) else {
            continue;
        };
        if suffix.contains(EXITED_MARKER) {
            continue;
        }
        sessions.push(Session {
            source: source.to_string(),
            name: name.to_string(),
            mux: "zellij".to_string(),
            // zellij's listing carries no tab count. One is the floor, not a count:
            // a session always holds at least one tab.
            windows: 1,
            attached: suffix.contains(CURRENT_MARKER),
            last_attached: now.saturating_sub(parse_age_secs(age)).max(0),
        });
    }
    sessions
}

/// Seconds in a `humantime`-formatted duration (`3h 6m 25s`, `2days 4h`, `0s`) - the
/// format zellij prints an age in. Tokens are `<digits><unit>` pairs; an unknown unit
/// contributes nothing, so a unit a future zellij adds costs precision rather than the
/// whole reading.
pub fn parse_age_secs(text: &str) -> i64 {
    let mut total: i64 = 0;
    for token in text.split_whitespace() {
        let split = token
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(token.len());
        let (digits, unit) = token.split_at(split);
        let Ok(n) = digits.parse::<i64>() else {
            continue;
        };
        let Some(secs) = unit_secs(unit) else {
            continue;
        };
        total = total.saturating_add(n.saturating_mul(secs));
    }
    total
}

/// Seconds in one `humantime` unit, or `None` for a unit this does not know.
///
/// The month and year lengths are `humantime`'s own calendar constants (a year is
/// 365.25 days, a month a twelfth of one), so an age reported in months or years is
/// approximate at the source. Sub-second units carry no information at the second
/// resolution the recency key uses, so they count as zero rather than being refused.
fn unit_secs(unit: &str) -> Option<i64> {
    Some(match unit {
        "year" | "years" => 31_557_600,
        "month" | "months" => 2_630_016,
        "day" | "days" => 86_400,
        "h" => 3_600,
        "m" => 60,
        "s" => 1,
        "ms" | "us" | "ns" => 0,
        _ => return None,
    })
}

/// Parses `zellij action list-tabs -a -j` into xmux windows-and-panes.
///
/// A zellij TAB is what xmux calls a window, and `position` is its index - what
/// `go-to-tab` addresses (one-based there). It is internal: a card names a tab the way
/// zellij's own tab bar does, by name alone. The records are sorted by position so the
/// rows read in the order the tab bar does, independent of the order zellij emitted
/// them.
///
/// Pane rows carry only their ordinal. This query reports how many selectable panes a
/// tab holds, not which one is focused or what each runs, and it is the query that
/// reports tab activeness at all - zellij's pane listing marks a focused pane per tab
/// and per layer, so it cannot name the one active tab. The nav renders the window
/// row, so the tab query answers everything displayed and the pane count stays
/// truthful without a second round trip per session.
pub fn parse_tabs(out: &str) -> Vec<WindowPanes> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out) else {
        return Vec::new();
    };
    let Some(records) = v.as_array() else {
        return Vec::new();
    };
    let mut windows: Vec<WindowPanes> = Vec::new();
    for tab in records {
        let Some(index) = tab.get("position").and_then(|p| p.as_i64()) else {
            continue;
        };
        let count = field_i64(tab, "selectable_tiled_panes_count")
            .saturating_add(field_i64(tab, "selectable_floating_panes_count"))
            .max(0);
        windows.push(WindowPanes {
            index,
            name: tab
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            active: tab.get("active").and_then(|a| a.as_bool()) == Some(true),
            panes: (0..count)
                .map(|index| Pane {
                    index,
                    active: false,
                    command: String::new(),
                })
                .collect(),
        });
    }
    windows.sort_by_key(|w| w.index);
    windows
}

/// One integer field of a tab record, or `0` when it is absent or not a number.
fn field_i64(tab: &serde_json::Value, key: &str) -> i64 {
    tab.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `zellij list-sessions -n` output (0.45.0): a live session, a session
    /// whose name holds a space, and a dead-but-resurrectable one. zellij prints a
    /// trailing space where the suffix is empty, so the lines carry it too.
    const SESSIONS: &str = "hug [Created 3h 5m 15s ago] \n\
        my build [Created 55m 10s ago] \n\
        gone [Created 36s ago] (EXITED - attach to resurrect)\n\
        fresh [Created 0s ago] \n";

    /// The clock reading the fixture's ages are measured back from.
    const NOW: i64 = 1_800_000_000;

    #[test]
    fn a_session_carries_its_name_kind_and_creation_recency() {
        let got = parse_sessions("jup", SESSIONS, NOW);
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["hug", "my build", "fresh"],
            "a name may hold a space, so the split is on the Created marker"
        );
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "zellij"));
        assert_eq!(
            got[0].last_attached,
            NOW - (3 * 3600 + 5 * 60 + 15),
            "recency is the creation instant: the only one zellij reports"
        );
        assert_eq!(got[2].last_attached, NOW, "a 0s age is right now");
        assert!(
            got[0].last_attached < got[1].last_attached,
            "the older session sorts behind the newer one"
        );
    }

    #[test]
    fn a_resurrectable_session_is_not_offered() {
        // zellij lists a session it kept a resurrectable record of beside the live
        // ones. Nothing is running behind it, so attaching would resurrect it rather
        // than show it.
        let got = parse_sessions("jup", SESSIONS, NOW);
        assert!(
            !got.iter().any(|s| s.name == "gone"),
            "an EXITED record is not a session to switch to: {got:?}"
        );
    }

    #[test]
    fn windows_is_the_floor_and_attachment_is_only_the_current_session() {
        // zellij's listing reports neither a tab count nor a client count. Every
        // session holds at least one tab; only the session the command RAN INSIDE is
        // reported as attached, and xmux runs outside every session.
        let got = parse_sessions("jup", SESSIONS, NOW);
        assert!(got.iter().all(|s| s.windows == 1));
        assert!(got.iter().all(|s| !s.attached));
        let inside = parse_sessions("local", "hug [Created 1m ago] (current)\n", NOW);
        assert!(
            inside[0].attached,
            "(current) is the one attachment reported"
        );
    }

    #[test]
    fn a_line_that_is_not_a_session_row_is_skipped() {
        // A banner, an MOTD, or a truncated row cannot become a session.
        for junk in [
            "",
            "Please install zellij\n",
            "no-age-here\n",
            " [Created 1m ago] \n",
            "half [Created 1m\n",
        ] {
            assert!(
                parse_sessions("jup", junk, NOW).is_empty(),
                "skipped: {junk:?}"
            );
        }
    }

    #[test]
    fn an_age_reads_every_humantime_unit_it_can_carry() {
        assert_eq!(parse_age_secs("0s"), 0);
        assert_eq!(parse_age_secs("55m 10s"), 55 * 60 + 10);
        assert_eq!(parse_age_secs("3h 6m 25s"), 3 * 3600 + 6 * 60 + 25);
        assert_eq!(parse_age_secs("1day 2h"), 86_400 + 7_200);
        assert_eq!(parse_age_secs("2days"), 2 * 86_400);
        assert_eq!(parse_age_secs("1year 1month"), 31_557_600 + 2_630_016);
        // Sub-second units are zero, not a refusal, so the rest of the age survives.
        assert_eq!(parse_age_secs("4m 500ms"), 240);
        // An unrecognised unit costs its own term only.
        assert_eq!(parse_age_secs("7fortnights 30s"), 30);
        assert_eq!(parse_age_secs(""), 0);
    }

    /// Verbatim `zellij action list-tabs -a -j` output (0.45.0), trimmed to the fields
    /// this reads, with the tabs OUT of position order to pin the sort.
    const TABS: &str = r#"[
      { "position": 2, "name": "deploy", "active": false,
        "selectable_tiled_panes_count": 1, "selectable_floating_panes_count": 0,
        "tab_id": 2 },
      { "position": 0, "name": "Tab #1", "active": false,
        "selectable_tiled_panes_count": 1, "selectable_floating_panes_count": 1,
        "tab_id": 0 },
      { "position": 1, "name": "build", "active": true,
        "selectable_tiled_panes_count": 2, "selectable_floating_panes_count": 0,
        "tab_id": 1 }
    ]"#;

    #[test]
    fn tabs_are_windows_in_tab_bar_order() {
        let got = parse_tabs(TABS);
        assert_eq!(
            got.iter().map(|w| w.index).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "sorted by position, not by the order zellij emitted"
        );
        assert_eq!(
            got.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["Tab #1", "build", "deploy"]
        );
        assert!(got[1].active, "the focused tab is the active window");
        assert!(!got[0].active && !got[2].active);
    }

    #[test]
    fn a_pane_row_carries_only_its_ordinal() {
        // The tab query reports how many selectable panes a tab holds (tiled plus
        // floating), not which is focused or what it runs.
        let got = parse_tabs(TABS);
        assert_eq!(got[0].panes.len(), 2, "one tiled plus one floating");
        assert_eq!(got[1].panes.len(), 2);
        assert_eq!(got[2].panes.len(), 1);
        assert_eq!(
            got[1].panes.iter().map(|p| p.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(got[1]
            .panes
            .iter()
            .all(|p| !p.active && p.command.is_empty()));
    }

    #[test]
    fn output_that_is_not_a_tab_array_yields_nothing() {
        // A zellij that cannot answer (session gone mid-poll, a version whose flag
        // names differ) must leave the window list empty rather than half-built.
        for junk in [
            "",
            "Session 'x' not found. The following sessions are active:",
            "{}",
            "[]",
            r#"[{"name":"no position"}]"#,
        ] {
            assert!(parse_tabs(junk).is_empty(), "yields nothing: {junk:?}");
        }
    }
}
