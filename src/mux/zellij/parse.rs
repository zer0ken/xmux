//! zellij's CLI output shapes, as pure functions over raw stdout.
//!
//! zellij shares no output format with tmux: its session listing is a human line
//! (`<name> [Created <age> ago] <suffix>`). The parser
//! lives here so the `Mux` impl stays argv-and-policy only, and is total: a line
//! that does not fit is skipped rather than poisoning the list.

use crate::session::Session;

/// JSON counts tab objects, so duplicate names and embedded newlines are harmless.
pub fn tab_count(out: &[u8]) -> Result<i64, serde_json::Error> {
    let tabs: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_slice(out)?;
    Ok(tabs.len() as i64)
}

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

/// Parses `zellij list-sessions -n` into sessions tagged with `host`.
///
/// Each line is `<name> [Created <age> ago] <suffix>`. The ` [Created ` marker
/// and the ` ago]` suffix part the name from the suffix; the age text between
/// them is not carried further.
///
/// A session marked `EXITED` is SKIPPED. zellij keeps a resurrectable record of a
/// session after its server is gone and lists it alongside the live ones, so
/// including it would offer a row with nothing running behind it: attaching would
/// resurrect the session rather than show it.
pub fn parse_sessions(host: &str, out: &str) -> Vec<Session> {
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
        let Some((_, suffix)) = rest.split_once(AGE_SUFFIX) else {
            continue;
        };
        if suffix.contains(EXITED_MARKER) {
            continue;
        }
        sessions.push(Session {
            host: host.to_string(),
            name: name.to_string(),
            mux: "zellij".to_string(),
            // The listing names a session only, so a rename cannot be told from a kill
            // plus a create.
            id: String::new(),
            // The session listing carries no count; enumeration fills it from list-tabs.
            windows: 0,
            attached: suffix.contains(CURRENT_MARKER),
        });
    }
    sessions
}

/// The session a zellij client is connected to, from `ss -xn` rows already narrowed to
/// the sockets whose peer is one of that client's own sockets.
///
/// A zellij server listens on a socket named after its session, and the socket it
/// accepts a client on carries the same name, so the one row with a path is the
/// server's end of the client's connection and the path's last component is the
/// session. A client is connected to one server at a time; rows naming two sessions
/// are not a single answer and answer `None`.
///
/// A row reads `u_str ESTAB <recv-q> <send-q> <path> <inode> * <peer inode>`. The
/// path is what lies between the fourth field and the last three, so a session name
/// holding spaces survives. A row with no path (`*`), or whose last three fields are
/// not an inode, `*`, and an inode, is skipped.
pub fn connected_session(out: &str) -> Option<String> {
    let mut found: Option<&str> = None;
    for row in out.lines() {
        let Some(path) = server_end_path(row) else {
            continue;
        };
        let session = path.rsplit('/').next().unwrap_or(path);
        if session.is_empty() {
            continue;
        }
        match found {
            Some(seen) if seen != session => return None,
            _ => found = Some(session),
        }
    }
    found.map(str::to_string)
}

/// The local path of one `ss -xn` row, or `None` for a row without one.
fn server_end_path(row: &str) -> Option<&str> {
    let mut rest = row.trim();
    for _ in 0..4 {
        rest = rest.split_once(char::is_whitespace)?.1.trim_start();
    }
    let is_inode = |field: &str| !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
    let (head, peer) = rest.rsplit_once(char::is_whitespace)?;
    let (head, star) = head.trim_end().rsplit_once(char::is_whitespace)?;
    let (path, inode) = head.trim_end().rsplit_once(char::is_whitespace)?;
    if !is_inode(peer) || star != "*" || !is_inode(inode) {
        return None;
    }
    let path = path.trim_end();
    (!path.is_empty() && path != "*").then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_json_counts_objects_instead_of_names_or_lines() {
        assert_eq!(
            tab_count(br#"[{"name":""},{"name":"same\nname"},{"name":"same\nname"}]"#).unwrap(),
            3
        );
        assert_eq!(tab_count(b"[]").unwrap(), 0);
        for junk in [
            b"".as_slice(),
            b"null",
            b"{}",
            b"[1]",
            b"[null]",
            b"banner\n[]",
        ] {
            assert!(tab_count(junk).is_err());
        }
    }

    /// Verbatim `zellij list-sessions -n` output (0.45.0): a live session, a session
    /// whose name holds a space, and a dead-but-resurrectable one. zellij prints a
    /// trailing space where the suffix is empty, so the lines carry it too.
    const SESSIONS: &str = "hug [Created 3h 5m 15s ago] \n\
        my build [Created 55m 10s ago] \n\
        gone [Created 36s ago] (EXITED - attach to resurrect)\n\
        fresh [Created 0s ago] \n";

    #[test]
    fn a_session_carries_its_name_and_kind() {
        let got = parse_sessions("jup", SESSIONS);
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["hug", "my build", "fresh"],
            "a name may hold a space, so the split is on the Created marker"
        );
        assert!(got.iter().all(|s| s.host == "jup" && s.mux == "zellij"));
    }

    #[test]
    fn a_resurrectable_session_is_not_offered() {
        // zellij lists a session it kept a resurrectable record of beside the live
        // ones. Nothing is running behind it, so attaching would resurrect it rather
        // than show it.
        let got = parse_sessions("jup", SESSIONS);
        assert!(
            !got.iter().any(|s| s.name == "gone"),
            "an EXITED record is not a session to switch to: {got:?}"
        );
    }

    #[test]
    fn the_listing_has_no_count_and_attachment_is_only_the_current_session() {
        // Only the session the command ran inside is reported as attached.
        let got = parse_sessions("jup", SESSIONS);
        assert!(got.iter().all(|s| s.windows == 0));
        assert!(got.iter().all(|s| !s.attached));
        let inside = parse_sessions("local", "hug [Created 1m ago] (current)\n");
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
            assert!(parse_sessions("jup", junk).is_empty(), "skipped: {junk:?}");
        }
    }

    /// `ss -xn` rows narrowed to a client's peers, verbatim from a Debian 12 machine with
    /// zellij 0.45.1: the client's internal socket pair, which has no path, and the
    /// server end of its connection to a session whose name holds a space.
    const CONNECTED: &str = "u_str ESTAB 0      0                                                 * 6125772            * 6125773       
        u_str ESTAB 0      0      /tmp/zellij-1000/contract_version_1/my build 6119266            * 6110103       
        u_str ESTAB 0      0                                                 * 6125773            * 6125772       
";

    #[test]
    fn the_server_end_of_the_connection_names_the_session() {
        assert_eq!(connected_session(CONNECTED).as_deref(), Some("my build"));
    }

    #[test]
    fn no_single_server_end_is_no_answer() {
        for out in [
            "",
            "u_str ESTAB 0 0 * 6125772 * 6125773
",
            "u_str ESTAB 0 0 /run/user/1000/zellij/api 1 * x
",
            "u_str ESTAB 0 0 /run/user/1000/zellij/api 1 2 3
",
            "u_str ESTAB 0 0 /run/user/1000/zellij/api 1 * 2
             u_str ESTAB 0 0 /run/user/1000/zellij/web 3 * 4
",
        ] {
            assert_eq!(connected_session(out), None, "{out:?}");
        }
        assert_eq!(
            connected_session(
                "u_str ESTAB 0 0 /run/user/1000/zellij/api 1 * 2
                 u_str ESTAB 0 0 /run/user/1000/zellij/api 5 * 6
"
            )
            .as_deref(),
            Some("api"),
            "two connections to one server are one answer"
        );
    }
}
