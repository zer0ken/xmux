//! The domain command both the UI key handlers and `xmux ctl` resolve to. ctl
//! speaks THIS, not keystrokes, so an agent issues the same commands a keypress
//! does. `Switch.address` is the `source/session[:window]` form spelled out the
//! same way everywhere (Session::address / Selection::address). Session
//! create/kill/rename is OUT of this plan's scope: no ctl parser, no apply arm,
//! and no Mux lifecycle method exist for it, so those variants are intentionally
//! absent (a future release adds the variant + verb + apply arm + Mux method
//! together).

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Switch the display to this `source/session[:window]` address.
    Switch {
        address: String,
    },
    /// Move focus between the tree sidebar and the terminal pane.
    Focus(FocusTarget),
    /// Re-enumerate every host (the `r` re-scan).
    Rescan,
    /// Adjust the tree width by a signed delta.
    TreeWidth(i32),
    /// Toggle auto-hide-tree mode.
    ToggleAutoHide,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Tree,
    Terminal,
}

impl FocusTarget {
    /// Parses the ctl `focus` argument. `mux` is accepted as a render-side alias
    /// for `terminal` (the mux pane IS the terminal pane in the cockpit's vocab).
    #[allow(clippy::should_implement_trait)] // intentionally not FromStr: returns Option, not Result
    pub fn from_str(s: &str) -> Option<FocusTarget> {
        match s.trim() {
            "tree" => Some(FocusTarget::Tree),
            "terminal" | "mux" => Some(FocusTarget::Terminal),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_target_parses_aliases() {
        assert_eq!(FocusTarget::from_str("tree"), Some(FocusTarget::Tree));
        assert_eq!(
            FocusTarget::from_str("terminal"),
            Some(FocusTarget::Terminal)
        );
        assert_eq!(
            FocusTarget::from_str("mux"),
            Some(FocusTarget::Terminal),
            "mux is an alias for terminal"
        );
        assert_eq!(
            FocusTarget::from_str(" tree "),
            Some(FocusTarget::Tree),
            "trims"
        );
        assert_eq!(FocusTarget::from_str("sideways"), None);
    }
    #[test]
    fn operation_equality_is_structural() {
        assert_eq!(
            Operation::Switch {
                address: "jup/api".into()
            },
            Operation::Switch {
                address: "jup/api".into()
            }
        );
        assert_ne!(
            Operation::Switch {
                address: "jup/api".into()
            },
            Operation::Switch {
                address: "jup/db".into()
            }
        );
        assert_eq!(
            Operation::Focus(FocusTarget::Tree),
            Operation::Focus(FocusTarget::Tree)
        );
        assert_ne!(Operation::TreeWidth(1), Operation::TreeWidth(-1));
    }
}
