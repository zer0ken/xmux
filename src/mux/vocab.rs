//! Builds the argv for mux (tmux/psmux) subcommands and parses their
//! tab-delimited output. Builders are pure: they assemble `Vec<String>` argv with
//! no shell involved (`argv[0]` is the mux binary name). Parsers are pure
//! functions over the raw command output.

use crate::session::Session;

/// The `list-sessions -F` template. The free-form session name is LAST so a tab
/// inside a name cannot shift the fixed numeric columns.
pub const SESSION_FORMAT: &str = "#{session_windows}\t#{session_attached}\t#{session_name}";

/// Whether `key` is a mux session variable that a child spawned by xmux must not
/// inherit (it would mis-target the server or be refused as nesting). This is the
/// SSOT for the mux env vars: matches exactly tmux's session markers and any
/// psmux var; NOT a blanket `TMUX` prefix, which would also drop unrelated vars like
/// `TMUX_TMPDIR` (selects the socket dir) or `TMUXP_*` (the separate tmuxp tool).
pub fn is_mux_var(key: &str) -> bool {
    matches!(key, "TMUX" | "TMUX_PANE") || key.starts_with("PSMUX")
}

/// From a set of env var names, the subset that are mux session vars - the keys a
/// child spawned by xmux must have cleared. Lets a spawner strip mux vars from its
/// environment without itself naming any mux var (the list stays here).
pub fn mux_env_keys_to_clear(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    keys.into_iter().filter(|k| is_mux_var(k)).collect()
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Lists all sessions on the server in [`SESSION_FORMAT`].
pub fn list_sessions(bin: &str) -> Vec<String> {
    argv(&[bin, "list-sessions", "-F", SESSION_FORMAT])
}

/// Attaches the current client to session `name`.
pub fn attach(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "attach", "-t", name])
}

/// Creates-or-attaches a DETACHED session and prints its assigned name. `-A`
/// makes it idempotent, `-d` keeps it detached, and `-P -F` prints the assigned
/// name even when the mux auto-names (e.g. `"0"`). A non-empty name is requested
/// with `-s`; an empty name lets the mux auto-name.
pub fn new_session(bin: &str, name: &str) -> Vec<String> {
    let mut v = argv(&[
        bin,
        "new-session",
        "-A",
        "-d",
        "-P",
        "-F",
        "#{session_name}",
    ]);
    if !name.is_empty() {
        v.push("-s".to_string());
        v.push(name.to_string());
    }
    v
}

/// Quotes a `-t` target for a CONTROL-MODE command line (the tmux/psmux command
/// parser, not a shell). A name of only safe characters passes through bare;
/// anything else (space, quote, metachar) is single-quoted with embedded single
/// quotes escaped as `'\''` - the parser reads a backslash-escaped quote outside
/// quotes as a literal, so `a'b` becomes `'a'\''b'`.
pub fn quote_target(t: &str) -> String {
    let safe = !t.is_empty()
        && t.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'/' | b'@' | b'%')
        });
    if safe {
        t.to_string()
    } else {
        format!("'{}'", t.replace('\'', "'\\''"))
    }
}

/// Splits raw mux output into non-blank lines, tolerating both `\r\n` and `\n`.
fn split_lines(out: &str) -> Vec<&str> {
    out.split('\n')
        .map(|ln| ln.strip_suffix('\r').unwrap_or(ln))
        .filter(|ln| !ln.is_empty())
        .collect()
}

/// Parses `list-sessions` output ([`SESSION_FORMAT`]) into sessions tagged with
/// `source` and the enumerating mux's `mux` kind. Malformed lines (short,
/// non-numeric numeric columns, or empty name) are skipped so banners and garbage
/// cannot poison the list. The name is rejoined from `fields[2..]` so a tab
/// inside a name survives. Order is preserved.
pub fn parse_sessions(source: &str, mux: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in split_lines(out) {
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 3 {
            continue;
        }
        let Ok(windows) = fields[0].parse::<i64>() else {
            continue;
        };
        let Ok(attached_n) = fields[1].parse::<i64>() else {
            continue;
        };
        let name = fields[2..].join("\t");
        if name.is_empty() {
            continue;
        }
        sessions.push(Session {
            source: source.to_string(),
            name,
            mux: mux.to_string(),
            windows,
            attached: attached_n > 0,
        });
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn session_format_template() {
        assert_eq!(
            SESSION_FORMAT,
            "#{session_windows}\t#{session_attached}\t#{session_name}"
        );
    }

    #[test]
    fn list_sessions_argv() {
        assert_eq!(
            list_sessions("tmux"),
            sv(&["tmux", "list-sessions", "-F", SESSION_FORMAT])
        );
    }

    #[test]
    fn is_mux_var_matches_exactly_tmux_and_psmux_markers() {
        // Strips exactly tmux's session markers and psmux vars.
        assert!(is_mux_var("TMUX"));
        assert!(is_mux_var("TMUX_PANE"));
        assert!(is_mux_var("PSMUX_SESSION"));
        // Keeps unrelated vars that merely share the TMUX prefix.
        assert!(!is_mux_var("TMUXP_LAYOUT")); // tmuxp, a different tool
        assert!(!is_mux_var("TMUX_TMPDIR")); // selects the socket dir - must survive
        assert!(!is_mux_var("PATH"));
    }

    #[test]
    fn mux_env_keys_to_clear_selects_only_mux_vars() {
        // The caller (display's attach spawner) hands us the current process env
        // keys; we return exactly the mux session vars to strip, order preserved.
        let out = mux_env_keys_to_clear(
            ["TMUX", "PATH", "PSMUX_SESSION", "TMUX_PANE", "TMUX_TMPDIR"]
                .into_iter()
                .map(String::from),
        );
        assert_eq!(out, vec!["TMUX", "PSMUX_SESSION", "TMUX_PANE"]);
    }

    #[test]
    fn quote_target_bare_and_quoted() {
        // Safe names pass through bare (so simple sessions/windows are unchanged).
        assert_eq!(quote_target("0"), "0");
        assert_eq!(quote_target("editor:1"), "editor:1");
        assert_eq!(quote_target("api-2"), "api-2");
        // Spaces and quotes are escaped for the control-mode parser.
        assert_eq!(quote_target("my proj"), "'my proj'");
        assert_eq!(quote_target("a'b"), "'a'\\''b'");
        assert_eq!(quote_target(""), "''");
    }

    #[test]
    fn attach_argv() {
        assert_eq!(
            attach("tmux", "main"),
            sv(&["tmux", "attach", "-t", "main"])
        );
    }

    #[test]
    fn new_session_named_argv() {
        assert_eq!(
            new_session("tmux", "dev"),
            sv(&[
                "tmux",
                "new-session",
                "-A",
                "-d",
                "-P",
                "-F",
                "#{session_name}",
                "-s",
                "dev"
            ])
        );
    }

    #[test]
    fn new_session_auto_argv() {
        assert_eq!(
            new_session("tmux", ""),
            sv(&[
                "tmux",
                "new-session",
                "-A",
                "-d",
                "-P",
                "-F",
                "#{session_name}"
            ])
        );
    }

    #[test]
    fn parse_sessions_basic() {
        let out = "3\t1\tmain\n2\t0\tother\n";
        let got = parse_sessions("local", "tmux", out);
        assert_eq!(
            got,
            vec![
                Session {
                    source: "local".into(),
                    name: "main".into(),
                    mux: "tmux".into(),
                    windows: 3,
                    attached: true,
                },
                Session {
                    source: "local".into(),
                    name: "other".into(),
                    mux: "tmux".into(),
                    windows: 2,
                    attached: false,
                },
            ]
        );
    }

    #[test]
    fn parse_sessions_crlf() {
        let out = "1\t1\ta\r\n1\t0\tb\r\n";
        let got = parse_sessions("local", "tmux", out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
    }

    #[test]
    fn parse_sessions_name_with_tab_and_slash() {
        let out = "4\t1\tproj/a\tb\n";
        let got = parse_sessions("ssh-host", "tmux", out);
        assert_eq!(
            got,
            vec![Session {
                source: "ssh-host".into(),
                mux: "tmux".into(),
                name: "proj/a\tb".into(),
                windows: 4,
                attached: true,
            }]
        );
    }

    #[test]
    fn parse_sessions_skips_garbage() {
        let out = concat!(
            "some random banner text\n",
            "\n",
            "x\t1\tbadwin\n",
            "1\tnope\tbadattach\n",
            "1\t1\t\n",
            "2\t1\tgood\n",
        );
        let got = parse_sessions("local", "tmux", out);
        assert_eq!(
            got,
            vec![Session {
                source: "local".into(),
                name: "good".into(),
                mux: "tmux".into(),
                windows: 2,
                attached: true,
            }]
        );
    }

    #[test]
    fn parse_sessions_empty_output() {
        assert!(parse_sessions("local", "tmux", "").is_empty());
    }

    #[test]
    fn parse_sessions_order_preserved() {
        let out = "1\t0\tz\n1\t0\ta\n1\t0\tm\n";
        let got = parse_sessions("local", "tmux", out);
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["z", "a", "m"]);
    }
}
