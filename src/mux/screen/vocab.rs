//! GNU screen argv builders and parsers. Screen shares no argv with tmux, so these
//! are screen-native: `-ls` lists sessions, `-x` attaches in multi-display mode,
//! and `-dmS` starts a detached session. Parsers are pure over the raw output.

use crate::session::Session;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `screen -ls` — lists this user's screen sessions. Exits 0 with sockets present,
/// 1 (stdout `No Sockets found`) when empty.
pub fn list_sessions(bin: &str) -> Vec<String> {
    argv(&[bin, "-ls"])
}

/// `screen -x <name>` — attach in multi-display mode. Unlike `-r` (detached-only) it
/// attaches whether the session is detached or already attached elsewhere, which is
/// the attach a switcher needs: xmux adds its own display client without kicking one.
pub fn attach(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "-x", name])
}

/// `screen -dmS <name>` — start a DETACHED session. Prints nothing, so `manage::create`
/// keeps the requested name.
pub fn new_session(bin: &str, name: &str) -> Vec<String> {
    if name.is_empty() {
        argv(&[bin, "-dmS"])
    } else {
        argv(&[bin, "-dmS", name])
    }
}

/// `screen -S <name> -X select <index>` — makes window `index` active server-side
/// (all attached displays follow).
pub fn select_window(bin: &str, name: &str, index: i64) -> Vec<String> {
    argv(&[bin, "-S", name, "-X", "select", &index.to_string()])
}

/// Parses `screen -ls` output into sessions tagged with `source`/`mux`. Each socket
/// line is `\t<pid>.<name>\t(<date> <time> <ampm>)\t(<state>)`; the name is everything
/// after the first dot, and `attached` is read from the state column. Lines that carry
/// no socket id (header/footer) or a non-numeric pid are skipped so banners cannot
/// poison the list. `windows`/`last_attached` are unknown from `-ls`, so they are 0.
pub fn parse_sessions(source: &str, mux: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in out.split('\n') {
        let ln = ln.strip_suffix('\r').unwrap_or(ln);
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let id = fields[1];
        let Some((pid, name)) = id.split_once('.') else {
            continue;
        };
        if pid.is_empty() || name.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let state = fields[3].to_lowercase();
        let attached = state.contains("attached") && !state.contains("detached");
        sessions.push(Session {
            source: source.to_string(),
            name: name.to_string(),
            mux: mux.to_string(),
            windows: 0,
            attached,
            last_attached: 0,
        });
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_sessions_is_dash_ls() {
        assert_eq!(list_sessions("screen"), argv(&["screen", "-ls"]));
    }

    #[test]
    fn attach_is_multi_display_dash_x() {
        assert_eq!(attach("screen", "api"), argv(&["screen", "-x", "api"]));
    }

    #[test]
    fn new_session_is_detached_creation() {
        assert_eq!(
            new_session("screen", "dev"),
            argv(&["screen", "-dmS", "dev"])
        );
    }

    #[test]
    fn new_session_empty_is_bare_creation() {
        assert_eq!(new_session("screen", ""), argv(&["screen", "-dmS"]));
    }

    #[test]
    fn select_window_sends_select_via_dash_x() {
        assert_eq!(
            select_window("screen", "dev", 2),
            argv(&["screen", "-S", "dev", "-X", "select", "2"])
        );
    }

    #[test]
    fn parse_sessions_reads_the_ls_listing() {
        let out = concat!(
            "There are screens on:\r\n",
            "\t2589.parsetest\t(08/25/2026 11:05:05 PM)\t(Detached)\r\n",
            "\t4190.alpha\t(08/25/2026 11:00:07 PM)\t(Attached)\r\n",
            "3 Sockets in /run/screen/S-hrlee.\r\n",
        );
        let got = parse_sessions("jup", "screen", out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "parsetest");
        assert!(!got[0].attached);
        assert_eq!(got[1].name, "alpha");
        assert!(got[1].attached);
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "screen"));
    }

    #[test]
    fn parse_sessions_skips_banners_and_footers() {
        let out = concat!(
            "There is a screen on:\n",
            "\t1.work\t(08/25/2026 11:00:00 AM)\t(Detached)\n",
            "1 Socket in /run/screen/S-hrlee.\n",
        );
        let got = parse_sessions("local", "screen", out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "work");
    }

    #[test]
    fn parse_sessions_name_keeps_dots_and_spaces() {
        let out = "There are screens on:\n\t1.foo.bar\t(08/25/2026 11:00:00 AM)\t(Detached)\n\t2.my sess\t(08/25/2026 11:00:00 AM)\t(Detached)\n";
        let got = parse_sessions("local", "screen", out);
        assert_eq!(got[0].name, "foo.bar");
        assert_eq!(got[1].name, "my sess");
    }

    #[test]
    fn parse_sessions_empty() {
        assert!(parse_sessions("local", "screen", "").is_empty());
    }
}
