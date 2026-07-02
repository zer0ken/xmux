//! The PURE, stateless input-routing core: the decode/resolve functions and the small
//! value types they use. Tree-focus key resolution ([`resolve_tree_key`]), the mouse
//! focus×position router ([`resolve_mouse_chain`]/[`ChainAction`]), the gesture/geometry
//! predicates ([`to_grid_local`], [`leading_ctrl_arrow`], [`view_border_drag_width`],
//! [`tree_menu_may_open`]), and the per-read gesture/outcome carriers
//! ([`MouseState`]/[`StdinOutcome`]). None of these touch app or switcher state, so they
//! are unit-testable in isolation; the stateful handlers in `runtime.rs` thread the
//! runtime's world and call into this core.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::app::runtime::{TREE_WIDTH_MAX, TREE_WIDTH_MIN};
use crate::display::dispatch::Action;

/// The tree width a view border drag to 1-based screen column `col` sets: the dragged
/// column becomes the view border position (= the tree width), clamped to the allowed range.
pub(crate) fn view_border_drag_width(col: u16) -> u16 {
    col.saturating_sub(1).clamp(TREE_WIDTH_MIN, TREE_WIDTH_MAX)
}

/// If `bytes` STARTS with a Ctrl+←/→ (`ESC [ 1 ; 5 D/C`), the resize delta and the
/// 6-byte length it consumed; else `None`. Peeling leading Ctrl-arrows (rather than
/// matching the whole read) lets a coalesced autorepeat burst — several presses
/// delivered in one stdin read — keep resizing instead of ending the repeat window.
/// Restricted to Ctrl-arrows (not bare arrows or h/l) so it never hijacks navigation
/// or typed pane input outside the window.
pub(crate) fn leading_ctrl_arrow(bytes: &[u8]) -> Option<(i32, usize)> {
    if bytes.len() >= 6 && bytes[0] == 0x1b && bytes[1] == b'[' && &bytes[2..5] == b"1;5" {
        match bytes[5] {
            b'C' => return Some((1, 6)),
            b'D' => return Some((-1, 6)),
            _ => {}
        }
    }
    None
}

/// Maps a 1-based SGR mouse cell to 1-based grid-local coords if it falls inside
/// `area` (a 0-based screen Rect), else None. SGR uses 1-based coordinates; ratatui
/// Rects use 0-based screen positions. The result is 1-based so it can be directly
/// re-encoded in a new SGR sequence forwarded to the mux.
pub(crate) fn to_grid_local(area: ratatui::layout::Rect, col: u16, row: u16) -> Option<(u16, u16)> {
    let c0 = col.checked_sub(1)?; // SGR 1-based → 0-based screen cell
    let r0 = row.checked_sub(1)?;
    if c0 >= area.x && c0 < area.x + area.width && r0 >= area.y && r0 < area.y + area.height {
        Some((c0 - area.x + 1, r0 - area.y + 1)) // back to 1-based, grid-local
    } else {
        None
    }
}

/// The single key that moves focus from the tree into the terminal view.
/// (Arrows navigate the tree; the prefix-Esc path returns focus — see TermInput.)
fn is_focus_in(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter)
}

/// Whether a wheel event should drive the TREE (scroll, or Ctrl-wheel level change).
/// Only when the tree is focused AND the pointer is over the tree: mouse input acts on
/// the view under the selection, and only when that view is focused — the same rule clicks
/// and motion already follow. A wheel over the terminal view while the tree is focused is not
/// a tree scroll.
fn wheel_targets_tree(tree_focused: bool, over_mux: bool) -> bool {
    tree_focused && !over_mux
}

/// Whether a right-button press may open the tree context menu. Tree-focus only: the
/// menu operates on a tree row, so it is a tree-view action, not a view-independent
/// global — it never opens (nor steals focus) while the terminal view is focused. Position-
/// gated to the tree column; a right-click over the terminal view forwards to the child.
pub(crate) fn tree_menu_may_open(is_right_press: bool, tree_focused: bool, over_mux: bool) -> bool {
    is_right_press && tree_focused && !over_mux
}

/// What a mouse event resolves to once the modal/gesture gates (menu, view border drag,
/// idle-view border-hover, menu-open) have declined it — the focus×position routing core.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChainAction {
    /// Scroll the tree by one row (wheel, tree focus, over tree). `down` = scroll down.
    ScrollTree(bool),
    /// Change the tree level (Ctrl+wheel, tree focus, over tree). `down` = descend.
    LevelChange(bool),
    /// Toggle focus to the terminal view (left-click the terminal view while the tree is focused).
    FocusTerminal,
    /// Select the clicked tree row (left-click a tree row while the tree is focused).
    SelectRow,
    /// Toggle focus to the tree (left-click the tree while the terminal view is focused).
    FocusTree,
    /// Forward the event to the focused mux child (terminal focus, over the terminal view).
    ForwardToMux,
    /// Nothing — the event is dropped.
    Nothing,
}

/// Pure focus×position routing for a mouse event that fell through every gate. The one
/// rule: input acts on the view under the selection, and only when that view is focused.
/// A wheel over the terminal view while the tree is focused, or over the tree while the terminal view is
/// focused, resolves to Nothing — it never crosses to the unfocused view.
pub(crate) fn resolve_mouse_chain(
    is_wheel: bool,
    ctrl: bool,
    down: bool,
    is_left_press: bool,
    tree_focused: bool,
    over_mux: bool,
) -> ChainAction {
    if is_wheel && wheel_targets_tree(tree_focused, over_mux) {
        return if ctrl {
            ChainAction::LevelChange(down)
        } else {
            ChainAction::ScrollTree(down)
        };
    }
    if is_left_press && tree_focused && over_mux {
        return ChainAction::FocusTerminal;
    }
    if is_left_press && tree_focused && !over_mux {
        return ChainAction::SelectRow;
    }
    if is_left_press && !tree_focused && !over_mux {
        return ChainAction::FocusTree;
    }
    if !tree_focused && over_mux {
        return ChainAction::ForwardToMux;
    }
    ChainAction::Nothing
}

/// Pure resolution of ONE TREE-focus key into an [`Action`] (or none, when the key
/// only arms the prefix or is an unrecognized armed command). Touches no app or
/// switcher state, so it is unit-testable in isolation (mirrors how `TermInput::feed`
/// resolves the terminal-view focus path). `is_inputting` suppresses prefix arming and the Enter
/// focus-switch so the input row receives those keys verbatim. Resolved per key — not
/// per read — because `is_inputting` can flip mid-read (a key that opens the input row
/// changes how the next key in the same read is treated), so the caller re-queries it
/// and applies each action before resolving the next key.
pub(crate) fn resolve_tree_key(
    key: ratatui::crossterm::event::KeyEvent,
    armed: &mut bool,
    prefix: u8,
    is_inputting: bool,
) -> Option<Action> {
    if *armed {
        *armed = false;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        return match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Left if ctrl => Some(Action::Width(-1)),
            KeyCode::Right if ctrl => Some(Action::Width(1)),
            KeyCode::Char('h') => Some(Action::Width(-1)),
            KeyCode::Char('l') => Some(Action::Width(1)),
            KeyCode::Char('t') => Some(Action::ToggleAutoHide),
            KeyCode::Char('?') => Some(Action::ShowHelp),
            // prefix Tab cycles focus to the terminal (toggle, mirroring the terminal side's
            // prefix Tab → tree); prefix → also focuses the terminal view. The byte decoder yields
            // Char('\t') for Tab, never KeyCode::Tab, so match both. (prefix ←/Esc focus
            // the tree, where we already are — a no-op that resolves to nothing.)
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('\t') => Some(Action::FocusTerminal),
            _ => None,
        };
    }
    if !is_inputting && key.code == KeyCode::Char(prefix as char) {
        *armed = true;
        return None;
    }
    // Enter focuses the terminal view. ←/→ navigate the tree inside `handle_key`.
    if !is_inputting && is_focus_in(key.code) {
        return Some(Action::FocusTerminal);
    }
    Some(Action::TreeKey(key))
}

/// The per-event mouse-gesture/input state the `stdin_rx` arm carries across reads,
/// bundled so the extracted handlers stay behavior-preserving (the gesture latches
/// must persist across reads). Field-for-field the loop locals `run_app` held.
#[derive(Default)]
pub(crate) struct MouseState {
    /// True while the left button is dragging the tree/terminal view border rule to resize.
    pub(crate) dragging_view_border: bool,
    /// True while the mouse hovers the view border rule (no button) — the drag-resize cue.
    pub(crate) hovered_view_border: bool,
    /// The resize-repeat window: a bare Ctrl+←/→ keeps resizing until it lapses.
    pub(crate) repeat_until: Option<std::time::Instant>,
    /// True while a prefix has been pressed in tree focus, awaiting the command key.
    pub(crate) tree_armed: bool,
}

/// The outcome of one stdin read: what the loop must act on after the handler runs.
/// The stdin handler is a function of (bytes, state) → outcome — it mutates no loop
/// local directly, so it is unit-testable without the loop. `focus_*` and `tree_replay`
/// carry the resolved focus path (applied inside the handler) for the per-handler
/// round-trip test + observability.
#[derive(Default)]
pub(crate) struct StdinOutcome {
    pub(crate) quit: bool,
    pub(crate) focus_terminal: bool,
    pub(crate) focus_tree: bool,
    pub(crate) dirty: bool,
    pub(crate) tree_replay: Vec<u8>,
    /// True if any `apply_width_delta` call changed the natural tree width; the loop
    /// uses this to schedule the debounced persist (instead of writing per tick).
    pub(crate) width_changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_tree_key: pure TREE-focus key resolution -------------------
    /// Resolve one read at the default prefix (C-g = 0x07), fresh decoder/armed,
    /// folding the per-key resolver over the decoded keys.
    fn rt(bytes: &[u8], is_inputting: bool) -> Vec<Action> {
        let mut dec = crate::display::decode::KeyDecoder::new();
        let mut armed = false;
        dec.feed(bytes)
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, is_inputting))
            .collect()
    }

    #[test]
    fn resolve_tree_prefix_commands() {
        assert_eq!(rt(b"\x07q", false), vec![Action::Quit], "prefix q quits");
        assert_eq!(
            rt(b"\x07l", false),
            vec![Action::Width(1)],
            "prefix l widens"
        );
        assert_eq!(
            rt(b"\x07h", false),
            vec![Action::Width(-1)],
            "prefix h narrows"
        );
        assert_eq!(
            rt(b"\x07t", false),
            vec![Action::ToggleAutoHide],
            "prefix t toggles hide"
        );
        assert_eq!(
            rt(b"\x07?", false),
            vec![Action::ShowHelp],
            "prefix ? toggles help"
        );
        // prefix Tab cycles focus to the terminal view, and prefix Right does too. (Tab
        // arrives as Char('\t') from the byte decoder, not KeyCode::Tab — both map to
        // FocusTerminal so prefix Tab toggles tree⇄terminal like it does from the terminal side.)
        assert_eq!(
            rt(b"\x07\t", false),
            vec![Action::FocusTerminal],
            "prefix Tab cycles focus to mux"
        );
        assert_eq!(
            rt(b"\x07\x1b[C", false),
            vec![Action::FocusTerminal],
            "prefix Right focuses mux"
        );
        assert_eq!(
            rt(b"\x07\x1b[1;5C", false),
            vec![Action::Width(1)],
            "prefix Ctrl-Right widens"
        );
        assert_eq!(
            rt(b"\x07\x1b[1;5D", false),
            vec![Action::Width(-1)],
            "prefix Ctrl-Left narrows"
        );
    }

    #[test]
    fn resolve_tree_enter_focuses_mux_and_nav_is_a_tree_key() {
        assert_eq!(
            rt(b"\r", false),
            vec![Action::FocusTerminal],
            "Enter focuses the terminal view"
        );
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            rt(b"j", false),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ))],
            "a nav key is delegated to the tree verbatim"
        );
    }

    #[test]
    fn resolve_tree_while_inputting_passes_prefix_and_enter_to_the_tree() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // While the input row is open, the prefix is NOT special (typed into the buffer)
        // and Enter does NOT focus the terminal (it submits the input) — both go to the tree.
        assert_eq!(
            rt(b"\x07", true),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Char('\u{7}'),
                KeyModifiers::NONE
            ))],
            "prefix while inputting is a literal tree key, not an arm"
        );
        assert_eq!(
            rt(b"\r", true),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))],
            "Enter while inputting goes to the tree, not focus-switch"
        );
    }

    // --- mouse focus/position rules ----------------------------------------
    #[test]
    fn wheel_targets_tree_only_when_tree_focused_and_over_tree() {
        assert!(
            wheel_targets_tree(true, false),
            "tree focus + over tree → drive the tree"
        );
        assert!(
            !wheel_targets_tree(true, true),
            "tree focus + over the MUX pane → NOT the tree"
        );
        assert!(
            !wheel_targets_tree(false, false),
            "terminal-view focus + over tree → not the tree"
        );
        assert!(
            !wheel_targets_tree(false, true),
            "terminal-view focus + over the terminal view → the mux child, not the tree"
        );
    }

    #[test]
    fn resolve_mouse_chain_routes_by_focus_and_position() {
        use ChainAction::*;
        // wheel: only drives the tree when tree-focused AND over the tree.
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, true, false),
            ScrollTree(true),
            "wheel, tree focus, over tree → scroll"
        );
        assert_eq!(
            resolve_mouse_chain(true, true, false, false, true, false),
            LevelChange(false),
            "Ctrl+wheel, tree focus, over tree → level"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, true, true),
            Nothing,
            "wheel, tree focus, over MUX → nothing (never crosses panes)"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, false, true),
            ForwardToMux,
            "wheel, terminal-view focus, over the terminal view → forward to child"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, false, false),
            Nothing,
            "wheel, terminal-view focus, over tree → nothing"
        );
        // left press: focus-switch on the unfocused view, act on the focused one.
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, true, true),
            FocusTerminal,
            "left, tree focus, over terminal → focus terminal"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, true, false),
            SelectRow,
            "left, tree focus, over tree → select row"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, false, false),
            FocusTree,
            "left, terminal-view focus, over tree → focus tree"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, false, true),
            ForwardToMux,
            "left, terminal-view focus, over the terminal view → forward to child"
        );
        // a non-left, non-wheel press (e.g. right-press that the menu gate declined):
        // forwards to the child only when the terminal view is focused and the pointer is over it.
        assert_eq!(
            resolve_mouse_chain(false, false, false, false, false, true),
            ForwardToMux,
            "right-press, terminal-view focus, over the terminal view → forward"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, false, true, false),
            Nothing,
            "right-press, tree focus, over tree → nothing"
        );
    }

    #[test]
    fn tree_menu_opens_only_in_tree_focus_over_the_tree() {
        assert!(
            tree_menu_may_open(true, true, false),
            "right-press, tree focus, over tree → may open"
        );
        assert!(
            !tree_menu_may_open(true, false, false),
            "right-press while the MUX is focused → never"
        );
        assert!(
            !tree_menu_may_open(true, true, true),
            "right-press over the terminal view → forwards, no tree menu"
        );
        assert!(
            !tree_menu_may_open(false, true, false),
            "a non-right press never opens the menu"
        );
    }

    #[test]
    fn resolve_tree_arming_persists_across_reads() {
        let mut dec = crate::display::decode::KeyDecoder::new();
        let mut armed = false;
        let r1: Vec<Action> = dec
            .feed(b"\x07")
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r1, Vec::<Action>::new());
        assert!(
            armed,
            "the prefix arms even when its command arrives in the next read"
        );
        let r2: Vec<Action> = dec
            .feed(b"q")
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r2, vec![Action::Quit]);
        assert!(!armed, "the command consumes the armed state");
    }

    #[test]
    fn enter_focuses_terminal_tab_does_not() {
        assert!(is_focus_in(KeyCode::Enter));
        assert!(!is_focus_in(KeyCode::Char('\t')));
        assert!(!is_focus_in(KeyCode::Right));
    }

    #[test]
    fn leading_ctrl_arrow_peels_one_and_ignores_others() {
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5C"),
            Some((1, 6)),
            "Ctrl-Right widens"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5D"),
            Some((-1, 6)),
            "Ctrl-Left narrows"
        );
        // A LEADING Ctrl-arrow is peeled even with trailing bytes (the caller loops /
        // routes the remainder) — this is what makes a coalesced autorepeat keep going.
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5C\x1b[1;5C"),
            Some((1, 6)),
            "peels the first of a burst"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5Cx"),
            Some((1, 6)),
            "peels past trailing input"
        );
        // Bare arrows and h/l are not repeat keys.
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[C"),
            None,
            "bare arrow is not a repeat key"
        );
        assert_eq!(leading_ctrl_arrow(b"l"), None, "h/l are not repeat keys");
        assert_eq!(leading_ctrl_arrow(b""), None, "empty is not a repeat key");
    }

    #[test]
    fn view_border_drag_width_clamps_to_range() {
        // The dragged 1-based column becomes the 0-based tree width, clamped to range.
        assert_eq!(view_border_drag_width(51), 50);
        assert_eq!(
            view_border_drag_width(5),
            TREE_WIDTH_MIN,
            "too far left clamps to min"
        );
        assert_eq!(
            view_border_drag_width(500),
            TREE_WIDTH_MAX,
            "too far right clamps to max"
        );
    }

    #[test]
    fn to_grid_local_inside_area_maps_correctly() {
        // Terminal area starts at screen col 50 (x=49 0-based), row 0, size 80×24.
        // SGR cell (52,3) = 0-based (51,2) which is inside (49..129, 0..24).
        // grid-local = (51-49+1, 2-0+1) = (3, 3) in 1-based.
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 52, 3), Some((3, 3)));
    }

    #[test]
    fn to_grid_local_in_tree_column_returns_none() {
        // Terminal area starts at screen col 50 (0-based). SGR col 10 is in the tree.
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 10, 5), None);
    }

    #[test]
    fn to_grid_local_boundary_cells() {
        // area (49,0,80,24): valid cols 49..129, valid rows 0..24 (0-based).
        // Top-left corner: SGR (50,1) → 0-based (49,0) → grid-local (1,1).
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 50, 1), Some((1, 1)));
        // Bottom-right corner: SGR (129,24) → 0-based (128,23) → grid-local (80,24).
        assert_eq!(to_grid_local(area, 129, 24), Some((80, 24)));
        // One past the right edge: 0-based col 129 >= 49+80=129 → None.
        assert_eq!(to_grid_local(area, 130, 1), None);
        // One past the bottom: 0-based row 24 >= 0+24=24 → None.
        assert_eq!(to_grid_local(area, 50, 25), None);
    }

    #[test]
    fn to_grid_local_zero_col_or_row_returns_none() {
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(
            to_grid_local(area, 0, 5),
            None,
            "col=0 triggers checked_sub None"
        );
        assert_eq!(
            to_grid_local(area, 5, 0),
            None,
            "row=0 triggers checked_sub None"
        );
    }

    #[test]
    fn to_grid_local_full_width_area_maps_left_edge() {
        // Tree hidden (auto-hide-tree): the terminal view owns the whole screen, so the
        // input handler builds term_area at x=0. The top-left cell SGR (1,1) must map
        // to grid-local (1,1) rather than being rejected as it would in the tree column.
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(to_grid_local(area, 1, 1), Some((1, 1)));
        assert_eq!(to_grid_local(area, 80, 24), Some((80, 24)));
    }
}
