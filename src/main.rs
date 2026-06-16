//! Command `xmux` is a stateless cross-environment session switcher: one terminal
//! that sees and moves between every reachable tmux/psmux session — local and over
//! ssh — regardless of OS or mux kind.

use std::sync::Arc;

use clap::{Parser, Subcommand};

use xmux::attach::{self, OsExecer};
use xmux::env::{self, ls_lines, Env};
use xmux::manage;
use xmux::session;
use xmux::source::Source;
use xmux::ui::run::{no_control, run_switcher};

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
            Ok(env) => run_home(Arc::new(env)).await,
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

/// The full-screen switcher. Pick a session → attach; on detach, control returns
/// here, the tree is re-scanned and re-rendered (detach-to-home). Loops until the
/// user quits.
async fn run_home(env: Arc<Env>) -> i32 {
    if attach::in_mux() {
        eprintln!(
            "xmux: warning — inside a mux; attach is refused here. Detach first (prefix d), or bind `xmux popup`."
        );
    }
    let ops = env.ops();
    loop {
        eprintln!("xmux: scanning sessions… (probing local + ssh hosts)");
        let scan = env.deep_scan().await;
        let result = match run_switcher(scan, ops.clone(), no_control).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("xmux: {e}");
                return 1;
            }
        };
        let Some(chosen) = result.chosen else {
            return 0;
        };
        let Some(src) = env.by_alias.get(&chosen.source).cloned() else {
            eprintln!("xmux: unknown source {:?}", chosen.source);
            continue;
        };
        if let Err(e) = attach::nest_guard(attach::in_mux()) {
            eprintln!("xmux: {e}");
            continue;
        }
        if result.window >= 0 {
            // Land on the chosen window (best-effort; an attach still proceeds).
            let _ = manage::select_window(&src, &chosen.name, result.window).await;
        }
        if let Err(e) = attach::run_attach(&OsExecer, &src.attach_command(&chosen.name)) {
            eprintln!("xmux: attach failed: {e}");
        }
    }
}

/// The in-mux switcher (bound via `display-popup -E "xmux popup"`). Same-server
/// pick teleports (switch-client); cross-server detaches to the home loop. Exits
/// after one action so the popup closes back onto the pane.
async fn run_popup(env: Arc<Env>) -> i32 {
    eprintln!("xmux: scanning sessions… (probing local + ssh hosts)");
    let scan = env.deep_scan().await;
    let result = match run_switcher(scan, env.ops(), no_control).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };
    let Some(chosen) = result.chosen else {
        return 0;
    };
    let plan = attach::plan_switch(session::LOCAL_SOURCE, &env.local_bin, &chosen);
    if result.window >= 0 && plan.teleport {
        // Same-server teleport: pre-select the window so switch-client lands on it.
        if let Some(src) = env.by_alias.get(&chosen.source) {
            let _ = manage::select_window(src, &chosen.name, result.window).await;
        }
    }
    if let Err(e) = attach::run_attach(&OsExecer, &plan.argv) {
        eprintln!("xmux: {e}");
        return 1;
    }
    0
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
    if let Err(e) = attach::run_attach(&OsExecer, &src.attach_command(&target.name)) {
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
