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
    // A single char is preserved exactly as given, including case — checked
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
/// buggy client that never sends a newline cannot grow the buffer without limit —
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

/// Builds the interprocess endpoint name for a `ctl-<pid>.sock` path. On unix the
/// path IS the AF_UNIX socket (and the discovery marker); on Windows, where local
/// sockets are named pipes, the endpoint is the namespaced name `xmux-ctl-<pid>`
/// derived from the pid, and the `.sock` file is a separate filesystem marker the
/// [`discover_all`] glob finds.
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
/// session-lifecycle verbs (`new-session`, `kill-session`, `rename-session`,
/// `new-window`, `split-window`, `kill-window`, `rename-window`) become a domain
/// [`Action`]; the raw keystroke surface is `raw:key` / `raw:keys` / `raw:text`.
/// Anything else is `Unknown` (the dispatcher replies `err: ...`). ctl speaks the
/// DOMAIN here, not internal key names (C-CTL): the wire never references an input
/// Action/KeyCode again. A session is addressed `<source>/<session>`, a window
/// `<source>/<session>:<window>` — the same grammar as `switch` and the tree.
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
        "switch" if !req.arg.trim().is_empty() => CtlRequest::Op(Action::Switch {
            address: req.arg.trim().to_string(),
        }),
        "focus" => match FocusTarget::from_str(&req.arg) {
            Some(t) => CtlRequest::Op(Action::Focus(t)),
            None => unknown(),
        },
        "width" => match req.arg.trim().parse::<i32>() {
            Ok(d) => CtlRequest::Op(Action::TreeWidth(d)),
            Err(_) => unknown(),
        },
        // Session lifecycle. Each maps to the SAME domain `Action` a keypress
        // produces; only the addressing is parsed here. `new-session`/`new-window`
        // take an optional name (empty ⇒ the mux auto-names). `KillSession` /
        // `RenameSession` carry a `Session` built from the address via `parse_target`
        // (the op uses only its source + name — see `ui::ops::run_op`).
        "new-session" if !req.arg.trim().is_empty() => {
            let (source, name) = split_first(&req.arg);
            CtlRequest::Op(Action::CreateSession { source, name })
        }
        "kill-session" => match crate::session::parse_target(req.arg.trim()) {
            Ok(sess) => CtlRequest::Op(Action::KillSession { sess }),
            Err(_) => unknown(),
        },
        "rename-session" => {
            let (addr, new_name) = split_first(&req.arg);
            match crate::session::parse_target(&addr) {
                Ok(sess) if !new_name.trim().is_empty() => CtlRequest::Op(Action::RenameSession {
                    sess,
                    new_name: new_name.trim().to_string(),
                }),
                _ => unknown(),
            }
        }
        "new-window" => {
            let (addr, name) = split_first(&req.arg);
            match crate::session::parse_target(&addr) {
                Ok(sess) => CtlRequest::Op(Action::NewWindow {
                    source: sess.source,
                    session: sess.name,
                    name,
                }),
                Err(_) => unknown(),
            }
        }
        "split-window" => {
            let (addr, dir) = split_first(&req.arg);
            match parse_window_addr(&addr) {
                Some((source, session, target)) => {
                    // Vertical unless the direction is horizontal (`h`, `-h`,
                    // `horizontal`) — mirrors the interactive split default.
                    let d = dir.trim().trim_start_matches('-').to_ascii_lowercase();
                    let vertical = !(d == "h" || d == "horizontal");
                    CtlRequest::Op(Action::SplitWindow {
                        source,
                        target,
                        session,
                        vertical,
                    })
                }
                None => unknown(),
            }
        }
        "kill-window" => match parse_window_addr(req.arg.trim()) {
            Some((source, session, target)) => CtlRequest::Op(Action::KillWindow {
                source,
                session,
                target,
            }),
            None => unknown(),
        },
        "rename-window" => {
            let (addr, new_name) = split_first(&req.arg);
            match parse_window_addr(&addr) {
                Some((source, session, target)) if !new_name.trim().is_empty() => {
                    CtlRequest::Op(Action::RenameWindow {
                        source,
                        session,
                        target,
                        new_name: new_name.trim().to_string(),
                    })
                }
                _ => unknown(),
            }
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
/// remainder keeps its inner spaces (a session/window name may contain them) and is
/// empty when the arg is a single token. Both halves are trimmed of surrounding space.
fn split_first(arg: &str) -> (String, String) {
    let arg = arg.trim();
    match arg.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.to_string(), rest.trim().to_string()),
        None => (arg.to_string(), String::new()),
    }
}

/// Parses a window address `<source>/<session>:<window>` into `(source, session,
/// target)`, where `target` is the `<session>:<window>` half the window `Action`s
/// carry. Splits the source on the FIRST `/` (a session name may contain `/`) and the
/// window on the LAST `:` (a session name may contain `:`; the window index never
/// does). `None` unless both a `/` and a `:` are present with non-empty source/session.
fn parse_window_addr(addr: &str) -> Option<(String, String, String)> {
    let (source, rest) = addr.trim().split_once('/')?; // rest = <session>:<window>
    let (session, _window) = rest.rsplit_once(':')?;
    if source.is_empty() || session.is_empty() {
        return None;
    }
    Some((source.to_string(), session.to_string(), rest.to_string()))
}

/// Returns the control socket path for a given pid in `dir`.
pub fn socket_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("ctl-{pid}.sock"))
}

/// Extracts the pid embedded in a `ctl-<pid>.sock` filename, or `None`.
fn pid_from_sock(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("ctl-")?
        .strip_suffix(".sock")?
        .parse()
        .ok()
}

/// Every live `ctl-<pid>.sock` in `dir` as `(path, pid)`, most-recently-modified
/// first (ties broken by the higher pid). The enumeration behind `xmux ctl list` and
/// the multi-instance targeting guard. An empty (or absent-entry) dir yields an empty
/// vec, not an error — the caller decides what "no instances" means.
pub fn discover_all(dir: &Path) -> std::io::Result<Vec<(PathBuf, u32)>> {
    let mut cands: Vec<(PathBuf, std::time::SystemTime, u32)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(pid) = pid_from_sock(&path) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        cands.push((path, modified, pid));
    }
    cands.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    Ok(cands.into_iter().map(|(p, _, pid)| (p, pid)).collect())
}

/// The parsed `status` reply — the per-instance identity `xmux ctl list` shows. The
/// wire form is TAB-separated `key=value` (tab, not space, so a value may itself
/// contain spaces — e.g. a Windows `cwd`). [`format_status`] / [`parse_status`] are
/// inverses; keeping both here is what stops the producer (the app's `status_line`)
/// and the consumer (`xmux ctl list`) from drifting.
#[derive(Debug, Default, PartialEq)]
pub struct StatusFields {
    /// `tree` or `terminal` — which view has focus.
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
        "focus={}\ttarget={}\tcwd={}\ttty={}",
        f.focus, f.target, f.cwd, f.tty
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
            parse_ctl_op("switch jup/api"),
            CtlRequest::Op(Action::Switch {
                address: "jup/api".into()
            })
        );
        assert_eq!(
            parse_ctl_op("focus terminal"),
            CtlRequest::Op(Action::Focus(FocusTarget::Terminal))
        );
        assert_eq!(
            parse_ctl_op("focus tree"),
            CtlRequest::Op(Action::Focus(FocusTarget::Tree))
        );
        assert_eq!(
            parse_ctl_op("rescan"),
            CtlRequest::Op(Action::Rescan),
            "COR-1: rescan is now reachable over ctl"
        );
        assert_eq!(parse_ctl_op("quit"), CtlRequest::Op(Action::Quit));
        assert_eq!(
            parse_ctl_op("width -2"),
            CtlRequest::Op(Action::TreeWidth(-2))
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
    fn parse_ctl_op_session_lifecycle_verbs() {
        use crate::model::Action;
        use crate::session::parse_target;

        // new-session: source + optional name (empty ⇒ the mux auto-names).
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
        // kill/rename-session: a Session built from the <source>/<session> address.
        assert_eq!(
            parse_ctl_op("kill-session local/api"),
            CtlRequest::Op(Action::KillSession {
                sess: parse_target("local/api").unwrap()
            })
        );
        assert_eq!(
            parse_ctl_op("rename-session local/api svc"),
            CtlRequest::Op(Action::RenameSession {
                sess: parse_target("local/api").unwrap(),
                new_name: "svc".into(),
            })
        );
        // window verbs: <source>/<session>:<window>.
        assert_eq!(
            parse_ctl_op("new-window jup/api log"),
            CtlRequest::Op(Action::NewWindow {
                source: "jup".into(),
                session: "api".into(),
                name: "log".into(),
            })
        );
        assert_eq!(
            parse_ctl_op("split-window jup/api:1 -h"),
            CtlRequest::Op(Action::SplitWindow {
                source: "jup".into(),
                target: "api:1".into(),
                session: "api".into(),
                vertical: false,
            })
        );
        assert_eq!(
            parse_ctl_op("split-window jup/api:1"), // no direction ⇒ vertical
            CtlRequest::Op(Action::SplitWindow {
                source: "jup".into(),
                target: "api:1".into(),
                session: "api".into(),
                vertical: true,
            })
        );
        assert_eq!(
            parse_ctl_op("kill-window jup/api:2"),
            CtlRequest::Op(Action::KillWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:2".into(),
            })
        );
        assert_eq!(
            parse_ctl_op("rename-window jup/api:2 build"),
            CtlRequest::Op(Action::RenameWindow {
                source: "jup".into(),
                session: "api".into(),
                target: "api:2".into(),
                new_name: "build".into(),
            })
        );
    }

    #[test]
    fn parse_ctl_op_lifecycle_rejects_malformed() {
        use crate::model::Action;
        assert!(
            matches!(parse_ctl_op("new-session"), CtlRequest::Unknown(_)),
            "new-session needs a source"
        );
        assert!(
            matches!(parse_ctl_op("kill-session noslash"), CtlRequest::Unknown(_)),
            "session verbs need a <source>/<session> address"
        );
        assert!(
            matches!(
                parse_ctl_op("rename-session local/api"),
                CtlRequest::Unknown(_)
            ),
            "rename needs a new name"
        );
        assert!(
            matches!(parse_ctl_op("kill-window jup/api"), CtlRequest::Unknown(_)),
            "window verbs need a :window index"
        );
        // A session name may contain `/` and `:`; the window splits on the LAST `:`.
        assert_eq!(
            parse_ctl_op("kill-window jup/a/b:3"),
            CtlRequest::Op(Action::KillWindow {
                source: "jup".into(),
                session: "a/b".into(),
                target: "a/b:3".into(),
            })
        );
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
        // A bare `key` (no raw: prefix) is not a recognized verb — the keystroke
        // surface is only behind raw:, so it parses as Unknown.
        assert!(matches!(parse_ctl_op("key down"), CtlRequest::Unknown(_)));
        assert!(
            matches!(parse_ctl_op("overlay"), CtlRequest::Unknown(_)),
            "overlay verb retired → focus tree"
        );
    }
    #[test]
    fn parse_ctl_op_rejects_malformed() {
        assert!(
            matches!(parse_ctl_op("switch"), CtlRequest::Unknown(_)),
            "switch needs an address"
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
        // buffer without limit — mirror the response path's MAX_FRAME bound.
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
            socket_path(Path::new("/some/dir"), 1234),
            Path::new("/some/dir").join("ctl-1234.sock")
        );
    }

    #[test]
    fn discover_all_newest_then_higher_pid() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(
            discover_all(&dir).unwrap().is_empty(),
            "empty dir yields no instances"
        );

        let older = socket_path(&dir, 100);
        let newer = socket_path(&dir, 200);
        std::fs::write(&older, b"").unwrap();
        std::fs::write(&newer, b"").unwrap();
        // Make `older` distinctly older.
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        filetime_set(&older, hour_ago);

        let all = discover_all(&dir).unwrap();
        assert_eq!(all.len(), 2, "both sockets enumerated");
        assert_eq!(all[0], (newer, 200), "newest first");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_all_tie_break_higher_pid() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-tie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let a = socket_path(&dir, 100);
        let b = socket_path(&dir, 200);
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        // Same mtime for both: tie-break must order the higher pid first.
        let ts = std::time::SystemTime::now();
        filetime_set(&a, ts);
        filetime_set(&b, ts);

        assert_eq!(discover_all(&dir).unwrap()[0], (b, 200));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_format_parse_roundtrips_with_spaces_in_cwd() {
        let f = StatusFields {
            focus: "terminal".into(),
            target: "jup/api".into(),
            cwd: r"C:\Program Files\xmux".into(), // a value WITH spaces
            tty: "-".into(),
        };
        // Tab-separated so the spaced cwd survives the round-trip.
        assert_eq!(parse_status(&format_status(&f)), f);
    }

    /// Sets a file's mtime via the filesystem so Discover's ordering is testable
    /// without depending on write order resolution.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
