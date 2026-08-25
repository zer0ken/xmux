//! The `xmux` CLI: argument parsing and command dispatch (`ls`/`attach`/`doctor`/
//! `instances`/`send`/`version` and the default interactive app). `run` is the single
//! entry the binary shim calls.
//!
//! A running instance is addressed by NAME, not pid: it takes one at startup (auto
//! generated, or `--name`), owns `ctl-<name>.sock` while it lives, and answers to
//! `xmux send <name> <command>`. `xmux instances` lists the live ones. Names are
//! resolved by exact match first, then by unique prefix, so `xmux send am <cmd>`
//! reaches `amber-otter` when nothing else starts with `am`.

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
    long_about = "xmux shows every reachable tmux/psmux/zellij session (local + ssh) as one list and switches between them."
)]
struct Cli {
    /// Name this instance (default: an auto-generated `<adjective>-<noun>`). Lowercase
    /// letters, digits, and `-` only; it becomes this instance's control socket name
    /// and its address for `xmux send`.
    #[arg(long, global = true)]
    name: Option<String>,
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
    /// List every running instance (name, pid, cwd, tty, displayed session, focus).
    Instances,
    /// Send a command to a running instance, e.g. `xmux send amber-otter switch prod/api`.
    Send {
        /// The instance name, or any unambiguous prefix of one (`xmux instances` lists
        /// them). With exactly one instance running, `-` targets it.
        id: String,
        /// The command to send (e.g. `switch prod/api`, `focus terminal`, `dump`);
        /// empty reads commands from stdin, one per line.
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
        None => match interactive_env().await {
            Ok(env) => match resolve_requested_name(cli.name.as_deref()) {
                Ok(requested) => runtime::run_app(Arc::new(env), requested).await,
                Err(code) => code,
            },
            Err(code) => code,
        },
        Some(Command::Ls) => match interactive_env().await {
            Ok(env) => run_ls(&env).await,
            Err(code) => code,
        },
        Some(Command::Attach { target }) => match interactive_env().await {
            Ok(env) => run_direct_attach(&env, &target).await,
            Err(code) => code,
        },
        Some(Command::Doctor) => {
            // Tolerate a malformed config — report it, don't die on it.
            let (env, cfg_err) = env::build_env().await;
            run_doctor(&env, cfg_err).await
        }
        Some(Command::Instances) => {
            // Instance listing only needs the xmux dir (independent of config validity).
            let (env, _cfg_err) = env::build_env().await;
            run_instances(&env).await
        }
        Some(Command::Send { id, args }) => {
            let (env, _cfg_err) = env::build_env().await;
            run_send(&env, &id, args).await
        }
        Some(Command::Version) => {
            println!("xmux {}", env!("CARGO_PKG_VERSION"));
            0
        }
    }
}

/// Validates an explicit `--name`. Rejected up front rather than silently rewritten,
/// because the name is how the user will address this instance later: a name that came
/// back different from what they typed would make `xmux send` fail confusingly. `None`
/// means "generate one".
fn resolve_requested_name(requested: Option<&str>) -> Result<Option<String>, i32> {
    match requested {
        None => Ok(None),
        Some(raw) => match control::sanitize_name(raw) {
            Some(n) => Ok(Some(n)),
            None => {
                eprintln!(
                    "xmux: invalid --name {raw:?} (1-32 chars of a-z, 0-9, and '-', not starting with '-')"
                );
                Err(1)
            }
        },
    }
}

/// Builds the env for an interactive command, treating a config-parse error as
/// fatal (printing it and returning the exit code in `Err`).
async fn interactive_env() -> Result<Env, i32> {
    let (env, cfg_err) = env::build_env().await;
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
    let Some(src) = env.source(&target.source) else {
        eprintln!(
            "xmux: unknown source {:?} (not local or an ssh-config host)",
            target.source
        );
        return 1;
    };
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

    // Where the list came from matters when it is short a mux the user expected: a
    // discovered list means the mux did not answer here, a configured one means it was
    // never asked for.
    let source = if env.cfg.local.mux.is_auto() {
        "discovered"
    } else {
        "from config.toml"
    };
    println!("local mux: {} ({source})", env.local_muxes.join(", "));
    println!(
        "{}",
        crate::ui::palette::selection_report(crate::ui::chrome::parse_selection_bg(
            &env.cfg.ui.selection_style
        ))
    );
    if ssh_on_path() {
        println!("ssh: ok");
    } else {
        println!("ssh: NOT FOUND on PATH — remote sources unavailable");
    }

    println!("sources:");
    for s in &env.source_list() {
        // The pair reads as one label, the way every surface shows it. The binary follows
        // only where it is not the mux's own name (an alias, a path), which is a fact the
        // label cannot carry and a diagnostic wants.
        let kind = crate::mux::for_binary(&s.binary).kind().to_string();
        let label = crate::session::source_label(crate::session::machine_of(&s.alias), &kind);
        let via = if s.binary == kind {
            String::new()
        } else {
            format!(" ({})", s.binary)
        };
        match probe(s).await {
            Ok(n) => println!("  {label}{via}: ok, {n} session(s)"),
            Err(e) => println!("  {label}{via}: UNREACHABLE — {e}"),
        }
    }
    i32::from(config_broken)
}

/// Sends one command (or a stdin stream of them) to the instance `id` names. `id` is
/// an instance name, any unambiguous prefix of one, or `-` for the sole live instance.
/// Resolution runs over LIVE instances only, so a crashed instance's leftover marker
/// never shadows a running one that shares its prefix.
async fn run_send(env: &Env, id: &str, args: Vec<String>) -> i32 {
    let live = live_instances(&env.xmux_dir).await;
    let path = match resolve_target(id, &live) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xmux send: {e}");
            return 1;
        }
    };
    let mut client = match control::Client::dial(&path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xmux send: {e}");
            return 1;
        }
    };
    if !args.is_empty() {
        return send_one(&mut client, &args.join(" ")).await;
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
        let rc = send_one(&mut client, line).await;
        if rc != 0 {
            return rc;
        }
    }
    0
}

/// The live instances (a dialable `ctl-<name>.sock`) among the discovered markers. A
/// crashed instance's stale marker does not dial and is filtered out, so it never
/// resolves a name nor counts toward `-`.
async fn live_instances(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut live = Vec::new();
    for (path, name) in control::discover_all(dir).unwrap_or_default() {
        if control::Client::dial(&path).await.is_ok() {
            live.push((path, name));
        }
    }
    live
}

/// Resolves `id` against the live instances: `-` takes the sole one, an exact name wins
/// outright, else a unique name PREFIX. Ambiguity is an error naming the candidates
/// rather than a guess, because sending a command to the wrong instance switches the
/// wrong terminal. Pure over the already-filtered live set, so it is unit-testable.
fn resolve_target(id: &str, live: &[(PathBuf, String)]) -> anyhow::Result<PathBuf> {
    if live.is_empty() {
        anyhow::bail!("no running xmux instance found");
    }
    if id == "-" {
        return match live {
            [(path, _)] => Ok(path.clone()),
            many => anyhow::bail!(
                "{} instances running; name one (run `xmux instances`)",
                many.len()
            ),
        };
    }
    if let Some((path, _)) = live.iter().find(|(_, n)| n == id) {
        return Ok(path.clone());
    }
    let hits: Vec<&(PathBuf, String)> = live.iter().filter(|(_, n)| n.starts_with(id)).collect();
    match hits.as_slice() {
        [] => anyhow::bail!("no instance named {id:?} (run `xmux instances`)"),
        [(path, _)] => Ok(path.clone()),
        many => {
            let names: Vec<&str> = many.iter().map(|(_, n)| n.as_str()).collect();
            anyhow::bail!("{id:?} matches {}", names.join(", "))
        }
    }
}

/// A framed reply beginning `err:` is the control protocol's command-level failure
/// signal (an unknown verb or a rejected op - see `ui::run::dispatch`). Distinguished
/// from a transport error so `xmux send` reports a refused command as a failure a script
/// can detect, not a silent success.
fn reply_is_err(resp: &str) -> bool {
    resp.starts_with("err:")
}

/// Whether a transport failure AFTER the request went out means the command
/// succeeded. Only `quit` qualifies: it asks the instance to exit, so the socket
/// closing under the reply is the instance doing exactly that. Reporting it as a
/// failure would make the one verb whose success is indistinguishable from a crash
/// always exit non-zero.
fn closing_is_success(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("quit")
}

async fn send_one(client: &mut control::Client, line: &str) -> i32 {
    match client.do_cmd(line).await {
        Ok(resp) => {
            let text = resp.strip_suffix('\n').unwrap_or(&resp);
            if reply_is_err(text) {
                // A command-level error: route it like a transport error - to stderr,
                // with the `xmux send:` prefix, exit non-zero - so it is not mistaken
                // for a successful reply on stdout.
                let msg = text.strip_prefix("err: ").unwrap_or(text);
                eprintln!("xmux send: {msg}");
                1
            } else {
                println!("{text}");
                0
            }
        }
        Err(_) if closing_is_success(line) => {
            println!("ok");
            0
        }
        Err(e) => {
            eprintln!("xmux send: {e}");
            1
        }
    }
}

/// Lists every running xmux instance so a specific one can be driven with `xmux send`.
/// Enumerates the `ctl-<name>.sock` markers, then dials each for its `status` (name /
/// pid / cwd / tty / displayed session / focus). A socket that does not answer is a
/// crashed instance's stale marker and is skipped, so the listing shows only live,
/// drivable instances.
async fn run_instances(env: &Env) -> i32 {
    let instances = control::discover_all(&env.xmux_dir).unwrap_or_default();
    let mut rows: Vec<[String; 6]> = vec![[
        "NAME".into(),
        "PID".into(),
        "CWD".into(),
        "TTY".into(),
        "DISPLAYED".into(),
        "FOCUS".into(),
    ]];
    for (path, name) in instances {
        let Ok(mut client) = control::Client::dial(&path).await else {
            continue; // stale marker for a crashed instance
        };
        let Ok(resp) = client.do_cmd("status").await else {
            continue;
        };
        let f = control::parse_status(&resp);
        let cell = |s: String| if s.is_empty() { "-".to_string() } else { s };
        rows.push([
            // The marker name is the authority: it is what `xmux send` dials. The
            // reply's own `name` is only a cross-check, so a mismatch cannot make the
            // listing print an address that does not work.
            name,
            cell(f.pid),
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
/// leave dangling whitespace. Generic over the column count so the row shape stays a
/// fixed-size array (a missing cell cannot compile).
fn format_table<const N: usize>(rows: &[[String; N]]) -> String {
    let mut widths = [0usize; N];
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

    /// A live-instance entry keyed by name, as `live_instances` returns.
    fn inst(name: &str) -> (PathBuf, String) {
        (PathBuf::from(format!("ctl-{name}.sock")), name.to_string())
    }

    #[test]
    fn only_quit_treats_a_closing_socket_as_success() {
        // `quit` asks the instance to exit, so losing the socket under the reply IS the
        // success signal; every other verb must still report a lost socket as a failure.
        assert!(closing_is_success("quit"));
        assert!(closing_is_success("  QUIT  "));
        for line in ["status", "dump", "switch local/api", "quitter", ""] {
            assert!(
                !closing_is_success(line),
                "{line:?} must not swallow an error"
            );
        }
    }

    #[test]
    fn resolve_target_takes_a_name_then_a_unique_prefix() {
        let live = [inst("amber-otter"), inst("brisk-wren")];
        // An exact name wins.
        assert_eq!(
            resolve_target("amber-otter", &live).unwrap(),
            PathBuf::from("ctl-amber-otter.sock")
        );
        // A unique prefix resolves.
        assert_eq!(
            resolve_target("br", &live).unwrap(),
            PathBuf::from("ctl-brisk-wren.sock")
        );
        // An unknown name is an error, not a guess.
        assert!(resolve_target("zz", &live).is_err());
        // Nothing live is an error whatever the id.
        assert!(resolve_target("amber-otter", &[]).is_err());
    }

    #[test]
    fn resolve_target_refuses_an_ambiguous_prefix() {
        // Sending to the wrong instance switches the wrong terminal, so ambiguity must
        // fail loudly and name the candidates.
        let live = [inst("amber-otter"), inst("amber-wren")];
        let err = resolve_target("amber", &live).unwrap_err().to_string();
        assert!(
            err.contains("amber-otter") && err.contains("amber-wren"),
            "{err}"
        );
        // An exact name is still exact even when it PREFIXES another.
        let live = [inst("amber"), inst("amber-wren")];
        assert_eq!(
            resolve_target("amber", &live).unwrap(),
            PathBuf::from("ctl-amber.sock")
        );
    }

    #[test]
    fn resolve_target_dash_takes_the_sole_instance() {
        assert_eq!(
            resolve_target("-", &[inst("amber-otter")]).unwrap(),
            PathBuf::from("ctl-amber-otter.sock")
        );
        // With several running, `-` refuses to guess.
        assert!(resolve_target("-", &[inst("a"), inst("b")]).is_err());
        assert!(resolve_target("-", &[]).is_err());
    }

    #[tokio::test]
    async fn live_instances_filters_out_dead_markers() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Two markers with no live listener: both undialable, so neither counts. This is
        // the crux — a pile of crashed-instance markers must resolve to zero live, not
        // to "many".
        std::fs::write(control::socket_path(&dir, "amber-otter"), b"").unwrap();
        std::fs::write(control::socket_path(&dir, "brisk-wren"), b"").unwrap();
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
                "NAME".into(),
                "PID".into(),
                "CWD".into(),
                "TTY".into(),
                "DISPLAYED".into(),
                "FOCUS".into(),
            ],
            [
                "amber-otter".into(),
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
        assert!(!reply_is_err("focus=nav\ttarget=api"));
    }
}
