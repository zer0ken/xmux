//! Thin per-source config/data for a mux server reachable from this machine (the
//! local mux, or a remote one over ssh): alias, mux binary, machine kind (socket /
//! ssh alias, control path, os), and an injectable runner. The off-loop `Ops`/CLI
//! paths assemble a value [`Host`](crate::model::Host) from this config (`host()`)
//! and drive its enumerate/manage/attach through the `Host`/`Mux`/`Transport` APIs;
//! the machine boundary itself — argv assembly and the ssh transport (connect-timeout,
//! injection-safe quoting) — lives entirely in `Transport`, built at the single
//! `MachineKind::transport` site. The mux-env vocabulary lives in `mux::vocab`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::provision::config::Config;
use crate::transport::MachineKind;
use crate::session;

/// A failed command's outcome. Only a real non-zero exit carries stderr (and can
/// be classified benign); a missing binary or a connection failure surfaces as
/// [`RunError::Other`] (never benign).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A real process exit: carries stderr and the exit code. `126/127/255` are
    /// never a healthy-but-empty mux.
    #[error("command failed (exit {code}): {stderr}")]
    Exit { stderr: String, code: i32 },
    /// A spawn/transport failure (missing binary, connect failure) — never benign.
    #[error("{0}")]
    Other(String),
}

/// Runs an external command and returns its stdout. A trait so the source layer
/// is testable without spawning processes.
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError>;
}

/// The real runner: spawns the command via tokio, stripping mux env so a local
/// command run from inside a mux is not refused as nesting.
pub struct ExecRunner;

#[async_trait]
impl Runner for ExecRunner {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
        let mut cmd = tokio::process::Command::new(name);
        cmd.args(args);
        // Isolate stdin: these are non-interactive mux/ssh commands (list-sessions,
        // switch-client, …) that read no input. Without this, ssh inherits the parent
        // console tty and resets its mode (raw → canonical) for its own escape handling,
        // wrecking the app's raw mode until ssh exits — the terminal then echoes keys
        // and only flushes input on Enter.
        cmd.stdin(std::process::Stdio::null());
        cmd.kill_on_drop(true); // a cancelled (timed-out) scan kills the child
        cmd.env_clear();
        for (k, v) in std::env::vars() {
            if !crate::mux::vocab::is_mux_var(&k) {
                cmd.env(k, v);
            }
        }
        let output = cmd
            .output()
            .await
            .map_err(|e| RunError::Other(e.to_string()))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(RunError::Exit {
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                code: output.status.code().unwrap_or(-1),
            })
        }
    }
}

/// One mux server. Remote sources run their mux over ssh.
#[derive(Clone)]
pub struct Source {
    /// `"local"` or an ssh-config alias.
    pub alias: String,
    /// mux binary name on that machine.
    pub binary: String,
    /// Which machine family (and its construction data — socket / ssh alias, control
    /// path, os) this source reaches its mux over. The single representation of transport
    /// kind; `transport()` maps it to a concrete `Transport` at one site.
    pub kind: MachineKind,
    /// injectable; `None` ⇒ the real exec runner.
    pub runner: Option<Arc<dyn Runner>>,
}

impl Source {
    pub(crate) fn run_with(&self) -> &dyn Runner {
        match &self.runner {
            Some(r) => r.as_ref(),
            None => &ExecRunner,
        }
    }

    /// Assembles a value [`Host`](crate::model::Host) from this source's config —
    /// transport from [`kind`](Self::kind) at the single `MachineKind::transport` site,
    /// mux from [`binary`](Self::binary) — for the off-loop `Ops`/CLI paths that cannot
    /// borrow the event loop's live `&mut Host`. The runner stays with the source
    /// (`run_with`), injected into the host's enumerate/manage/attach calls.
    pub(crate) fn host(&self) -> crate::model::Host {
        crate::model::Host::new(
            self.kind.clone().transport(),
            crate::mux::for_binary(&self.binary),
        )
    }
}

// The reachable-but-empty classification lives in `mux/`. The app reaches its
// `%exit`/`%error`-reason check through `crate::model::source::reason_is_no_sessions`, so the
// name is re-exported here to keep that path resolving.
pub(crate) use crate::mux::reason_is_no_sessions;

/// Assembles the source list for a config: local first, then each ssh host
/// (ssh-config aliases merged with config overrides) in order, then each WSL
/// distribution. WSL comes last so adding the family leaves every id an existing
/// install already had in the position it had.
///
/// `local_muxes` is the RESOLVED local mux list (`Env` resolves it once, discovering
/// what this box has when the config says `auto`), passed in rather than re-derived so
/// the source ids here and the host ids in `Hosts::build` cannot disagree.
pub fn build(
    cfg: &Config,
    ssh_aliases: &[String],
    wsl_distros: &[String],
    os: &str,
    local_muxes: &[String],
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Vec<Source> {
    // One source per (machine, mux): this box contributes one for each mux it serves.
    let qualified = local_muxes.len() > 1;
    let mut srcs: Vec<Source> = local_muxes
        .iter()
        .map(|bin| {
            let id = session::source_id(session::LOCAL_SOURCE, bin, qualified);
            for_machine_mux(
                session::LOCAL_SOURCE,
                bin,
                id,
                os,
                xmux_dir,
                local_socket.clone(),
            )
        })
        .collect();
    for spec in cfg
        .host_specs(ssh_aliases)
        .into_iter()
        .chain(cfg.wsl_specs(wsl_distros))
    {
        srcs.push(for_machine_mux(
            &spec.alias,
            &spec.bin,
            spec.id,
            os,
            xmux_dir,
            None,
        ));
    }
    srcs
}

/// One [`Source`] for the mux binary `bin` on `machine`, answering as the source `id`.
/// The machine half comes from [`crate::transport::kind_for`], so this source and the
/// `Host` the loop drives for the same pair reach the machine the same way. A source
/// DISCOVERED after launch is built here too, which is what makes it as operable as a
/// configured one (create / panes / border styles all resolve through the source list).
///
/// The socket is filtered by what `bin` accepts before the machine is given it: this is
/// one of the two sites where the mux is known alongside the machine, and the machine
/// axis names no mux, so the choice can only be made here. Both sites filter the same
/// raw value the same way, which is what keeps a source and its `Host` on one server.
pub fn for_machine_mux(
    machine: &str,
    bin: &str,
    id: String,
    os: &str,
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Source {
    let local_socket = crate::mux::server_socket_for(bin, local_socket);
    Source {
        alias: id.clone(),
        binary: bin.to_string(),
        kind: crate::transport::kind_for(machine, id, os, xmux_dir, local_socket),
        runner: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_puts_local_first() {
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let srcs = build(
            &cfg,
            &aliases,
            &[],
            "linux",
            &["tmux".to_string()],
            Path::new("/home/u/.xmux"),
            None,
        );
        assert_eq!(srcs.len(), 3);
        assert_eq!(srcs[0].alias, "local");
        assert!(matches!(srcs[0].kind, MachineKind::Local { .. }));
        assert_eq!(srcs[1].alias, "prod");
        assert!(matches!(srcs[1].kind, MachineKind::Ssh { .. }));
        assert_eq!(srcs[1].binary, "tmux");
    }
    #[test]
    fn a_local_zellij_source_is_built_without_the_tmux_socket() {
        // The socket comes from `$TMUX`, which is set whenever xmux runs inside a mux -
        // the normal case. Handing it to zellij made every local zellij source fail its
        // listing on argument parsing, so it never reaches the machine at all.
        let dir = Path::new("/tmp/xmux");
        let sock = Some("/tmp/psmux-1/default".to_string());
        let z = for_machine_mux(
            "local",
            "zellij",
            "local:zellij".into(),
            "linux",
            dir,
            sock.clone(),
        );
        assert_eq!(z.kind.local_socket(), None, "no socket reaches zellij");

        // The tmux family still targets the server it was told to.
        let p = for_machine_mux(
            "local",
            "psmux",
            "local:psmux".into(),
            "linux",
            dir,
            sock.clone(),
        );
        assert_eq!(p.kind.local_socket(), sock);
    }
}
