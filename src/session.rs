//! The cross-environment data types: a [`Session`] living on a source (mux
//! server), its windows-and-panes detail, and the `<source>/<name>` address that
//! targets one session across the server boundary.

/// The reserved source name for the local mux server.
pub const LOCAL_SOURCE: &str = "local";

/// One mux session as seen on a source.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    /// `"local"` or an ssh alias.
    pub source: String,
    /// Session name (may contain `/`).
    pub name: String,
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

/// Joins a source and session name into a `"<source>/<name>"` address — the single
/// spelling of the address grammar (the inverse of [`source_of`] / [`parse_target`]).
pub fn address_of(source: &str, name: &str) -> String {
    format!("{source}/{name}")
}

/// The source half of a `"<source>/<name>"` address: everything before the first
/// `/` (the same split rule as [`parse_target`]). A string with no `/` is returned
/// whole. Does not validate — use [`parse_target`] when both halves are required.
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
