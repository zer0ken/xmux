//! Where xmux's own herdr client is, read from the host the client runs on.
//!
//! A herdr client attaches to one session, its `Local`, and moves between saved SSH
//! machines from inside itself; each saved machine names one session on one host. herdr
//! reports a client's current endpoint nowhere per client. The one record of a move is the
//! endpoint selection every client of the user on that host writes when it switches and
//! reads when it starts, which `herdr machine list --json` reports as `selected`. That
//! record describes xmux's client only while xmux's client is the user's one herdr client
//! there, so the query also lists the user's herdr processes and the answer is pinned on
//! xmux's client only when it is the single one.

use super::super::{client_pid_assignment, ClientAt, DisplayClient};
use serde::Deserialize;

/// The byte the query writes between a process's arguments, standing for the NUL that
/// separates them in `/proc/<pid>/cmdline`, so an argument with a space stays whole.
const ARG_SEP: char = '\u{1f}';

/// The query's shell text: the user's herdr processes with their arguments, the client's
/// own pid, the machine's host name, then herdr's saved machine listing.
pub(super) fn query(bin: &str, client: &DisplayClient) -> Vec<String> {
    let bin = crate::transport::vocab::quote(bin);
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{}[ -r /proc/\"$p\"/cmdline ] || exit 0; u=$(id -u); s=$(printf '\\037'); \
             for d in /proc/[0-9]*; do \
             [ \"$(stat -c %u \"$d\" 2>/dev/null)\" = \"$u\" ] || continue; \
             c=$(tr '\\0' '\\037' <\"$d\"/cmdline 2>/dev/null) || continue; \
             case \"$c\" in herdr\"$s\"*|*/herdr\"$s\"*|herdr|*/herdr) echo \"proc ${{d#/proc/}} $c\";; esac; \
             done; echo \"self $p\"; echo \"host $(cat /proc/sys/kernel/hostname)\"; echo list; \
             {bin} machine list --json",
            client_pid_assignment(client)
        ),
    ]
}

#[derive(Deserialize)]
struct SavedMachine {
    label: String,
    target: String,
    session: String,
    #[serde(default)]
    selected: bool,
}

/// Where the query output places xmux's client. `None` when the client is gone, when the
/// user runs any other herdr client on the host, or when the listing does not parse.
pub(super) fn parse(out: &str) -> Option<ClientAt> {
    let (head, listing) = out.split_once("\nlist\n")?;
    let mut clients = Vec::new();
    let mut own = None;
    let mut host = "";
    for line in head.lines() {
        if let Some(rest) = line.strip_prefix("proc ") {
            let (pid, args) = rest.split_once(' ')?;
            if let Some(session) = client_session(args) {
                clients.push((pid, session));
            }
        } else if let Some(pid) = line.strip_prefix("self ") {
            own = Some(pid);
        } else if let Some(name) = line.strip_prefix("host ") {
            host = name;
        }
    }
    let [(pid, local)] = clients.as_slice() else {
        return None;
    };
    if Some(*pid) != own {
        return None;
    }
    let machines: Vec<SavedMachine> = serde_json::from_str(listing).ok()?;
    let Some(selected) = machines.into_iter().find(|machine| machine.selected) else {
        return Some(ClientAt::Session(local.clone()));
    };
    if is_this_machine(&selected.target, host) {
        Some(ClientAt::Session(selected.session))
    } else {
        // The label is drawn on a card, so a control character in it never reaches the
        // terminal.
        let label = selected.label.chars().filter(|c| !c.is_control()).collect();
        Some(ClientAt::Away(label))
    }
}

/// The session a herdr process attached as a TUI client runs on, from its arguments, or
/// `None` for any other herdr process (a server, a bridge, a CLI command).
fn client_session(args: &str) -> Option<String> {
    let mut parts = args.split(ARG_SEP).filter(|part| !part.is_empty());
    let program = parts.next()?;
    if program.rsplit('/').next() != Some("herdr") {
        return None;
    }
    let rest: Vec<&str> = parts.collect();
    match rest.as_slice() {
        [] => Some("default".to_string()),
        ["--session", session]
        | ["--session", session, "client"]
        | ["session", "attach", session] => Some(session.to_string()),
        _ => None,
    }
}

/// Whether a saved machine's ssh target names the machine the client runs on: a loopback
/// name, or the machine's own host name, short or full.
fn is_this_machine(target: &str, host: &str) -> bool {
    let target = target.strip_prefix("ssh://").unwrap_or(target);
    let target = target.rsplit('@').next().unwrap_or(target);
    let name = match target.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(bracketed),
        None => target.split(':').next().unwrap_or(target),
    }
    .to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    let short = host.split('.').next().unwrap_or(&host);
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
        || (!host.is_empty() && (name == host || name == short))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_line(pid: u32, args: &[&str]) -> String {
        let mut joined = args.join(&ARG_SEP.to_string());
        joined.push(ARG_SEP);
        format!("proc {pid} {joined}")
    }

    fn output(procs: &[String], own: u32, host: &str, listing: &str) -> String {
        format!(
            "{}\nself {own}\nhost {host}\nlist\n{listing}",
            procs.join("\n")
        )
    }

    const NONE_SELECTED: &str = r#"[
      {"id": "d3", "label": "web/agents", "target": "web", "session": "agents", "enabled": true, "selected": false}
    ]"#;
    const AWAY: &str = r#"[
      {"id": "d3", "label": "web/agents", "target": "dev@web", "session": "agents", "enabled": true, "selected": true}
    ]"#;
    const LOOPBACK: &str = r#"[
      {"id": "a1", "label": "jup/herdr2", "target": "jup", "session": "herdr2", "enabled": true, "selected": true}
    ]"#;

    fn own_client() -> Vec<String> {
        vec![
            proc_line(40, &["herdr", "--session", "herdr1", "server"]),
            proc_line(
                41,
                &["/home/dev/.local/bin/herdr", "session", "attach", "herdr1"],
            ),
            proc_line(
                42,
                &[
                    "/home/dev/.local/bin/herdr",
                    "--session",
                    "agents",
                    "remote-client-bridge",
                ],
            ),
        ]
    }

    #[test]
    fn nothing_selected_is_the_session_the_client_attached() {
        assert_eq!(
            parse(&output(&own_client(), 41, "jup", NONE_SELECTED)),
            Some(ClientAt::Session("herdr1".into()))
        );
    }

    #[test]
    fn connect_only_client_is_attributed_and_a_server_is_not_a_client() {
        let processes = vec![
            proc_line(40, &["herdr", "--session", "herdr1", "server"]),
            proc_line(41, &["herdr", "--session", "herdr1", "client"]),
        ];
        assert_eq!(
            parse(&output(&processes, 41, "jup", NONE_SELECTED)),
            Some(ClientAt::Session("herdr1".into()))
        );
        assert_eq!(
            parse(&output(&processes, 41, "jup", AWAY)),
            Some(ClientAt::Away("web/agents".into()))
        );
    }

    #[test]
    fn a_machine_elsewhere_is_away_under_its_label() {
        assert_eq!(
            parse(&output(&own_client(), 41, "jup", AWAY)),
            Some(ClientAt::Away("web/agents".into()))
        );
        let escaped = AWAY.replace("web/agents", "web/\\u001b[2Jagents");
        assert_eq!(
            parse(&output(&own_client(), 41, "jup", &escaped)),
            Some(ClientAt::Away("web/[2Jagents".into()))
        );
    }

    #[test]
    fn a_saved_machine_on_this_host_is_a_session_here() {
        assert_eq!(
            parse(&output(&own_client(), 41, "jup.example.net", LOOPBACK)),
            Some(ClientAt::Session("herdr2".into()))
        );
        let localhost = LOOPBACK.replace(
            "\"target\": \"jup\"",
            "\"target\": \"ssh://dev@localhost:22\"",
        );
        assert_eq!(
            parse(&output(&own_client(), 41, "other", &localhost)),
            Some(ClientAt::Session("herdr2".into()))
        );
    }

    /// The selection is shared by every client of the user on the host, so a second
    /// client makes it impossible to say whose it is.
    #[test]
    fn a_second_client_on_the_host_answers_nothing() {
        let mut procs = own_client();
        procs.push(proc_line(50, &["herdr"]));
        assert_eq!(parse(&output(&procs, 41, "jup", AWAY)), None);
    }

    #[test]
    fn a_client_that_is_not_xmux_answers_nothing() {
        assert_eq!(parse(&output(&own_client(), 99, "jup", AWAY)), None);
        assert_eq!(
            parse(&output(&own_client(), 41, "jup", "error: unknown command")),
            None
        );
        assert_eq!(parse(""), None);
    }

    #[test]
    fn only_tui_clients_count() {
        assert_eq!(client_session("herdr\u{1f}"), Some("default".into()));
        assert_eq!(
            client_session("herdr\u{1f}--session\u{1f}a b\u{1f}"),
            Some("a b".into())
        );
        assert_eq!(
            client_session("herdr\u{1f}session\u{1f}list\u{1f}--json\u{1f}"),
            None
        );
        assert_eq!(
            client_session("herdr\u{1f}--session\u{1f}x\u{1f}server\u{1f}"),
            None
        );
        assert_eq!(client_session("notherdr\u{1f}"), None);
    }

    /// The query is POSIX shell. Where `sh` exists it must parse, and a client that is not
    /// there answers nothing.
    #[cfg(unix)]
    #[test]
    fn the_query_is_valid_shell_and_a_missing_client_answers_nothing() {
        let argv = query("herdr", &DisplayClient::Recorded("absent-record".into()));
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
    }

    #[test]
    fn the_query_names_the_client_by_pid_or_by_its_record() {
        let by_pid = query("herdr", &DisplayClient::Pid(4242));
        assert!(by_pid[2].starts_with("p=4242; "), "{by_pid:?}");
        assert!(
            by_pid[2].ends_with("herdr machine list --json"),
            "{by_pid:?}"
        );
        let recorded = query(
            "/opt/my herdr",
            &DisplayClient::Recorded("jup-x;rm-1".into()),
        );
        assert!(
            recorded[2].starts_with("p=$(cat /tmp/.xmux-client-jup-x_rm-1 2>/dev/null); "),
            "{recorded:?}"
        );
        assert!(recorded[2].ends_with("'/opt/my herdr' machine list --json"));
    }
}
