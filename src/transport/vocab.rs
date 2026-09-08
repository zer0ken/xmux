//! Shared transport-axis shell helpers: rendering an argv safe for the POSIX
//! shell an ssh connection hands its remote command to. The ssh transport
//! (`super::ssh::Ssh`) is the sole consumer — a local transport never issues a
//! remote shell command. This is the transport axis's own vocab home, the peer of
//! `mux/vocab.rs`.

/// The command that asks a remote which shell family answers it, and the number of
/// round trips it costs: none of its own. It replaces the bare reachability probe, so
/// one connection both proves the machine answers and says what its shell is.
///
/// `$0` is the one expansion every POSIX shell fills in and no other shell does:
///
/// ```text
/// sh    -> sh          bash -> bash
/// pwsh  -> (empty)     cmd  -> $0
/// ```
///
/// All three exit 0, so the exit code keeps meaning reachability alone. `uname` cannot
/// stand in: a Windows box with Git installed answers it.
pub const SHELL_PROBE: &str = "echo $0";

/// Which shell family a remote answers with, as [`SHELL_PROBE`]'s output reads.
///
/// The distinction earns its keep twice: a POSIX snippet (`exec`, `c=$(tty)`) runs only
/// on `Posix`, and only `Posix` is a shell whose command line [`remote_command`] was
/// written for. PowerShell reads a single-quoted string as a literal, so the same
/// quoting is injection-safe there too, which is why the two are one variant apart
/// rather than a supported target and a refused one. `cmd.exe` remains unsupported for
/// the reason it always was: it treats single quotes as ordinary characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteShell {
    /// A POSIX shell (`sh`, `bash`, `zsh`): the assumed default, and the only family a
    /// POSIX snippet may be sent to.
    #[default]
    Posix,
    /// Anything else that answered. Named for what xmux does with it - send no POSIX
    /// snippet - rather than for which shell it is, because the two non-POSIX families
    /// xmux meets need the same restraint.
    Other,
}

impl RemoteShell {
    /// Reads [`SHELL_PROBE`]'s stdout. A POSIX shell substitutes its own name, so the
    /// answer is non-empty and carries no `$`; PowerShell substitutes nothing and
    /// `cmd.exe` echoes the text back unexpanded.
    ///
    /// Only the LAST non-empty line counts. A login script that writes to stdout puts
    /// its own lines ahead of the answer, and reading the whole stream would let one
    /// stray `$` in a host's greeting cost it every POSIX snippet it can in fact run.
    pub fn from_probe(stdout: &[u8]) -> RemoteShell {
        let out = String::from_utf8_lossy(stdout);
        let answer = out
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or_default();
        if answer.is_empty() || answer.contains('$') {
            RemoteShell::Other
        } else {
            RemoteShell::Posix
        }
    }

    /// Whether a POSIX shell snippet may be sent to this remote: the `exec` an attach
    /// prepends, and a mux's `SwitchPlan::Shell` text.
    pub fn runs_posix_snippets(self) -> bool {
        matches!(self, RemoteShell::Posix)
    }
}

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
/// Written for a POSIX shell (`sh`/`bash`/`zsh`), where [`quote`]'s single-quote
/// escaping is correct and injection-safe. PowerShell reads a single-quoted string as a
/// literal too, so the same line is safe on a remote whose ssh default shell is
/// PowerShell; what such a remote cannot take is a POSIX SNIPPET, which
/// [`RemoteShell::runs_posix_snippets`] is the gate for.
///
/// `cmd.exe` remains an unsupported remote: it treats single quotes as ordinary
/// characters, so this line's quoting neutralizes nothing there. The probe classifies it
/// [`RemoteShell::Other`] alongside PowerShell, which withholds every POSIX snippet from
/// it; making its own quoting safe would be a further per-host shell rendering.
pub fn remote_command(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_probe_reads_every_family_from_its_own_answer() {
        // The exact stdout each shell gives for `echo $0`, measured. A POSIX shell
        // substitutes its own name; PowerShell substitutes nothing; cmd.exe echoes the
        // text back. Trailing newlines and CR are what a pipe actually carries.
        let cases: &[(&str, RemoteShell)] = &[
            ("sh\n", RemoteShell::Posix),
            ("bash\n", RemoteShell::Posix),
            ("/bin/zsh\n", RemoteShell::Posix),
            ("-sh\r\n", RemoteShell::Posix),
            ("", RemoteShell::Other),
            ("\r\n", RemoteShell::Other),
            ("$0\r\n", RemoteShell::Other),
            // A login script that greets on stdout puts its lines ahead of the answer.
            ("Welcome to $HOSTNAME!\nbash\n", RemoteShell::Posix),
            ("motd line\n\n/bin/sh\n\n", RemoteShell::Posix),
        ];
        for &(out, want) in cases {
            assert_eq!(
                RemoteShell::from_probe(out.as_bytes()),
                want,
                "from_probe({out:?})"
            );
        }
    }

    #[test]
    fn only_a_posix_remote_takes_a_posix_snippet() {
        assert!(RemoteShell::Posix.runs_posix_snippets());
        assert!(!RemoteShell::Other.runs_posix_snippets());
        // An unasked machine is addressed as POSIX, which is what every remote xmux
        // reached before the probe learned to ask.
        assert!(RemoteShell::default().runs_posix_snippets());
    }

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
