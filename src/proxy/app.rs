//! The cockpit's focus state. Both states draw the SAME split (tree on the left,
//! the cursor session's live grid on the right); the state only chooses the
//! focused side and where keys go. `Tree` focuses the tree (keys navigate);
//! `Terminal` focuses the terminal pane (keys forward to the session's pane). The
//! divider rule is colored to mark the focused side. `prefix Tab` toggles.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Tree focused — keys navigate the host/session tree.
    Tree,
    /// Terminal focused — keys forward to the selected session's active pane.
    Terminal,
}

pub struct App {
    pub state: Focus,
}

impl App {
    /// Starts in `Tree` (the cursor preselected on the most-recent session).
    pub fn new() -> Self {
        App { state: Focus::Tree }
    }

    /// Whether the tree sidebar currently owns keystrokes. Reads as a question at
    /// every call site (e.g. `if app.is_tree_focused() { app.toggle(); }`).
    pub fn is_tree_focused(&self) -> bool {
        matches!(self.state, Focus::Tree)
    }

    /// Tree ⇄ terminal focus (the `prefix Tab` toggle).
    pub fn toggle(&mut self) {
        self.state = match self.state {
            Focus::Tree => Focus::Terminal,
            Focus::Terminal => Focus::Tree,
        };
    }
}

impl Default for App {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_starts_tree_focused_and_toggles() {
        let mut app = App::new();
        assert!(app.is_tree_focused(), "starts on the tree (cursor preselected)");
        app.toggle();
        assert_eq!(app.state, Focus::Terminal);
        app.toggle();
        assert_eq!(app.state, Focus::Tree);
    }
}
