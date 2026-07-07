//! Builds the argv for mux (tmux/psmux) subcommands and parses their
//! tab-delimited output. Builders are pure: they assemble `Vec<String>` argv with
//! no shell involved (`argv[0]` is the mux binary name). Parsers are pure
//! functions over the raw command output.

use crate::session::{Pane, Session, WindowPanes};

/// The `list-sessions -F` template. The free-form session name is LAST so a tab
/// inside a name cannot shift the fixed numeric columns.
pub const SESSION_FORMAT: &str =
    "#{session_windows}\t#{session_attached}\t#{session_last_attached}\t#{session_name}";

/// The `list-panes -F` template. The free-form window name is LAST so a tab
/// inside it cannot shift the fixed columns; `pane_current_command` sits in a
/// fixed slot because a process name has no tabs.
pub const PANE_FORMAT: &str =
    "#{window_index}\t#{window_active}\t#{pane_index}\t#{pane_active}\t#{pane_current_command}\t#{window_name}";

/// Whether `key` is a mux session variable that a child spawned by xmux must not
/// inherit (it would mis-target the server or be refused as nesting). This is the
/// SSOT for the mux env vocabulary: matches exactly tmux's session markers and any
/// psmux var; NOT a blanket `TMUX` prefix, which would also drop unrelated vars like
/// `TMUX_TMPDIR` (selects the socket dir) or `TMUXP_*` (the separate tmuxp tool).
pub fn is_mux_var(key: &str) -> bool {
    matches!(key, "TMUX" | "TMUX_PANE") || key.starts_with("PSMUX")
}

/// From a set of env var names, the subset that are mux session vars — the keys a
/// child spawned by xmux must have cleared. Lets a spawner strip mux vars from its
/// environment without itself naming any mux var (the vocabulary stays here).
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

/// Lists every pane across ALL windows of session `name`. The `-s` flag widens
/// scope to the whole session (never `-a`, which leaks across servers).
pub fn list_panes(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "list-panes", "-s", "-t", name, "-F", PANE_FORMAT])
}

/// Attaches the current client to session `name`.
pub fn attach(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "attach", "-t", name])
}

/// Reads one global server option's value: `<bin> show -gv <name>`. `-g` reads the
/// global scope and `-v` prints just the value, so stdout is the option string
/// (e.g. `pane-active-border-style` → `fg=blue,bg=default`). Used to match the
/// view border colours to the displayed session's live mux server.
pub fn show_option(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "show", "-gv", name])
}

/// Extracts the foreground colour token from a tmux style string (the value of a
/// `pane-*-border-style` option). tmux styles are comma-separated attributes, so a
/// `fg=<colour>` part yields its colour; a bare single token (`green`, `default`)
/// is itself the fg; anything else (only `bg=`/attributes, or empty) yields `""`,
/// letting the caller fall back to the configured/default colour. Complements
/// [`crate::ui::chrome::map_color`], which tolerates a leading `fg=` but not the
/// comma-separated form.
pub fn border_fg(style: &str) -> String {
    let s = style.trim();
    if s.is_empty() {
        return String::new();
    }
    for part in s.split(',') {
        if let Some(fg) = part.trim().strip_prefix("fg=") {
            return fg.trim().to_string();
        }
    }
    // A bare token with no `=` is itself the colour (`green`, `default`).
    if !s.contains('=') {
        return s.to_string();
    }
    String::new()
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

/// Builds a `"session:window"` target.
pub fn window_target(session: &str, window: i64) -> String {
    format!("{session}:{window}")
}

/// The session-name part of a `-t` target: everything before the first `:`
/// (`api:2` → `api`, `api` → `api`). tmux/psmux session names cannot contain `:`
/// (it separates `session:window`), so the split is unambiguous. Used to validate
/// an active-pane probe against the session that `%session-changed` reports.
pub fn session_name(target: &str) -> &str {
    target.split(':').next().unwrap_or(target)
}

/// Quotes a `-t` target for a CONTROL-MODE command line (the tmux/psmux command
/// parser, not a shell). A name of only safe characters passes through bare;
/// anything else (space, quote, metachar) is single-quoted with embedded single
/// quotes escaped as `'\''` — the parser reads a backslash-escaped quote outside
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

/// Makes the target window active in its session.
pub fn select_window(bin: &str, target: &str) -> Vec<String> {
    argv(&[bin, "select-window", "-t", target])
}

/// Kills session `name`.
pub fn kill_session(bin: &str, name: &str) -> Vec<String> {
    argv(&[bin, "kill-session", "-t", name])
}

/// Creates a new window in `session` (optionally named). The target is
/// `<session>:` (trailing colon) so a numeric session name — `"0"`, the
/// tmux/psmux default — is parsed as the SESSION, not a window index: a bare
/// `-t 0` is read as "create at window index 0" and fails with "index 0 in use".
pub fn new_window(bin: &str, session: &str, name: &str) -> Vec<String> {
    let target = format!("{session}:");
    let mut v = argv(&[bin, "new-window", "-t", &target]);
    if !name.is_empty() {
        v.push("-n".to_string());
        v.push(name.to_string());
    }
    v
}

/// Splits `target` (a `session` or `session:window`): `-v` stacks the new pane
/// below, `-h` puts it to the right.
pub fn split_window(bin: &str, target: &str, vertical: bool) -> Vec<String> {
    argv(&[
        bin,
        "split-window",
        if vertical { "-v" } else { "-h" },
        "-t",
        target,
    ])
}

/// Renames session `old_name` to `new_name`.
pub fn rename_session(bin: &str, old_name: &str, new_name: &str) -> Vec<String> {
    argv(&[bin, "rename-session", "-t", old_name, new_name])
}

/// Kills the window `target` (`session:window`).
pub fn kill_window(bin: &str, target: &str) -> Vec<String> {
    argv(&[bin, "kill-window", "-t", target])
}

/// Renames the window `target` (`session:window`) to `new_name`.
pub fn rename_window(bin: &str, target: &str, new_name: &str) -> Vec<String> {
    argv(&[bin, "rename-window", "-t", target, new_name])
}

/// Splits raw mux output into non-blank lines, tolerating both `\r\n` and `\n`.
fn split_lines(out: &str) -> Vec<&str> {
    out.split('\n')
        .map(|ln| ln.strip_suffix('\r').unwrap_or(ln))
        .filter(|ln| !ln.is_empty())
        .collect()
}

/// Parses `list-sessions` output ([`SESSION_FORMAT`]) into sessions tagged with
/// `source`. Malformed lines (short, non-numeric numeric columns, or empty name)
/// are skipped so banners and garbage cannot poison the list. The name is
/// rejoined from `fields[3..]` so a tab inside a name survives. Order is
/// preserved.
pub fn parse_sessions(source: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in split_lines(out) {
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let Ok(windows) = fields[0].parse::<i64>() else {
            continue;
        };
        let Ok(attached_n) = fields[1].parse::<i64>() else {
            continue;
        };
        let last_attached = if fields[2].is_empty() {
            0
        } else {
            match fields[2].parse::<i64>() {
                Ok(n) => n,
                Err(_) => continue,
            }
        };
        let name = fields[3..].join("\t");
        if name.is_empty() {
            continue;
        }
        sessions.push(Session {
            source: source.to_string(),
            name,
            windows,
            attached: attached_n > 0,
            last_attached,
        });
    }
    sessions
}

/// Parses `list-panes` output ([`PANE_FORMAT`]) into windows-and-panes, grouping
/// panes by `window_index` in first-seen order. Each window takes its index,
/// name, and active flag from the first row seen for that window; the window name
/// is rejoined from `fields[5..]` so a tab inside it survives. Malformed lines
/// (short or non-numeric `window_index`) are skipped.
pub fn parse_panes(out: &str) -> Vec<WindowPanes> {
    let mut windows: Vec<WindowPanes> = Vec::new();
    let mut pos: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for ln in split_lines(out) {
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 6 {
            continue;
        }
        let Ok(win_idx) = fields[0].parse::<i64>() else {
            continue;
        };
        let Ok(win_active) = fields[1].parse::<i64>() else {
            continue;
        };
        let Ok(pane_idx) = fields[2].parse::<i64>() else {
            continue;
        };
        let Ok(pane_active) = fields[3].parse::<i64>() else {
            continue;
        };
        let command = fields[4].to_string();
        let win_name = fields[5..].join("\t");

        let pane = Pane {
            index: pane_idx,
            active: pane_active > 0,
            command,
        };
        if let Some(&i) = pos.get(&win_idx) {
            windows[i].panes.push(pane);
            continue;
        }
        pos.insert(win_idx, windows.len());
        windows.push(WindowPanes {
            index: win_idx,
            name: win_name,
            active: win_active > 0,
            panes: vec![pane],
        });
    }
    windows
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
            "#{session_windows}\t#{session_attached}\t#{session_last_attached}\t#{session_name}"
        );
    }

    #[test]
    fn pane_format_template() {
        assert_eq!(
            PANE_FORMAT,
            "#{window_index}\t#{window_active}\t#{pane_index}\t#{pane_active}\t#{pane_current_command}\t#{window_name}"
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
        assert!(!is_mux_var("TMUX_TMPDIR")); // selects the socket dir — must survive
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
    fn list_panes_argv() {
        assert_eq!(
            list_panes("psmux", "work"),
            sv(&["psmux", "list-panes", "-s", "-t", "work", "-F", PANE_FORMAT])
        );
    }

    #[test]
    fn attach_argv() {
        assert_eq!(
            attach("tmux", "main"),
            sv(&["tmux", "attach", "-t", "main"])
        );
    }

    #[test]
    fn border_fg_extracts_fg_component() {
        // The fg part of a comma-separated style; a bare token is itself the colour.
        assert_eq!(border_fg("fg=blue,bg=default"), "blue");
        assert_eq!(border_fg("default"), "default");
        // Only a bg (no fg) → empty, so the caller falls back.
        assert_eq!(border_fg("bg=red"), "");
        assert_eq!(border_fg(""), "");
        // The option-read argv is `show -gv <name>`.
        assert_eq!(
            show_option("tmux", "pane-border-style"),
            sv(&["tmux", "show", "-gv", "pane-border-style"])
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
    fn kill_session_argv() {
        assert_eq!(
            kill_session("tmux", "old"),
            sv(&["tmux", "kill-session", "-t", "old"])
        );
    }

    #[test]
    fn rename_session_argv() {
        assert_eq!(
            rename_session("tmux", "old", "new"),
            sv(&["tmux", "rename-session", "-t", "old", "new"])
        );
    }

    #[test]
    fn kill_and_rename_window_argv() {
        assert_eq!(
            kill_window("tmux", "api:2"),
            sv(&["tmux", "kill-window", "-t", "api:2"])
        );
        assert_eq!(
            rename_window("tmux", "api:2", "logs"),
            sv(&["tmux", "rename-window", "-t", "api:2", "logs"])
        );
    }

    #[test]
    fn target_builders() {
        assert_eq!(window_target("editor", 2), "editor:2");
    }

    #[test]
    fn session_name_strips_window_suffix() {
        assert_eq!(session_name("api"), "api");
        assert_eq!(session_name("api:2"), "api");
        assert_eq!(session_name("0:1"), "0");
        assert_eq!(session_name(""), "");
    }

    #[test]
    fn new_window_targets_session_unambiguously() {
        // The target carries a trailing `:` so a numeric session name (e.g. "0",
        // the tmux/psmux default) is parsed as the SESSION, not a window index — a
        // bare `-t 0` is read as "window index 0" and fails "index 0 in use".
        assert_eq!(
            new_window("tmux", "0", ""),
            sv(&["tmux", "new-window", "-t", "0:"])
        );
        assert_eq!(
            new_window("tmux", "work", "logs"),
            sv(&["tmux", "new-window", "-t", "work:", "-n", "logs"])
        );
    }

    #[test]
    fn select_window_argv() {
        assert_eq!(
            select_window("tmux", "if:3"),
            sv(&["tmux", "select-window", "-t", "if:3"])
        );
    }

    #[test]
    fn parse_sessions_basic() {
        let out = "3\t1\t1700000000\tmain\n2\t0\t1699999999\tother\n";
        let got = parse_sessions("local", out);
        assert_eq!(
            got,
            vec![
                Session {
                    source: "local".into(),
                    name: "main".into(),
                    windows: 3,
                    attached: true,
                    last_attached: 1700000000
                },
                Session {
                    source: "local".into(),
                    name: "other".into(),
                    windows: 2,
                    attached: false,
                    last_attached: 1699999999
                },
            ]
        );
    }

    #[test]
    fn parse_sessions_crlf() {
        let out = "1\t1\t100\ta\r\n1\t0\t200\tb\r\n";
        let got = parse_sessions("local", out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
    }

    #[test]
    fn parse_sessions_name_with_tab_and_slash() {
        let out = "4\t1\t1700000000\tproj/a\tb\n";
        let got = parse_sessions("ssh-host", out);
        assert_eq!(
            got,
            vec![Session {
                source: "ssh-host".into(),
                name: "proj/a\tb".into(),
                windows: 4,
                attached: true,
                last_attached: 1700000000
            }]
        );
    }

    #[test]
    fn parse_sessions_empty_last_attached() {
        let out = "1\t0\t\tlegacy\n";
        let got = parse_sessions("local", out);
        assert_eq!(
            got,
            vec![Session {
                source: "local".into(),
                name: "legacy".into(),
                windows: 1,
                attached: false,
                last_attached: 0
            }]
        );
    }

    #[test]
    fn parse_sessions_skips_garbage() {
        let out = concat!(
            "some random banner text\n",
            "\n",
            "x\t1\t100\tbadwin\n",
            "1\tnope\t100\tbadattach\n",
            "1\t1\tabc\tbadtime\n",
            "1\t1\t100\t\n",
            "2\t1\t300\tgood\n",
        );
        let got = parse_sessions("local", out);
        assert_eq!(
            got,
            vec![Session {
                source: "local".into(),
                name: "good".into(),
                windows: 2,
                attached: true,
                last_attached: 300
            }]
        );
    }

    #[test]
    fn parse_sessions_empty_output() {
        assert!(parse_sessions("local", "").is_empty());
    }

    #[test]
    fn parse_sessions_order_preserved() {
        let out = "1\t0\t1\tz\n1\t0\t2\ta\n1\t0\t3\tm\n";
        let got = parse_sessions("local", out);
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["z", "a", "m"]);
    }

    #[test]
    fn parse_panes_basic() {
        let out = concat!(
            "0\t1\t0\t1\tbash\teditor\n",
            "0\t1\t1\t0\tvim\teditor\n",
            "1\t0\t0\t1\tssh\tserver\n",
        );
        let got = parse_panes(out);
        assert_eq!(
            got,
            vec![
                WindowPanes {
                    index: 0,
                    name: "editor".into(),
                    active: true,
                    panes: vec![
                        Pane {
                            index: 0,
                            active: true,
                            command: "bash".into()
                        },
                        Pane {
                            index: 1,
                            active: false,
                            command: "vim".into()
                        },
                    ]
                },
                WindowPanes {
                    index: 1,
                    name: "server".into(),
                    active: false,
                    panes: vec![Pane {
                        index: 0,
                        active: true,
                        command: "ssh".into()
                    }]
                },
            ]
        );
    }

    #[test]
    fn parse_panes_window_name_with_spaces_and_tab() {
        let out = "2\t1\t0\t1\tzsh\tmy window\tname\n";
        let got = parse_panes(out);
        assert_eq!(
            got,
            vec![WindowPanes {
                index: 2,
                name: "my window\tname".into(),
                active: true,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "zsh".into()
                }]
            }]
        );
    }

    #[test]
    fn parse_panes_grouping_order_preserved() {
        let out = concat!(
            "5\t1\t0\t1\ta\tfive\n",
            "2\t0\t0\t1\tb\ttwo\n",
            "5\t1\t1\t0\tc\tfive\n",
            "2\t0\t1\t0\td\ttwo\n",
        );
        let got = parse_panes(out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].index, 5);
        assert_eq!(got[1].index, 2);
        assert_eq!(got[0].panes.len(), 2);
        assert_eq!(got[1].panes.len(), 2);
        assert_eq!(got[0].panes[0].command, "a");
        assert_eq!(got[0].panes[1].command, "c");
    }

    #[test]
    fn parse_panes_skips_short_lines() {
        let out = concat!(
            "short line\n",
            "0\t1\t0\n",
            "\n",
            "x\t1\t0\t1\tbash\twin\n",
            "0\t1\t0\t1\tbash\twin\n",
        );
        let got = parse_panes(out);
        assert_eq!(
            got,
            vec![WindowPanes {
                index: 0,
                name: "win".into(),
                active: true,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "bash".into()
                }]
            }]
        );
    }

    #[test]
    fn parse_panes_empty_output() {
        assert!(parse_panes("").is_empty());
    }
}
