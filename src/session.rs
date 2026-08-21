//! The cross-environment data types: a [`Session`] living on a source (mux
//! server), its windows-and-panes detail, and the `<source>/<name>` address that
//! targets one session across the server boundary.

/// The reserved MACHINE name for this box. A local source id is this alone, or this
/// qualified by a mux (see [`source_id`]).
pub const LOCAL_SOURCE: &str = "local";

/// Separates a machine from the mux it serves inside a source id. Not `/`, which
/// already separates a source from a session name, and not a character an ssh alias or
/// a mux binary name carries.
pub const MUX_SEP: char = ':';

/// A source id: one MUX on one MACHINE. `qualified` is false when the machine serves a
/// single mux, and the id is then the bare machine alias - the spelling xmux has always
/// used, kept so a one-mux machine reads and is typed exactly as before. It is true when
/// the machine serves SEVERAL, and the id then names which of them this source is.
///
/// Whether to qualify follows the machine's CONFIGURED mux list, not what is found on
/// it, so an id does not change under the user when a mux turns out to be absent.
pub fn source_id(machine: &str, mux: &str, qualified: bool) -> String {
    if qualified {
        format!("{machine}{MUX_SEP}{mux}")
    } else {
        machine.to_string()
    }
}

/// The MACHINE half of a source id (`local:zellij` -> `local`, `prod` -> `prod`).
/// Everything before the first [`MUX_SEP`]; an unqualified id is returned whole.
pub fn machine_of(source: &str) -> &str {
    source.split(MUX_SEP).next().unwrap_or(source)
}

/// The MUX half of a source id (`local:zellij` -> `zellij`), or `""` when the id is
/// unqualified because its machine serves a single mux.
pub fn mux_of(source: &str) -> &str {
    source.split_once(MUX_SEP).map_or("", |(_, mux)| mux)
}

/// True when `source` names a mux on THIS box, qualified or not. The one spelling of
/// "is this local", so no caller compares a source id to [`LOCAL_SOURCE`] directly and
/// silently stops recognising `local:zellij` as local.
pub fn is_local_source(source: &str) -> bool {
    machine_of(source) == LOCAL_SOURCE
}

/// One mux session as seen on a source.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    /// `"local"` or an ssh alias.
    pub source: String,
    /// Session name (may contain `/`).
    pub name: String,
    /// The kind of the mux serving this session (`"tmux"` / `"psmux"`), stamped by
    /// the path that enumerated it. Empty when unknown (a parsed target, or a
    /// just-created session awaiting re-enumeration); the nav omits it then.
    pub mux: String,
    pub windows: i64,
    pub attached: bool,
    /// Unix seconds; `0` when the mux does not report it.
    pub last_attached: i64,
}

impl Session {
    /// The cross-environment target string, `"<source>/<name>"`.
    pub fn address(&self) -> String {
        address_of(&self.source, &self.name)
    }
}

/// Joins a source and session name into a `"<source>/<name>"` address - the single
/// spelling of the address grammar (the inverse of [`source_of`] / [`parse_target`]).
pub fn address_of(source: &str, name: &str) -> String {
    format!("{source}/{name}")
}

/// The source half of a `"<source>/<name>"` address: everything before the first
/// `/` (the same split rule as [`parse_target`]). A string with no `/` is returned
/// whole. Does not validate - use [`parse_target`] when both halves are required.
pub fn source_of(addr: &str) -> &str {
    addr.split('/').next().unwrap_or(addr)
}

/// One pane within a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub index: i64,
    pub active: bool,
    /// `pane_current_command`.
    pub command: String,
}

/// The panes of a single window, in window order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPanes {
    pub index: i64,
    pub name: String,
    pub active: bool,
    pub panes: Vec<Pane>,
}

/// Splits a `"<source>/<name>"` address on the FIRST `/` so a session name
/// containing `/` is preserved. Both halves must be non-empty.
pub fn parse_target(addr: &str) -> Result<Session, String> {
    match addr.find('/') {
        None => Err(format!("invalid target {addr:?}: want <source>/<session>")),
        Some(i) => {
            let (source, name) = (&addr[..i], &addr[i + 1..]);
            if source.is_empty() || name.is_empty() {
                return Err(format!(
                    "invalid target {addr:?}: source and session must be non-empty"
                ));
            }
            Ok(Session {
                source: source.to_string(),
                name: name.to_string(),
                ..Default::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address() {
        let s = Session {
            source: "local".into(),
            name: "editor".into(),
            ..Default::default()
        };
        assert_eq!(s.address(), "local/editor");
    }

    #[test]
    fn a_source_id_names_its_mux_only_when_the_machine_serves_several() {
        // One mux on a machine keeps the bare alias xmux has always used, so an
        // existing setup's ids, addresses, and typed targets do not move.
        assert_eq!(source_id("local", "psmux", false), "local");
        assert_eq!(source_id("prod", "tmux", false), "prod");
        // Several, and the id says which one this is.
        assert_eq!(source_id("local", "zellij", true), "local:zellij");
        assert_eq!(source_id("prod", "tmux", true), "prod:tmux");
    }

    #[test]
    fn the_machine_half_and_localness_survive_qualification() {
        // Everything that asks "which machine" or "is this box" must keep working on a
        // qualified id; a bare comparison against LOCAL_SOURCE would stop seeing
        // `local:zellij` as local.
        assert_eq!(machine_of("local:zellij"), "local");
        assert_eq!(machine_of("prod"), "prod");
        assert_eq!(machine_of(""), "");
        assert!(is_local_source("local"));
        assert!(is_local_source("local:zellij"));
        assert!(!is_local_source("localhost"));
        assert!(!is_local_source("prod:tmux"));
    }

    #[test]
    fn a_qualified_source_still_addresses_a_session() {
        // The address grammar splits on the FIRST `/`, and a source id carries no `/`,
        // so qualifying it leaves both halves recoverable - including a zellij session
        // name holding a colon.
        let s = Session {
            source: "local:zellij".into(),
            name: "a:b".into(),
            ..Default::default()
        };
        assert_eq!(s.address(), "local:zellij/a:b");
        let back = parse_target(&s.address()).unwrap();
        assert_eq!(back.source, "local:zellij");
        assert_eq!(back.name, "a:b");
        assert_eq!(source_of(&s.address()), "local:zellij");
    }

    #[test]
    fn parse_target_cases() {
        // (input, want_source, want_name, want_err)
        let cases: &[(&str, &str, &str, bool)] = &[
            ("local/editor", "local", "editor", false),
            ("prod/api", "prod", "api", false),
            ("host/a/b", "host", "a/b", false), // session names may contain "/"
            ("noslash", "", "", true),
            ("", "", "", true),
            ("/leading", "", "", true),  // empty source
            ("trailing/", "", "", true), // empty name
        ];
        for &(input, want_source, want_name, want_err) in cases {
            match parse_target(input) {
                Err(_) => assert!(want_err, "parse_target({input:?}) errored unexpectedly"),
                Ok(got) => {
                    assert!(!want_err, "parse_target({input:?}) = {got:?}, want error");
                    assert_eq!(got.source, want_source, "source for {input:?}");
                    assert_eq!(got.name, want_name, "name for {input:?}");
                }
            }
        }
    }

    #[test]
    fn local_source_const() {
        assert_eq!(LOCAL_SOURCE, "local");
    }

    #[test]
    fn source_of_returns_the_source_half() {
        // The source is everything before the first `/` (same split rule as
        // parse_target), so a session name containing `/` keeps its source.
        assert_eq!(source_of("jup/api"), "jup");
        assert_eq!(source_of("local/a/b"), "local");
        // No `/`: the whole string is the source (mirrors split's fallback).
        assert_eq!(source_of("noslash"), "noslash");
    }

    #[test]
    fn address_of_joins_source_and_name() {
        assert_eq!(address_of("jup", "api"), "jup/api");
        // Round-trips with source_of on the source half.
        assert_eq!(source_of(&address_of("jup", "api")), "jup");
    }
}
