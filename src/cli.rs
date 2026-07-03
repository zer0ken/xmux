//! The `xmux` CLI: argument parsing and command dispatch (`ls`/`attach`/
//! `doctor`/`ctl`/`version` and the default interactive app). `run` is the
//! single entry the binary shim calls.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::app::runtime;
use crate::attach::{self, OsExecer};
use crate::control;
use crate::env::{self, ls_lines, Env};
use crate::session;
use crate::source::Source;

#[derive(Parser)]
#[command(
    name = "xmux",
    version,
    about = "cross-environment mux session switcher",
    long_about = "xmux shows every reachable tmux/psmux session (local + ssh) as one tree and switches between them."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List every reachable session (scriptable).
    Ls,
    /// Attach one session directly, e.g. `xmux attach prod/api`.
    Attach {
        /// `<source>/<session>` target.
        target: String,
    },
    /// Diagnose configuration and source reachability.
    Doctor,
    /// Drive a running switcher over its control socket, or `list` running instances.
    Ctl {
        /// Target the instance with this pid (see `xmux ctl list`).
        #[arg(long)]
        pid: Option<u32>,
        /// Target this socket path.
        #[arg(long)]
        sock: Option<PathBuf>,
        /// The command to send (e.g. `list`, `switch prod/api`, `dump`); empty reads
        /// from stdin. With no target and several instances running, use --pid.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print version.
    Version,
}

pub async fn run() -> i32 {
    // Initialise the file-based tracing subscriber before any terminal or mux
    // setup so log records from every subsequent code path are captured. The
    // guard must outlive `run` (i.e. live until the process exits); binding it
    // here keeps it alive for the full call. The directory mirrors what
    // `env::build_env` resolves so the log lands next to the other xmux files.
    let xmux_dir = env::xmux_dir_path();
    let _log_guard = crate::logging::init(&xmux_dir);

    let cli = Cli::parse();
    match cli.command {
        None => match interactive_env() {
            Ok(env) => runtime::run_app(Arc::new(env)).await,
            Err(code) => code,
        },
        Some(Command::Ls) => match interactive_env() {
            Ok(env) => run_ls(&env).await,
            Err(code) => code,
        },
        Some(Command::Attach { target }) => match interactive_env() {
            Ok(env) => run_direct_attach(&env, &target).await,
            Err(code) => code,
        },
        Some(Command::Doctor) => {
            // Tolerate a malformed config — report it, don't die on it.
            let (env, cfg_err) = env::build_env();
            run_doctor(&env, cfg_err).await
        }
        Some(Command::Ctl { pid, sock, args }) => {
            // ctl only needs the xmux dir (independent of config validity).
            let (env, _cfg_err) = env::build_env();
            run_ctl(&env, pid, sock, args).await
        }
        Some(Command::Version) => {
            println!("xmux {}", env!("CARGO_PKG_VERSION"));
            0
        }
    }
}

/// Builds the env for an interactive command, treating a config-parse error as
/// fatal (printing it and returning the exit code in `Err`).
fn interactive_env() -> Result<Env, i32> {
    let (env, cfg_err) = env::build_env();
    if let Some(e) = cfg_err {
        eprintln!("xmux: {e}");
        return Err(1);
    }
    Ok(env)
}

/// Prints every reachable session as one `<source>/<name>` line; dead sources go
/// to stderr. Fails only when every source is unreachable.
async fn run_ls(env: &Env) -> i32 {
    let groups = env.scan().await;
    let (lines, unreachable, all_unreachable) = ls_lines(&groups);
    for l in &lines {
        println!("{l}");
    }
    for u in &unreachable {
        eprintln!("{u}");
    }
    if all_unreachable {
        1
    } else {
        0
    }
}

/// Attaches one `<source>/<session>` without the tree.
async fn run_direct_attach(env: &Env, addr: &str) -> i32 {
    let target = match session::parse_target(addr) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };
    let Some(src) = env.by_alias.get(&target.source).cloned() else {
        eprintln!(
            "xmux: unknown source {:?} (not local or an ssh-config host)",
            target.source
        );
        return 1;
    };
    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        return 1;
    }
    if let Err(e) = attach::run_attach(
        &OsExecer,
        &src.host().interactive_attach_command(&target.name, None),
    ) {
        eprintln!("xmux: attach failed: {e}");
        return 1;
    }
    0
}

/// Reports configuration health and per-source reachability. A diagnostic: a
/// malformed config or an unreachable host is reported, not fatal.
async fn run_doctor(env: &Env, cfg_err: Option<anyhow::Error>) -> i32 {
    println!("xmux doctor");

    // A config that failed to parse is a real error the diagnostic must signal in its
    // exit code (like `ls` does for all-unreachable); an unreachable source is reported
    // but not itself a doctor failure.
    let config_broken = cfg_err.is_some();
    if let Some(e) = cfg_err {
        println!("config.toml: ERROR — {e} (using defaults)");
    } else if !env.cfg_warnings.is_empty() {
        for w in &env.cfg_warnings {
            println!("config.toml: WARNING — {w}");
        }
    } else {
        println!("config.toml: ok");
    }

    println!("local mux: {}", env.local_bin);
    if ssh_on_path() {
        println!("ssh: ok");
    } else {
        println!("ssh: NOT FOUND on PATH — remote sources unavailable");
    }

    println!("sources:");
    for s in &env.srcs {
        match probe(s).await {
            Ok(n) => println!("  {} ({}): ok, {} session(s)", s.alias, s.binary, n),
            Err(e) => println!("  {} ({}): UNREACHABLE — {}", s.alias, s.binary, e),
        }
    }
    i32::from(config_broken)
}

/// Drives a running switcher over its control socket. `list` enumerates instances;
/// otherwise, with command args it sends one command and with none it streams
/// commands from stdin. The target is the explicit `--sock`, else `--pid`'s socket,
/// else the sole running instance (an error when several run — pick one with --pid).
async fn run_ctl(env: &Env, pid: Option<u32>, sock: Option<PathBuf>, args: Vec<String>) -> i32 {
    // `list` is a meta-command over ALL instances, not one — handle it before
    // resolving a single target socket. Matched case-insensitively, like every socket
    // verb (which `parse_request` lowercases), so `LIST` is not forwarded and rejected.
    if args.first().is_some_and(|s| s.eq_ignore_ascii_case("list")) {
        return run_ctl_list(env).await;
    }
    let path = match (sock, pid) {
        (Some(s), _) => s,
        (None, Some(p)) => control::socket_path(&env.xmux_dir, p),
        // No explicit target: drive the sole LIVE instance. Enumerating LIVE instances
        // (a dialable socket), not markers, is what keeps a crashed instance's stale
        // `ctl-*.sock` from counting — several dead markers must not read as "many".
        (None, None) => match choose_sole_instance(&live_instances(&env.xmux_dir).await) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("xmux ctl: {e}");
                return 1;
            }
        },
    };
    let mut client = match control::Client::dial(&path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xmux ctl: {e}");
            return 1;
        }
    };
    if !args.is_empty() {
        return ctl_one(&mut client, &args.join(" ")).await;
    }
    // Dispatch each line as it arrives rather than buffering until EOF, so a
    // piped/interactive stream of commands is processed incrementally.
    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rc = ctl_one(&mut client, line).await;
        if rc != 0 {
            return rc;
        }
    }
    0
}

/// The live instances (a dialable `ctl-<pid>.sock`) among the discovered markers — the
/// same liveness `xmux ctl list` uses. A crashed instance's stale marker does not dial
/// and is filtered out, so it never counts toward which instance to drive.
async fn live_instances(dir: &std::path::Path) -> Vec<(PathBuf, u32)> {
    let mut live = Vec::new();
    for (path, pid) in control::discover_all(dir).unwrap_or_default() {
        if control::Client::dial(&path).await.is_ok() {
            live.push((path, pid));
        }
    }
    live
}

/// Picks the socket to drive when `xmux ctl` is given no explicit target: exactly one
/// live instance → its socket; none → an error; several → refuse to guess (name one
/// with --pid, listed by `xmux ctl list`). Pure over the already-filtered live set.
fn choose_sole_instance(live: &[(PathBuf, u32)]) -> anyhow::Result<PathBuf> {
    match live {
        [] => Err(anyhow::anyhow!("no running xmux instance found")),
        [(path, _)] => Ok(path.clone()),
        many => Err(anyhow::anyhow!(
            "{} xmux instances running; target one with --pid <pid> (run `xmux ctl list`)",
            many.len()
        )),
    }
}

/// A framed reply beginning `err:` is the control protocol's command-level failure
/// signal (an unknown verb or a rejected op — see `ui::run::dispatch`). Distinguished
/// from a transport error so `xmux ctl` reports a refused command as a failure a script
/// can detect, not a silent success.
fn reply_is_err(resp: &str) -> bool {
    resp.starts_with("err:")
}

async fn ctl_one(client: &mut control::Client, line: &str) -> i32 {
    match client.do_cmd(line).await {
        Ok(resp) => {
            let text = resp.strip_suffix('\n').unwrap_or(&resp);
            if reply_is_err(text) {
                // A command-level error: route it like a transport error — to stderr,
                // with the `xmux ctl:` prefix, exit non-zero — so it is not mistaken
                // for a successful reply on stdout.
                let msg = text.strip_prefix("err: ").unwrap_or(text);
                eprintln!("xmux ctl: {msg}");
                1
            } else {
                println!("{text}");
                0
            }
        }
        Err(e) => {
            eprintln!("xmux ctl: {e}");
            1
        }
    }
}

/// Lists every running xmux instance so a specific one can be driven with `--pid`.
/// Enumerates the `ctl-<pid>.sock` markers, then dials each for its `status` (cwd /
/// tty / displayed session / focus). A socket that does not answer is a crashed
/// instance's stale marker and is skipped, so the listing shows only live, drivable
/// instances.
async fn run_ctl_list(env: &Env) -> i32 {
    let instances = control::discover_all(&env.xmux_dir).unwrap_or_default();
    let mut rows: Vec<[String; 5]> = vec![[
        "PID".into(),
        "CWD".into(),
        "TTY".into(),
        "DISPLAYED".into(),
        "FOCUS".into(),
    ]];
    for (path, pid) in instances {
        let Ok(mut client) = control::Client::dial(&path).await else {
            continue; // stale marker for a crashed instance
        };
        let Ok(resp) = client.do_cmd("status").await else {
            continue;
        };
        let f = control::parse_status(&resp);
        let cell = |s: String| if s.is_empty() { "-".to_string() } else { s };
        rows.push([
            pid.to_string(),
            cell(f.cwd),
            cell(f.tty),
            cell(f.target),
            cell(f.focus),
        ]);
    }
    if rows.len() == 1 {
        println!("no running xmux instances");
        return 0;
    }
    print!("{}", format_table(&rows));
    0
}

/// Renders rows as a left-aligned table: each column is padded to its widest cell.
/// The final column is not padded, and trailing space is trimmed, so `-` cells never
/// leave dangling whitespace.
fn format_table(rows: &[[String; 5]]) -> String {
    let mut widths = [0usize; 5];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(c);
            } else {
                line.push_str(&format!("{c:<width$}  ", width = widths[i]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

async fn probe(s: &Source) -> Result<usize, String> {
    let probe = async {
        let mut host = s.host();
        host.enumerate_with(s.run_with())
            .await
            .map(|()| host.inventory.sessions.len())
    };
    match tokio::time::timeout(std::time::Duration::from_secs(6), probe).await {
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".to_string()),
    }
}

/// Reports whether an `ssh` binary is resolvable, by attempting to run `ssh -V`.
fn ssh_on_path() -> bool {
    std::process::Command::new("ssh")
        .arg("-V")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_sole_instance_needs_exactly_one_live() {
        let inst = |n: u32| (PathBuf::from(format!("ctl-{n}.sock")), n);
        // None → error (nothing live to drive).
        assert!(choose_sole_instance(&[]).is_err(), "none → error");
        // Exactly one → that socket, no --pid needed.
        assert_eq!(
            choose_sole_instance(&[inst(100)]).unwrap(),
            PathBuf::from("ctl-100.sock")
        );
        // Several live → refuse to guess (the multi-instance guard).
        assert!(
            choose_sole_instance(&[inst(100), inst(200)]).is_err(),
            "multiple → refuse to guess"
        );
    }

    #[tokio::test]
    async fn live_instances_filters_out_dead_markers() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Two markers with no live listener: both undialable, so neither counts. This is
        // the crux — a pile of crashed-instance markers must resolve to zero live, not
        // to "many".
        std::fs::write(control::socket_path(&dir, 100), b"").unwrap();
        std::fs::write(control::socket_path(&dir, 200), b"").unwrap();
        assert!(
            live_instances(&dir).await.is_empty(),
            "stale markers never count as live instances"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_table_aligns_columns_and_trims() {
        let rows = vec![
            [
                "PID".into(),
                "CWD".into(),
                "TTY".into(),
                "DISPLAYED".into(),
                "FOCUS".into(),
            ],
            [
                "48213".into(),
                "/home/u/xmux".into(),
                "-".into(),
                "jup/api".into(),
                "terminal".into(),
            ],
        ];
        let out = format_table(&rows);
        let lines: Vec<&str> = out.lines().collect();
        // The CWD column starts at the same offset in the header and the data row
        // (PID padded to the width of "48213").
        let col = lines[0].find("CWD").unwrap();
        assert_eq!(&lines[1][col..col + "/home/u/xmux".len()], "/home/u/xmux");
        // No row carries trailing whitespace, and the last column is unpadded.
        assert!(
            lines.iter().all(|l| *l == l.trim_end()),
            "no trailing space"
        );
        assert!(lines[1].ends_with("terminal"));
    }

    #[test]
    fn reply_is_err_only_for_the_err_prefix() {
        assert!(reply_is_err("err: unknown command"));
        assert!(!reply_is_err("ok"));
        assert!(!reply_is_err("pong"));
        assert!(!reply_is_err("focus=tree\ttarget=api"));
    }
}
