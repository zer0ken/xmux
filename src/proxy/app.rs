//! The shared application state machine. Two states only: Passthrough (a
//! foreground attachment owns raw stdout at cols×(rows-1) plus the status bar)
//! and Overlay (ratatui owns stdout, drawing the sidebar + terminal view). The
//! transition handoff guarantees exactly one writer touches real stdout: in
//! Overlay every pump is gated off via `LiveOwner`; entering Passthrough paints
//! the restore + status bar under a per-write lock BEFORE re-enabling the
//! foreground pump.

use std::io::Write;

use crate::proxy::run::LiveOwner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppState {
    /// `fg` is the foreground Address; `fg_id` its Attachment id.
    Passthrough { fg: String, fg_id: u64 },
    Overlay,
}

pub struct App {
    pub state: AppState,
    /// The foreground to return to on Esc; `None` at the initial Overlay.
    pub prev_fg: Option<(String, u64)>,
    live: LiveOwner,
}

impl App {
    /// Starts in Overlay (the cursor preselected elsewhere), no previous fg.
    pub fn new(live: LiveOwner) -> Self {
        live.set_overlay();
        App {
            state: AppState::Overlay,
            prev_fg: None,
            live,
        }
    }

    pub fn is_overlay(&self) -> bool {
        matches!(self.state, AppState::Overlay)
    }

    /// What Esc returns to (the previous foreground), or None at the initial state.
    pub fn esc_target(&self) -> Option<(String, u64)> {
        self.prev_fg.clone()
    }

    /// Passthrough → Overlay: remember the foreground, gate all pumps off stdout.
    ///
    /// # Residual TOCTOU window
    ///
    /// After `live.set_overlay()` returns, a foreground pump that already passed
    /// its `is_owner(id)` check but has not yet executed its `pump_write` call
    /// can still land one chunk on real stdout. This is an accepted one-frame
    /// flicker. The consuming loop (Task 12) MUST call `term.clear()` followed
    /// by a full ratatui redraw immediately after `enter_overlay` returns, which
    /// overdrawing any such stray chunk.
    pub fn enter_overlay(&mut self) {
        if let AppState::Passthrough { fg, fg_id } = &self.state {
            self.prev_fg = Some((fg.clone(), *fg_id));
        }
        self.state = AppState::Overlay;
        self.live.set_overlay();
    }

    /// Overlay → Passthrough{fg}: while pumps are still gated off stdout, paint
    /// the foreground restore + status bar under ONE per-write lock; then set the
    /// state and re-enable the foreground pump (it resumes raw writes). This
    /// mirrors the reviewed overlay-close restore in `proxy_attach`.
    pub fn enter_passthrough(&mut self, fg: String, fg_id: u64, restore: &[u8], status_bar: &[u8]) {
        // Pumps are gated off here (we are still Overlay/owner=sentinel), so this
        // single lock cannot race a pump write.
        {
            let out = std::io::stdout();
            let mut lock = out.lock();
            let _ = lock.write_all(restore);
            let _ = lock.write_all(status_bar);
            let _ = lock.flush();
        }
        self.state = AppState::Passthrough { fg, fg_id };
        self.live.set_owner(fg_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::run::LiveOwner;

    #[test]
    fn starts_in_overlay_with_no_prev_fg() {
        let app = App::new(LiveOwner::new());
        assert!(app.is_overlay());
        assert!(app.esc_target().is_none(), "initial Overlay has no Esc target");
    }

    #[test]
    fn overlay_remembers_then_esc_returns_previous() {
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        // Enter passthrough on local/a (id 1): live owner becomes 1.
        app.enter_passthrough("local/a".into(), 1, b"", b"");
        assert!(!app.is_overlay());
        assert!(live.is_owner(1));
        // Open overlay: pumps stop writing stdout; prev_fg remembers local/a.
        app.enter_overlay();
        assert!(app.is_overlay());
        assert!(!live.is_owner(1), "overlay gates all pumps off stdout");
        assert_eq!(app.esc_target(), Some(("local/a".to_string(), 1)));
    }

    #[test]
    fn passthrough_switch_changes_live_owner() {
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        app.enter_passthrough("local/a".into(), 1, b"", b"");
        app.enter_overlay();
        // Enter on a new session promotes jupiter06/b (id 2) to foreground.
        app.enter_passthrough("jupiter06/b".into(), 2, b"", b"");
        assert!(live.is_owner(2));
        assert!(!live.is_owner(1));
    }
}
