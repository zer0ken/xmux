//! The WSL machine transport: runs a mux argv inside a Windows Subsystem for Linux
//! distribution on THIS box. Local (no network hop) yet shell-based, which is the
//! combination the two capability predicates exist to express. Untrusted argv elements
//! are per-arg quoted via [`super::vocab::remote_command`], the same vocabulary the ssh
//! family uses, because the command is handed to a POSIX shell inside the distro.
//!
//! This file also owns the family's machine-name provider ([`distros`]): the sibling of
//! the roster's ssh providers, kept here because listing distributions is `wsl.exe`
//! mechanics (its own flags, its own output encoding) rather than roster policy.

use std::process::Command;

use super::vocab::remote_command;
use super::Transport;
use crate::session;

/// The Windows-side launcher every WSL command goes through. Spelled with `.exe` so it
/// resolves from `System32` without depending on `PATHEXT`.
const WSL_BIN: &str = "wsl.exe";

/// A WSL distribution on this box. `distro` is the name `wsl.exe -d` takes; `id` is the
/// SOURCE id this transport answers as - the machine name `wsl.<distro>` when the distro
/// serves a single mux, and that name qualified by the mux when it serves several.
#[derive(Clone, Debug)]
pub struct Wsl {
    pub id: String,
    pub distro: String,
}

impl Wsl {
    /// The `wsl.exe` argv that runs one POSIX shell command inside the distribution.
    ///
    /// `--exec` is what makes the call safe: it exec's the remaining argv elements
    /// directly, so the shell sees exactly the three elements below. The alternative
    /// (`wsl.exe -- <command line>`) hands the RAW Windows command line to the
    /// distribution's shell, which then re-reads Windows quoting as shell syntax - a
    /// session name holding `$(...)` would be substituted there, and no amount of POSIX
    /// quoting on this side could prevent it.
    ///
    /// The shell is `sh -lc`: a LOGIN shell, because the login `PATH` is the one the
    /// user's own mux lives on. The environment `--exec` starts with omits `~/.local/bin`
    /// and `/snap/bin`, which is exactly where a cargo- or snap-installed mux sits.
    fn shell_argv(&self, command: &str) -> Vec<String> {
        vec![
            "-d".into(),
            self.distro.clone(),
            "--exec".into(),
            "sh".into(),
            "-lc".into(),
            command.to_string(),
        ]
    }

    /// [`shell_argv`](Self::shell_argv) with the launcher in front, for the callers that
    /// return one whole argv rather than a `(command, args)` split.
    fn full_argv(&self, command: &str) -> Vec<String> {
        let mut v = vec![WSL_BIN.to_string()];
        v.extend(self.shell_argv(command));
        v
    }
}

impl Transport for Wsl {
    fn host_id(&self) -> &str {
        // The SOURCE id, not the distribution: several muxes in one distro are several
        // sources reached at the same `distro`.
        &self.id
    }

    /// A WSL command runs THROUGH the distribution's shell, so an attach can record its
    /// own tty and a `SwitchPlan::Shell` can execute. The distro's mux registry lives
    /// inside the distro, so `local_registry_scope` stays the default `false` - this box's
    /// `~/.psmux` describes a different machine. `is_remote` stays `false` as well: there
    /// is no network hop and no ssh option to shape.
    fn runs_through_shell(&self) -> bool {
        true
    }

    /// `tty` is ignored: a WSL child inherits the Windows console it was spawned on, and
    /// the distribution allocates its pty from that. There is no option to ask for one.
    fn exec_argv(&self, _tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        (
            WSL_BIN.to_string(),
            self.shell_argv(&remote_command(mux_argv)),
        )
    }

    /// Runs `[<pre_select> ; ] exec <attach>` in the distribution, exactly as the ssh
    /// family does: `exec` replaces the shell so the attach owns the pty for its whole
    /// life, and folding `pre_select` into the SAME command means the selection cannot be
    /// lost to a second launch that races the attach.
    fn interactive_attach_argv(
        &self,
        mux_attach_argv: &[String],
        pre_select: Option<&[String]>,
    ) -> (String, Vec<String>) {
        let attach = remote_command(mux_attach_argv);
        let command = match pre_select {
            Some(sel) => format!("{} ; exec {}", remote_command(sel), attach),
            None => format!("exec {attach}"),
        };
        (WSL_BIN.to_string(), self.shell_argv(&command))
    }

    /// Wraps the control child in `script`, the distribution-side stand-in for `ssh -tt`.
    ///
    /// A `-CC` control client reads its own stdin's terminal attributes and exits when
    /// that is not a terminal, and the control child's stdio IS pipes - xmux reads that
    /// stream line by line rather than painting it. `script` runs the mux on a pty it
    /// allocates inside the distribution, so the mux sees a terminal while xmux still
    /// reads plain pipes. `-q` drops the banner, `-f` flushes every line so the stream
    /// stays live, and the typescript is written to `/dev/null` because only the stream
    /// is wanted.
    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        let inner = remote_command(mux_control_argv);
        let script = remote_command(&[
            "script".to_string(),
            "-q".to_string(),
            "-f".to_string(),
            "-c".to_string(),
            inner,
            "/dev/null".to_string(),
        ]);
        self.full_argv(&format!("exec {script}"))
    }

    /// Joins a raw shell command behind the WSL wrapper. The caller must quote any
    /// untrusted value inside `shell_cmd` (see [`super::vocab::quote`]).
    fn raw_shell_argv(&self, shell_cmd: &str) -> Option<Vec<String>> {
        Some(self.full_argv(shell_cmd))
    }

    fn clone_box(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
}

/// The WSL distributions installed on this box, as MACHINE names (`wsl.Ubuntu-24.04`).
///
/// A box without WSL, a `wsl.exe` that cannot start, and unreadable output all yield an
/// empty list rather than an error - the same rule the roster's providers follow, for the
/// same reason: one quiet provider must not stop the machines that did answer.
pub fn distros() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    match Command::new(WSL_BIN).args(["--list", "--quiet"]).output() {
        Ok(o) if o.status.success() => parse_distros(&decode_wsl_output(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Decodes `wsl.exe --list` output, which is UTF-16LE unless the environment sets
/// `WSL_UTF8`. Both have to be read, because that variable belongs to the USER's
/// environment and is inherited: deciding by encoding rather than by version is what
/// makes this hold either way. A NUL byte is the tell - no UTF-8 text carries one.
fn decode_wsl_output(bytes: &[u8]) -> String {
    if !bytes.contains(&0) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Turns decoded `--list --quiet` output into machine names, in the order `wsl.exe`
/// reported them.
///
/// A name carrying [`session::MUX_SEP`] or `/` is dropped: those two characters are the
/// source-id and address grammar, so such a name could not be addressed back. Nothing
/// else is filtered - a distribution running no mux answers as unreachable, which is a
/// legible answer, whereas a hidden one is not.
fn parse_distros(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_matches('\u{feff}').trim())
        .filter(|name| !name.is_empty() && !name.contains(session::MUX_SEP) && !name.contains('/'))
        .map(|name| format!("{}{name}", session::WSL_PREFIX))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsl(distro: &str) -> Wsl {
        Wsl {
            id: session::WSL_PREFIX.to_string() + distro,
            distro: distro.into(),
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_id_is_the_machine_name_and_is_not_remote() {
        let t = wsl("Ubuntu-24.04");
        assert_eq!(t.host_id(), "wsl.Ubuntu-24.04");
        assert!(!t.is_remote(), "a distro on this box crosses no network");
    }

    #[test]
    fn the_capability_pair_is_shell_yes_registry_no() {
        // The combination the two predicates exist for, and the one no other family has:
        // an attach runs through a shell (so it can record its tty), while the mux
        // registry that describes its sessions lives inside the distro, not on this box.
        let t = wsl("Ubuntu-24.04");
        assert!(t.runs_through_shell());
        assert!(!t.local_registry_scope());
    }

    #[test]
    fn every_command_goes_through_exec_and_a_login_shell() {
        // `--exec` is the injection-safe door (see `shell_argv`), and `sh -lc` is what
        // puts the user's own mux on PATH. Both must hold for every call shape, so a
        // later edit cannot quietly drop one of them on one path.
        let t = wsl("Ubuntu-24.04");
        let (_n, exec) = t.exec_argv(false, &argv(&["tmux", "list-sessions"]));
        let (_n, attach) = t.interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]), None);
        let control = t.control_argv(&argv(&["tmux", "-CC", "attach"]));
        let raw = t.raw_shell_argv("c=$(tty); echo $c").unwrap();
        let want = argv(&["-d", "Ubuntu-24.04", "--exec", "sh", "-lc"]);
        for shape in [&exec, &attach, &control[1..].to_vec(), &raw[1..].to_vec()] {
            assert_eq!(
                shape[..5],
                want[..],
                "every shape targets the distro through --exec sh -lc: {shape:?}"
            );
        }
    }

    #[test]
    fn exec_argv_runs_the_mux_command_in_the_distro() {
        let (n, a) =
            wsl("Ubuntu-24.04").exec_argv(false, &argv(&["tmux", "kill-session", "-t", "x"]));
        assert_eq!(n, "wsl.exe");
        assert_eq!(a.last().unwrap(), "tmux kill-session -t x");
    }

    #[test]
    fn an_untrusted_name_is_quoted_for_the_distro_shell() {
        // The command reaches a POSIX shell inside the distro, so a session name holding
        // shell syntax must arrive as one word. `--exec` keeps the WINDOWS layer from
        // re-reading it first, and `remote_command` neutralizes it for the shell.
        let (_n, a) = wsl("Ubuntu-24.04").exec_argv(
            false,
            &argv(&["tmux", "rename-session", "-t", "old", "evil; rm -rf /"]),
        );
        assert_eq!(
            a.last().unwrap(),
            "tmux rename-session -t old 'evil; rm -rf /'"
        );
    }

    #[test]
    fn interactive_attach_execs_and_folds_the_pre_select() {
        let t = wsl("Ubuntu-24.04");
        let (_n, a) = t.interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]), None);
        assert_eq!(a.last().unwrap(), "exec tmux attach -t api");
        let (_n, a) = t.interactive_attach_argv(
            &argv(&["tmux", "attach", "-t", "api"]),
            Some(&argv(&["tmux", "select-window", "-t", "api:2"])),
        );
        assert_eq!(
            a.last().unwrap(),
            "tmux select-window -t 'api:2' ; exec tmux attach -t api"
        );
    }

    #[test]
    fn control_argv_gives_the_mux_a_pty_inside_the_distro() {
        // `-CC` exits when its stdin is not a terminal, and the control child's stdio is
        // pipes. `script` is where that pty comes from; the mux payload rides inside it
        // quoted, so the transport never rewrites a mux flag to work around the pipe.
        let got = wsl("Ubuntu-24.04").control_argv(&argv(&["tmux", "-CC", "attach"]));
        assert_eq!(got[0], "wsl.exe");
        assert_eq!(
            got.last().unwrap(),
            "exec script -q -f -c 'tmux -CC attach' /dev/null"
        );
    }

    #[test]
    fn raw_shell_argv_is_some_for_wsl() {
        // A `SwitchPlan::Shell` needs a host shell to run in; this family has one, so an
        // in-place switch does not fall back to a full reattach.
        let got = wsl("Ubuntu-24.04")
            .raw_shell_argv("c=$(cat /tmp/.xmux-cli-x); echo $c")
            .unwrap();
        assert_eq!(got[0], "wsl.exe");
        assert_eq!(got.last().unwrap(), "c=$(cat /tmp/.xmux-cli-x); echo $c");
    }

    #[test]
    fn decode_reads_both_encodings_wsl_exe_uses() {
        // UTF-16LE (the default), and UTF-8 (what an inherited WSL_UTF8 gives).
        let utf16: Vec<u8> = "Ubuntu\r\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(
            parse_distros(&decode_wsl_output(&utf16)),
            vec!["wsl.Ubuntu"]
        );
        assert_eq!(
            parse_distros(&decode_wsl_output(b"Ubuntu\r\ndocker-desktop\r\n")),
            vec!["wsl.Ubuntu", "wsl.docker-desktop"]
        );
    }

    #[test]
    fn a_name_the_id_grammar_cannot_carry_is_dropped() {
        // `:` separates a machine from its mux and `/` separates a source from a session,
        // so a distribution named with either could be listed but never addressed back.
        assert_eq!(parse_distros("ok\nbad:name\nbad/name\n\n"), vec!["wsl.ok"]);
    }
}
