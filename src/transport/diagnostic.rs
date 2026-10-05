pub(crate) const MAX_DIAGNOSTIC: usize = 4096;

/// Restores the bytes OpenSSH renders as octal escapes when its diagnostic locale
/// cannot write them directly. Invalid byte sequences stay in their escaped form.
pub fn decode_openssh_octal(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let run_start = i;
        let mut escaped = Vec::new();
        while i + 3 < bytes.len()
            && bytes[i] == b'\\'
            && bytes[i + 1..i + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = u16::from(bytes[i + 1] - b'0') * 64
                + u16::from(bytes[i + 2] - b'0') * 8
                + u16::from(bytes[i + 3] - b'0');
            let Ok(value) = u8::try_from(value) else {
                break;
            };
            if value < 0x80 {
                break;
            }
            escaped.push(value);
            i += 4;
        }
        let decoded = (!escaped.is_empty() && escaped.iter().all(|value| *value >= 0x80))
            .then(|| std::str::from_utf8(&escaped).ok())
            .flatten();
        if let Some(decoded) = decoded {
            out.push_str(decoded);
        } else {
            if i == run_start {
                let ch = input[i..].chars().next().expect("valid input boundary");
                out.push(ch);
                i += ch.len_utf8();
            } else {
                out.push_str(&input[run_start..i]);
            }
        }
    }
    out
}

pub fn sanitize(input: &str) -> String {
    let decoded = decode_openssh_octal(input);
    let mut plain = String::with_capacity(decoded.len());
    let mut chars = decoded.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            if ch != '\r' && (ch == '\n' || ch == '\t' || !ch.is_control()) {
                plain.push(ch);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                        break;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && (!line.to_ascii_lowercase().contains("password:")
                    || line.starts_with("xmux askpass refused prompt:"))
                && !line.starts_with("xmux-shell:")
                // ssh's note that it saved a first-seen host key reports success, so
                // it would only push the failure's own line down.
                && !line.starts_with("Warning: Permanently added")
        })
        .collect();
    let result = lines.join("\n");
    if result.len() <= MAX_DIAGNOSTIC {
        return result;
    }
    let mut start = result.len() - MAX_DIAGNOSTIC;
    while !result.is_char_boundary(start) {
        start += 1;
    }
    result[start..].to_string()
}

/// OpenSSH's final authentication refusal, excluding remote command errors that happen
/// to contain the same words.
pub fn is_auth_refusal_line(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_suffix('.').unwrap_or(line);
    let Some((identity, methods)) = line.split_once(": Permission denied (") else {
        return false;
    };
    let identity = identity.split_whitespace().next_back().unwrap_or_default();
    !identity.is_empty()
        && identity.contains('@')
        && methods.ends_with(')')
        && !methods[..methods.len() - 1].is_empty()
}

pub fn contains_auth_refusal(stderr: &str) -> bool {
    stderr.lines().any(is_auth_refusal_line)
}

/// ssh refused a host whose recorded key no longer matches: its own warning banner
/// precedes the verification failure.
pub fn host_key_changed(stderr: &str) -> bool {
    stderr
        .to_ascii_lowercase()
        .contains("remote host identification has changed")
}

/// ssh refused a host it has no recorded key for. Without the changed-key banner, the
/// verification failure means a non-interactive ssh had no way to ask whether to trust
/// a first-seen key.
pub fn host_key_unknown(stderr: &str) -> bool {
    stderr
        .to_ascii_lowercase()
        .contains("host key verification failed")
        && !host_key_changed(stderr)
}

/// True when `stderr` is a failure the user can answer FROM xmux, which is the state
/// apart from unreachable. What the login pane collects decides the set: it takes the
/// address, the port, and the username, so every failure those three values can fix
/// belongs here.
///
/// Two of ssh's own refusals qualify. The auth failure carries `Permission denied (`
/// with the rejected-methods list; the `(` is ssh's own mark, which a generic mux
/// permission error does not have. A first-seen host key is the other: a background
/// probe cannot trust it, and the submitted login accepts a new key.
///
/// Connectivity, name resolution, remote command permissions, and changed host keys need
/// action the pane cannot take, so they stay unreachable.
pub fn requires_login(stderr: &str) -> bool {
    contains_auth_refusal(stderr) || host_key_unknown(stderr)
}

/// True when the server dropped an established connection without refusing
/// authentication. When askpass never handed over the password, this is how a host
/// that accepts a key but cannot open a session for it reads on the client: a Windows
/// sshd cannot build an Entra account's logon token without the password. A drop during
/// the version exchange happens before authentication and is excluded.
pub fn closed_without_refusal(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    (lower.contains("connection closed")
        || lower.contains("connection reset")
        || lower.contains("connection was closed"))
        && !lower.contains("kex_exchange_identification")
        && !contains_auth_refusal(stderr)
        && !host_key_changed(stderr)
        && !host_key_unknown(stderr)
}

const BROKER_UNAVAILABLE: &str = "xmux could not provide the held password";
const PASSWORD_REFUSED: &str = "the password was refused";
const AUTH_REFUSED: &str = "authentication was refused";
const HOST_KEY_CHANGED: &str = "the host key changed";
const HOST_KEY_UNKNOWN: &str = "the host key is not known yet";
const NAME_UNRESOLVED: &str = "the host name could not be resolved";
const UNREACHED: &str = "the host could not be reached";
const TIMED_OUT: &str = "timed out";
const CLOSED_AFTER_PASSWORD: &str =
    "the server accepted the password but closed the session before it started";

/// Every summary [`explain`] writes, so its text can be parted again.
const SUMMARIES: &[&str] = &[
    BROKER_UNAVAILABLE,
    PASSWORD_REFUSED,
    AUTH_REFUSED,
    HOST_KEY_CHANGED,
    HOST_KEY_UNKNOWN,
    NAME_UNRESOLVED,
    UNREACHED,
    TIMED_OUT,
    CLOSED_AFTER_PASSWORD,
];

/// The summaries only a command that handed ssh the held password writes.
const PASSWORD_SUMMARIES: &[&str] = &[BROKER_UNAVAILABLE, PASSWORD_REFUSED, CLOSED_AFTER_PASSWORD];

pub fn explain(input: &str, password_supplied: bool) -> String {
    let detail = sanitize(input);
    let lower = detail.to_ascii_lowercase();
    let summary = if lower.contains("xmux credential broker unavailable") {
        Some(BROKER_UNAVAILABLE)
    } else if contains_auth_refusal(&detail) {
        Some(if password_supplied {
            PASSWORD_REFUSED
        } else {
            AUTH_REFUSED
        })
    } else if host_key_changed(&detail) {
        Some(HOST_KEY_CHANGED)
    } else if host_key_unknown(&detail) {
        Some(HOST_KEY_UNKNOWN)
    } else if lower.contains("could not resolve hostname") {
        Some(NAME_UNRESOLVED)
    } else if lower.contains("connection refused")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
    {
        Some(UNREACHED)
    } else if lower.contains("connection timed out")
        || lower.contains("timed out")
        || lower.contains("did not answer within")
    {
        Some(TIMED_OUT)
    } else if password_supplied
        && (lower.contains("connection closed")
            || lower.contains("connection reset")
            || lower.contains("connection was closed"))
    {
        Some(CLOSED_AFTER_PASSWORD)
    } else {
        None
    };
    match (summary, detail.is_empty()) {
        (Some(summary), false) if detail.starts_with(summary) => detail,
        (Some(summary), false) => format!("{summary}\n{detail}"),
        (Some(summary), true) => summary.to_string(),
        (None, _) => detail,
    }
}

/// Text [`explain`] wrote, parted again into xmux's summary and the detail under it.
#[derive(Debug, PartialEq, Eq)]
pub struct Explained<'a> {
    pub summary: Option<&'static str>,
    pub detail: &'a str,
    /// Whether the summary is one only a command that handed ssh the held password
    /// writes, so the detail's refusal is a refused password.
    pub password_supplied: bool,
}

/// Parts text [`explain`] wrote. Text whose first line is no summary of its own is all
/// detail.
pub fn split_explained(text: &str) -> Explained<'_> {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    match SUMMARIES.iter().find(|s| **s == first.trim_end()) {
        Some(summary) => Explained {
            summary: Some(summary),
            detail: rest,
            password_supplied: PASSWORD_SUMMARIES.contains(summary),
        },
        None => Explained {
            summary: None,
            detail: text,
            password_supplied: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openssh_octal_utf8_is_restored() {
        let input = r"ssh: Could not resolve hostname e2e-box: \354\225\214\353\240\244\354\247\204 \355\230\270...";
        let decoded = decode_openssh_octal(input);
        assert_eq!(
            decoded,
            "ssh: Could not resolve hostname e2e-box: 알려진 호..."
        );
    }

    #[test]
    fn octal_values_above_one_byte_are_left_as_text() {
        assert_eq!(
            decode_openssh_octal(r"bad \400 \777 good \101"),
            r"bad \400 \777 good \101"
        );
    }

    #[test]
    fn ascii_octal_text_is_not_treated_as_openssh_locale_escaping() {
        assert_eq!(
            decode_openssh_octal(r"C:\dir\101 and mode \060\060\060"),
            r"C:\dir\101 and mode \060\060\060"
        );
    }

    #[test]
    fn utf8_octal_run_decodes_without_consuming_adjacent_ascii_octal() {
        assert_eq!(decode_openssh_octal(r"\354\225\214\101"), r"알\101");
    }

    #[test]
    fn only_opensshs_final_auth_line_is_a_refusal() {
        assert!(contains_auth_refusal(
            "dev@127.0.0.1: Permission denied (publickey,password)."
        ));
        assert!(!contains_auth_refusal(
            "tmux: error connecting to /tmp/tmux-1000/default (Permission denied)"
        ));
        assert!(!contains_auth_refusal(
            "sh: cannot create ~/.ssh/authorized_keys: Permission denied"
        ));
    }

    #[test]
    fn ssh_reasons_lead_with_plain_language_and_keep_decoded_detail() {
        let input = r"ssh: Could not resolve hostname e2e-box: \354\225\214\353\240\244\354\247\204 \355\230\270...";
        assert_eq!(
            explain(input, false),
            "the host name could not be resolved\nssh: Could not resolve hostname e2e-box: 알려진 호..."
        );
    }

    #[test]
    fn a_saved_host_key_note_is_not_part_of_the_failure() {
        let input =
            "Warning: Permanently added '172.17.0.2' (ED25519) to the list of known hosts.\r\n\
                     dev@172.17.0.2: Permission denied (publickey,password).";
        assert_eq!(
            explain(input, true),
            "the password was refused\ndev@172.17.0.2: Permission denied (publickey,password)."
        );
    }

    #[test]
    fn a_first_seen_host_key_is_told_apart_from_a_changed_one() {
        assert!(host_key_unknown("Host key verification failed."));
        assert!(!host_key_changed("Host key verification failed."));
        let changed = "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
                       Host key verification failed.";
        assert!(host_key_changed(changed));
        assert!(!host_key_unknown(changed));
        assert!(explain("Host key verification failed.", false)
            .starts_with("the host key is not known yet"));
        assert!(explain(changed, false).starts_with("the host key changed"));
    }

    #[test]
    fn a_broker_failure_is_not_misreported_as_bad_authentication() {
        let detail = "xmux credential broker unavailable: connection refused\n\
                      dev@box: Permission denied (publickey,password).";
        assert!(explain(detail, false).starts_with("xmux could not provide the held password\n"));
    }

    #[test]
    fn requires_login_covers_the_failures_the_login_pane_can_answer() {
        for text in [
            "pwtest@127.0.0.1: Permission denied (publickey,password).",
            "command failed (exit 255): pwtest@127.0.0.1: Permission denied (publickey).",
            "Host key verification failed.",
            "command failed (exit 255): Host key verification failed.",
        ] {
            assert!(requires_login(text), "the pane can answer this: {text}");
        }
    }

    #[test]
    fn requires_login_leaves_a_machine_that_is_down_unreachable() {
        // No value the pane holds reaches a machine that is not answering, and a generic
        // mux error is not an ssh barrier at all.
        for text in [
            "ssh: connect to host 192.0.2.1 port 22: Connection timed out",
            "ssh: connect to host prod port 22: Connection refused",
            "ssh: connect to host prod port 22: No route to host",
            "tmux: open /tmp/tmux-0/default: Permission denied",
            "Permission denied (publickey,password,keyboard-interactive).",
            "ssh: Could not resolve hostname jupiter00: No address associated with hostname",
            "no server running on /tmp/tmux-1000/default",
        ] {
            assert!(!requires_login(text), "not answerable here: {text}");
        }
    }

    #[test]
    fn a_drop_without_refusal_is_told_apart_from_other_failures() {
        for text in [
            "Connection reset by 127.0.0.1 port 22",
            "Connection closed by 10.0.0.5 port 22",
            "client_loop: send disconnect: Connection reset",
        ] {
            assert!(closed_without_refusal(text), "dropped: {text}");
        }
        for text in [
            "kex_exchange_identification: Connection closed by remote host\nConnection closed by 10.0.0.5 port 22",
            "dev@box: Permission denied (publickey,password).\nConnection closed by 10.0.0.5 port 22",
            "ssh: connect to host prod port 22: Connection refused",
            "ssh: connect to host 192.0.2.1 port 22: Connection timed out",
            "Host key verification failed.\nConnection closed by 10.0.0.5 port 22",
        ] {
            assert!(!closed_without_refusal(text), "not a drop after auth: {text}");
        }
    }

    #[test]
    fn requires_login_refuses_a_changed_host_key() {
        // A key that changed under a host is not an answer the user gives in xmux: ssh's
        // own warning accompanies the same verification-failed line, and that warning is
        // what keeps the host unreachable.
        assert!(!requires_login(
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
             @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
             Host key verification failed."
        ));
    }
}
