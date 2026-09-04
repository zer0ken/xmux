//! A programmatic control channel that drives the running switcher headlessly. It
//! backs xmux's own tests and the `xmux ctl` command, injecting keystrokes and
//! dumping the rendered screen over a local socket.
//!
//! This module holds the wire protocol (length-framed messages, request parsing,
//! key parsing), socket discovery, and the `xmux ctl` [`Client`]. Keys parse to
//! crossterm [`KeyEvent`]s so the switcher handles injected and real keys through
//! one path. The socket server (accept loop + dispatch) lives in `ui::run`, where
//! it forwards into the event loop's command channel.

use std::path::{Path, PathBuf};

use crate::model::{Action, FocusTarget};

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use interprocess::local_socket::Name;
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};

/// Bounds a single length-framed payload, guarding against a corrupt or hostile
/// length header.
pub const MAX_FRAME: usize = 1 << 24;

/// Maps a key name to a crossterm event. Named keys and `ctrl+<letter>` are
/// matched case-insensitively; a single char is taken verbatim (case preserved,
/// so `"R"` differs from `"r"`). Returns `None` for anything unrecognized.
pub fn parse_key(name: &str) -> Option<KeyEvent> {
    // A single char is preserved exactly as given, including case - checked
    // before lowercasing so "R" and "r" stay distinct.
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    let lc = name.to_lowercase();

    if lc == "space" {
        return Some(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    }

    let named = match lc.as_str() {
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "enter" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pgup" => Some(KeyCode::PageUp),
        "pgdn" => Some(KeyCode::PageDown),
        "backspace" => Some(KeyCode::Backspace),
        "delete" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        _ => None,
    };
    if let Some(code) = named {
        return Some(KeyEvent::new(code, KeyModifiers::NONE));
    }

    // ctrl+<letter> or ctrl-<letter>, letter a-z (case-insensitive).
    if let Some(rest) = lc
        .strip_prefix("ctrl+")
        .or_else(|| lc.strip_prefix("ctrl-"))
    {
        let mut rc = rest.chars();
        if let (Some(c), None) = (rc.next(), rc.next()) {
            if c.is_ascii_lowercase() {
                return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
            }
        }
    }

    None
}

/// A parsed request line: a lowercased verb and the verbatim remainder.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub verb: String,
    pub arg: String,
}

/// Splits a request line on its first space. The verb is lowercased; the arg is
/// the remainder verbatim. A trailing CR/LF is trimmed from the line first.
pub fn parse_request(line: &str) -> Request {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.find(' ') {
        Some(i) => Request {
            verb: line[..i].to_lowercase(),
            arg: line[i + 1..].to_string(),
        },
        None => Request {
            verb: line.to_lowercase(),
            arg: String::new(),
        },
    }
}

/// Writes `payload` as a length-framed message: a decimal byte count, a newline,
/// then the raw payload bytes.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &str) -> std::io::Result<()> {
    w.write_all(format!("{}\n", payload.len()).as_bytes())
        .await?;
    w.write_all(payload.as_bytes()).await?;
    w.flush().await
}

/// Reads a length-framed message written by [`write_frame`]. The length header
/// must not exceed [`MAX_FRAME`].
pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<String> {
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let n: i64 = line
        .trim_end_matches(['\r', '\n'])
        .parse()
        .map_err(|_| frame_err(format!("bad frame length {line:?}")))?;
    if n < 0 || n as usize > MAX_FRAME {
        return Err(frame_err(format!("frame length {n} out of range")));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf).await?;
    String::from_utf8(buf).map_err(|e| frame_err(e.to_string()))
}

/// Reads one newline-terminated request line, bounded to [`MAX_FRAME`] so a local
/// buggy client that never sends a newline cannot grow the buffer without limit -
/// the request path's symmetric counterpart to [`read_frame`]'s bound. `Ok(None)`
/// signals EOF (close the connection); an over-limit line without a terminating
/// newline is an error.
pub async fn read_request_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let mut bounded = r.take(MAX_FRAME as u64);
    let n = bounded.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if bounded.limit() == 0 && !line.ends_with('\n') {
        return Err(frame_err("request line exceeds MAX_FRAME".into()));
    }
    Ok(Some(line))
}

fn frame_err(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("control: {msg}"))
}

/// Builds the interprocess endpoint name for a `ctl-<name>.sock` path. On unix the
/// path IS the AF_UNIX socket (and the discovery marker); on Windows, where local
/// sockets are named pipes, the endpoint is the namespaced name `xmux-ctl-<name>`
/// derived from the file stem, and the `.sock` file is a separate filesystem marker
/// [`discover_all`] finds. Instance names are restricted to `[a-z0-9-]` by
/// [`sanitize_name`], so a name is always a legal path segment and pipe name.
pub fn endpoint_name(path: &Path) -> std::io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        path.to_owned()
            .into_os_string()
            .to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        // Local sockets are named pipes on Windows; derive the namespaced name
        // from the file stem so `ctl-<pid>` maps cleanly.
        let stem = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "control: socket path has no usable file stem",
            )
        })?;
        format!("xmux-{stem}").to_ns_name::<GenericNamespaced>()
    }
}

/// A minimal control-channel client for the `xmux ctl` command.
pub struct Client {
    stream: BufReader<Stream>,
}

impl Client {
    /// Connects to a control socket.
    pub async fn dial(path: &Path) -> std::io::Result<Client> {
        let stream = Stream::connect(endpoint_name(path)?).await?;
        Ok(Client {
            stream: BufReader::new(stream),
        })
    }

    /// Sends one request line and returns the framed response payload. The read is
    /// bounded so `xmux ctl` cannot hang forever on a switcher that never replies.
    pub async fn do_cmd(&mut self, line: &str) -> std::io::Result<String> {
        self.stream
            .write_all(format!("{line}\n").as_bytes())
            .await?;
        self.stream.flush().await?;
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_frame(&mut self.stream),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "control: response timed out",
            )),
        }
    }
}

/// Parses a hex string (`"1b5b41"`) into bytes. Lives here so the wire parser and
/// the dispatcher share one decoder.
pub(crate) fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("err: odd-length hex string".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("err: invalid hex '{}'", &s[i..i + 2]))
        })
        .collect()
}

/// A parsed ctl command resolved to its domain meaning. The semantic verbs map to a
/// domain [`Action`]; the keystroke-injection surface lives behind a `raw:` namespace
/// and is unstable/test-only.
#[derive(Debug, PartialEq)]
pub enum CtlRequest {
    Op(Action),
    Status,
    Ping,
    Dump,
    /// Unstable, test-only: inject a raw key event (`raw:key down`).
    RawKey(KeyEvent),
    /// Unstable, test-only: inject raw bytes (`raw:keys 1b5b41`) or text (`raw:text hi`).
    RawBytes(Vec<u8>),
    Unknown(String),
}

/// Resolves a ctl request line to a `CtlRequest`. The navigation/display verbs
/// (`switch`, `focus`, `rescan`, `quit`, `width`, `toggle-auto-hide`) and the
/// one session-lifecycle verb (`new-session`) become a domain [`Action`]; the raw
/// keystroke surface is `raw:key` / `raw:keys` / `raw:text`. Anything else is
/// `Unknown` (the dispatcher replies `err: ...`). ctl speaks the DOMAIN here, not
/// internal key names (C-CTL): the wire never references an input Action/KeyCode
/// again. A session is named by its source and session separately (`switch <source>
/// <session>`, `new-session <source> [name]`); nothing on the wire joins the two.
/// There are no kill/rename/window verbs: xmux aggregates and switches, so
/// editing a session stays with the mux that owns it.
pub fn parse_ctl_op(line: &str) -> CtlRequest {
    let req = parse_request(line);
    let unknown = || CtlRequest::Unknown(line.trim().to_string());
    match req.verb.as_str() {
        "ping" => CtlRequest::Ping,
        "dump" => CtlRequest::Dump,
        "status" => CtlRequest::Status,
        "rescan" => CtlRequest::Op(Action::Rescan),
        "quit" => CtlRequest::Op(Action::Quit),
        "toggle-auto-hide" => CtlRequest::Op(Action::ToggleAutoHide),
        "switch" => match split_first(&req.arg) {
            (source, session) if !source.is_empty() && !session.is_empty() => CtlRequest::Op(
                Action::Switch(crate::session::Address::new(source, session)),
            ),
            _ => unknown(),
        },
        "focus" => match FocusTarget::from_str(&req.arg) {
            Some(t) => CtlRequest::Op(Action::Focus(t)),
            None => unknown(),
        },
        "width" => match req.arg.trim().parse::<i32>() {
            Ok(d) => CtlRequest::Op(Action::NavWidth(d)),
            Err(_) => unknown(),
        },
        // Session lifecycle. Each maps to the SAME domain `Action` a keypress
        // produces; only the addressing is parsed here. `switch` and `new-session`
        // split the first token off as the SOURCE and keep the rest as the session
        // name, so a name containing spaces needs no quoting. `new-session` takes an
        // optional name (empty ⇒ auto-named: by the mux, or by the manage layer for a
        // mux that cannot name its own).
        "new-session" if !req.arg.trim().is_empty() => {
            let (source, name) = split_first(&req.arg);
            CtlRequest::Op(Action::CreateSession { source, name })
        }
        "raw:key" => match parse_key(&req.arg) {
            Some(ev) => CtlRequest::RawKey(ev),
            None => unknown(),
        },
        "raw:keys" => match parse_hex(req.arg.trim()) {
            Ok(b) => CtlRequest::RawBytes(b),
            Err(_) => unknown(),
        },
        "raw:text" => CtlRequest::RawBytes(req.arg.into_bytes()),
        _ => unknown(),
    }
}

/// Splits an arg into (first whitespace-delimited token, verbatim remainder). The
/// remainder keeps its inner spaces (a session name may contain them) and is
/// empty when the arg is a single token. Both halves are trimmed of surrounding space.
fn split_first(arg: &str) -> (String, String) {
    let arg = arg.trim();
    match arg.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.to_string(), rest.trim().to_string()),
        None => (arg.to_string(), String::new()),
    }
}

/// Returns the control socket path for an instance NAME in `dir`. The name is the
/// instance's identity on the wire and on disk: `xmux send <name>` dials exactly this
/// path, so the two cannot drift.
pub fn socket_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("ctl-{name}.sock"))
}

/// Extracts the instance name embedded in a `ctl-<name>.sock` filename, or `None`.
/// Re-sanitized on the way out, so a hand-dropped file with a hostile stem never
/// becomes a name the CLI will print or dial.
fn name_from_sock(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    sanitize_name(file.strip_prefix("ctl-")?.strip_suffix(".sock")?)
}

/// Normalizes a requested instance name, or `None` if it cannot be one. Accepts 1 to
/// 32 characters of `[a-z0-9-]` after lowercasing; anything else (a path separator, a
/// space, a dot) is refused rather than escaped, because the name becomes both a file
/// name and a Windows pipe name. A leading `-` is refused too, so a name can never be
/// mistaken for a CLI flag.
pub fn sanitize_name(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s.len() > 32 || s.starts_with('-') {
        return None;
    }
    if s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some(s)
    } else {
        None
    }
}

/// The adjective half of the generated instance names.
const ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "clever", "cosmic", "eager", "fluent", "gentle", "humble", "keen",
    "lucid", "mellow", "nimble", "placid", "quiet", "rapid", "solid", "spry", "sunny", "vivid",
];

/// The noun half of the generated instance names.
const NOUNS: &[&str] = &[
    "otter", "heron", "lynx", "marten", "osprey", "puffin", "raven", "shrike", "stoat", "tern",
    "vole", "wren", "auk", "civet", "dingo", "egret", "finch", "gecko", "ibex", "kite",
];

/// The `n`th generated instance name: `<adjective>-<noun>`. The two halves advance at
/// different rates (nouns every step, adjectives every full pass), so consecutive `n`
/// never repeat a pair until all 400 are used. Deterministic, so no RNG dependency and
/// a test can name the sequence exactly.
pub fn nth_name(n: u64) -> String {
    let noun = NOUNS[(n as usize) % NOUNS.len()];
    let adj = ADJECTIVES[((n as usize) / NOUNS.len()) % ADJECTIVES.len()];
    format!("{adj}-{noun}")
}

/// How many distinct names the [`nth_name`] walk yields before it repeats: the bound
/// every walk over it shares (instance naming below, session naming in the manage
/// layer).
pub(crate) fn nth_name_total() -> u64 {
    (ADJECTIVES.len() * NOUNS.len()) as u64
}

/// Picks a free instance name in `dir`, starting the [`nth_name`] walk at `seed` (the
/// pid, so two instances started at once rarely probe the same name first) and stepping
/// until a name no LIVE instance holds. A marker whose socket does not answer is a
/// crashed instance's leftover and is reused, which is what keeps a long-running
/// machine from exhausting the list. Falls back to `<seed>` after a full pass, so this
/// always returns a usable name.
pub async fn pick_free_name(dir: &Path, seed: u64) -> String {
    for step in 0..nth_name_total() {
        let name = nth_name(seed.wrapping_add(step));
        let path = socket_path(dir, &name);
        if !path.exists() || Client::dial(&path).await.is_err() {
            return name;
        }
    }
    seed.to_string()
}

/// Every `ctl-<name>.sock` marker in `dir` as `(path, name)`, most-recently-modified
/// first (ties broken by name). The enumeration behind `xmux instances` and `xmux
/// send`'s name resolution. An empty (or absent-entry) dir yields an empty vec, not an
/// error: the caller decides what "no instances" means.
pub fn discover_all(dir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut cands: Vec<(PathBuf, std::time::SystemTime, String)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = name_from_sock(&path) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        cands.push((path, modified, name));
    }
    cands.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    Ok(cands.into_iter().map(|(p, _, n)| (p, n)).collect())
}

/// Removes every stale `ctl-<name>.sock` marker in `dir`, one whose control socket no
/// longer answers a dial, except `keep` (this instance's own name). A cleanly-exiting
/// instance
/// removes its own marker on drop, but a crash or hard-kill leaves one behind; without
/// this sweep they accumulate and make instance discovery over-count dead instances.
/// Best-effort: a marker that cannot be removed is skipped. Safe against a just-started
/// peer because the marker is written only AFTER its listener binds, so an existing
/// marker whose dial fails is genuinely dead.
pub async fn prune_stale(dir: &Path, keep: &str) {
    for (path, name) in discover_all(dir).unwrap_or_default() {
        if name == keep {
            continue;
        }
        if Client::dial(&path).await.is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The parsed `status` reply: the per-instance identity `xmux instances` shows. The
/// wire form is TAB-separated `key=value` (tab, not space, so a value may itself
/// contain spaces - e.g. a Windows `cwd`). [`format_status`] / [`parse_status`] are
/// inverses; keeping both here is what stops the producer (the app's `status_line`)
/// and the consumer (`xmux instances`) from drifting.
#[derive(Debug, Default, PartialEq)]
pub struct StatusFields {
    /// The instance name: its identity for `xmux send`.
    pub name: String,
    /// The instance's process id, so a listing can be acted on outside xmux.
    pub pid: String,
    /// `tree` or `terminal`: which view has focus.
    pub focus: String,
    /// The displayed session address (`source/session`).
    pub target: String,
    /// The instance's working directory.
    pub cwd: String,
    /// The instance's controlling tty (`-` where there is none / on Windows).
    pub tty: String,
}

/// Renders a [`StatusFields`] to the tab-separated wire line the `status` verb replies.
pub fn format_status(f: &StatusFields) -> String {
    format!(
        "name={}\tpid={}\tfocus={}\ttarget={}\tcwd={}\ttty={}",
        f.name, f.pid, f.focus, f.target, f.cwd, f.tty
    )
}

/// Parses a `status` reply line back into [`StatusFields`]. Unknown keys are ignored
/// and missing keys stay empty, so a format that gains a field never breaks an older
/// reader.
pub fn parse_status(line: &str) -> StatusFields {
    let mut f = StatusFields::default();
    for field in line.trim().split('\t') {
        if let Some((k, v)) = field.split_once('=') {
            match k {
                "name" => f.name = v.to_string(),
                "pid" => f.pid = v.to_string(),
                "focus" => f.focus = v.to_string(),
                "target" => f.target = v.to_string(),
                "cwd" => f.cwd = v.to_string(),
                "tty" => f.tty = v.to_string(),
                _ => {}
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader as TokioBufReader;

    #[test]
    fn parse_ctl_op_semantic_verbs() {
        use crate::model::{Action, FocusTarget};
        assert_eq!(
            parse_ctl_op("switch jup api"),
            CtlRequest::Op(Action::Switch(crate::session::Address::new("jup", "api")))
        );
        // The rest after the first token is the session name, spaces and all.
        assert_eq!(
            parse_ctl_op("switch jup my session"),
            CtlRequest::Op(Action::Switch(crate::session::Address::new(
                "jup",
                "my session"
            )))
        );
        assert_eq!(
            parse_ctl_op("focus terminal"),
            CtlRequest::Op(Action::Focus(FocusTarget::Terminal))
        );
        assert_eq!(
            parse_ctl_op("focus nav"),
            CtlRequest::Op(Action::Focus(FocusTarget::Nav))
        );
        assert_eq!(
            parse_ctl_op("rescan"),
            CtlRequest::Op(Action::Rescan),
            "COR-1: rescan is now reachable over ctl"
        );
        assert_eq!(parse_ctl_op("quit"), CtlRequest::Op(Action::Quit));
        assert_eq!(
            parse_ctl_op("width -2"),
            CtlRequest::Op(Action::NavWidth(-2))
        );
        assert_eq!(
            parse_ctl_op("toggle-auto-hide"),
            CtlRequest::Op(Action::ToggleAutoHide)
        );
        assert_eq!(parse_ctl_op("status"), CtlRequest::Status);
        assert_eq!(parse_ctl_op("ping"), CtlRequest::Ping);
        assert_eq!(parse_ctl_op("dump"), CtlRequest::Dump);
    }

    #[test]
    fn parse_ctl_op_new_session_is_the_only_lifecycle_verb() {
        use crate::model::Action;
        // new-session: source + optional name (empty ⇒ auto-named).
        assert_eq!(
            parse_ctl_op("new-session jup api"),
            CtlRequest::Op(Action::CreateSession {
                source: "jup".into(),
                name: "api".into()
            })
        );
        assert_eq!(
            parse_ctl_op("new-session jup"),
            CtlRequest::Op(Action::CreateSession {
                source: "jup".into(),
                name: String::new()
            })
        );
        assert!(
            matches!(parse_ctl_op("new-session"), CtlRequest::Unknown(_)),
            "new-session needs a source"
        );
        // Every mutating verb xmux dropped is no longer a verb at all: the mux owns
        // renaming, killing, and window editing.
        for line in [
            "kill-session local/api",
            "rename-session local/api svc",
            "new-window jup/api log",
            "split-window jup/api:1 -h",
            "kill-window jup/api:2",
            "rename-window jup/api:2 build",
        ] {
            assert!(
                matches!(parse_ctl_op(line), CtlRequest::Unknown(_)),
                "{line:?} must not be a verb"
            );
        }
    }

    #[test]
    fn parse_ctl_op_raw_namespace_is_test_only_surface() {
        assert!(matches!(
            parse_ctl_op("raw:key down"),
            CtlRequest::RawKey(_)
        ));
        assert!(
            matches!(parse_ctl_op("raw:keys 1b5b41"), CtlRequest::RawBytes(b) if b == vec![0x1b, 0x5b, 0x41])
        );
        assert!(
            matches!(parse_ctl_op("raw:text hi"), CtlRequest::RawBytes(b) if b == b"hi".to_vec())
        );
        // A bare `key` (no raw: prefix) is not a recognized verb - the keystroke
        // surface is only behind raw:, so it parses as Unknown.
        assert!(matches!(parse_ctl_op("key down"), CtlRequest::Unknown(_)));
        assert!(
            matches!(parse_ctl_op("overlay"), CtlRequest::Unknown(_)),
            "overlay verb retired → focus nav"
        );
    }
    #[test]
    fn parse_ctl_op_rejects_malformed() {
        assert!(
            matches!(parse_ctl_op("switch"), CtlRequest::Unknown(_)),
            "switch needs a source and a session"
        );
        assert!(
            matches!(parse_ctl_op("switch jup"), CtlRequest::Unknown(_)),
            "switch with no session is refused"
        );
        assert!(matches!(
            parse_ctl_op("focus sideways"),
            CtlRequest::Unknown(_)
        ));
        assert!(matches!(parse_ctl_op("width xx"), CtlRequest::Unknown(_)));
        assert!(
            matches!(parse_ctl_op("raw:keys zz"), CtlRequest::Unknown(_)),
            "bad hex"
        );
        assert!(matches!(parse_ctl_op("bogus"), CtlRequest::Unknown(_)));
    }
    #[test]
    fn parse_hex_round_trips_and_rejects() {
        assert_eq!(parse_hex("1b5b41").unwrap(), vec![0x1b, 0x5b, 0x41]);
        assert!(parse_hex("abc").is_err(), "odd length");
        assert!(parse_hex("zz").is_err(), "non-hex");
    }

    #[test]
    fn parse_key_named() {
        let cases: &[(&str, KeyCode)] = &[
            ("up", KeyCode::Up),
            ("DOWN", KeyCode::Down),
            ("left", KeyCode::Left),
            ("Right", KeyCode::Right),
            ("enter", KeyCode::Enter),
            ("esc", KeyCode::Esc),
            ("escape", KeyCode::Esc),
            ("tab", KeyCode::Tab),
            ("backtab", KeyCode::BackTab),
            ("home", KeyCode::Home),
            ("end", KeyCode::End),
            ("pgup", KeyCode::PageUp),
            ("pgdn", KeyCode::PageDown),
            ("backspace", KeyCode::Backspace),
            ("delete", KeyCode::Delete),
            ("insert", KeyCode::Insert),
        ];
        for &(name, want) in cases {
            let ev = parse_key(name).unwrap_or_else(|| panic!("parse_key({name:?}) = None"));
            assert_eq!(ev.code, want, "parse_key({name:?})");
        }
    }

    #[test]
    fn parse_key_space() {
        let ev = parse_key("space").unwrap();
        assert_eq!(ev.code, KeyCode::Char(' '));
    }

    #[test]
    fn parse_key_ctrl() {
        for name in ["ctrl+c", "ctrl-c", "CTRL+C", "Ctrl-C"] {
            let ev = parse_key(name).unwrap_or_else(|| panic!("parse_key({name:?}) = None"));
            assert_eq!(ev.code, KeyCode::Char('c'), "{name:?}");
            assert!(ev.modifiers.contains(KeyModifiers::CONTROL), "{name:?}");
        }
    }

    #[test]
    fn parse_key_single_rune_case_preserved() {
        let upper = parse_key("R").unwrap();
        assert_eq!(upper.code, KeyCode::Char('R'));
        let lower = parse_key("r").unwrap();
        assert_eq!(lower.code, KeyCode::Char('r'));
        assert_ne!(upper.code, lower.code);
    }

    #[test]
    fn parse_key_unknown() {
        for name in ["nope", "ctrl+", "ctrl+1", "", "fnord"] {
            assert!(
                parse_key(name).is_none(),
                "parse_key({name:?}) should be None"
            );
        }
    }

    #[tokio::test]
    async fn frame_round_trip() {
        for payload in [
            "pong",
            "",
            "a single line",
            "line one\nline two\nline three",
        ] {
            let mut buf = Vec::new();
            write_frame(&mut buf, payload).await.unwrap();
            let mut r = TokioBufReader::new(Cursor::new(buf));
            let got = read_frame(&mut r).await.unwrap();
            assert_eq!(got, payload);
        }
    }

    #[tokio::test]
    async fn read_frame_oversized() {
        let mut r = TokioBufReader::new(Cursor::new(b"99999999\nx".to_vec()));
        assert!(read_frame(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn read_request_line_bounds_unterminated_input() {
        // A local buggy client that never sends a newline must not grow the request
        // buffer without limit - mirror the response path's MAX_FRAME bound.
        let mut r = TokioBufReader::new(Cursor::new(vec![b'x'; MAX_FRAME + 1]));
        assert!(read_request_line(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn read_request_line_reads_normal_and_eof() {
        let mut r = TokioBufReader::new(Cursor::new(b"ping\n".to_vec()));
        assert_eq!(
            read_request_line(&mut r).await.unwrap(),
            Some("ping\n".to_string())
        );
        let mut empty = TokioBufReader::new(Cursor::new(Vec::<u8>::new()));
        assert_eq!(read_request_line(&mut empty).await.unwrap(), None);
    }

    #[test]
    fn parse_request_cases() {
        let cases: &[(&str, &str, &str)] = &[
            ("ping", "ping", ""),
            ("PING\r\n", "ping", ""),
            ("key down", "key", "down"),
            ("text hello world", "text", "hello world"),
            ("text  leading", "text", " leading"),
            ("", "", ""),
        ];
        for &(line, want_verb, want_arg) in cases {
            let got = parse_request(line);
            assert_eq!(got.verb, want_verb, "verb for {line:?}");
            assert_eq!(got.arg, want_arg, "arg for {line:?}");
        }
    }

    #[test]
    fn socket_path_format() {
        assert_eq!(
            socket_path(Path::new("/some/dir"), "amber-otter"),
            Path::new("/some/dir").join("ctl-amber-otter.sock")
        );
    }

    #[test]
    fn sanitize_name_accepts_safe_names_and_refuses_the_rest() {
        assert_eq!(sanitize_name("amber-otter").as_deref(), Some("amber-otter"));
        assert_eq!(
            sanitize_name(" Amber-Otter ").as_deref(),
            Some("amber-otter")
        );
        assert_eq!(sanitize_name("api2").as_deref(), Some("api2"));
        // The name becomes a file name AND a Windows pipe name, so a separator or a
        // dot is refused outright rather than escaped.
        for bad in ["", "   ", "a/b", "a\\b", "a.b", "a b", "a:b", "-lead", ".."] {
            assert!(sanitize_name(bad).is_none(), "{bad:?} must be refused");
        }
        // Over the 32-char cap.
        assert!(sanitize_name(&"a".repeat(33)).is_none());
        assert!(sanitize_name(&"a".repeat(32)).is_some());
    }

    #[test]
    fn nth_name_pairs_do_not_repeat_within_a_pass() {
        // The generated names must stay distinct long enough to be useful: consecutive
        // seeds differ, and a full pass produces no duplicate pair.
        assert_ne!(nth_name(0), nth_name(1));
        let total = (ADJECTIVES.len() * NOUNS.len()) as u64;
        let all: std::collections::HashSet<String> = (0..total).map(nth_name).collect();
        assert_eq!(all.len() as u64, total, "every pair in a pass is distinct");
        // A name is always a legal instance name (so it round-trips through the socket
        // path and back out of `name_from_sock`).
        for n in [0u64, 1, 7, total - 1] {
            let name = nth_name(n);
            assert_eq!(sanitize_name(&name).as_deref(), Some(name.as_str()));
        }
    }

    #[tokio::test]
    async fn pick_free_name_skips_a_live_marker_but_reuses_a_dead_one() {
        let dir = std::env::temp_dir().join(format!("xmux-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A marker with no listener is a crashed instance's leftover: its name is free
        // again, which is what stops a long-running machine exhausting the list.
        let first = nth_name(0);
        std::fs::write(socket_path(&dir, &first), b"").unwrap();
        assert_eq!(
            pick_free_name(&dir, 0).await,
            first,
            "an undialable marker's name is reused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_all_newest_then_name_order() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(
            discover_all(&dir).unwrap().is_empty(),
            "empty dir yields no instances"
        );

        let older = socket_path(&dir, "brisk-wren");
        let newer = socket_path(&dir, "amber-otter");
        std::fs::write(&older, b"").unwrap();
        std::fs::write(&newer, b"").unwrap();
        // Make `older` distinctly older.
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        filetime_set(&older, hour_ago);

        let all = discover_all(&dir).unwrap();
        assert_eq!(all.len(), 2, "both sockets enumerated");
        assert_eq!(
            all[0],
            (newer, "amber-otter".to_string()),
            "newest first, whatever the name sorts as"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_all_tie_break_by_name() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-tie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let a = socket_path(&dir, "amber-otter");
        let b = socket_path(&dir, "brisk-wren");
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        // Same mtime for both: the tie-break is the name, so the listing is stable.
        let ts = std::time::SystemTime::now();
        filetime_set(&a, ts);
        filetime_set(&b, ts);

        assert_eq!(
            discover_all(&dir).unwrap()[0],
            (a, "amber-otter".to_string())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_format_parse_roundtrips_with_spaces_in_cwd() {
        let f = StatusFields {
            name: "amber-otter".into(),
            pid: "48213".into(),
            focus: "terminal".into(),
            target: "jup/api".into(),
            cwd: r"C:\Program Files\xmux".into(), // a value WITH spaces
            tty: "-".into(),
        };
        // Tab-separated so the spaced cwd survives the round-trip.
        assert_eq!(parse_status(&format_status(&f)), f);
    }

    #[tokio::test]
    async fn prune_stale_removes_dead_markers_and_keeps_own() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let own = socket_path(&dir, "amber-otter");
        let dead = socket_path(&dir, "brisk-wren");
        std::fs::write(&own, b"").unwrap();
        std::fs::write(&dead, b"").unwrap();

        // Neither marker has a live listener; prune keeps our own name (skipped without
        // dialing) and removes the other (its dial fails, so that instance is dead).
        prune_stale(&dir, "amber-otter").await;
        assert!(own.exists(), "our own marker is kept");
        assert!(!dead.exists(), "a dead instance's marker is removed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sets a file's mtime via the filesystem so Discover's ordering is testable
    /// without depending on write order resolution.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
