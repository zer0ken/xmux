//! The PURE, stateless input-routing core: the decode/resolve functions and the small
//! value types they use. Nav-focus key resolution ([`resolve_nav_key`]), the mouse
//! focus×position router ([`resolve_mouse_chain`]/[`ChainAction`]), the gesture/geometry
//! predicates ([`to_grid_local`], [`leading_ctrl_arrow`], [`view_border_drag_width`]),
//! and the per-read gesture/outcome carriers
//! ([`MouseState`]/[`StdinOutcome`]). None of these touch app or switcher state, so they
//! are unit-testable in isolation; the stateful handlers in `runtime.rs` thread the
//! runtime's world and call into this core.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::app::runtime::{NAV_HEIGHT_MAX, NAV_HEIGHT_MIN, NAV_WIDTH_MAX};
use crate::display::dispatch::Action;

/// The nav width a view border drag to 1-based screen column `col` sets, clamped to the
/// allowed range. With the nav on the left the dragged column becomes the border position
/// (= the nav width); with the nav on the right the mirror applies and the size is the
/// window minus the dragged column.
pub(crate) fn view_border_drag_width(
    col: u16,
    ui_prefix: &str,
    window_cols: u16,
    nav_on_right: bool,
) -> u16 {
    let w = if nav_on_right {
        window_cols.saturating_sub(col)
    } else {
        col.saturating_sub(1)
    };
    w.clamp(crate::app::runtime::nav_width_min(ui_prefix), NAV_WIDTH_MAX)
}

/// The band-layout nav height a horizontal view border drag to 1-based screen row `row`
/// sets, clamped to the allowed range. With the nav on top the dragged row becomes the
/// border position (0-based), which is the nav height; with the nav on the bottom the
/// mirror applies and the size is the window minus the dragged row. compute_regions
/// clamps further to the live body height.
pub(crate) fn view_border_drag_height(row: u16, window_rows: u16, nav_on_bottom: bool) -> u16 {
    let h = if nav_on_bottom {
        window_rows.saturating_sub(row)
    } else {
        row.saturating_sub(1)
    };
    h.clamp(NAV_HEIGHT_MIN, NAV_HEIGHT_MAX)
}

/// If `bytes` STARTS with a Ctrl-arrow (`ESC [ 1 ; 5 A/B/C/D`), returns `(horizontal,
/// delta, len)`: the axis (true = ←/→ width, false = ↑/↓ height), the signed step (→/↓ = +1,
/// ←/↑ = -1), and the 6 bytes it consumed; else `None`. Peeling leading Ctrl-arrows (rather
/// than matching the whole read) lets a coalesced autorepeat burst - several presses in one
/// stdin read - keep resizing. Restricted to Ctrl-arrows (not bare arrows or h/l) so it never
/// hijacks navigation or typed pane input outside the repeat window.
pub(crate) fn leading_ctrl_arrow(bytes: &[u8]) -> Option<(bool, i32, usize)> {
    if bytes.len() >= 6 && bytes[0] == 0x1b && bytes[1] == b'[' && &bytes[2..5] == b"1;5" {
        match bytes[5] {
            b'C' => return Some((true, 1, 6)),   // Ctrl+→ : width +
            b'D' => return Some((true, -1, 6)),  // Ctrl+← : width -
            b'B' => return Some((false, 1, 6)),  // Ctrl+↓ : height +
            b'A' => return Some((false, -1, 6)), // Ctrl+↑ : height -
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

/// The single key that moves focus from the nav into the terminal view.
/// (Arrows navigate the nav; the prefix-Tab path returns focus - see TermInput.)
fn is_focus_in(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter)
}

/// Whether a wheel event should drive the NAV (a scroll: the flat list has no levels).
/// Only when the nav is focused AND the pointer is over the nav: mouse input acts on
/// the view under the selection, and only when that view is focused - the same rule clicks
/// and motion already follow. A wheel over the terminal view while the nav is focused is not
/// a nav scroll.
fn wheel_targets_nav(nav_focused: bool, over_mux: bool) -> bool {
    nav_focused && !over_mux
}

/// What a mouse event resolves to once the modal/gesture gates (menu, view border drag,
/// idle-view border-hover, menu-open) have declined it - the focus×position routing core.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChainAction {
    /// Scroll the nav by one row (wheel, nav focus, over nav). `down` = scroll down.
    /// Ctrl held changes nothing: the nav is a flat list, so there is no level to
    /// change and a wheel is a wheel.
    ScrollNav(bool),
    /// Toggle focus to the terminal view (left-click the terminal view while the nav is focused).
    FocusTerminal,
    /// Select the clicked nav row (left-click a nav row while the nav is focused).
    SelectRow,
    /// Toggle focus to the nav (left-click the nav while the terminal view is focused).
    FocusNav,
    /// Forward the event to the focused mux child (terminal focus, over the terminal view).
    ForwardToMux,
    /// Nothing - the event is dropped.
    Nothing,
}

/// Pure focus×position routing for a mouse event that fell through every gate. The one
/// rule: input acts on the view under the selection, and only when that view is focused.
/// A wheel over the terminal view while the nav is focused, or over the nav while the terminal view is
/// focused, resolves to Nothing - it never crosses to the unfocused view.
pub(crate) fn resolve_mouse_chain(
    is_wheel: bool,
    down: bool,
    is_left_press: bool,
    nav_focused: bool,
    over_mux: bool,
) -> ChainAction {
    if is_wheel && wheel_targets_nav(nav_focused, over_mux) {
        return ChainAction::ScrollNav(down);
    }
    if is_left_press && nav_focused && over_mux {
        return ChainAction::FocusTerminal;
    }
    if is_left_press && nav_focused && !over_mux {
        return ChainAction::SelectRow;
    }
    if is_left_press && !nav_focused && !over_mux {
        return ChainAction::FocusNav;
    }
    if !nav_focused && over_mux {
        return ChainAction::ForwardToMux;
    }
    ChainAction::Nothing
}

/// Pure resolution of ONE NAV-focus key into an [`Action`] (or none, when the key
/// only arms the prefix or is an unrecognized armed command). Touches no app or
/// switcher state, so it is unit-testable in isolation (mirrors how `TermInput::feed`
/// resolves the terminal-view focus path). `is_inputting` suppresses prefix arming and the Enter
/// focus-switch so the input row receives those keys verbatim. Resolved per key - not
/// per read - because `is_inputting` can flip mid-read (a key that opens the input row
/// changes how the next key in the same read is treated), so the caller re-queries it
/// and applies each action before resolving the next key.
pub(crate) fn resolve_nav_key(
    key: ratatui::crossterm::event::KeyEvent,
    armed: &mut bool,
    prefix: u8,
    is_inputting: bool,
) -> Option<Action> {
    // The prefix key is the configured control byte. A terminal reports no key-up, so
    // this is the only form it ever arrives in.
    let is_prefix_key = key.code == KeyCode::Char(prefix as char);
    // A prefix arms ready. A second one while already armed is the doubled-prefix
    // literal, which only the terminal view can act on (it forwards the byte to the
    // pane); in nav focus there is no pane, so it simply stays armed.
    if is_prefix_key && !is_inputting {
        *armed = true;
        return None;
    }
    if *armed {
        // Any key while ready CONSUMES the prefix (even a no-op like focusing the
        // already-focused view): ready clears, the bar hides.
        *armed = false;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        return match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Left if ctrl => Some(Action::Width(-1)),
            KeyCode::Right if ctrl => Some(Action::Width(1)),
            KeyCode::Char('h') => Some(Action::Width(-1)),
            KeyCode::Char('l') => Some(Action::Width(1)),
            // prefix Ctrl+↑/↓ resize the nav HEIGHT (the vertical axis, Top layout); ↓ grows.
            KeyCode::Up if ctrl => Some(Action::Height(-1)),
            KeyCode::Down if ctrl => Some(Action::Height(1)),
            KeyCode::Char('t') => Some(Action::ToggleAutoHide),
            KeyCode::Char('?') => Some(Action::ShowHelp),
            // An arrow points AT the view it focuses, in either layout: the terminal is
            // right of the nav in Side and below it in Top, so prefix → and prefix ↓ both
            // focus the terminal, and prefix ← / prefix ↑ both name the nav, which already
            // has focus here, so they resolve to nothing. prefix Tab
            // cycles, mirroring the terminal side's prefix Tab → nav. The byte decoder
            // yields Char('\t') for Tab, never KeyCode::Tab, so match both.
            KeyCode::Right | KeyCode::Down | KeyCode::Tab | KeyCode::Char('\t') => {
                Some(Action::FocusTerminal)
            }
            // Tier A: the state-changing nav actions are prefix-gated. The prefix arms
            // them; they then resolve to the nav executor via the existing NavKey path.
            // A digit joins them: `prefix <digit>` opens the card-jump popup seeded with
            // it, so a bare digit stays free for the pane and cannot jump by accident.
            KeyCode::Char('r') | KeyCode::Char('n') => Some(Action::NavKey(key)),
            KeyCode::Char(c) if c.is_ascii_digit() => Some(Action::NavKey(key)),
            // An unrecognized key simply consumes the prefix like any other: ready is
            // already cleared above.
            _ => None,
        };
    }
    // Enter focuses the terminal view. ←/→ navigate the nav inside `handle_key`.
    if !is_inputting && is_focus_in(key.code) {
        return Some(Action::FocusTerminal);
    }
    // Tier A: bare (unprefixed) r/n and bare digits are inert - they require the prefix.
    // Navigation, Enter, and `/` filter stay bare. Only applies when not inputting, so
    // every key is still literal text while an input row (filter / new / jump) is open.
    if !is_inputting
        && (matches!(key.code, KeyCode::Char('r') | KeyCode::Char('n'))
            || matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit()))
    {
        return None;
    }
    Some(Action::NavKey(key))
}

/// The per-event mouse-gesture/input state the `stdin_rx` arm carries across reads,
/// bundled so the extracted handlers stay behavior-preserving (the gesture latches
/// must persist across reads). Field-for-field the loop locals `run_app` held.
#[derive(Default)]
pub(crate) struct MouseState {
    /// True while the left button is dragging the nav/terminal view border rule to resize.
    pub(crate) dragging_view_border: bool,
    /// True while the mouse hovers the view border rule (no button) - the drag-resize cue.
    pub(crate) hovered_view_border: bool,
    /// The resize-repeat window: a bare Ctrl+←/→ keeps resizing until it lapses.
    pub(crate) repeat_until: Option<std::time::Instant>,
    /// True while a prefix has been pressed in nav focus, awaiting the command key.
    pub(crate) nav_armed: bool,
}

/// The outcome of one stdin read: what the loop must act on after the handler runs.
/// The stdin handler is a function of (bytes, state) → outcome - it mutates no loop
/// local directly, so it is unit-testable without the loop. `focus_*` and `nav_replay`
/// carry the resolved focus path (applied inside the handler) for the per-handler
/// round-trip test + observability.
#[derive(Default)]
pub(crate) struct StdinOutcome {
    pub(crate) quit: bool,
    pub(crate) focus_terminal: bool,
    pub(crate) focus_nav: bool,
    pub(crate) dirty: bool,
    pub(crate) nav_replay: Vec<u8>,
    /// True if any `apply_width_delta` call changed the natural nav width; the loop
    /// uses this to schedule the debounced persist (instead of writing per tick).
    pub(crate) width_changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_nav_key: pure NAV-focus key resolution -------------------
    /// Resolve one read at the default prefix (C-g = 0x07), fresh decoder/armed,
    /// folding the per-key resolver over the decoded keys.
    fn rt(bytes: &[u8], is_inputting: bool) -> Vec<Action> {
        let mut dec = crate::display::decode::KeyDecoder::new();
        let mut armed = false;
        dec.feed(bytes)
            .into_iter()
            .filter_map(|k| resolve_nav_key(k, &mut armed, 0x07, is_inputting))
            .collect()
    }

    #[test]
    fn resolve_nav_prefix_commands() {
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
        // arrives as Char('\t') from the byte decoder, not KeyCode::Tab - both map to
        // FocusTerminal so prefix Tab toggles nav⇄terminal like it does from the terminal side.)
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
        // An arrow names the view it focuses, whichever way the two are stacked: the
        // terminal is right of the nav in Side and below it in Top, so ↓ focuses it too.
        assert_eq!(
            rt(b"\x07\x1b[B", false),
            vec![Action::FocusTerminal],
            "prefix Down focuses mux, like prefix Right"
        );
        // ← and ↑ both name the nav, which already has focus here: nothing to do.
        for k in [b"\x07\x1b[D" as &[u8], b"\x07\x1b[A"] {
            assert_eq!(
                rt(k, false),
                Vec::<Action>::new(),
                "prefix Left / prefix Up focus the nav, where we already are"
            );
        }
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
        // prefix Ctrl+↑/↓ resize the HEIGHT (vertical axis); the runtime applies it only in Top.
        assert_eq!(
            rt(b"\x07\x1b[1;5B", false),
            vec![Action::Height(1)],
            "prefix Ctrl-Down grows height"
        );
        assert_eq!(
            rt(b"\x07\x1b[1;5A", false),
            vec![Action::Height(-1)],
            "prefix Ctrl-Up shrinks height"
        );
    }

    #[test]
    fn resolve_nav_action_keys_require_prefix() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let tk = |c: char| Action::NavKey(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // Bare state-changing keys are inert without the prefix. Digits are in that
        // tier too: a card jump is a deliberate chord, not a stray keystroke.
        for k in [b"r" as &[u8], b"n", b"0", b"4", b"9"] {
            assert_eq!(
                rt(k, false),
                Vec::<Action>::new(),
                "a bare action key does nothing without the prefix"
            );
        }
        // The prefix arms them → they resolve to the nav executor (NavKey).
        assert_eq!(rt(b"\x07r", false), vec![tk('r')], "prefix r arms rescan");
        assert_eq!(rt(b"\x07n", false), vec![tk('n')], "prefix n arms new");
        assert_eq!(
            rt(b"\x070", false),
            vec![tk('0')],
            "prefix 0 opens the jump popup"
        );
        assert_eq!(
            rt(b"\x077", false),
            vec![tk('7')],
            "prefix 7 opens the jump popup"
        );
        // Bare navigation and `/` filter stay bare (fast-switcher identity preserved).
        assert_eq!(rt(b"/", false), vec![tk('/')], "/ filter stays bare");
        assert_eq!(rt(b"j", false), vec![tk('j')], "navigation stays bare");
        // While an input row is open the keys are literal text again.
        assert_eq!(
            rt(b"4", true),
            vec![tk('4')],
            "a digit is literal while inputting"
        );
    }

    #[test]
    fn resolve_nav_enter_focuses_mux_and_nav_is_a_nav_key() {
        assert_eq!(
            rt(b"\r", false),
            vec![Action::FocusTerminal],
            "Enter focuses the terminal view"
        );
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            rt(b"j", false),
            vec![Action::NavKey(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ))],
            "a nav key is delegated to the nav verbatim"
        );
    }

    #[test]
    fn resolve_nav_while_inputting_passes_prefix_and_enter_to_the_nav() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // While the input row is open, the prefix is NOT special (typed into the buffer)
        // and Enter does NOT focus the terminal (it submits the input) - both go to the nav.
        assert_eq!(
            rt(b"\x07", true),
            vec![Action::NavKey(KeyEvent::new(
                KeyCode::Char('\u{7}'),
                KeyModifiers::NONE
            ))],
            "prefix while inputting is a literal nav key, not an arm"
        );
        assert_eq!(
            rt(b"\r", true),
            vec![Action::NavKey(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))],
            "Enter while inputting goes to the nav, not focus-switch"
        );
    }

    // --- mouse focus/position rules ----------------------------------------
    #[test]
    fn wheel_targets_nav_only_when_nav_focused_and_over_nav() {
        assert!(
            wheel_targets_nav(true, false),
            "nav focus + over nav → drive the nav"
        );
        assert!(
            !wheel_targets_nav(true, true),
            "nav focus + over the MUX pane → NOT the nav"
        );
        assert!(
            !wheel_targets_nav(false, false),
            "terminal-view focus + over nav → not the nav"
        );
        assert!(
            !wheel_targets_nav(false, true),
            "terminal-view focus + over the terminal view → the mux child, not the nav"
        );
    }

    #[test]
    fn resolve_mouse_chain_routes_by_focus_and_position() {
        use ChainAction::*;
        // wheel: only drives the nav when nav-focused AND over the nav.
        assert_eq!(
            resolve_mouse_chain(true, true, false, true, false),
            ScrollNav(true),
            "wheel, nav focus, over nav → scroll"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, false, true, false),
            ScrollNav(false),
            "Ctrl+wheel is just a wheel: a flat list has no level to change"
        );
        assert_eq!(
            resolve_mouse_chain(true, true, false, true, true),
            Nothing,
            "wheel, nav focus, over MUX → nothing (never crosses panes)"
        );
        assert_eq!(
            resolve_mouse_chain(true, true, false, false, true),
            ForwardToMux,
            "wheel, terminal-view focus, over the terminal view → forward to child"
        );
        assert_eq!(
            resolve_mouse_chain(true, true, false, false, false),
            Nothing,
            "wheel, terminal-view focus, over nav → nothing"
        );
        // left press: focus-switch on the unfocused view, act on the focused one.
        assert_eq!(
            resolve_mouse_chain(false, false, true, true, true),
            FocusTerminal,
            "left, nav focus, over terminal → focus terminal"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, true, true, false),
            SelectRow,
            "left, nav focus, over nav → select row"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, true, false, false),
            FocusNav,
            "left, terminal-view focus, over nav → focus nav"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, true, false, true),
            ForwardToMux,
            "left, terminal-view focus, over the terminal view → forward to child"
        );
        // a non-left, non-wheel press (e.g. right-press that the menu gate declined):
        // forwards to the child only when the terminal view is focused and the pointer is over it.
        assert_eq!(
            resolve_mouse_chain(false, false, false, false, true),
            ForwardToMux,
            "right-press, terminal-view focus, over the terminal view → forward"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, false),
            Nothing,
            "right-press, nav focus, over nav → nothing"
        );
    }

    #[test]
    fn resolve_nav_arming_persists_across_reads() {
        let mut dec = crate::display::decode::KeyDecoder::new();
        let mut armed = false;
        let r1: Vec<Action> = dec
            .feed(b"\x07")
            .into_iter()
            .filter_map(|k| resolve_nav_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r1, Vec::<Action>::new());
        assert!(
            armed,
            "the prefix arms even when its command arrives in the next read"
        );
        let r2: Vec<Action> = dec
            .feed(b"q")
            .into_iter()
            .filter_map(|k| resolve_nav_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r2, vec![Action::Quit]);
        assert!(!armed, "the command consumes the armed state");
    }

    #[test]
    fn a_noop_nav_arrow_still_consumes_ready() {
        // prefix Left / prefix Up name the nav, which already has focus here: they are
        // no-ops (produce no action) but still CONSUME the prefix, so the bar hides.
        use ratatui::crossterm::event::KeyEvent;
        for code in [KeyCode::Left, KeyCode::Up] {
            let mut armed = false;
            resolve_nav_key(
                KeyEvent::new(KeyCode::Char('\x07'), KeyModifiers::NONE),
                &mut armed,
                0x07,
                false,
            );
            assert!(armed);
            assert!(
                resolve_nav_key(
                    KeyEvent::new(code, KeyModifiers::NONE),
                    &mut armed,
                    0x07,
                    false
                )
                .is_none(),
                "a no-op nav arrow produces no action"
            );
            assert!(!armed, "a no-op nav arrow still consumes ready");
        }
    }

    #[test]
    fn resolve_nav_key_uses_the_configured_prefix() {
        use ratatui::crossterm::event::KeyEvent;
        // The prefix is configurable (`[ui] prefix`), default C-g. A non-default
        // prefix (C-b = 0x02) must arm and resolve its commands like the default.
        let mut armed = false;
        let press = KeyEvent::new(KeyCode::Char('\x02'), KeyModifiers::NONE);
        assert!(resolve_nav_key(press, &mut armed, 0x02, false).is_none());
        assert!(armed, "the configured prefix arms");
        assert_eq!(
            resolve_nav_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &mut armed,
                0x02,
                false
            ),
            Some(Action::Quit),
            "the command after the configured prefix resolves"
        );
        assert!(!armed);
    }

    #[test]
    fn a_command_consumes_ready() {
        // A command key CONSUMES the prefix: ready clears, so the hint bar hides.
        // Resize continuation is the RUNTIME repeat window (bare Ctrl-arrows), not a
        // re-armed prefix, so a plain `h` after consumption is a bare nav key again.
        use ratatui::crossterm::event::KeyEvent;
        let mut armed = false;
        resolve_nav_key(
            KeyEvent::new(KeyCode::Char(''), KeyModifiers::NONE),
            &mut armed,
            0x07,
            false,
        );
        assert!(armed);
        let cmd = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(
            resolve_nav_key(cmd, &mut armed, 0x07, false),
            Some(Action::Width(-1)),
            "the key resizes"
        );
        assert!(!armed, "a key while ready consumes ready");
        assert!(
            matches!(
                resolve_nav_key(cmd, &mut armed, 0x07, false),
                Some(Action::NavKey(_))
            ),
            "after consumption a plain key is bare again, not a prefix command"
        );
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
            Some((true, 1, 6)),
            "Ctrl-Right widens (horizontal +)"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5D"),
            Some((true, -1, 6)),
            "Ctrl-Left narrows (horizontal -)"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5B"),
            Some((false, 1, 6)),
            "Ctrl-Down grows height (vertical +)"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5A"),
            Some((false, -1, 6)),
            "Ctrl-Up shrinks height (vertical -)"
        );
        // A LEADING Ctrl-arrow is peeled even with trailing bytes (the caller loops /
        // routes the remainder) - this is what makes a coalesced autorepeat keep going.
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5C\x1b[1;5C"),
            Some((true, 1, 6)),
            "peels the first of a burst"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5Cx"),
            Some((true, 1, 6)),
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
        // The dragged 1-based column becomes the 0-based nav width, clamped to range.
        // The floor is the resting prefix "C-g" (3 cells) plus a one-cell gap each side.
        assert_eq!(view_border_drag_width(51, "C-g", 140, false), 50);
        assert_eq!(
            view_border_drag_width(5, "C-g", 140, false),
            crate::app::runtime::nav_width_min("C-g"),
            "too far left clamps to the prefix floor"
        );
        assert_eq!(
            view_border_drag_width(500, "C-g", 140, false),
            NAV_WIDTH_MAX,
            "too far right clamps to max"
        );
        assert_eq!(
            view_border_drag_width(5, "C-Space", 140, false),
            crate::app::runtime::nav_width_min("C-Space"),
            "a wider prefix raises the floor"
        );
    }

    #[test]
    fn view_border_drag_mirrors_to_the_right_and_bottom() {
        // On the right/bottom the drag measures from the FAR edge: the dragged 1-based
        // column/row is where the border lands, so the size is the window minus it.
        // Dragging the right border (0-based col 91 at a 48 width) to SGR 100 gives 40.
        assert_eq!(view_border_drag_width(91, "C-g", 140, true), 49);
        assert_eq!(view_border_drag_width(100, "C-g", 140, true), 40);
        assert_eq!(
            view_border_drag_width(135, "C-g", 140, true),
            crate::app::runtime::nav_width_min("C-g"),
            "dragging the right border leftward clamps to the prefix floor"
        );
        // Same mirror on the height: dragging the bottom border (0-based row 35 at the
        // auto 24) to SGR 30 in a 60-row window gives 30; near the terminal it floors.
        assert_eq!(view_border_drag_height(30, 60, true), 30);
        assert_eq!(
            view_border_drag_height(58, 60, true),
            NAV_HEIGHT_MIN,
            "dragging the bottom border upward clamps to the height floor"
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
    fn to_grid_local_in_nav_column_returns_none() {
        // Terminal area starts at screen col 50 (0-based). SGR col 10 is in the nav.
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
        // Nav hidden (auto-hide-nav): the terminal view owns the whole screen, so the
        // input handler builds term_area at x=0. The top-left cell SGR (1,1) must map
        // to grid-local (1,1) rather than being rejected as it would in the nav column.
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(to_grid_local(area, 1, 1), Some((1, 1)));
        assert_eq!(to_grid_local(area, 80, 24), Some((80, 24)));
    }
}
