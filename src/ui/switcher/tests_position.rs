//! The nav's position-independent layout, checked at every attachment side on fixed
//! backend sizes: the group grammar, the one seam, the prefix hint, the overflow
//! marks, the collapsed shape, the one-row band, and the hit-test that reads them back.

use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
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
    position.layout() == ViewLayout::Horizontal
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

fn sess(host: &str, name: &str) -> Session {
    Session {
        host: host.into(),
        name: name.into(),
        mux: String::new(),
        id: String::new(),
        windows: 1,
        clients: 0,
        stopped: false,
    }
}

fn scan_of(groups: Vec<(&str, Vec<&str>)>) -> Scan {
    Scan {
        groups: groups
            .into_iter()
            .map(|(host, names)| Group {
                host: host.into(),
                err: None,
                sessions: names.into_iter().map(|n| sess(host, n)).collect(),
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
            host: "local".into(),
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

#[test]
fn three_card_groups_and_focus_policy_hold_at_every_position() {
    for position in ALL {
        let mut scan = scan_of(vec![("local", vec!["work"]), ("z-empty", vec![])]);
        scan.groups.push(Group {
            host: "a-offline".into(),
            sessions: vec![],
            err: Some("connection refused".into()),
        });
        let mut shot = Shot::new(scan, nav_at(position), false);
        let session = shot
            .sw
            .rows
            .iter()
            .position(|r| matches!(r.reference, RowRef::Session { .. }))
            .unwrap();
        let empty = shot
            .sw
            .rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Host { host, .. } if host == "z-empty"))
            .unwrap();
        let offline = shot
            .sw
            .rows
            .iter()
            .position(
                |r| matches!(&r.reference, RowRef::Machine { machine, .. } if machine == "a-offline"),
            )
            .unwrap();
        assert!(session < empty && empty < offline, "{position:?}");
        shot.sw.set_selected(session);
        shot.sw.nav_horizontal(1);
        assert_eq!(shot.sw.selected, empty);
        shot.sw.nav_horizontal(1);
        assert_eq!(shot.sw.selected, offline);
        shot.sw.nav_horizontal(1);
        assert_eq!(shot.sw.selected, session);
        let rect = |i| {
            shot.plan
                .nav_cells
                .iter()
                .find(|(idx, _)| *idx == i)
                .unwrap()
                .1
        };
        let empty_rect = rect(empty);
        let offline_rect = rect(offline);
        if is_band(position) {
            assert!(offline_rect.x > empty_rect.right(), "{position:?}");
        } else {
            assert!(offline_rect.y > empty_rect.bottom(), "{position:?}");
        }
        let numbers = [
            shot.sw.card_number(session),
            shot.sw.card_number(empty),
            shot.sw.card_number(offline),
        ];
        shot.sw.set_selected(session);
        shot.sw.sync_view_focus(true);
        shot.state.chrome.armed = true;
        shot.draw(true);
        assert!(
            shot.plan.nav_cells.iter().all(|(i, _)| *i < empty),
            "{position:?}"
        );
        for selected in [empty, offline] {
            shot.sw.sync_view_focus(false);
            shot.sw.set_selected(selected);
            shot.sw.sync_view_focus(true);
            shot.draw(true);
            assert!(
                shot.plan.nav_cells.iter().any(|(i, _)| *i == empty),
                "{position:?}"
            );
            assert!(
                shot.plan.nav_cells.iter().any(|(i, _)| *i == offline),
                "{position:?}"
            );
        }
        shot.sw.sync_view_focus(false);
        shot.draw(false);
        assert_eq!(
            numbers,
            [
                shot.sw.card_number(session),
                shot.sw.card_number(empty),
                shot.sw.card_number(offline)
            ]
        );
    }
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
                    Rect::new(0, 0, W, r.nav_border.y)
                } else {
                    Rect::new(0, r.nav_border.bottom(), W, H - r.nav_border.bottom())
                }
            }
        } else if self.nav.position == NavPosition::Left {
            Rect::new(0, 0, r.nav_border.x, H)
        } else {
            Rect::new(r.nav_border.right(), 0, W - r.nav_border.right(), H)
        };
        nav.union(r.nav_border)
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
        self.area_text(self.plan.regions.nav_border)
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
        let after = shot.row(ty, tx + 9, shot.plan.regions.nav.right());
        assert!(
            !after.contains('─') && !after.contains('│'),
            "{position:?}: no rule or connector marks the group: {after:?}"
        );
    }
}

#[test]
fn pl1_a_band_continuation_starts_with_sessions_without_a_title() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        for height in [2, 3, 4] {
            let nav = nav_at(position).with_height(height);
            let mut scan = many_sessions(9, 2);
            scan.groups.push(Group {
                host: "remote".into(),
                err: None,
                sessions: vec![sess("remote", "work")],
            });
            let shot = Shot::new(scan, nav, false);
            let tree = shot.plan.nav_inner;
            let text = shot.area_text(tree);
            assert_eq!(text.matches("local").count(), 1, "{position:?}: {text}");
            assert_eq!(text.matches("remote").count(), 1, "{position:?}: {text}");
            for i in 0..9 {
                let name = format!("{i:02}");
                let (_, y) = shot.find_in(tree, &name).expect("every session is visible");
                assert_eq!(y, tree.y + (i + 1) % tree.height, "{position:?}: {text}");
            }
        }
    }
}

#[test]
fn pl2_the_selected_card_is_highlighted_in_both_focus_states() {
    for position in ALL {
        for terminal_focused in [false, true] {
            let shot = Shot::new(two_groups(), nav_at(position), terminal_focused);
            let (x, y) = shot
                .find_in(shot.nav_area(), "1 build")
                .unwrap_or_else(|| panic!("{position:?}: the card is painted"));
            let highlighted = shot.buf[(x + 2, y)].bg == Color::LightGreen;
            assert!(
                highlighted,
                "{position:?} terminal_focused={terminal_focused}: the selected card stays highlighted"
            );
        }
    }
}

#[test]
fn pl3_the_prefix_starts_at_the_nav_start() {
    for position in ALL {
        let shot = Shot::new(two_groups(), nav_at(position), false);
        if is_band(position) {
            let seam = shot.seam_text();
            assert!(
                seam.starts_with(" C-g"),
                "{position:?}: the border row starts with the prefix: {seam:?}"
            );
            assert_eq!(
                shot.plan.regions.nav.height, BAND_H,
                "{position:?}: every band row holds cards"
            );
            assert!(
                !shot.area_text(shot.plan.regions.nav).contains("C-g"),
                "{position:?}: no band row is spent on the prefix"
            );
        } else {
            let nav = shot.nav_area();
            let top = shot.row(0, nav.x, nav.right());
            let top = top.trim_matches(|c: char| c == ' ' || c == '│');
            assert_eq!(
                top, "C-g",
                "{position:?}: the nav's first line is the prefix alone"
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
            let (x, y) = shot.find_in(shot.plan.regions.nav_border, " ›").unwrap();
            assert!(
                shot.buf[(x - 1, y)]
                    .symbol()
                    .chars()
                    .all(|c| c.is_ascii_digit()),
                "{position:?}: a count stands before the mark: {seam:?}"
            );
        } else {
            let thumb = shot.plan.border_thumb;
            let thick_rows: Vec<u16> = (shot.plan.regions.nav_border.y
                ..shot.plan.regions.nav_border.bottom())
                .filter(|&y| shot.buf[(shot.plan.regions.nav_border.x, y)].symbol() == "┃")
                .collect();
            assert_eq!(
                thick_rows,
                (thumb.y..thumb.bottom()).collect::<Vec<_>>(),
                "{position:?}: the seam thickens exactly where the visible cards are"
            );
            assert!(
                !thick_rows.is_empty() && (thick_rows.len() as u16) < shot.plan.nav_inner.height,
                "{position:?}: the thumb is a proportion of the card rows: {seam:?}"
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
                    "{position:?}: the collapsed horizontal nav is the border line only"
                );
                assert!(
                    shot.seam_text().starts_with(" C-g"),
                    "{position:?}: the border row still carries the prefix"
                );
            }
            NavPosition::Left => {
                assert_eq!(r.nav_border.x, 2, "on the prefix's last column");
                assert_eq!(r.terminal.x, 3);
            }
            NavPosition::Right => {
                assert_eq!(r.nav_border.x, W - 3, "on the prefix's first column");
                assert_eq!(r.terminal.width, W - 3);
            }
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
        let tree = shot.plan.regions.nav;
        assert_eq!(tree.height, 1, "{position:?}");
        let line = shot.row(tree.y, 0, W);
        // Each card owns its padding and Enter mark without touching the next card.
        let enter = super::render::ENTER_MARK;
        assert!(
            line.contains(&format!("local  1 build {enter} ")) && line.contains(" 2 editor "),
            "{position:?}: the title runs straight into its cards: {line:?}"
        );
    }
}

#[test]
fn selecting_a_session_card_keeps_its_section_title_on_screen() {
    // A tall side list: moving the selection far down scrolls a middle section's
    // title off the top, and moving back up to that section's card pulls the list
    // back so the machine/host title shares the screen with the selected card.
    let mut scan = Scan::default();
    for i in 0..16 {
        scan.groups.push(Group {
            host: format!("host{i:02}"),
            err: None,
            sessions: vec![sess(&format!("host{i:02}"), "a")],
        });
    }
    let mut shot = Shot::new(scan, nav_at(NavPosition::Left), false);
    shot.sw.move_to(-1);
    shot.draw(false);
    assert!(
        shot.find_in(shot.nav_area(), "host04").is_none(),
        "the middle section's title is scrolled off before the selection returns"
    );
    shot.sw.move_to(4);
    shot.draw(false);
    assert!(
        shot.find_in(shot.nav_area(), "host04").is_some(),
        "selecting the session card reveals its machine/host section title"
    );
}

#[test]
fn selecting_a_card_of_a_split_section_pulls_its_title_back() {
    // A section taller than a column splits, and its title stays where the section
    // starts. A card in the continuation hangs under a title left of the window: when
    // the columns from the title through the card fit together, the scroll shows the
    // title instead of leaving it off screen.
    let mut scan = Scan::default();
    scan.groups.push(Group {
        host: "alpha".into(),
        err: None,
        sessions: (0..9).map(|i| sess("alpha", &format!("s{i:02}"))).collect(),
    });
    scan.groups.push(Group {
        host: "jupiter00".into(),
        err: None,
        sessions: (0..20)
            .map(|i| sess("jupiter00", &format!("w{i:018}")))
            .collect(),
    });
    let mut shot = Shot::new(scan, nav_at(NavPosition::Top), false);
    shot.sw.move_to(28);
    shot.draw(false);
    shot.sw.move_to(6);
    shot.draw(false);
    assert!(
        shot.find_in(shot.nav_area(), "alpha").is_some(),
        "the split section's title shares the screen with its card"
    );
}

#[test]
fn pl7_a_one_row_band_scrolls_to_the_selection() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut shot = Shot::new(many_sessions(30, 6), nav_at(position).with_height(1), false);
        shot.sw.move_to(-1);
        shot.draw(false);
        let tree = shot.plan.regions.nav;
        assert!(
            shot.row(tree.y, 0, W).contains("000029"),
            "{position:?}: the last card scrolled into view"
        );
    }
}

#[test]
fn pl9_the_resting_nav_text_carries_no_arrow_glyphs() {
    for position in ALL {
        let shot = Shot::new(two_groups(), nav_at(position), false);
        let text = shot.area_text(shot.nav_area());
        for token in ["<<", ">>", "▲", "▼"] {
            assert!(
                !text.contains(token),
                "{position:?}: the nav text holds no {token}: {text:?}"
            );
        }
    }
}

#[test]
fn hit_test_reads_every_band_row_as_cards() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut shot = Shot::new(many_sessions(20, 4), nav_at(position), false);
        let tree = shot.plan.regions.nav;
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
        shot.sw.mouse_select(&plan, x, y);
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
        let seam = shot.plan.regions.nav_border;
        let (x, y) = shot
            .find_in(seam, " ›")
            .unwrap_or_else(|| panic!("{position:?}: {}", shot.seam_text()));
        let before = shot.sw.selected;
        let plan = shot.plan.clone();
        shot.sw.mouse_select(&plan, x + 1, y);
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
fn pl9_the_key_list_and_the_help_name_prefix_z() {
    let list = crate::ui::keylist::key_list("C-g", NavPosition::Left, 160, 30).unwrap();
    assert!(
        list.columns.iter().flatten().any(|c| matches!(
            c,
            crate::ui::keylist::Cell::Key { key, desc } if key == "z" && desc.contains("collapse")
        )),
        "the key list names z: {list:?}"
    );
    let (_, lines) = crate::ui::modal::help_lines(
        "C-g",
        NavPosition::Left,
        &Default::default(),
        "",
        0,
        None,
        None,
        200,
        u16::MAX,
    );
    let help: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    assert!(
        help.iter()
            .any(|l| l.contains("C-g z") && l.contains("collapse")),
        "the help names prefix z: {help:#?}"
    );
}

#[test]
fn a_toast_floats_in_the_terminal_corner_nearest_the_hint_at_every_position() {
    for position in ALL {
        let mut shot = Shot::new(two_groups(), nav_at(position), true);
        shot.state.notify.toast(
            "gpu-02",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Success,
                "logged in",
            )],
        );
        shot.draw(true);
        let terminal = shot.plan.regions.terminal;
        let (id, rect) = shot.plan.toasts[0];
        let expected_x = if position == NavPosition::Left {
            terminal.x
        } else {
            terminal.right() - rect.width
        };
        let expected_y = if position == NavPosition::Top {
            terminal.y
        } else {
            terminal.bottom() - rect.height
        };
        assert_eq!(
            (rect.x, rect.y),
            (expected_x, expected_y),
            "{position:?}: {rect:?} in {terminal:?}"
        );
        assert!(rect.width <= W * 2 / 5, "{position:?}: {rect:?}");
        let row: String = (rect.x..rect.right())
            .map(|x| shot.buf[(x, rect.y + 1)].symbol())
            .collect();
        assert!(row.contains("✓ logged in"), "{position:?}: {row:?}");
        assert_eq!(
            shot.plan.toast_at(rect.x + 1, rect.y + 1),
            Some(id),
            "{position:?}: a click inside lands on the toast"
        );
        assert_eq!(shot.plan.toast_at(terminal.x, terminal.y + H), None);
    }
}

#[test]
fn no_toast_covers_the_prefix_key_list() {
    for position in ALL {
        let mut shot = Shot::new(two_groups(), nav_at(position), false);
        shot.state.chrome.set_armed(true);
        shot.state.chrome.set_nav_position(position);
        // A toast exactly as tall as the terminal view would fit there, and would cover
        // whichever of its rows the key list opens on.
        let rows = shot.plan.regions.terminal.height as usize - 2;
        shot.state.notify.toast(
            "gpu-02",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Error,
                vec!["denied"; rows].join("\n"),
            )],
        );
        shot.draw(false);
        let (list, _) = shot.plan.key_list.clone().expect("the key list is open");
        assert!(
            shot.plan.toasts.iter().all(|(_, r)| !r.intersects(list)),
            "{position:?}: {:?} against the key list at {list:?}",
            shot.plan.toasts
        );
        let text: String = (list.y..list.bottom())
            .map(|y| shot.row(y, list.x, list.right()))
            .collect();
        assert!(
            text.contains("history"),
            "{position:?}: the key list stays readable: {text:?}"
        );
    }
}

#[test]
fn the_prefix_key_list_opens_toward_the_terminal_and_the_indicator_keeps_the_prefix() {
    for position in ALL {
        let mut shot = Shot::new(two_groups(), nav_at(position), false);
        shot.state.chrome.set_armed(true);
        shot.state.chrome.set_nav_position(position);
        shot.draw(false);
        let r = shot.plan.regions;
        let (list, _) = shot.plan.key_list.clone().expect("the key list is open");
        // At the card flow's start, against the nav border on the terminal view's side.
        match position {
            NavPosition::Left => {
                assert_eq!(list.x, r.terminal.x, "{position:?}: {list:?}");
                assert_eq!(list.y, r.terminal.y, "{position:?}: {list:?}");
            }
            NavPosition::Right => {
                assert_eq!(list.right(), r.terminal.right(), "{position:?}: {list:?}");
                assert_eq!(list.y, r.terminal.y, "{position:?}: {list:?}");
            }
            // A list that would leave a sliver of one or two cells at the left edge
            // snaps to that edge instead.
            NavPosition::Top => {
                assert_eq!(list.y, r.nav_border.bottom(), "{position:?}: {list:?}");
                assert!(
                    list.right() == W || (list.x == 0 && list.width + 2 >= W),
                    "{position:?}: {list:?}"
                );
            }
            NavPosition::Bottom => {
                assert_eq!(list.bottom(), r.nav_border.y, "{position:?}: {list:?}");
                assert!(
                    list.right() == W || (list.x == 0 && list.width + 2 >= W),
                    "{position:?}: {list:?}"
                );
            }
        }
        assert!(
            !list.intersects(r.nav) && !list.intersects(r.nav_border),
            "{position:?}: the list covers no card and no border: {list:?}"
        );
        // A boxed list titled with the prefix, its sections named.
        assert_eq!(shot.buf[(list.x, list.y)].symbol(), "╭", "{position:?}");
        let text: String = (list.y..list.bottom())
            .map(|y| shot.row(y, list.x, list.right()))
            .collect::<Vec<_>>()
            .join("\n");
        for word in ["C-g", "navigate", "sessions", "view", "app", "history"] {
            assert!(text.contains(word), "{position:?}: {word} in {text}");
        }
        let chip = shot.row(r.prefix_hint.y, r.prefix_hint.x, r.prefix_hint.right());
        assert!(
            chip.contains("C-g"),
            "{position:?}: the prefix hint keeps the prefix: {chip:?}"
        );
        // The prefix ends and the list closes.
        shot.state.chrome.set_armed(false);
        shot.draw(false);
        assert!(shot.plan.key_list.is_none(), "{position:?}");
    }
}

/// Every surface a prefix key opens, as the modal it sets.
fn prefix_surfaces() -> Vec<crate::state::Modal> {
    use crate::state::{Input, InputMode, Modal};
    let input = |mode, host: Option<&str>| {
        let mut input = Input::new(mode, String::new(), host.map(str::to_string));
        if matches!(mode, InputMode::Logout | InputMode::LogoutKeys) {
            input.facts = vec![
                ("machine", "local".into()),
                ("SSH login", "public key".into()),
                (
                    "key",
                    "removed from local; asks first if xmux did not add it".into(),
                ),
                ("connections", "closes local connections".into()),
            ];
        }
        Modal::Input(Box::new(input))
    };
    vec![
        input(InputMode::Filter, None),
        input(InputMode::Jump, None),
        input(InputMode::New, Some("local")),
        input(InputMode::Logout, Some("local")),
        input(InputMode::LogoutKeys, Some("local")),
        Modal::Palette {
            query: String::new(),
            selected: 0,
            hover: None,
            open: false,
            decoder: crate::display::decode::KeyDecoder::new(),
        },
        Modal::Help {
            query: String::new(),
            scroll: 0,
            tab: None,
            hover: None,
            decoder: crate::display::decode::KeyDecoder::new(),
        },
        Modal::Check {
            selected: 0,
            hover: None,
            open: false,
        },
        Modal::History { scroll: 0 },
    ]
}

#[test]
fn every_prefix_surface_opens_where_the_key_list_opens() {
    let area = Rect::new(0, 0, 140, 38);
    let navs = [
        Some(NavPosition::Left),
        Some(NavPosition::Right),
        Some(NavPosition::Top),
        Some(NavPosition::Bottom),
        None,
    ];
    for position in navs {
        for (n, modal) in prefix_surfaces().into_iter().enumerate() {
            let mut state = crate::state::State::from_scan(two_groups());
            let switcher = Switcher::new(&mut state);
            let nav = match position {
                Some(p) => {
                    let nav = NavSize::visible(24).with_position(p);
                    if is_band(p) {
                        nav.with_height(BAND_H)
                    } else {
                        nav
                    }
                }
                None => NavSize::hidden(24),
            };
            state.chrome.set_nav_position(nav.position);
            state.modal = Some(modal);
            let plan = switcher.layout(area, nav, &state, &RenderPlan::default());
            let pop = plan.popup_rect;
            let r = plan.regions;
            assert!(!pop.is_empty(), "{position:?} #{n}");
            assert!(pop.right() <= area.right() && pop.bottom() <= area.bottom());
            match position {
                Some(NavPosition::Left) => {
                    assert_eq!((pop.x, pop.y), (r.terminal.x, r.terminal.y), "#{n}")
                }
                Some(NavPosition::Right) => {
                    assert_eq!(
                        (pop.right(), pop.y),
                        (r.terminal.right(), r.terminal.y),
                        "#{n}"
                    )
                }
                Some(NavPosition::Bottom) => {
                    assert_eq!((pop.x, pop.bottom()), (area.x, r.nav_border.y), "#{n}")
                }
                Some(NavPosition::Top) => {
                    assert_eq!((pop.x, pop.y), (area.x, r.nav_border.bottom()), "#{n}")
                }
                None => assert_eq!(
                    (pop.right(), pop.bottom()),
                    (area.right(), area.bottom()),
                    "#{n}"
                ),
            }
            if let Some(p) = position.filter(|p| is_band(*p)) {
                let card = plan
                    .nav_cells
                    .iter()
                    .find(|(i, _)| *i == switcher.selected)
                    .map(|(_, r)| *r)
                    .unwrap();
                assert!(
                    !card.intersects(pop),
                    "{p:?} #{n}: the selected card stays in view"
                );
            }
        }
    }
}

#[test]
fn an_input_popup_too_short_for_its_rows_keeps_its_field() {
    let mut state = crate::state::State::from_scan(two_groups());
    let switcher = Switcher::new(&mut state);
    for modal in prefix_surfaces().into_iter().take(5) {
        state.modal = Some(modal);
        let (_, lines) = switcher.input_popup_at(&state, 50, 1).expect("an input");
        assert_eq!(lines.len(), 1);
        assert!(crate::ui::modal::caret_offset(&lines[0]).is_some());
    }
}

/// The collapsed side column, cell by cell, at rest, armed, hovered, and under auto-hide:
/// the prefix keeps all three of its cells on the first row, the border runs down the
/// prefix's terminal-side column on every row below it, and the terminal view starts
/// on the next column.
/// A named chrome state to draw in, and the border glyph that state paints.
type ChromeCase = (&'static str, fn(&mut crate::state::State), &'static str);

#[test]
fn pl5_b_a_collapsed_column_is_the_prefix_with_the_border_on_its_edge() {
    let cases: [ChromeCase; 4] = [
        ("rest", |_| {}, "│"),
        ("armed", |s| s.chrome.armed = true, "│"),
        ("hovered", |s| s.chrome.nav_border_hovered = true, "┃"),
        ("auto-hide", |s| s.chrome.auto_hide = true, "║"),
    ];
    for position in [NavPosition::Left, NavPosition::Right] {
        for (name, set, glyph) in cases {
            let mut shot = Shot::new(two_groups(), collapsed_at(position), false);
            set(&mut shot.state);
            shot.draw(false);
            let r = shot.plan.regions;
            let (nav_x, edge_x, terminal_x) = match position {
                NavPosition::Left => (0, 2, 3),
                _ => (W - 3, W - 3, 0),
            };
            assert_eq!(
                r.prefix_hint,
                Rect::new(nav_x, 0, 3, 1),
                "{position:?} {name}"
            );
            assert_eq!(
                r.terminal,
                Rect::new(terminal_x, 0, W - 3, H),
                "{position:?} {name}"
            );
            assert_eq!(
                shot.row(0, nav_x, nav_x + 3),
                "C-g",
                "{position:?} {name}: the prefix keeps every cell, no padding"
            );
            for y in 1..H {
                assert_eq!(
                    shot.buf[(edge_x, y)].symbol(),
                    glyph,
                    "{position:?} {name}: the border on row {y}"
                );
            }
            let off_edge = if position == NavPosition::Left {
                0
            } else {
                W - 1
            };
            assert!(
                (1..H).all(|y| shot.buf[(off_edge, y)].symbol() == " "),
                "{position:?} {name}: the rest of the column is blank"
            );
        }
    }
}
