//! The switcher's modal surfaces: the single open-modal enum ([`Modal`]) and its
//! variants (help / inline input / kill confirm / context menu), plus the data
//! types they carry. The switcher owns the modal *behavior* and the transient
//! popup geometry; this module owns the modal data model. Side-effect-free.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::session::Session;
use crate::ui::tree::RowRef;

/// An armed kill confirm (awaiting y/n). One slot enforces "at most one armed".
#[derive(Debug, Clone)]
pub(crate) enum PendingKill {
    Session(Session),
    /// (source, session, target="session:window")
    Window {
        source: String,
        session: String,
        target: String,
    },
}

/// One context-menu entry. The variant drives the action taken on release; the
/// label is the row text. Words match the rest of the tree UI ("focus the terminal",
/// "new", "rename", "kill" — never "open"/"split", which are not used elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuItem {
    Focus,
    NewSession,
    NewWindow,
    Rename,
    Kill,
}

impl MenuItem {
    pub(crate) fn label(self) -> &'static str {
        match self {
            MenuItem::Focus => "focus",
            MenuItem::NewSession => "new session",
            MenuItem::NewWindow => "new window",
            MenuItem::Rename => "rename",
            MenuItem::Kill => "kill",
        }
    }
}

/// What the app must do after a menu release. Most items are handled inside the
/// switcher (they open an input or arm a kill); `FocusTerminal` is the one outcome the
/// app owns (the "focus" item moves focus to the terminal view).
pub enum MenuOutcome {
    None,
    Handled,
    FocusTerminal,
}

/// An open right-click context menu. `target` is the node it acts on, re-located by
/// identity at release so a tree rebuild during the brief hold cannot misfire on a
/// stale row. `title` names that node (shown in the box's top border, like tmux's
/// menu title — so the menu reads as "actions for <this node>"). `rect` is the
/// bordered box in 0-based screen coords; `hovered` is the highlighted item.
pub(crate) struct Menu {
    pub(crate) target: RowRef,
    pub(crate) title: String,
    pub(crate) rect: Rect,
    pub(crate) items: Vec<MenuItem>,
    pub(crate) hovered: Option<usize>,
}

/// An active border-drag of a modal popup: the grabbed screen cell and the
/// popup offset at grab time, so motion can compute the new offset.
#[derive(Clone, Copy)]
struct PopupDrag {
    grab: (u16, u16),
    origin: (i16, i16),
}

/// The transient geometry of the active modal popup, owned by the switcher: the
/// drag `offset` from the centered position, the `rect` it was last drawn at (for
/// border hit-testing), and the in-flight border `drag`. The drag behavior is
/// self-contained here so the switcher only forwards mouse events.
#[derive(Default)]
pub(crate) struct PopupGeometry {
    /// Drag offset (cells) applied to a modal popup's centered position. Reset
    /// to (0,0) when a popup opens; updated while its border is dragged.
    pub(crate) offset: (i16, i16),
    /// The drawn rect of the active modal popup (help/input/confirm), cached
    /// each render so a mouse press can hit-test its border. `Rect::default()`
    /// ⇒ no modal popup open.
    pub(crate) rect: Rect,
    /// Active border-drag of a modal popup. `None` ⇒ not dragging.
    drag: Option<PopupDrag>,
}

impl PopupGeometry {
    /// True while a modal popup is being border-dragged.
    pub(crate) fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    /// A left press on the active modal popup's border begins a move-drag. `open` is
    /// whether a modal popup is live: `rect` is only refreshed on render (frame-gated),
    /// so a popup closed by a keystroke can leave a stale rect — the caller gates on
    /// the live modal state so a press can't grab a popup that no longer exists.
    /// Returns true iff it grabbed (so the app consumes the event).
    pub(crate) fn begin_drag(&mut self, col: u16, row: u16, open: bool) -> bool {
        if !open {
            return false;
        }
        let r = self.rect;
        if r.width < 2 || r.height < 2 {
            return false; // no modal popup drawn yet
        }
        let inside = col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        let on_border = inside
            && (col == r.x || col == r.x + r.width - 1 || row == r.y || row == r.y + r.height - 1);
        if !on_border {
            return false;
        }
        self.drag = Some(PopupDrag {
            grab: (col, row),
            origin: self.offset,
        });
        true
    }

    /// Updates `offset` from the pointer while a border-drag is active.
    pub(crate) fn drag(&mut self, col: u16, row: u16) {
        if let Some(d) = self.drag {
            let dx = col as i32 - d.grab.0 as i32;
            let dy = row as i32 - d.grab.1 as i32;
            self.offset = (
                (d.origin.0 as i32 + dx) as i16,
                (d.origin.1 as i32 + dy) as i16,
            );
        }
    }

    /// Ends a border-drag.
    pub(crate) fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Resets a modal popup to its centered position (called when one opens).
    pub(crate) fn reset(&mut self) {
        self.offset = (0, 0);
        self.drag = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Filter,
    New,
    NewWindow,
    SplitWindow,
    Rename,
}

pub(crate) struct Input {
    pub(crate) mode: InputMode,
    pub(crate) label: String,
    pub(crate) buffer: String,
    /// The create source / rename target captured when the input opened, so the
    /// action lands on the node the user was on — not wherever streaming results
    /// moved the selection by the time they pressed Enter.
    pub(crate) source: Option<String>,
    pub(crate) sess: Option<Session>,
    /// The split target (`session:window`) for [`InputMode::SplitWindow`].
    pub(crate) target: Option<String>,
}

/// The single open modal, if any — at most one of help / inline input / kill
/// confirm / context menu. Modeling it as one `Option` (not four independent
/// fields) makes the modals' mutual exclusion structural: opening one drops
/// whatever was open, and the compiler guarantees two can never coexist, so the
/// hand-maintained "clear the others" invariant cannot drift. Lives on
/// [`crate::state::State`]; the switcher owns only the behavior and the transient
/// popup geometry (drag offset / drawn rect).
pub(crate) enum Modal {
    Help,
    Input(Input),
    Kill(PendingKill),
    Menu(Menu),
}

/// True while a centered modal popup (help / inline input / kill confirm) is open.
/// These three are draggable and drive [`ModalKind::Popup`]; the context menu is
/// separate (pointer-anchored).
///
/// [`ModalKind::Popup`]: crate::app::focus::ModalKind::Popup
pub(crate) fn is_popup_open(modal: &Option<Modal>) -> bool {
    matches!(modal, Some(Modal::Help | Modal::Input(_) | Modal::Kill(_)))
}

/// True while an inline input (filter / rename / new) is open.
pub(crate) fn is_inputting(modal: &Option<Modal>) -> bool {
    matches!(modal, Some(Modal::Input(_)))
}

/// True while the right-click context menu is open.
pub(crate) fn is_menu_active(modal: &Option<Modal>) -> bool {
    matches!(modal, Some(Modal::Menu(_)))
}

/// Which kind of modal is open — the focus machine derives its modal dimension from
/// this each loop-top, so focus can never mirror-and-desync from the open popup. A
/// centered popup and the context menu are mutually exclusive.
pub(crate) fn modal_kind(modal: &Option<Modal>) -> Option<crate::app::focus::ModalKind> {
    use crate::app::focus::ModalKind;
    match modal {
        Some(Modal::Help | Modal::Input(_) | Modal::Kill(_)) => Some(ModalKind::Popup),
        Some(Modal::Menu(_)) => Some(ModalKind::Menu),
        None => None,
    }
}

/// Feeds a raw key read to the help modal, tmux view-mode style. While help is open
/// every key is consumed (returns true — nothing reaches the tree or the terminal
/// view); `q` or a lone Esc closes it, every other key is swallowed. Returns false
/// when help is closed, so the read falls through to normal routing.
pub(crate) fn feed_help(modal: &mut Option<Modal>, bytes: &[u8]) -> bool {
    if !matches!(modal, Some(Modal::Help)) {
        return false;
    }
    // `q`, or a real Esc (a lone ESC, not the ESC `[` that starts an arrow/CSI).
    let esc = bytes.contains(&0x1b) && !bytes.windows(2).any(|w| w == [0x1b, b'[']);
    if bytes.contains(&b'q') || esc {
        *modal = None;
    }
    true
}

/// The menu entries for a node, by type. Non-selectable rows (pane/loading) get none.
/// `focus` is first so a press-release with no drag falls on the safe default.
pub(crate) fn menu_items(target: &RowRef) -> Vec<MenuItem> {
    use MenuItem::*;
    match target {
        RowRef::Host { .. } => vec![NewSession],
        RowRef::Session(_) => vec![Focus, NewWindow, Rename, Kill],
        RowRef::Window { .. } => vec![Focus, Rename, Kill],
        RowRef::Pane | RowRef::Loading => Vec::new(),
    }
}

/// The menu's title — the human name of the node it acts on (host alias, session
/// name, or `session:window`), shown in the box's top border.
pub(crate) fn menu_title(target: &RowRef) -> String {
    match target {
        RowRef::Host { source, .. } => source.clone(),
        RowRef::Session(s) => s.name.clone(),
        RowRef::Window { sess, window } => crate::mux::window_target(&sess.name, *window),
        RowRef::Pane | RowRef::Loading => String::new(),
    }
}

/// Greedily word-wraps `text` to lines no wider than `width` display columns
/// (Unicode-aware), breaking on spaces; a word longer than `width` is hard-split so
/// nothing is ever clipped. Always returns at least one line. Used so the input
/// prompt's description wraps across a narrow tree column instead of being truncated.
pub(crate) fn wrap_text(text: &str, width: u16) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let width = (width as usize).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split(' ') {
        let ww = UnicodeWidthStr::width(word);
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty() && cur_w + sep + ww > width {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if ww > width {
            // Longer than a whole line: hard-split across as many lines as needed.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if cur_w + cw > width && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += cw;
            }
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
        }
    }
    lines.push(cur);
    lines
}

/// The help modal's `(title, lines)`, built once and rendered through the
/// shared modal-popup path. `prefix` is the configured `[ui] prefix` binding.
pub(crate) fn help_lines(prefix: &str) -> (String, Vec<Line<'static>>) {
    // tmux mode-tree style: a right-aligned, bold key column, a `│` rule, then
    // the description. `Head` breaks the flat list into tree/focus/terminal sections;
    // `Note` is a description-only row (the mux state has no keys of its own).
    //
    // The tree and terminal sections have no configurable keys so they are static.
    // The focus section uses `prefix` so the help modal matches the
    // active binding from config.
    enum HelpRow {
        Head(String),
        Key(String, String),
        Note(&'static str),
        Gap,
    }

    let p = prefix;

    // Tree section — the mutating keys carry the prefix (bare presses are inert);
    // navigation and the `/` filter stay bare.
    let rows: Vec<HelpRow> = vec![
        HelpRow::Head("tree".into()),
        HelpRow::Key("↑/↓ · j/k".into(), "move between siblings".into()),
        HelpRow::Key(
            "→/l · ←/h".into(),
            "descend into / ascend out of a node".into(),
        ),
        HelpRow::Key("PgUp/PgDn".into(), "jump by 10".into()),
        HelpRow::Key("Home/End".into(), "first / last node".into()),
        HelpRow::Key(format!("{p} n"), "new (session / window, by level)".into()),
        HelpRow::Key(format!("{p} R"), "rename the focused session or window".into()),
        HelpRow::Key(format!("{p} x"), "kill it (y / n confirm)".into()),
        HelpRow::Key("/".into(), "fuzzy filter <source>/<name>".into()),
        HelpRow::Key(format!("{p} r"), "re-scan every host".into()),
        HelpRow::Gap,
        // Focus section — prefix rows built from `prefix`.
        HelpRow::Head(format!("focus ({p} = prefix)")),
        HelpRow::Key(format!("Enter · {p} →"), "focus the terminal".into()),
        HelpRow::Key(
            format!("{p} Tab"),
            "toggle focus between tree and terminal".into(),
        ),
        HelpRow::Key(format!("{p} ← · {p} Esc"), "focus the tree".into()),
        HelpRow::Key(
            format!("{p} C-←/→ · h/l"),
            "resize the tree (C-←/→ then repeats briefly)".into(),
        ),
        HelpRow::Key(
            format!("{p} t"),
            "toggle auto-hide-tree (║ view border = on)".into(),
        ),
        HelpRow::Key(format!("{p} ?"), "show this help (q / Esc closes)".into()),
        HelpRow::Key("click a view".into(), "focus that view".into()),
        HelpRow::Key("drag the view border".into(), "resize the tree".into()),
        HelpRow::Key(
            "right-click a row".into(),
            "hold for its menu, release on an item".into(),
        ),
        HelpRow::Key(format!("{p} q"), "quit".into()),
        HelpRow::Key(format!("{p} {p}"), format!("send a literal {p} to the mux")),
        HelpRow::Gap,
        // Mux section — no configurable keys; keep as literals.
        HelpRow::Head("mux (focused)".into()),
        HelpRow::Note("keys, scroll & clicks go to the pane"),
        HelpRow::Note("(the mux needs its own mouse mode on)"),
    ];

    let kw = rows
        .iter()
        .filter_map(|r| match r {
            HelpRow::Key(k, _) => Some(k.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|r| match r {
            HelpRow::Gap => Line::from(""),
            HelpRow::Head(h) => Line::from(Span::styled(
                format!(" {h}"),
                bold.add_modifier(Modifier::UNDERLINED),
            )),
            HelpRow::Key(k, d) => Line::from(vec![
                Span::styled(format!(" {k:>kw$} "), bold),
                Span::raw("│ "),
                Span::raw(d),
            ]),
            HelpRow::Note(n) => Line::from(vec![
                Span::raw(format!(" {:>kw$} ", "")),
                Span::raw("│ "),
                Span::raw(n),
            ]),
        })
        .collect();
    ("keys".to_string(), lines)
}

/// The active input rendered as popup `(title, lines)`: the instructional label,
/// the `❯ buffer` entry line, and a dim Esc hint.
pub(crate) fn input_lines(input: &Input) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let lines = vec![
        Line::from(Span::styled(format!(" {}", input.label.trim()), dim)),
        Line::from(format!(" ❯ {}", input.buffer)),
        Line::from(Span::styled(" Esc to cancel", dim)),
    ];
    (input_title(input.mode).to_string(), lines)
}

/// The armed kill confirm rendered as popup `(title, lines)`, in red.
pub(crate) fn confirm_lines(armed: &PendingKill) -> (String, Vec<Line<'static>>) {
    let red = Style::default().fg(Color::Red);
    let q = match armed {
        PendingKill::Session(sess) => format!(" kill {}?", sess.address()),
        PendingKill::Window { source, target, .. } => format!(" kill {source}/{target}?"),
    };
    let lines = vec![
        Line::from(Span::styled(q, red)),
        Line::from(Span::styled(" [y]es / [n]o · Esc cancel", red)),
    ];
    ("kill?".to_string(), lines)
}

/// Renders an opaque bordered popup at `rect` (titled, content `lines`), in tmux's
/// edge style. Two things make it tmux-consistent:
///
/// 1. **Opaque, no margin.** The box is filled with the reset (default) style so the
///    mux grid's background colours behind it cannot bleed through, and ONLY `rect`
///    itself is cleared — there is no blanket one-cell margin around the box, so
///    half-width neighbours sit flush against the border.
/// 2. **Wide-glyph edge handling.** A double-width (CJK) glyph whose right half the
///    LEFT border now covers would otherwise leave its orphaned left half rendering
///    as a broken glyph just outside the box. That single cell is blanked — and only
///    that cell, only when it is actually a wide glyph. The right edge needs no fixup:
///    ratatui stores a wide char as `[glyph][space]`, so a glyph whose lead the box
///    covers leaves only its already-blank continuation outside.
pub(crate) fn render_popup(
    frame: &mut Frame,
    area: Rect,
    rect: Rect,
    title: &str,
    lines: Vec<Line>,
) {
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .style(Style::reset());
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), rect);
    if rect.x > area.x {
        let x = rect.x - 1;
        let y_end = (rect.y + rect.height).min(area.y + area.height);
        let buf = frame.buffer_mut();
        for y in rect.y..y_end {
            if buf[(x, y)].symbol().width() > 1 {
                buf[(x, y)].set_symbol(" ");
            }
        }
    }
}

impl Menu {
    /// The item index at 0-based screen (col,row), or None if outside the item area
    /// (the box's bordered interior, one row per item below the top border).
    pub(crate) fn item_at(&self, col: u16, row: u16) -> Option<usize> {
        let inside_x = col > self.rect.x && col + 1 < self.rect.x + self.rect.width;
        if !inside_x || row <= self.rect.y {
            return None;
        }
        let i = (row - self.rect.y - 1) as usize;
        (i < self.items.len()).then_some(i)
    }

    /// Whether 0-based screen (col,row) is anywhere inside the box (border included).
    /// Used to keep the highlight while the selection is over the title border but off an
    /// item — only dragging fully outside the box clears it.
    pub(crate) fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }
}

/// Mouse moved while the menu is held: highlight the item under the pointer. Over the
/// box but off an item (the title border) keeps the current highlight; only dragging
/// fully OUTSIDE the box clears it, so releasing there cancels. No-op when no menu.
pub(crate) fn menu_hover(modal: &mut Option<Modal>, col: u16, row: u16) {
    if let Some(Modal::Menu(menu)) = modal.as_mut() {
        if let Some(i) = menu.item_at(col, row) {
            menu.hovered = Some(i);
        } else if !menu.contains(col, row) {
            menu.hovered = None;
        }
    }
}

/// The bordered menu box for an anchor at 0-based screen (col,row): sized to the wider
/// of the widest item label (+ a pad cell each side) and the title, plus borders, and
/// the item count; clamped so it stays fully inside `area` (shifts up/left near an edge).
pub(crate) fn menu_rect(col: u16, row: u16, items: &[MenuItem], title: &str, area: Rect) -> Rect {
    let item_w = items
        .iter()
        .map(|it| UnicodeWidthStr::width(it.label()))
        .max()
        .unwrap_or(0);
    let content_w = (item_w + 2).max(UnicodeWidthStr::width(title)) as u16;
    let w = (content_w + 2).min(area.width.max(1));
    let h = (items.len() as u16 + 2).min(area.height.max(1));
    // Anchor the title row (top border) on the pointer, tmux-style: the pointer lands on
    // the title line, a column left so it sits just inside the box rather than on the left
    // border. item_at() is None on the title row, so no item is pre-selected — an
    // accidental right-click releases off every item (cancel), and a deliberate pick is a
    // short drag straight down onto an item.
    let ax = col.saturating_sub(1);
    let ay = row;
    let max_x = (area.x + area.width).saturating_sub(w).max(area.x);
    let max_y = (area.y + area.height).saturating_sub(h).max(area.y);
    Rect {
        x: ax.clamp(area.x, max_x),
        y: ay.clamp(area.y, max_y),
        width: w,
        height: h,
    }
}

fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// `centered_rect` shifted by `offset` (cells) and clamped fully inside `area`.
pub(crate) fn offset_centered(w: u16, h: u16, area: Rect, offset: (i16, i16)) -> Rect {
    let base = centered_rect(w, h, area);
    let max_x = area.x + area.width.saturating_sub(base.width);
    let max_y = area.y + area.height.saturating_sub(base.height);
    let x = (base.x as i32 + offset.0 as i32).clamp(area.x as i32, max_x as i32) as u16;
    let y = (base.y as i32 + offset.1 as i32).clamp(area.y as i32, max_y as i32) as u16;
    Rect {
        x,
        y,
        width: base.width,
        height: base.height,
    }
}

/// A short popup title for an input mode (shown on the box's top border).
fn input_title(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Filter => "filter",
        InputMode::New => "new session",
        InputMode::NewWindow => "new window",
        InputMode::SplitWindow => "split",
        InputMode::Rename => "rename",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn modal_help_variant_constructs() {
        let m = Modal::Help;
        assert!(matches!(m, Modal::Help));
    }

    #[test]
    fn modal_kind_classifies_help_input_kill_as_popup_menu_as_menu() {
        use crate::app::focus::ModalKind;
        assert_eq!(modal_kind(&None), None);
        assert_eq!(modal_kind(&Some(Modal::Help)), Some(ModalKind::Popup));
        assert!(is_popup_open(&Some(Modal::Help)));
        assert!(!is_menu_active(&Some(Modal::Help)));
        let menu = Menu {
            target: RowRef::Pane,
            title: String::new(),
            rect: Rect::default(),
            items: Vec::new(),
            hovered: None,
        };
        assert_eq!(modal_kind(&Some(Modal::Menu(menu))), Some(ModalKind::Menu));
    }

    #[test]
    fn help_feed_consumes_and_closes_on_q_or_esc() {
        // tmux view-mode style: while open, every key is consumed; q/Esc closes, the
        // rest are swallowed; while closed, nothing is consumed (falls through).
        let mut m: Option<Modal> = None;
        assert!(!feed_help(&mut m, b"q"), "closed → not consumed");

        m = Some(Modal::Help);
        assert!(feed_help(&mut m, b"j"), "open → consumed");
        assert!(
            matches!(m, Some(Modal::Help)),
            "a non-close key is swallowed but keeps help open"
        );
        assert!(
            feed_help(&mut m, b"\x1b[A"),
            "an arrow (ESC [) is swallowed, not a close"
        );
        assert!(matches!(m, Some(Modal::Help)), "arrow keeps help open");
        assert!(feed_help(&mut m, b"q"), "q → consumed");
        assert!(m.is_none(), "q closes help");

        m = Some(Modal::Help);
        assert!(feed_help(&mut m, b"\x1b"), "lone Esc → consumed");
        assert!(m.is_none(), "Esc closes help");
    }

    #[test]
    fn menu_rect_clamps_into_screen() {
        use super::MenuItem::*;
        let area = Rect::new(0, 0, 80, 24);
        let items = [Focus, Rename, Kill];
        // Anchored near the bottom-right corner → shifted up/left to stay on-screen.
        let r = menu_rect(78, 23, &items, "editor", area);
        assert!(
            r.x + r.width <= area.width,
            "box stays within the right edge"
        );
        assert!(
            r.y + r.height <= area.height,
            "box stays within the bottom edge"
        );
    }

    #[test]
    fn menu_rect_fits_a_title_wider_than_the_items() {
        use super::MenuItem::*;
        let area = Rect::new(0, 0, 80, 24);
        let r = menu_rect(0, 0, &[Focus], "a-very-long-session-name", area);
        assert!(
            r.width as usize >= "a-very-long-session-name".len() + 2,
            "title fits in the box"
        );
    }

    #[test]
    fn menu_rect_measures_cjk_title_by_display_width() {
        use super::MenuItem::*;
        let area = Rect::new(0, 0, 80, 24);
        let title = "한국한국한국한국";
        let r = menu_rect(0, 0, &[Focus], title, area);
        assert_eq!(r.width as usize, UnicodeWidthStr::width(title) + 2);
    }

    #[test]
    fn wrap_text_wraps_on_words_and_hard_splits_long_words() {
        use unicode_width::UnicodeWidthStr;
        let s = "filter sessions · Esc to cancel";
        let lines = wrap_text(s, 19);
        assert!(
            lines.len() >= 2,
            "wraps when narrower than the text: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| l.as_str().width() <= 19),
            "no line exceeds width: {lines:?}"
        );
        assert!(
            lines.join(" ").contains("cancel"),
            "tail survives (not clipped): {lines:?}"
        );
        // A single word longer than the width is hard-split, each piece within width.
        let long = wrap_text("supercalifragilistic", 5);
        assert!(
            long.len() >= 4 && long.iter().all(|l| l.as_str().width() <= 5),
            "{long:?}"
        );
        // A wide enough width keeps it on one line.
        assert_eq!(wrap_text(s, 100).len(), 1);
    }

    #[test]
    fn popup_blanks_only_a_wide_glyph_bisected_by_the_left_border() {
        // tmux edge behaviour: no blanket margin. A double-width glyph whose right half
        // the left border covers is blanked (its orphaned half would render broken); a
        // half-width char at the same edge column stays flush; the box covers opaquely.
        let backend = TestBackend::new(40, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.buffer_mut()[(9u16, 3u16)].set_symbol("한"); // wide; right half under the border at x=10
            f.buffer_mut()[(9u16, 4u16)].set_symbol("Y"); // half-width at the same edge column
            f.buffer_mut()[(15u16, 4u16)].set_style(Style::default().bg(Color::Red)); // behind the popup
            let rect = Rect::new(10, 2, 12, 5);
            render_popup(
                f,
                area,
                rect,
                "t",
                vec![Line::from("focus"), Line::from("kill"), Line::from("x")],
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(
            buf[(9u16, 3u16)].symbol(),
            " ",
            "wide glyph bisected by the left border is blanked"
        );
        assert_eq!(
            buf[(9u16, 4u16)].symbol(),
            "Y",
            "a half-width char at the edge stays flush — no margin"
        );
        assert_eq!(
            buf[(15u16, 4u16)].bg,
            Color::Reset,
            "the popup covers the background colour opaquely"
        );
    }
}
