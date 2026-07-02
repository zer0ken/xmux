//! Shared machine-axis shell vocabulary: rendering an argv safe for the POSIX
//! shell an ssh connection hands its remote command to. The ssh transport
//! (`super::ssh::Ssh`) is the sole consumer — a local transport never issues a
//! remote shell command. This is the machine axis's own vocab home, the peer of
//! `mux/vocab.rs`.

/// Renders one argument safe for a POSIX shell. A string of only safe characters
/// passes through; anything else is single-quoted with embedded single-quotes
/// escaped as `'\''`. This is the SOLE point an untrusted value (a session name
/// from a remote list-sessions) enters a remote shell command.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if is_shell_safe(s) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn is_shell_safe(s: &str) -> bool {
    s.chars()
        .all(|r| r.is_ascii_alphanumeric() || matches!(r, '-' | '_' | '.' | '/'))
}

/// Joins a mux argv into a single shell command line, quoting each element, for
/// execution by the remote shell ssh hands it to.
///
/// Assumes the remote login shell is POSIX (`sh`/`bash`/`zsh`), which is the
/// supported remote target: a remote source runs `tmux`, and tmux remotes are
/// POSIX. [`quote`]'s single-quote escaping is correct and injection-safe there.
/// A Windows remote whose ssh default shell is `cmd.exe` is NOT a supported
/// remote (the local side may be Windows/psmux, but remotes are POSIX); cmd.exe
/// does not treat single quotes as quoting, so addressing it correctly would need
/// an explicit per-host shell — a separate feature, intentionally not assumed here.
pub fn remote_command(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_neutralizes_shell_metachars() {
        let cases: &[(&str, &str)] = &[
            ("plain", "plain"),
            ("with space", "'with space'"),
            ("", "''"),
            ("a/b-c_d.e", "a/b-c_d.e"),
            ("$(rm -rf /)", "'$(rm -rf /)'"),
            ("a';rm -rf /;'b", r"'a'\'';rm -rf /;'\''b'"),
            ("`whoami`", "'`whoami`'"),
        ];
        for &(input, want) in cases {
            assert_eq!(quote(input), want, "quote({input:?})");
        }
    }

    #[test]
    fn remote_command_joins_quoted() {
        let argv: Vec<String> = ["tmux", "rename-session", "-t", "old", "evil; rm -rf /"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            remote_command(&argv),
            "tmux rename-session -t old 'evil; rm -rf /'"
        );
    }
}
