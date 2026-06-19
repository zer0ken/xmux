//! Command `xmux` is a stateless cross-environment session switcher: one terminal
//! that sees and moves between every reachable tmux/psmux session — local and over
//! ssh — regardless of OS or mux kind.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use xmux::attach::{self, OsExecer};
use xmux::cockpit;
use xmux::control;
use xmux::env::{self, ls_lines, Env};
use xmux::manage;
use xmux::mux;
use xmux::session;
use xmux::source::Source;
use xmux::ui::run::run_switcher;

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
    /// In-mux switcher, bound via `display-popup -E "xmux popup"`.
    Popup,
    /// List every reachable session (scriptable).
    Ls,
    /// Attach one session directly, e.g. `xmux attach prod/api`.
    Attach {
        /// `<source>/<session>` target.
        target: String,
    },
    /// Diagnose configuration and source reachability.
    Doctor,
    /// Drive a running switcher over its control socket.
    Ctl {
        /// Target the instance with this pid.
        #[arg(long)]
        pid: Option<u32>,
        /// Target this socket path.
        #[arg(long)]
        sock: Option<PathBuf>,
        /// The command to send (e.g. `dump`, `key down`); empty reads from stdin.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print version.
    Version,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let cli = Cli::parse();
    match cli.command {
        None => match interactive_env() {
            Ok(env) => cockpit::run_cockpit(Arc::new(env)).await,
            Err(code) => code,
        },
        Some(Command::Popup) => match interactive_env() {
            Ok(env) => run_popup(Arc::new(env)).await,
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

/// The in-mux switcher (bound via `display-popup -E "xmux popup"`). Same-server
/// pick teleports (switch-client); cross-server detaches to the home loop. Exits
/// after one action so the popup closes back onto the pane.
async fn run_popup(env: Arc<Env>) -> i32 {
    // The switcher paints host skeletons immediately and streams each source's
    // sessions and panes in independently — nothing blocks the first frame.
    // The popup is a one-shot process — no persistent cockpit to cache across, so
    // it seeds fresh each time.
    let result = match run_switcher(env.ops(), control_path(&env), None).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };
    let Some(chosen) = result.chosen else {
        return 0;
    };
    let cockpit_sock = cockpit::read_cockpit_pointer(&env.xmux_dir);
    match cockpit::decide_popup_action(&chosen, session::LOCAL_SOURCE, cockpit_sock.is_some()) {
        cockpit::PopupAction::SwitchClient => {
            // Same-server: switch-client repaints the existing client in place.
            if result.window >= 0 {
                if let Some(src) = env.by_alias.get(&chosen.source) {
                    let _ = manage::select_window(src, &chosen.name, result.window).await;
                }
            }
            let argv = mux::switch_client(&env.local_bin, &chosen.name);
            if let Err(e) = attach::run_attach(&OsExecer, &argv) {
                eprintln!("xmux: {e}");
                return 1;
            }
            0
        }
        cockpit::PopupAction::SignalCockpit => {
            // Cross-server: switch-client cannot cross mux servers. Signal the
            // cockpit to re-attach the target (landing on the picked window), then
            // detach this client — the cockpit regains the terminal and attaches
            // with no picker between.
            let sock = cockpit_sock.expect("SignalCockpit implies a pointer");
            let window = (result.window >= 0).then_some(result.window);
            match cockpit::signal_cockpit_switch(&sock, &chosen.address(), window).await {
                Ok(true) => {
                    let argv = mux::detach_client(&env.local_bin);
                    if let Err(e) = attach::run_attach(&OsExecer, &argv) {
                        eprintln!("xmux: {e}");
                        return 1;
                    }
                    0
                }
                Ok(false) => {
                    eprintln!("xmux: cockpit rejected the switch");
                    1
                }
                Err(_) => {
                    // The pointer named a dead cockpit (e.g. hard-killed): clear it
                    // so the next popup takes the honest no-cockpit path at once.
                    cockpit::remove_cockpit_pointer(&env.xmux_dir);
                    eprintln!(
                        "xmux: cross-host switch needs the xmux cockpit; start your terminal with `xmux`"
                    );
                    1
                }
            }
        }
        cockpit::PopupAction::NoCockpit => {
            eprintln!(
                "xmux: cross-host switch needs the xmux cockpit; start your terminal with `xmux`"
            );
            1
        }
    }
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
    if let Err(e) = attach::run_attach(&OsExecer, &src.attach_command(&target.name, None)) {
        eprintln!("xmux: attach failed: {e}");
        return 1;
    }
    0
}

/// Reports configuration health and per-source reachability. A diagnostic: a
/// malformed config or an unreachable host is reported, not fatal.
async fn run_doctor(env: &Env, cfg_err: Option<anyhow::Error>) -> i32 {
    println!("xmux doctor");

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
    0
}

/// The control socket path to serve for this session, unless `XMUX_CONTROL=0`
/// disables it. A failure to create the dir or open the socket never breaks the
/// UI (the switcher just runs without a control channel).
fn control_path(env: &Env) -> Option<PathBuf> {
    if std::env::var("XMUX_CONTROL").as_deref() == Ok("0") {
        return None;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    Some(control::socket_path(&env.xmux_dir, std::process::id()))
}

/// Drives a running switcher over its control socket. With command args it sends
/// one command; with none it streams commands from stdin. The target is the
/// explicit `--sock`, else `--pid`'s socket, else the newest socket.
async fn run_ctl(env: &Env, pid: Option<u32>, sock: Option<PathBuf>, args: Vec<String>) -> i32 {
    let path = match resolve_ctl_socket(&env.xmux_dir, pid, sock) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xmux ctl: {e}");
            return 1;
        }
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

fn resolve_ctl_socket(
    xmux_dir: &std::path::Path,
    pid: Option<u32>,
    sock: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    match (sock, pid) {
        (Some(s), _) => Ok(s),
        (None, Some(p)) => Ok(control::socket_path(xmux_dir, p)),
        (None, None) => Ok(control::discover(xmux_dir)?),
    }
}

async fn ctl_one(client: &mut control::Client, line: &str) -> i32 {
    match client.do_cmd(line).await {
        Ok(resp) => {
            if resp.ends_with('\n') {
                print!("{resp}");
            } else {
                println!("{resp}");
            }
            0
        }
        Err(e) => {
            eprintln!("xmux ctl: {e}");
            1
        }
    }
}

async fn probe(s: &Source) -> Result<usize, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(6), s.list_sessions()).await {
        Ok(Ok(sessions)) => Ok(sessions.len()),
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
