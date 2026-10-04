//! The nav's position-independent layout, checked at every attachment side on fixed
//! backend sizes: the group grammar, the one seam, the prefix indicator, the overflow
//! marks, the collapsed shape, the one-row band, and the hit-test that reads them back.

use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::Terminal;

const W: u16 = 100;
const H: u16 = 20;
const SIDE_W: u16 = 30;
const BAND_H: u16 = 6;
const ALL: [NavPosition; 4] = [
    NavPosition::Left,
    NavPosition::Right,
    NavPosition::Top,
    NavPosition::Bottom,
];

fn is_band(position: NavPosition) -> bool {
    position.layout() == ViewLayout::Band
}

fn nav_at(position: NavPosition) -> NavSize {
    let nav = NavSize::visible(SIDE_W).with_position(position);
    if is_band(position) {
        nav.with_height(BAND_H)
    } else {
        nav
    }
}

fn collapsed_at(position: NavPosition) -> NavSize {
    NavSize {
        width: collapsed_nav_width("C-g"),
        collapsed: true,
        ..nav_at(position)
    }
}

fn sess(source: &str, name: &str) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        mux: String::new(),
        windows: 1,
        attached: false,
    }
}

fn scan_of(groups: Vec<(&str, Vec<&str>)>) -> Scan {
    Scan {
        groups: groups
            .into_iter()
            .map(|(source, names)| Group {
                source: source.into(),
                err: None,
                sessions: names.into_iter().map(|n| sess(source, n)).collect(),
            })
            .collect(),
    }
}

fn two_groups() -> Scan {
    scan_of(vec![
        ("local", vec!["build", "editor"]),
        ("jupiter00", vec!["inference"]),
    ])
}

fn many_sessions(n: usize, name_len: usize) -> Scan {
    let names: Vec<String> = (0..n)
        .map(|i| format!("{i:0>width$}", width = name_len))
        .collect();
    Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: names.iter().map(|n| sess("local", n)).collect(),
        }],
    }
}

struct Shot {
    sw: Switcher,
    state: crate::state::State,
    nav: NavSize,
    plan: RenderPlan,
    buf: Buffer,
}

impl Shot {
    fn new(scan: Scan, nav: NavSize, terminal_focused: bool) -> Self {
        let mut state = crate::state::State::from_scan(scan);
        let sw = Switcher::new(&mut state);
        let mut shot = Shot {
            sw,
            state,
            nav,
            plan: RenderPlan::default(),
            buf: Buffer::empty(Rect::new(0, 0, W, H)),
        };
        shot.draw(terminal_focused);
        shot
    }

    fn draw(&mut self, terminal_focused: bool) {
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        let previous = self.plan.clone();
        let mut next = None;
        term.draw(|f| {
            let plan = self.sw.layout(f.area(), self.nav, &self.state, &previous);
            self.sw
                .render(f, None, terminal_focused, &self.state, &plan);
            next = Some(plan);
        })
        .unwrap();
        self.plan = next.unwrap();
        self.buf = term.backend().buffer().clone();
    }

    /// The nav's own cells: its region plus the seam beside it.
    fn nav_area(&self) -> Rect {
        let r = self.plan.regions;
        let nav = if is_band(self.nav.position) {
            Rect {
                x: 0,
                width: W,
                ..if self.nav.position == NavPosition::Top {
                    Rect::new(0, 0, W, r.view_border.y)
                } else {
                    Rect::new(0, r.view_border.bottom(), W, H - r.view_border.bottom())
                }
            }
        } else if self.nav.position == NavPosition::Left {
            Rect::new(0, 0, r.view_border.x, H)
        } else {
            Rect::new(r.view_border.right(), 0, W - r.view_border.right(), H)
        };
        nav.union(r.view_border)
    }

    fn row(&self, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1).map(|x| self.buf[(x, y)].symbol()).collect()
    }

    fn area_text(&self, area: Rect) -> String {
        (area.y..area.bottom())
            .map(|y| self.row(y, area.x, area.right()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn find_in(&self, area: Rect, needle: &str) -> Option<(u16, u16)> {
        let chars: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if x as usize + chars.len() > area.right() as usize {
                    break;
                }
                if chars
                    .iter()
                    .enumerate()
                    .all(|(i, c)| self.buf[(x + i as u16, y)].symbol() == c)
                {
                    return Some((x, y));
                }
            }
        }
        None
    }

    fn seam_text(&self) -> String {
        self.area_text(self.plan.regions.view_border)
    }
}

#[test]
fn pl1_group_titles_are_dim_and_their_cards_indent_at_every_position() {
    for position in ALL {
        let shot = Shot::new(two_groups(), nav_at(position), false);
        let nav = shot.nav_area();
        let (tx, ty) = shot
            .find_in(nav, "jupiter00")
            .unwrap_or_else(|| panic!("{position:?}: the title is painted"));
        assert_eq!(
            shot.buf[(tx, ty)].fg,
            shot.sw.palette().decoration,
            "{position:?}: a group title is dim"
        );
        let (cx, cy) = shot
            .find_in(nav, "3 inference")
            .unwrap_or_else(|| panic!("{position:?}: the card is painted"));
        assert_eq!(cy, ty + 1, "{position:?}: the card hangs under its title");
        assert_eq!(cx, tx + 2, "{position:?}: the card indents under its title");
        let after = shot.row(ty, tx + 9, shot.plan.regions.tree.right());
        assert!(
            !after.contains('─') && !after.contains('│'),
            "{position:?}: no rule or connector marks the group: {after:?}"
        );
    }
}

#[test]
fn pl1_a_band_continuation_column_repeats_its_title() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let nav = nav_at(position).with_height(4);
        let shot = Shot::new(many_sessions(6, 2), nav, false);
        let tree = shot.plan.regions.tree;
        let first_row = Rect { height: 1, ..tree };
        let (x, _) = shot
            .find_in(first_row, "local …")
            .unwrap_or_else(|| panic!("{position:?}: {}", shot.area_text(tree)));
        assert!(x > 0, "{position:?}: the repeat leads a later column");
        assert_eq!(
            shot.buf[(x, tree.y)].fg,
            shot.sw.palette().decoration,
            "{position:?}: the repeated title is as dim as the title"
        );
    }
}

#[test]
fn pl2_the_selected_card_reverses_only_under_nav_focus() {
    for position in ALL {
        for terminal_focused in [false, true] {
            let shot = Shot::new(two_groups(), nav_at(position), terminal_focused);
            let (x, y) = shot
                .find_in(shot.nav_area(), "❯ build")
                .unwrap_or_else(|| panic!("{position:?}: the mark is painted"));
            let reversed = shot.buf[(x + 2, y)].modifier.contains(Modifier::REVERSED);
            assert_eq!(
                reversed, !terminal_focused,
                "{position:?} terminal_focused={terminal_focused}: nav focus reverses, terminal focus keeps the mark only"
            );
        }
    }
}

#[test]
fn pl3_the_prefix_sits_on_the_column_bottom_or_the_band_seam() {
    for position in ALL {
        let shot = Shot::new(two_groups(), nav_at(position), false);
        if is_band(position) {
            let seam = shot.seam_text();
            assert!(
                seam.trim_end().ends_with("C-g"),
                "{position:?}: the seam ends with the prefix: {seam:?}"
            );
            assert_eq!(
                shot.plan.regions.tree.height, BAND_H,
                "{position:?}: every band row holds cards"
            );
            assert!(
                !shot.area_text(shot.plan.regions.tree).contains("C-g"),
                "{position:?}: no band row is spent on the prefix"
            );
        } else {
            let nav = shot.nav_area();
            let bottom = shot.row(H - 1, nav.x, nav.right());
            let bottom = bottom.trim_matches(|c: char| c == ' ' || c == '│');
            assert_eq!(
                bottom, "C-g",
                "{position:?}: the column's bottom line is the prefix alone"
            );
        }
    }
}

#[test]
fn pl4_overflow_is_a_thick_seam_segment_or_counts_on_the_band_seam() {
    for position in ALL {
        let scan = if is_band(position) {
            many_sessions(60, 12)
        } else {
            many_sessions(40, 4)
        };
        let shot = Shot::new(scan, nav_at(position), false);
        let seam = shot.seam_text();
        if is_band(position) {
            assert!(
                seam.contains(" ›"),
                "{position:?}: the band seam counts the cards off to the right: {seam:?}"
            );
            let (x, y) = shot.find_in(shot.plan.regions.view_border, " ›").unwrap();
            assert!(
                shot.buf[(x - 1, y)]
                    .symbol()
                    .chars()
                    .all(|c| c.is_ascii_digit()),
                "{position:?}: a count stands before the mark: {seam:?}"
            );
        } else {
            assert!(
                seam.contains('┃'),
                "{position:?}: the seam thickens where the visible cards are"
            );
            assert!(
                seam.contains('│'),
                "{position:?}: the rest of the seam stays thin"
            );
            assert!(
                !shot.area_text(shot.nav_area()).contains('▐'),
                "{position:?}: no scrollbar column is reserved"
            );
        }
    }
}

#[test]
fn pl5_a_collapsed_nav_is_the_seam_line_or_a_prefix_wide_column() {
    for position in ALL {
        let shot = Shot::new(two_groups(), collapsed_at(position), false);
        let r = shot.plan.regions;
        match position {
            NavPosition::Top | NavPosition::Bottom => {
                assert_eq!(
                    r.terminal.height,
                    H - 1,
                    "{position:?}: the collapsed band is the seam line only"
                );
                assert!(
                    shot.seam_text().trim_end().ends_with("C-g"),
                    "{position:?}: the seam still carries the prefix"
                );
            }
            NavPosition::Left => {
                assert_eq!(r.view_border.x, 5, "the prefix plus a cell either side")
            }
            NavPosition::Right => assert_eq!(r.view_border.x, W - 6),
        }
        let text = shot.area_text(shot.nav_area());
        for token in ["<<", ">>", "▲", "▼"] {
            assert!(!text.contains(token), "{position:?}: no button: {text:?}");
        }
    }
}

#[test]
fn pl7_a_one_row_band_runs_title_and_cards_on_one_line() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let shot = Shot::new(two_groups(), nav_at(position).with_height(1), false);
        let tree = shot.plan.regions.tree;
        assert_eq!(tree.height, 1, "{position:?}");
        let line = shot.row(tree.y, 0, W);
        assert!(
            line.contains("local  ❯ build  2 editor"),
            "{position:?}: the title runs straight into its cards: {line:?}"
        );
    }
}

#[test]
fn pl7_a_one_row_band_scrolls_to_the_selection() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut shot = Shot::new(many_sessions(30, 6), nav_at(position).with_height(1), false);
        shot.sw.move_to(-1, &shot.state);
        shot.draw(false);
        let tree = shot.plan.regions.tree;
        assert!(
            shot.row(tree.y, 0, W).contains("❯ 000029"),
            "{position:?}: the last card scrolled into view"
        );
    }
}

#[test]
fn pl9_the_resting_nav_has_no_collapse_button() {
    for position in ALL {
        let shot = Shot::new(two_groups(), nav_at(position), false);
        let text = shot.area_text(shot.nav_area());
        for token in ["<<", ">>", "▲", "▼"] {
            assert!(
                !text.contains(token),
                "{position:?}: no {token} button: {text:?}"
            );
        }
    }
}

#[test]
fn hit_test_reads_every_band_row_as_cards() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut shot = Shot::new(many_sessions(20, 4), nav_at(position), false);
        let tree = shot.plan.regions.tree;
        let last_row = tree.bottom() - 1;
        let (x, y) = shot
            .find_in(
                Rect {
                    y: last_row,
                    height: 1,
                    ..tree
                },
                "0004",
            )
            .unwrap_or_else(|| panic!("{position:?}: {}", shot.area_text(tree)));
        let plan = shot.plan.clone();
        shot.sw.mouse_select(&plan, x, y, &shot.state);
        assert_eq!(
            shot.sw.current_ref().map(|r| match r {
                RowRef::Session { sess } => sess.name.clone(),
                _ => String::new(),
            }),
            Some("0004".to_string()),
            "{position:?}: a click on the band's last row selects its card"
        );
    }
}

#[test]
fn hit_test_a_band_overflow_count_selects_the_nearest_hidden_card() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut shot = Shot::new(many_sessions(60, 12), nav_at(position), false);
        let seam = shot.plan.regions.view_border;
        let (x, y) = shot
            .find_in(seam, " ›")
            .unwrap_or_else(|| panic!("{position:?}: {}", shot.seam_text()));
        let before = shot.sw.selected;
        let plan = shot.plan.clone();
        shot.sw.mouse_select(&plan, x + 1, y, &shot.state);
        assert_ne!(
            shot.sw.selected, before,
            "{position:?}: the count is a target"
        );
        let visible: Vec<usize> = plan.nav_cells.iter().map(|(i, _)| *i).collect();
        assert!(
            !visible.contains(&shot.sw.selected),
            "{position:?}: the selection lands on a card that was off screen"
        );
        shot.draw(false);
        assert!(
            shot.plan
                .nav_cells
                .iter()
                .any(|(i, _)| *i == shot.sw.selected),
            "{position:?}: and the band scrolls to show it"
        );
    }
}

#[test]
fn pl9_the_armed_hint_and_the_help_name_prefix_z() {
    let mut state = crate::state::State::from_scan(two_groups());
    state.chrome.set_ui_prefix("C-g".into());
    state.chrome.set_armed(true);
    let text = state.chrome.hint_bar_text(400, &state);
    assert!(
        text.contains("· z collapse"),
        "the armed hint names z: {text}"
    );
    let (_, lines) = crate::ui::modal::help_lines("C-g", NavPosition::Left, &Default::default());
    let help: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    assert!(
        help.iter()
            .any(|l| l.contains("C-g z") && l.contains("collapse")),
        "the help names prefix z: {help:#?}"
    );
}
