//! The cockpit's two-state machine. ratatui owns stdout in both states, so the
//! state only chooses what is drawn: Overlay paints the switcher (tree + live
//! terminal view); Passthrough draws the selected session's grid fullscreen plus
//! a one-line status bar. The prefix `s` toggles between them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Overlay,
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

    /// Overlay ⇄ Passthrough (the prefix `s` toggle).
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
