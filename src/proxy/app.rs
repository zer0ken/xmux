//! The cockpit's focus state. Both states draw the SAME split (tree on the left,
//! the cursor session's live grid on the right); the state only chooses the
//! focused side and where keys go. `Overlay` focuses the tree (keys navigate);
//! `Passthrough` focuses the terminal (keys forward to the session's pane). The
//! divider rule is colored to mark the focused side. `prefix Tab` toggles.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    /// Tree focused — keys navigate the host/session tree.
    Overlay,
    /// Terminal focused — keys forward to the selected session's active pane.
    Passthrough,
}

pub struct App {
    pub state: AppState,
}

impl App {
    /// Starts in Overlay (the cursor preselected on the most-recent session).
    pub fn new() -> Self {
        App {
            state: AppState::Overlay,
        }
    }

    pub fn is_overlay(&self) -> bool {
        matches!(self.state, AppState::Overlay)
    }

    /// Tree ⇄ terminal focus (the `prefix Tab` toggle).
    pub fn toggle(&mut self) {
        self.state = match self.state {
            AppState::Overlay => AppState::Passthrough,
            AppState::Passthrough => AppState::Overlay,
        };
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_starts_overlay_and_toggles() {
        let mut app = App::new();
        assert!(app.is_overlay());
        app.toggle();
        assert_eq!(app.state, AppState::Passthrough);
        app.toggle();
        assert_eq!(app.state, AppState::Overlay);
    }
}
