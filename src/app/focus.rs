//! The app's focus state machine. Every state draws the SAME split (tree on
//! the left, the cursor session's live grid on the right); focus only chooses
//! where keys go and which divider rule is highlighted. There are four states
//! along two dimensions: the VIEW dimension (`Tree` ⇄ `Terminal`, toggled by
//! `prefix Tab`) and a MODAL dimension layered on top (`Popup` for help / inline
//! input / kill-confirm, `Menu` for the right-click context menu). A modal is a
//! first-class focus state that CARRIES the view it was opened from, so closing it
//! restores that view structurally — no external "saved focus" variable. "Is a
//! modal open?" is therefore a `match` on `Focus`, and the modal/view state cannot
//! desync from the switcher because the loop derives it each pass via `sync_modal`.

/// The two real views — the only targets a modal can restore to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewFocus {
    Tree,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Tree view focused — keys navigate the host/session tree.
    #[default]
    Tree,
    /// Terminal view focused — keys forward to the selected session's active pane.
    Terminal,
    /// A centered modal popup (help / inline input / kill-confirm) owns keys;
    /// `prior` is the view to restore when it closes.
    Popup { prior: ViewFocus },
    /// The right-click context menu owns input; `prior` is the view to restore.
    Menu { prior: ViewFocus },
}

/// Which kind of modal the switcher currently has open — the loop-top hand-off the
/// reconciler reads to derive `Focus`. Popups and the menu are mutually exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Popup,
    Menu,
}

impl Focus {
    /// True only in the bare `Tree` view state — false during any modal, even one
    /// opened from the tree. Gates set-view / mux-active decisions; key-routing
    /// sites add `|| is_modal()` so a modal still receives keys.
    pub fn is_tree_focused(&self) -> bool {
        matches!(self, Focus::Tree)
    }

    /// True only in the bare `Terminal` view state — false during any modal.
    pub fn is_terminal_focused(&self) -> bool {
        matches!(self, Focus::Terminal)
    }

    /// True while a modal (popup or menu) is the focus state.
    pub fn is_modal(&self) -> bool {
        matches!(self, Focus::Popup { .. } | Focus::Menu { .. })
    }

    /// The EFFECTIVE view focus: the active view, or — during a modal — the view it
    /// was opened from. Lets `status` report the view behind a modal as the focus.
    pub fn view_is_tree(&self) -> bool {
        matches!(
            self,
            Focus::Tree
                | Focus::Popup {
                    prior: ViewFocus::Tree
                }
                | Focus::Menu {
                    prior: ViewFocus::Tree
                }
        )
    }

    /// Flips the VIEW dimension (Tree ⇄ Terminal) — the `prefix Tab` toggle. During a
    /// modal it flips the carried `prior` so the modal stays open and restores onto
    /// the flipped view.
    pub fn toggle(&mut self) {
        let flip = |p: ViewFocus| match p {
            ViewFocus::Tree => ViewFocus::Terminal,
            ViewFocus::Terminal => ViewFocus::Tree,
        };
        *self = match *self {
            Focus::Tree => Focus::Terminal,
            Focus::Terminal => Focus::Tree,
            Focus::Popup { prior } => Focus::Popup { prior: flip(prior) },
            Focus::Menu { prior } => Focus::Menu { prior: flip(prior) },
        };
    }

    /// Sets the VIEW dimension to `p`. Not modal → becomes that view. Modal → sets the
    /// carried `prior`, so a focus request during/closing a modal lands on `p` after
    /// restore (the context-menu "focus terminal" path).
    pub fn set_view_focus(&mut self, p: ViewFocus) {
        *self = match *self {
            Focus::Tree | Focus::Terminal => match p {
                ViewFocus::Tree => Focus::Tree,
                ViewFocus::Terminal => Focus::Terminal,
            },
            Focus::Popup { .. } => Focus::Popup { prior: p },
            Focus::Menu { .. } => Focus::Menu { prior: p },
        };
    }

    /// The loop-top reconciler: derives the modal dimension of `Focus` from the
    /// switcher's authoritative open-modal `kind`. Opening a modal captures the
    /// current view as `prior`; closing restores it; a kind-switch keeps `prior`; a
    /// re-sync of the already-open kind is a no-op (it must not re-capture over a
    /// mid-modal `toggle`).
    pub fn sync_modal(&mut self, kind: Option<ModalKind>) {
        let current_view = || {
            if self.view_is_tree() {
                ViewFocus::Tree
            } else {
                ViewFocus::Terminal
            }
        };
        *self = match (kind, *self) {
            // No modal: collapse any open modal back onto its prior view.
            (None, Focus::Popup { prior }) | (None, Focus::Menu { prior }) => match prior {
                ViewFocus::Tree => Focus::Tree,
                ViewFocus::Terminal => Focus::Terminal,
            },
            (None, s @ (Focus::Tree | Focus::Terminal)) => s,
            // Already the requested kind: no-op (preserve a mid-modal toggle of prior).
            (Some(ModalKind::Popup), s @ Focus::Popup { .. }) => s,
            (Some(ModalKind::Menu), s @ Focus::Menu { .. }) => s,
            // Kind switch between modals: keep prior, swap the variant.
            (Some(ModalKind::Popup), Focus::Menu { prior }) => Focus::Popup { prior },
            (Some(ModalKind::Menu), Focus::Popup { prior }) => Focus::Menu { prior },
            // Opening from a view: capture the current view as prior.
            (Some(ModalKind::Popup), Focus::Tree | Focus::Terminal) => Focus::Popup {
                prior: current_view(),
            },
            (Some(ModalKind::Menu), Focus::Tree | Focus::Terminal) => Focus::Menu {
                prior: current_view(),
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_starts_tree_focused_and_toggles() {
        let mut focus = Focus::default();
        assert!(
            focus.is_tree_focused(),
            "starts on the tree (cursor preselected)"
        );
        focus.toggle();
        assert_eq!(focus, Focus::Terminal);
        focus.toggle();
        assert_eq!(focus, Focus::Tree);
    }

    #[test]
    fn popup_carries_and_restores_prior_from_terminal() {
        let mut focus = Focus::Terminal;
        focus.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            }
        );
        focus.sync_modal(None);
        assert_eq!(
            focus,
            Focus::Terminal,
            "restored to the view it opened from"
        );
    }

    #[test]
    fn popup_carries_and_restores_prior_from_tree() {
        let mut focus = Focus::default(); // Tree
        focus.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            focus,
            Focus::Popup {
                prior: ViewFocus::Tree
            }
        );
        focus.sync_modal(None);
        assert_eq!(focus, Focus::Tree);
    }

    #[test]
    fn menu_carries_and_restores_prior_from_tree() {
        let mut focus = Focus::default(); // Tree
        focus.sync_modal(Some(ModalKind::Menu));
        assert_eq!(
            focus,
            Focus::Menu {
                prior: ViewFocus::Tree
            }
        );
        focus.sync_modal(None);
        assert_eq!(focus, Focus::Tree);
    }

    #[test]
    fn toggle_during_a_modal_flips_prior_and_keeps_the_modal() {
        let mut focus = Focus::default(); // Tree
        focus.sync_modal(Some(ModalKind::Popup));
        focus.toggle();
        assert_eq!(
            focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            },
            "toggle flips the carried prior, the modal stays open",
        );
        focus.sync_modal(None);
        assert_eq!(focus, Focus::Terminal, "restored to the flipped view");
    }

    #[test]
    fn sync_modal_is_idempotent_while_held_and_does_not_recapture() {
        let mut focus = Focus::default(); // Tree
        focus.sync_modal(Some(ModalKind::Popup));
        focus.toggle(); // prior -> Terminal
        focus.sync_modal(Some(ModalKind::Popup)); // same kind, still held
        assert_eq!(
            focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            },
            "re-sync of the same kind must not re-capture prior over a mid-modal toggle",
        );
    }

    #[test]
    fn kind_switch_keeps_prior() {
        let mut focus = Focus::Menu {
            prior: ViewFocus::Terminal,
        };
        focus.sync_modal(Some(ModalKind::Popup));
        assert_eq!(
            focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            },
            "switching menu->popup keeps prior, does not re-capture",
        );
    }

    #[test]
    fn set_view_focus_during_a_menu_targets_the_restore_view() {
        // The menu "focus terminal" path: state is Menu{prior:Tree}, focus-terminal requested.
        let mut focus = Focus::Menu {
            prior: ViewFocus::Tree,
        };
        focus.set_view_focus(ViewFocus::Terminal);
        assert_eq!(
            focus,
            Focus::Menu {
                prior: ViewFocus::Terminal
            }
        );
        focus.sync_modal(None);
        assert_eq!(focus, Focus::Terminal, "menu closed onto the mux");
    }

    #[test]
    fn set_view_focus_when_not_modal_sets_the_state() {
        let mut focus = Focus::default(); // Tree
        focus.set_view_focus(ViewFocus::Terminal);
        assert_eq!(focus, Focus::Terminal);
        focus.set_view_focus(ViewFocus::Tree);
        assert_eq!(focus, Focus::Tree);
    }

    #[test]
    fn focus_predicates_are_mutually_exclusive_and_exhaustive() {
        for state in [
            Focus::Tree,
            Focus::Terminal,
            Focus::Popup {
                prior: ViewFocus::Tree,
            },
            Focus::Popup {
                prior: ViewFocus::Terminal,
            },
            Focus::Menu {
                prior: ViewFocus::Tree,
            },
            Focus::Menu {
                prior: ViewFocus::Terminal,
            },
        ] {
            let n = [
                state.is_tree_focused(),
                state.is_terminal_focused(),
                state.is_modal(),
            ]
            .into_iter()
            .filter(|&b| b)
            .count();
            assert_eq!(
                n, 1,
                "exactly one of tree/terminal/modal holds for {state:?}"
            );
        }
        assert!(Focus::Tree.is_tree_focused());
        assert!(Focus::Terminal.is_terminal_focused());
        assert!(Focus::Popup {
            prior: ViewFocus::Tree
        }
        .is_modal());
        assert!(Focus::Menu {
            prior: ViewFocus::Terminal
        }
        .is_modal());
    }

    #[test]
    fn view_is_tree_reports_the_effective_view() {
        assert!(Focus::Tree.view_is_tree());
        assert!(Focus::Popup {
            prior: ViewFocus::Tree
        }
        .view_is_tree());
        assert!(Focus::Menu {
            prior: ViewFocus::Tree
        }
        .view_is_tree());
        assert!(!Focus::Terminal.view_is_tree());
        assert!(!Focus::Popup {
            prior: ViewFocus::Terminal
        }
        .view_is_tree());
        assert!(!Focus::Menu {
            prior: ViewFocus::Terminal
        }
        .view_is_tree());
    }
}
