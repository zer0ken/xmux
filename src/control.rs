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

use crate::model::{FocusTarget, Operation};

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

fn frame_err(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("control: {msg}"))
}

/// Builds the interprocess endpoint name for a `ctl-<pid>.sock` path. On unix the
/// path IS the AF_UNIX socket (and the discovery marker); on Windows, where local
/// sockets are named pipes, the endpoint is the namespaced name `xmux-ctl-<pid>`
/// derived from the pid, and the `.sock` file is a separate filesystem marker the
/// [`discover`] glob finds.
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
        // from the file stem so both `ctl-<pid>` and `cockpit-<pid>` map cleanly.
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

/// A parsed ctl command resolved to its domain meaning. The semantic verbs map to
/// `Operation`; the keystroke-injection surface lives behind a `raw:` namespace and
/// is unstable/test-only.
#[derive(Debug, PartialEq)]
pub enum CtlRequest {
    Op(Operation),
    Status,
    Ping,
    Dump,
    /// Unstable, test-only: inject a raw key event (`raw:key down`).
    RawKey(KeyEvent),
    /// Unstable, test-only: inject raw bytes (`raw:keys 1b5b41`) or text (`raw:text hi`).
    RawBytes(Vec<u8>),
    Unknown(String),
}

/// Resolves a ctl request line to a `CtlRequest`. Semantic verbs (`switch`,
/// `focus`, `rescan`, `quit`, `width`, `toggle-auto-hide`) become `Operation`; the
/// raw keystroke surface is `raw:key` / `raw:keys` / `raw:text`. Anything else is
/// `Unknown` (the dispatcher replies `err: ...`). ctl speaks the DOMAIN here, not
/// internal key names (C-CTL): the wire never references an Action/KeyCode again.
pub fn parse_ctl_op(line: &str) -> CtlRequest {
    let req = parse_request(line);
    match req.verb.as_str() {
        "ping" => CtlRequest::Ping,
        "dump" => CtlRequest::Dump,
        "status" => CtlRequest::Status,
        "rescan" => CtlRequest::Op(Operation::Rescan),
        "quit" => CtlRequest::Op(Operation::Quit),
        "toggle-auto-hide" => CtlRequest::Op(Operation::ToggleAutoHide),
        "switch" if !req.arg.trim().is_empty() => {
            CtlRequest::Op(Operation::Switch { address: req.arg.trim().to_string() })
        }
        "focus" => match FocusTarget::from_str(&req.arg) {
            Some(t) => CtlRequest::Op(Operation::Focus(t)),
            None => CtlRequest::Unknown(line.trim().to_string()),
        },
        "width" => match req.arg.trim().parse::<i32>() {
            Ok(d) => CtlRequest::Op(Operation::TreeWidth(d)),
            Err(_) => CtlRequest::Unknown(line.trim().to_string()),
        },
        "raw:key" => match parse_key(&req.arg) {
            Some(ev) => CtlRequest::RawKey(ev),
            None => CtlRequest::Unknown(line.trim().to_string()),
        },
        "raw:keys" => match parse_hex(req.arg.trim()) {
            Ok(b) => CtlRequest::RawBytes(b),
            Err(_) => CtlRequest::Unknown(line.trim().to_string()),
        },
        "raw:text" => CtlRequest::RawBytes(req.arg.into_bytes()),
        _ => CtlRequest::Unknown(line.trim().to_string()),
    }
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

/// Returns the path of the most-recently-modified `ctl-*.sock` in `dir`. Ties on
/// modification time are broken by the higher pid. Errors if none exist.
pub fn discover(dir: &Path) -> std::io::Result<PathBuf> {
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
    if cands.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("control: no ctl-*.sock found in {}", dir.display()),
        ));
    }
    cands.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    Ok(cands[0].0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader as TokioBufReader;

    #[test]
    fn parse_ctl_op_semantic_verbs() {
        use crate::model::{FocusTarget, Operation};
        assert_eq!(parse_ctl_op("switch jup/api"), CtlRequest::Op(Operation::Switch { address: "jup/api".into() }));
        assert_eq!(parse_ctl_op("focus terminal"), CtlRequest::Op(Operation::Focus(FocusTarget::Terminal)));
        assert_eq!(parse_ctl_op("focus tree"), CtlRequest::Op(Operation::Focus(FocusTarget::Tree)));
        assert_eq!(parse_ctl_op("rescan"), CtlRequest::Op(Operation::Rescan), "COR-1: rescan is now reachable over ctl");
        assert_eq!(parse_ctl_op("quit"), CtlRequest::Op(Operation::Quit));
        assert_eq!(parse_ctl_op("width -2"), CtlRequest::Op(Operation::TreeWidth(-2)));
        assert_eq!(parse_ctl_op("toggle-auto-hide"), CtlRequest::Op(Operation::ToggleAutoHide));
        assert_eq!(parse_ctl_op("status"), CtlRequest::Status);
        assert_eq!(parse_ctl_op("ping"), CtlRequest::Ping);
        assert_eq!(parse_ctl_op("dump"), CtlRequest::Dump);
    }
    #[test]
    fn parse_ctl_op_raw_namespace_is_test_only_surface() {
        assert!(matches!(parse_ctl_op("raw:key down"), CtlRequest::RawKey(_)));
        assert!(matches!(parse_ctl_op("raw:keys 1b5b41"), CtlRequest::RawBytes(b) if b == vec![0x1b, 0x5b, 0x41]));
        assert!(matches!(parse_ctl_op("raw:text hi"), CtlRequest::RawBytes(b) if b == b"hi".to_vec()));
        // A bare `key` (no raw: prefix) is no longer a recognized verb — the keystroke
        // surface is explicitly behind raw:, so the loose old verb is now Unknown.
        assert!(matches!(parse_ctl_op("key down"), CtlRequest::Unknown(_)));
        assert!(matches!(parse_ctl_op("overlay"), CtlRequest::Unknown(_)), "overlay verb retired → focus tree");
    }
    #[test]
    fn parse_ctl_op_rejects_malformed() {
        assert!(matches!(parse_ctl_op("switch"), CtlRequest::Unknown(_)), "switch needs an address");
        assert!(matches!(parse_ctl_op("focus sideways"), CtlRequest::Unknown(_)));
        assert!(matches!(parse_ctl_op("width xx"), CtlRequest::Unknown(_)));
        assert!(matches!(parse_ctl_op("raw:keys zz"), CtlRequest::Unknown(_)), "bad hex");
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
    fn endpoint_name_accepts_cockpit_socket() {
        // A cockpit-<pid>.sock must build a valid endpoint (no panic / error) on
        // every platform, just like ctl-<pid>.sock.
        let p = Path::new("/some/dir/cockpit-1234.sock");
        assert!(endpoint_name(p).is_ok());
    }

    #[test]
    fn discover_newest_then_higher_pid() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(discover(&dir).is_err(), "empty dir must error");

        let older = socket_path(&dir, 100);
        let newer = socket_path(&dir, 200);
        std::fs::write(&older, b"").unwrap();
        std::fs::write(&newer, b"").unwrap();
        // Make `older` distinctly older.
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        filetime_set(&older, hour_ago);

        assert_eq!(discover(&dir).unwrap(), newer);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_tie_break_higher_pid() {
        let dir = std::env::temp_dir().join(format!("xmux-ctl-tie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let a = socket_path(&dir, 100);
        let b = socket_path(&dir, 200);
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        // Same mtime for both: tie-break must pick the higher pid.
        let ts = std::time::SystemTime::now();
        filetime_set(&a, ts);
        filetime_set(&b, ts);

        assert_eq!(discover(&dir).unwrap(), b);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sets a file's mtime via the filesystem so Discover's ordering is testable
    /// without depending on write order resolution.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
