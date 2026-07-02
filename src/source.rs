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

use crate::config::Config;
use crate::machine::MachineKind;
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

    /// The local mux server socket (`-S`) this source targets when it is a local
    /// machine; `None` for a remote source or a local source on the default socket.
    pub(crate) fn local_socket(&self) -> Option<String> {
        match &self.kind {
            MachineKind::Local { socket } => socket.clone(),
            MachineKind::Ssh { .. } => None,
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
// `%exit`/`%error`-reason check through `crate::source::reason_is_no_sessions`, so the
// name is re-exported here to keep that path resolving.
pub(crate) use crate::mux::reason_is_no_sessions;

/// Assembles the source list for a config: local first, then each ssh host
/// (ssh-config aliases merged with config overrides) in order.
pub fn build(
    cfg: &Config,
    ssh_aliases: &[String],
    os: &str,
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Vec<Source> {
    let mut srcs = vec![Source {
        alias: session::LOCAL_SOURCE.to_string(),
        binary: cfg.local_bin(os),
        kind: MachineKind::Local {
            socket: local_socket,
        },
        runner: None,
    }];
    for spec in cfg.host_specs(ssh_aliases) {
        let control_path = xmux_dir
            .join(format!("cm-{}.sock", spec.alias))
            .to_string_lossy()
            .into_owned();
        srcs.push(Source {
            alias: spec.alias.clone(),
            binary: spec.bin,
            kind: MachineKind::Ssh {
                alias: spec.alias,
                control_path,
                os: os.to_string(),
            },
            runner: None,
        });
    }
    srcs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_puts_local_first() {
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let srcs = build(&cfg, &aliases, "linux", Path::new("/home/u/.xmux"), None);
        assert_eq!(srcs.len(), 3);
        assert_eq!(srcs[0].alias, "local");
        assert!(matches!(srcs[0].kind, MachineKind::Local { .. }));
        assert_eq!(srcs[1].alias, "prod");
        assert!(matches!(srcs[1].kind, MachineKind::Ssh { .. }));
        assert_eq!(srcs[1].binary, "tmux");
    }
}
