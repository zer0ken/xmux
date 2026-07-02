//! [`Selection`] — the canonical display target (`source`/`session`/`window`), the
//! single source of truth the display reads. A pure domain value with no dependency on
//! the orchestration (`app`) layer, so `model`/`state`/`driver` no longer import upward
//! to reach it. Deriving a `Selection` from a ui target lives in `app` (it depends on the
//! ui `TerminalViewTarget`); the value itself lives here.

/// The canonical selection — the single source of truth the display reads. The
/// `Switcher` owns the tree + selection; the app commits the selection's target into
/// this struct, and the render, input routing, and spinner all key off it. `window`
/// is `Some` only for a window-row selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub source: String,
    /// Empty ⇒ no terminal view (selection on a host/loading row).
    pub session: String,
    pub window: Option<i64>,
}

impl Selection {
    /// The `AttachRegistry` key — `source/session`, matching `Session::address()`.
    pub fn address(&self) -> String {
        format!("{}/{}", self.source, self.session)
    }

    pub fn is_empty(&self) -> bool {
        self.session.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_addresses_source_slash_session() {
        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
            window: None,
        };
        assert_eq!(sel.address(), "jup/api");
        assert!(Selection::default().is_empty());
    }
}
