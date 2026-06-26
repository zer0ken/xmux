//! The cockpit's focus state machine. Every state draws the SAME split (tree on
//! the left, the cursor session's live grid on the right); focus only chooses
//! where keys go and which divider rule is highlighted. There are four states
//! along two dimensions: the PANE dimension (`Tree` ⇄ `Terminal`, toggled by
//! `prefix Tab`) and a MODAL dimension layered on top (`Popup` for help / inline
//! input / kill-confirm, `Menu` for the right-click context menu). A modal is a
//! first-class focus state that CARRIES the pane it was opened from, so closing it
//! restores that pane structurally — no external "saved focus" variable. "Is a
//! modal open?" is therefore a `match` on `Focus`, and the modal/pane state cannot
//! desync from the switcher because the loop derives it each pass via `sync_modal`.

/// The two real panes — the only targets a modal can restore to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Tree,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Tree pane focused — keys navigate the host/session tree.
    Tree,
    /// Terminal pane focused — keys forward to the selected session's active pane.
    Terminal,
    /// A centered modal popup (help / inline input / kill-confirm) owns keys;
    /// `prior` is the pane to restore when it closes.
    Popup { prior: PaneFocus },
    /// The right-click context menu owns input; `prior` is the pane to restore.
    Menu { prior: PaneFocus },
}

/// Which kind of modal the switcher currently has open — the loop-top hand-off the
/// reconciler reads to derive `Focus`. Popups and the menu are mutually exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Popup,
    Menu,
}

pub struct App {
    pub state: Focus,
}

impl App {
    /// Starts in `Tree` (the cursor preselected on the most-recent session).
    pub fn new() -> Self {
        App { state: Focus::Tree }
    }

    /// True only in the bare `Tree` pane state — false during any modal, even one
    /// opened from the tree. Gates set-pane / mux-active decisions; key-routing
    /// sites add `|| is_modal()` so a modal still receives keys.
    pub fn is_tree_focused(&self) -> bool {
        matches!(self.state, Focus::Tree)
    }

    /// True only in the bare `Terminal` pane state — false during any modal.
    pub fn is_terminal_focused(&self) -> bool {
        matches!(self.state, Focus::Terminal)
    }

    /// True while a modal (popup or menu) is the focus state.
    pub fn is_modal(&self) -> bool {
        matches!(self.state, Focus::Popup { .. } | Focus::Menu { .. })
    }

    /// The EFFECTIVE pane focus: the active pane, or — during a modal — the pane it
    /// was opened from. Lets `status` report the pane behind a modal as the focus.
    pub fn pane_is_tree(&self) -> bool {
        matches!(
            self.state,
            Focus::Tree
                | Focus::Popup {
                    prior: PaneFocus::Tree
                }
                | Focus::Menu {
                    prior: PaneFocus::Tree
                }
        )
    }

    /// Flips the PANE dimension (Tree ⇄ Terminal) — the `prefix Tab` toggle. During a
    /// modal it flips the carried `prior` so the modal stays open and restores onto
    /// the flipped pane.
    pub fn toggle(&mut self) {
        let flip = |p: PaneFocus| match p {
            PaneFocus::Tree => PaneFocus::Terminal,
            PaneFocus::Terminal => PaneFocus::Tree,
        };
        self.state = match self.state {
            Focus::Tree => Focus::Terminal,
            Focus::Terminal => Focus::Tree,
            Focus::Popup { prior } => Focus::Popup { prior: flip(prior) },
            Focus::Menu { prior } => Focus::Menu { prior: flip(prior) },
        };
    }

    /// Sets the PANE dimension to `p`. Not modal → becomes that pane. Modal → sets the
    /// carried `prior`, so a focus request during/closing a modal lands on `p` after
    /// restore (the context-menu "focus mux" path).
    pub fn set_pane_focus(&mut self, p: PaneFocus) {
        self.state = match self.state {
            Focus::Tree | Focus::Terminal => match p {
                PaneFocus::Tree => Focus::Tree,
                PaneFocus::Terminal => Focus::Terminal,
            },
            Focus::Popup { .. } => Focus::Popup { prior: p },
            Focus::Menu { .. } => Focus::Menu { prior: p },
        };
    }

    /// The loop-top reconciler: derives the modal dimension of `Focus` from the
    /// switcher's authoritative open-modal `kind`. Opening a modal captures the
    /// current pane as `prior`; closing restores it; a kind-switch keeps `prior`; a
    /// re-sync of the already-open kind is a no-op (it must not re-capture over a
    /// mid-modal `toggle`).
    pub fn sync_modal(&mut self, kind: Option<ModalKind>) {
        let current_pane = || {
            if self.pane_is_tree() {
                PaneFocus::Tree
            } else {
                PaneFocus::Terminal
            }
        };
        self.state = match (kind, self.state) {
            // No modal: collapse any open modal back onto its prior pane.
            (None, Focus::Popup { prior }) | (None, Focus::Menu { prior }) => match prior {
                PaneFocus::Tree => Focus::Tree,
                PaneFocus::Terminal => Focus::Terminal,
            },
            (None, s @ (Focus::Tree | Focus::Terminal)) => s,
            // Already the requested kind: no-op (preserve a mid-modal toggle of prior).
            (Some(ModalKind::Popup), s @ Focus::Popup { .. }) => s,
            (Some(ModalKind::Menu), s @ Focus::Menu { .. }) => s,
            // Kind switch between modals: keep prior, swap the variant.
            (Some(ModalKind::Popup), Focus::Menu { prior }) => Focus::Popup { prior },
            (Some(ModalKind::Menu), Focus::Popup { prior }) => Focus::Menu { prior },
            // Opening from a pane: capture the current pane as prior.
            (Some(ModalKind::Popup), Focus::Tree | Focus::Terminal) => Focus::Popup {
                prior: current_pane(),
            },
            (Some(ModalKind::Menu), Focus::Tree | Focus::Terminal) => Focus::Menu {
                prior: current_pane(),
            },
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
    fn app_starts_tree_focused_and_toggles() {
        let mut app = App::new();
        assert!(
            app.is_tree_focused(),
            "starts on the tree (cursor preselected)"
        );
        app.toggle();
        assert_eq!(app.state, Focus::Terminal);
        app.toggle();
        assert_eq!(app.state, Focus::Tree);
    }

    #[test]
    fn popup_carries_and_restores_prior_from_terminal() {
        let mut app = App {
            state: Focus::Terminal,
        };
        app.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            app.state,
            Focus::Popup {
                prior: PaneFocus::Terminal
            }
        );
        app.sync_modal(None);
        assert_eq!(
            app.state,
            Focus::Terminal,
            "restored to the pane it opened from"
        );
    }

    #[test]
    fn popup_carries_and_restores_prior_from_tree() {
        let mut app = App::new(); // Tree
        app.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            app.state,
            Focus::Popup {
                prior: PaneFocus::Tree
            }
        );
        app.sync_modal(None);
        assert_eq!(app.state, Focus::Tree);
    }

    #[test]
    fn menu_carries_and_restores_prior_from_tree() {
        let mut app = App::new(); // Tree
        app.sync_modal(Some(ModalKind::Menu));
        assert_eq!(
            app.state,
            Focus::Menu {
                prior: PaneFocus::Tree
            }
        );
        app.sync_modal(None);
        assert_eq!(app.state, Focus::Tree);
    }

    #[test]
    fn toggle_during_a_modal_flips_prior_and_keeps_the_modal() {
        let mut app = App::new(); // Tree
        app.sync_modal(Some(ModalKind::Popup));
        app.toggle();
        assert_eq!(
            app.state,
            Focus::Popup {
                prior: PaneFocus::Terminal
            },
            "toggle flips the carried prior, the modal stays open",
        );
        app.sync_modal(None);
        assert_eq!(app.state, Focus::Terminal, "restored to the flipped pane");
    }

    #[test]
    fn sync_modal_is_idempotent_while_held_and_does_not_recapture() {
        let mut app = App::new(); // Tree
        app.sync_modal(Some(ModalKind::Popup));
        app.toggle(); // prior -> Terminal
        app.sync_modal(Some(ModalKind::Popup)); // same kind, still held
        assert_eq!(
            app.state,
            Focus::Popup {
                prior: PaneFocus::Terminal
            },
            "re-sync of the same kind must not re-capture prior over a mid-modal toggle",
        );
    }

    #[test]
    fn kind_switch_keeps_prior() {
        let mut app = App {
            state: Focus::Menu {
                prior: PaneFocus::Terminal,
            },
        };
        app.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            app.state,
            Focus::Popup {
                prior: PaneFocus::Terminal
            },
            "switching menu->popup keeps prior, does not re-capture",
        );
    }

    #[test]
    fn set_pane_focus_during_a_menu_targets_the_restore_pane() {
        // The menu "focus mux" path: state is Menu{prior:Tree}, focus-mux requested.
        let mut app = App {
            state: Focus::Menu {
                prior: PaneFocus::Tree,
            },
        };
        app.set_pane_focus(PaneFocus::Terminal);
        assert_eq!(
            app.state,
            Focus::Menu {
                prior: PaneFocus::Terminal
            }
        );
        app.sync_modal(None);
        assert_eq!(app.state, Focus::Terminal, "menu closed onto the mux");
    }

    #[test]
    fn set_pane_focus_when_not_modal_sets_the_state() {
        let mut app = App::new(); // Tree
        app.set_pane_focus(PaneFocus::Terminal);
        assert_eq!(app.state, Focus::Terminal);
        app.set_pane_focus(PaneFocus::Tree);
        assert_eq!(app.state, Focus::Tree);
    }

    #[test]
    fn focus_predicates_are_mutually_exclusive_and_exhaustive() {
        for state in [
            Focus::Tree,
            Focus::Terminal,
            Focus::Popup {
                prior: PaneFocus::Tree,
            },
            Focus::Popup {
                prior: PaneFocus::Terminal,
            },
            Focus::Menu {
                prior: PaneFocus::Tree,
            },
            Focus::Menu {
                prior: PaneFocus::Terminal,
            },
        ] {
            let app = App { state };
            let n = [
                app.is_tree_focused(),
                app.is_terminal_focused(),
                app.is_modal(),
            ]
            .into_iter()
            .filter(|&b| b)
            .count();
            assert_eq!(
                n, 1,
                "exactly one of tree/terminal/modal holds for {state:?}"
            );
        }
        assert!(App { state: Focus::Tree }.is_tree_focused());
        assert!(App {
            state: Focus::Terminal
        }
        .is_terminal_focused());
        assert!(App {
            state: Focus::Popup {
                prior: PaneFocus::Tree
            }
        }
        .is_modal());
        assert!(App {
            state: Focus::Menu {
                prior: PaneFocus::Terminal
            }
        }
        .is_modal());
    }

    #[test]
    fn pane_is_tree_reports_the_effective_pane() {
        assert!(App { state: Focus::Tree }.pane_is_tree());
        assert!(App {
            state: Focus::Popup {
                prior: PaneFocus::Tree
            }
        }
        .pane_is_tree());
        assert!(App {
            state: Focus::Menu {
                prior: PaneFocus::Tree
            }
        }
        .pane_is_tree());
        assert!(!App {
            state: Focus::Terminal
        }
        .pane_is_tree());
        assert!(!App {
            state: Focus::Popup {
                prior: PaneFocus::Terminal
            }
        }
        .pane_is_tree());
        assert!(!App {
            state: Focus::Menu {
                prior: PaneFocus::Terminal
            }
        }
        .pane_is_tree());
    }
}
