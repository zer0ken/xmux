//! The one look of the selection: on every surface that has one, the selected item's
//! cells are painted on the theme's accent with the theme's text-on-accent slot, and no
//! other item's are. The highlight keeps one cell of padding before and after the
//! standalone item's text inside its target area; a part of a shared
//! item has no padding. The harness paints the default
//! `auto-dark` theme: Black on LightGreen.

use super::tests_hierarchy::{fleet, landed, landing_link, session, H};
use super::*;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier};

const LOGGED_OUT: &str = crate::model::LOGGED_OUT;

impl H {
    /// Every cell of `rect` carries the selected look: Black on the LightGreen accent,
    /// neither reversed nor dimmed, so no colour of the surface survives inside the
    /// highlight.
    fn selected_look(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        !rect.is_empty()
            && (rect.x..rect.right()).all(|x| {
                let cell = &buf[(x, rect.y)];
                cell.bg == Color::LightGreen
                    && cell.fg == Color::Black
                    && !cell.modifier.intersects(Modifier::REVERSED | Modifier::DIM)
            })
    }

    /// The text inside `rect`, from its first to its last non-blank cell, is highlighted
    /// together with one cell of padding on each side.
    fn padded(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        let text: Vec<u16> = (rect.x..rect.right())
            .filter(|&x| buf[(x, rect.y)].symbol() != " ")
            .collect();
        let (Some(&first), Some(&last)) = (text.first(), text.last()) else {
            return false;
        };
        first > 0 && self.selected_look(Rect::new(first - 1, rect.y, last - first + 3, 1))
    }

    /// No cell of `rect` sits on the accent or is reversed.
    fn plain(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        !rect.is_empty()
            && (rect.x..rect.right()).all(|x| {
                let cell = &buf[(x, rect.y)];
                cell.bg != Color::LightGreen && !cell.modifier.contains(Modifier::REVERSED)
            })
    }

    /// The visible remainder of a popup entry and its right padding.
    fn row_in(&self, popup: Rect, text: &str) -> Rect {
        let at = self.find_in(popup, text);
        let buf = self.term.backend().buffer();
        let last = (at.x..popup.right() - 1)
            .rev()
            .find(|&x| buf[(x, at.y)].symbol() != " ")
            .unwrap();
        Rect::new(at.x, at.y, last + 2 - at.x, 1)
    }

    /// Where `text` is first painted inside `area`, reading rows top to bottom.
    fn find_in(&self, area: Rect, text: &str) -> Rect {
        let buf = self.term.backend().buffer();
        let needle: Vec<String> = text.chars().map(|c| c.to_string()).collect();
        let width = needle.len() as u16;
        for y in area.y..area.bottom() {
            for x in area.x..area.right().saturating_sub(width.saturating_sub(1)) {
                if needle
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c)
                {
                    return Rect::new(x, y, width, 1);
                }
            }
        }
        panic!("{text:?} is not painted in {area:?}");
    }

    fn screen(&self) -> Rect {
        let area = self.term.backend().buffer().area;
        Rect::new(0, 0, area.width, area.height)
    }

    fn draw_at(&mut self, nav: NavSize) {
        self.sw.sync_view_focus(self.terminal_focused);
        let previous = self.plan.clone();
        let (sw, state) = (&self.sw, &self.state);
        let focused = self.terminal_focused;
        let mut next = None;
        self.term
            .draw(|frame| {
                let plan = sw.layout(frame.area(), nav, state, &previous);
                sw.render(frame, None, focused, state, &plan);
                next = Some(plan);
            })
            .unwrap();
        self.plan = next.unwrap();
    }

    fn unpadded(&self, rect: Rect) -> bool {
        let area = self.term.backend().buffer().area;
        self.selected_look(rect)
            && (rect.x == area.x || self.plain(Rect::new(rect.x - 1, rect.y, 1, 1)))
            && (rect.right() == area.right() || self.plain(Rect::new(rect.right(), rect.y, 1, 1)))
    }
}

#[test]
fn a_selected_nav_card_is_highlighted_and_no_other_card_is() {
    let mut h = fleet();
    h.select("web", "api");
    let api = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "api"));
    let deploy = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "deploy"));
    assert!(h.selected_look(h.card(api)));
    assert!(
        h.padded(h.card(api)),
        "the indent cell and the cell after the name"
    );
    assert!(h.plain(h.card(deploy)));
}

#[test]
fn a_selected_section_title_half_is_highlighted_and_the_other_half_is_not() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    let title = h.title_row("web");
    let host = h.half(title, Part::Host);
    assert!(h.selected_look(host));
    assert!(h.plain(h.half(title, Part::Machine)));
    // The host half's highlight ends with its text.
    assert!(h.plain(Rect::new(host.right(), host.y, 1, 1)));
    assert!(h.plain(Rect::new(host.x - 1, host.y, 1, 1)));
}

#[test]
fn a_selected_empty_hosts_machine_half_has_no_highlight_padding() {
    let mut h = fleet();
    let idle = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "idle"));
    h.sw.set_selected(idle);
    h.ctrl(KeyCode::Up);
    let machine = h.half(idle, Part::Machine);
    assert!(h.selected_look(machine));
    assert!(h.plain(Rect::new(machine.x - 1, machine.y, 1, 1)));
    assert!(h.plain(Rect::new(machine.right(), machine.y, 1, 1)));
}

#[test]
fn hovering_a_shared_items_part_has_no_padding() {
    let mut h = fleet();
    h.select("web", "api");
    let title = h.title_row("web");
    let idle = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "idle"));
    for (row, part) in [(title, Part::Host), (idle, Part::Machine)] {
        let rect = h.half(row, part);
        h.sw.hover = Some((h.sw.rows[row].reference.clone(), part));
        h.draw();
        let buf = h.term.backend().buffer();
        for x in rect.x - 1..=rect.right() {
            assert_eq!(
                buf[(x, rect.y)].bg == crate::ui::palette::Palette::default().bar_bg,
                x >= rect.x && x < rect.right(),
                "hover at column {x} of {rect:?}"
            );
        }
    }
}

#[test]
fn hovering_a_standalone_card_pads_its_background() {
    let mut h = fleet();
    h.select("web", "api");
    let deploy = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "deploy"));
    let rect = h.card(deploy);
    h.sw.hover = Some((h.sw.rows[deploy].reference.clone(), Part::Card));
    h.draw();
    let buf = h.term.backend().buffer();
    let first = (rect.x..rect.right())
        .find(|&x| buf[(x, rect.y)].symbol() != " ")
        .unwrap();
    let last = (rect.x..rect.right())
        .rev()
        .find(|&x| buf[(x, rect.y)].symbol() != " ")
        .unwrap();
    for x in first - 1..=last + 1 {
        assert_eq!(
            buf[(x, rect.y)].bg,
            crate::ui::palette::Palette::default().bar_bg
        );
    }
}

#[test]
fn a_selected_screen_link_is_highlighted_and_no_other_link_is() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(h.selected_look(h.link_rect(0)));
    assert!(h.plain(h.link_rect(5)));
    h.sw.step_link(-1, &h.state);
    h.draw();
    assert!(h.selected_look(h.link_rect(5)));
    // The headline's machine link is part of the path and has no padding.
    let link = h.link_rect(5);
    assert!(h.selected_look(link));
    assert!(h.plain(Rect::new(link.x - 1, link.y, 1, 1)));
    assert!(h.plain(Rect::new(link.right(), link.y, 1, 1)));
    assert!(h.plain(h.link_rect(0)));
    // An action shares its row with its key, so only its words are highlighted.
    h.sw.step_link(-3, &h.state);
    h.draw();
    let action = h.link_rect(2);

    assert!(h.selected_look(action));
    assert!(h.unpadded(action));
    assert!(h.plain(h.link_rect(5)));
}

#[test]
fn a_selected_landing_link_is_highlighted_and_no_other_link_is() {
    let h = landed();
    assert!(h.selected_look(landing_link(&h, session("gpu", "train"))));
    assert!(h.unpadded(landing_link(&h, session("gpu", "train"))));
    assert!(h.plain(landing_link(&h, session("web", "api"))));
}

#[test]
fn the_selected_help_tab_is_highlighted_and_no_other_tab_is() {
    let mut h = fleet();
    h.sw.toggle_help(&mut h.state);
    h.draw();
    let titles: Vec<&str> = crate::model::keys::Section::ALL
        .iter()
        .map(|s| s.title())
        .collect();
    let popup = h.plan.popup_rect;
    assert!(h.selected_look(h.find_in(popup, titles[0])));
    assert!(h.padded(h.find_in(popup, titles[0])));
    assert!(h.plain(h.find_in(popup, titles[1])));
}

#[test]
fn the_selected_palette_entry_is_highlighted_and_no_other_entry_is() {
    let mut h = fleet();
    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let entries = h.sw.palette_entries(&h.state, "");
    let name = |i: usize| entries[i].0.chars().take(12).collect::<String>();
    let popup = h.plan.popup_rect;
    assert!(h.selected_look(h.row_in(popup, &name(0))));
    assert!(h.padded(h.find_in(popup, &name(0))));
    assert!(h.plain(h.find_in(popup, &name(1))));
}

#[test]
fn the_selected_check_row_is_highlighted_and_no_other_row_is() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db", &[], Some(LOGGED_OUT)),
        ("dead", &[], Some("connection refused")),
    ]);
    h.sw.toggle_check(&mut h.state);
    h.draw();
    let entries = h.sw.check_entries(&h.state);
    let popup = h.plan.popup_rect;
    assert!(h.selected_look(h.row_in(popup, &entries[0].host)));
    assert!(h.padded(h.find_in(popup, &entries[0].host)));
    assert!(h.plain(h.find_in(popup, &entries[1].host)));
}

/// The login pane of the logged-out machine `db`, taking keys, with `focus` on one stop.
fn login(focus: crate::state::LoginFocus) -> H {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &[], Some(LOGGED_OUT)),
    ]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    h.state.login = Some(crate::state::LoginDraft {
        host: "db:tmux".into(),
        address: "10.0.0.9".into(),
        port: "2222".into(),
        username: "alice".into(),
        focus,
        ..Default::default()
    });
    h.terminal_focused = true;
    h.draw();
    assert!(h.sw.login_pane_shown(&h.state));
    h
}

#[test]
fn the_focused_login_field_is_highlighted_and_no_other_field_is() {
    let h = login(crate::state::LoginFocus::Address);
    let screen = h.screen();
    let value = h.find_in(screen, "10.0.0.9");
    assert!(h.selected_look(value));
    // The value shares its row with a label; the caret is a separate input cell.
    assert!(h.plain(Rect::new(value.x - 1, value.y, 1, 1)));
    assert!(h.plain(h.find_in(screen, "alice")));
}

#[test]
fn the_focused_login_choice_is_highlighted_and_no_other_choice_is() {
    let h = login(crate::state::LoginFocus::AfterNothing);
    let screen = h.screen();
    assert!(h.selected_look(h.find_in(screen, "do nothing")));
    assert!(h.padded(h.find_in(screen, "(*) do nothing")));
    assert!(h.plain(h.find_in(screen, "register my public key")));
    assert!(h.plain(h.find_in(screen, "10.0.0.9")));
}

#[test]
fn the_focused_login_button_is_highlighted() {
    let h = login(crate::state::LoginFocus::Submit);
    let screen = h.screen();
    assert!(h.selected_look(h.find_in(screen, "Log in")));
    assert!(h.padded(h.find_in(screen, "[ Log in ]")));
    assert!(h.plain(h.find_in(screen, "do nothing")));
}

#[test]
fn no_surface_paints_a_selection_mark_glyph() {
    let mut h = fleet();
    h.select("web", "api");
    let nav = h.cells_text();
    assert!(!nav.contains('\u{276f}'), "{nav}");
    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let palette = h.cells_text();
    assert!(!palette.contains('\u{276f}'), "{palette}");
}

impl H {
    fn cells_text(&self) -> String {
        let buf = self.term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

impl H {
    /// Puts the open popup's selection on `index`: the help's tab, or the row of the
    /// palette or the machine problems.
    fn select_in_popup(&mut self, index: usize) {
        use crate::state::Modal;
        match self.state.modal.as_mut() {
            Some(Modal::Help { tab, .. }) => *tab = Some(index),
            Some(Modal::Palette { selected, .. } | Modal::Check { selected, .. }) => {
                *selected = index
            }
            _ => panic!("no popup with a selection is open"),
        }
        self.draw();
    }

    /// The characters painted in `rect`, row by row, without their styles.
    fn symbols(&self, rect: Rect) -> Vec<String> {
        let buf = self.term.backend().buffer();
        (rect.y..rect.bottom())
            .map(|y| {
                (rect.x..rect.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }
}

// Layout reserves the target area before painting: each test below paints one surface
// with the selection on one item
// and then on another, and the characters of the surface stay in the same cells.

#[test]
fn selecting_a_nav_card_or_a_title_half_moves_no_text() {
    // The Enter mark rides on the selection, so it is read as a blank cell here: what
    // must not move is the text of the cards.
    let text = |h: &H, nav: Rect| -> Vec<String> {
        h.symbols(nav)
            .into_iter()
            .map(|row| row.replace(super::render::ENTER_MARK, " "))
            .collect()
    };
    let mut h = fleet();
    h.select("web", "api");
    let nav = h.plan.nav_inner;
    let on_api = text(&h, nav);
    h.select("web", "deploy");
    assert_eq!(text(&h, nav), on_api, "a card selected");
    h.ctrl(KeyCode::Up);
    assert_eq!(text(&h, nav), on_api, "a title's host half selected");
    h.ctrl(KeyCode::Up);
    assert_eq!(text(&h, nav), on_api, "a title's machine half selected");
}

#[test]
fn selecting_a_screen_link_moves_no_text() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    let term = h.plan.regions.terminal;
    let first = h.symbols(term);
    h.sw.step_link(1, &h.state);
    h.draw();
    assert!(h.selected_look(h.link_rect(1)));
    assert_eq!(h.symbols(term), first);
    h.sw.step_link(1, &h.state);
    h.draw();
    assert!(h.selected_look(h.link_rect(2)), "an action link");
    assert_eq!(h.symbols(term), first);
}

#[test]
fn selecting_a_landing_link_moves_no_text() {
    let mut h = landed();
    let term = h.plan.regions.terminal;
    let first = h.symbols(term);
    h.key(KeyCode::Down);
    assert!(h.selected_look(landing_link(&h, session("web", "api"))));
    assert_eq!(h.symbols(term), first);
}

#[test]
fn selecting_a_help_tab_moves_no_text() {
    let mut h = fleet();
    h.sw.toggle_help(&mut h.state);
    h.draw();
    // From the second tab on, the row leads with `‹` and the tabs keep their columns.
    h.select_in_popup(1);
    let titles: Vec<&str> = crate::model::keys::Section::ALL
        .iter()
        .map(|s| s.title())
        .collect();
    let popup = h.plan.popup_rect;
    let row = h.find_in(popup, titles[2]).y;
    let tabs = Rect::new(popup.x, row, popup.width, 1);
    let second = h.symbols(tabs);
    h.select_in_popup(2);
    assert!(h.selected_look(h.find_in(popup, titles[2])));
    assert_eq!(h.symbols(tabs), second);
}

#[test]
fn selecting_a_palette_entry_or_a_check_row_moves_no_text() {
    let mut h = fleet();
    h.sw.toggle_palette(&mut h.state);
    h.draw();
    let popup = h.plan.popup_rect;
    let first = h.symbols(popup);
    h.select_in_popup(1);
    let entries = h.sw.palette_entries(&h.state, "");
    let name: String = entries[1].0.chars().take(12).collect();
    assert!(h.selected_look(h.find_in(popup, &name)));
    assert_eq!(h.symbols(h.plan.popup_rect), first, "the palette");

    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db", &[], Some(LOGGED_OUT)),
        ("dead", &[], Some("connection refused")),
    ]);
    h.sw.toggle_check(&mut h.state);
    h.draw();
    let entries = h.sw.check_entries(&h.state);
    let popup = h.plan.popup_rect;
    let first = h.symbols(popup);
    h.select_in_popup(1);
    assert!(h.selected_look(h.find_in(popup, &entries[1].host)));
    assert_eq!(h.symbols(h.plan.popup_rect), first, "the check list");
}

#[test]
fn focusing_a_login_stop_moves_no_text() {
    use crate::state::LoginFocus;
    let first = login(LoginFocus::Address);
    let term = first.plan.regions.terminal;
    let at_address = first.symbols(term);
    for focus in [
        LoginFocus::Username,
        LoginFocus::AfterNothing,
        LoginFocus::Submit,
    ] {
        let h = login(focus);
        assert_eq!(h.symbols(term), at_address, "{focus:?}");
    }
}

const NAV_POSITIONS: [NavPosition; 4] = [
    NavPosition::Left,
    NavPosition::Right,
    NavPosition::Top,
    NavPosition::Bottom,
];

fn nav_at(position: NavPosition) -> NavSize {
    NavSize::visible(30).with_position(position).with_height(6)
}

#[test]
fn shared_nav_parts_have_no_selection_or_hover_padding_at_every_position() {
    for position in NAV_POSITIONS {
        for target in 0..3 {
            let mut h = fleet();
            h.select("web", "api");
            h.draw_at(nav_at(position));
            let (row, part) = match target {
                0 => (h.title_row("web"), Part::Machine),
                1 => (h.title_row("web"), Part::Host),
                _ => (
                    h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "idle")),
                    Part::Machine,
                ),
            };
            let rect = h.half(row, part);
            h.sw.mouse_hover(&h.plan.clone(), rect.x, rect.y);
            h.draw_at(nav_at(position));
            let rect = h.half(row, part);
            let buf = h.term.backend().buffer();
            let left = rect.x.saturating_sub(1);
            let right = (rect.right() + 1).min(buf.area.right());
            for x in left..right {
                assert_eq!(
                    buf[(x, rect.y)].bg == h.sw.palette.bar_bg,
                    x >= rect.x && x < rect.right(),
                    "{position:?} {part:?} hover at {x} in {rect:?}"
                );
            }
            assert!(h.sw.mouse_select(&h.plan.clone(), rect.x, rect.y));
            h.sw.hover = None;
            h.draw_at(nav_at(position));
            assert!(h.unpadded(h.half(row, part)), "{position:?} {part:?}");
            let buf = h.term.backend().buffer();
            assert!((h.plan.nav_inner.x..h.plan.nav_inner.right())
                .all(|x| buf[(x, rect.y)].symbol() != super::render::ENTER_MARK));
        }
    }
}

#[test]
fn clipped_machine_selection_never_selects_the_host_or_the_whole_title() {
    for position in [NavPosition::Left, NavPosition::Right] {
        for width in 2..12 {
            let mut h = H::new(&[("web", &["api"], None)]);
            let title = h.title_row("web");
            let machine = h.half(title, Part::Machine);
            assert!(h.sw.mouse_select(&h.plan.clone(), machine.x, machine.y));
            h.draw_at(NavSize::visible(width).with_position(position));
            assert_eq!(h.sw.part, Part::Machine);
            let machine = h
                .plan
                .nav_parts
                .iter()
                .find(|(i, p, _)| *i == title && *p == Part::Machine)
                .map(|(_, _, rect)| *rect);
            let Some(card) = h
                .plan
                .nav_cells
                .iter()
                .find_map(|(i, rect)| (*i == title).then_some(*rect))
            else {
                continue;
            };
            let buf = h.term.backend().buffer();
            for x in card.x..card.right() {
                let in_machine = machine.is_some_and(|r| x >= r.x && x < r.right());
                assert_eq!(
                    buf[(x, card.y)].bg == Color::LightGreen,
                    in_machine,
                    "{position:?} width {width}, cell {x}, machine {machine:?}"
                );
                assert_ne!(buf[(x, card.y)].symbol(), super::render::ENTER_MARK);
            }
        }
    }
}

#[test]
fn hovering_the_selected_nav_card_adds_an_underline_at_every_position() {
    for position in NAV_POSITIONS {
        let mut h = H::new(&[("web", &["api"], None)]);
        h.select("web", "api");
        h.draw_at(nav_at(position));
        let row = h.card_row(|r| matches!(r, RowRef::Session { .. }));
        let card = h.card(row);
        h.sw.mouse_hover(&h.plan.clone(), card.x, card.y);
        h.draw_at(nav_at(position));
        let card = h.card(row);
        assert!(h.selected_look(card));
        let buf = h.term.backend().buffer();
        assert!((card.x..card.right())
            .all(|x| buf[(x, card.y)].modifier.contains(Modifier::UNDERLINED)));
    }
}

#[test]
fn overlapping_card_and_machine_hover_preserves_selection_at_every_position() {
    for position in NAV_POSITIONS {
        for select_machine in [false, true] {
            let mut h = H::new(&[("idle", &[], None)]);
            let row = h.card_row(|r| matches!(r, RowRef::Host { .. }));
            h.sw.set_selected(row);
            h.draw_at(nav_at(position));
            let machine = h.half(row, Part::Machine);
            if select_machine {
                assert!(h.sw.mouse_select(&h.plan.clone(), machine.x, machine.y));
                h.draw_at(nav_at(position));
                let card = h.card(row);
                h.sw.mouse_hover(&h.plan.clone(), card.x, card.y);
            } else {
                h.sw.mouse_hover(&h.plan.clone(), machine.x, machine.y);
            }
            h.draw_at(nav_at(position));
            let machine = h.half(row, Part::Machine);
            assert!(h.selected_look(machine), "{position:?}, {select_machine}");
            let buf = h.term.backend().buffer();
            assert!((machine.x..machine.right())
                .all(|x| buf[(x, machine.y)].modifier.contains(Modifier::UNDERLINED)));
        }
    }
}

#[test]
fn standalone_cards_reserve_exactly_one_inner_blank_at_every_position() {
    for position in NAV_POSITIONS {
        for groups in [
            vec![("web", &["api"][..], None)],
            vec![("idle", &[][..], None)],
            vec![("db", &[][..], Some(LOGGED_OUT))],
        ] {
            let mut h = H::new(&groups);
            let row = h.card_row(|r| !matches!(r, RowRef::Section { .. }));
            h.sw.set_selected(row);
            h.draw_at(nav_at(position));
            let card = h.card(row);
            let buf = h.term.backend().buffer();
            assert!(card.width >= 4, "{position:?}: {card:?}");
            assert_eq!(buf[(card.x, card.y)].symbol(), " ", "{position:?}");
            assert_ne!(buf[(card.x + 1, card.y)].symbol(), " ", "{position:?}");
            assert_eq!(
                buf[(card.right() - 1, card.y)].symbol(),
                " ",
                "{position:?}"
            );
            assert_ne!(
                buf[(card.right() - 2, card.y)].symbol(),
                " ",
                "{position:?}"
            );
            assert!(h.selected_look(card), "{position:?}: {card:?}");
            if card.right() < h.plan.nav_inner.right() {
                assert!(h.plain(Rect::new(card.right(), card.y, 1, 1)));
            }
        }
    }
}

#[test]
fn standalone_padding_and_enter_mark_are_pointer_targets() {
    for position in NAV_POSITIONS {
        let mut h = H::new(&[("web", &["api", "deploy"], None)]);
        h.select("web", "api");
        h.draw_at(nav_at(position));
        let row = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "api"));
        let card = h.card(row);
        let buf = h.term.backend().buffer();
        let enter = (card.x..card.right())
            .find(|&x| buf[(x, card.y)].symbol() == super::render::ENTER_MARK)
            .expect("the roomy selected card carries Enter");
        for x in [card.x, enter, card.right() - 1] {
            assert!(h.sw.mouse_select(&h.plan.clone(), x, card.y));
            assert_eq!(h.sw.selected_node(), session("web", "api"));
        }
    }
}

#[test]
fn standalone_state_words_preserve_enter_and_neighboring_cards() {
    for position in [NavPosition::Top, NavPosition::Bottom] {
        let mut h = H::new(&[("a", &[], None), ("b", &[], None)]);
        let first = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "a"));
        let second = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "b"));
        h.sw.set_selected(first);
        h.draw_at(nav_at(position).with_height(1));
        assert!(h.symbols(h.card(second))[0].contains("b/tmux"));
        assert!(h.plain(h.card(second)));

        let mut h = H::new(&[("db", &[], Some(LOGGED_OUT))]);
        let row = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
        h.sw.set_selected(row);
        h.draw_at(nav_at(position));
        let card = h.card(row);
        assert!(h.symbols(card)[0].contains(super::render::ENTER_MARK));
        assert!(h.selected_look(card));
    }
}

#[test]
fn enter_marks_preserve_wide_session_names_at_every_position() {
    for position in NAV_POSITIONS {
        let mut h = H::new(&[("web", &["作業"], None)]);
        h.select("web", "作業");
        h.draw_at(nav_at(position));
        let row = h.card_row(|r| matches!(r, RowRef::Session { .. }));
        let card = h.card(row);
        let buf = h.term.backend().buffer();
        let last = (card.x..card.right())
            .find(|&x| buf[(x, card.y)].symbol() == "業")
            .expect("the wide final character is intact");
        assert_eq!(buf[(last + 1, card.y)].symbol(), " ");
        let enter = (card.x..card.right())
            .find(|&x| buf[(x, card.y)].symbol() == super::render::ENTER_MARK)
            .expect("the roomy card carries Enter");
        assert!(enter > last + 1);
        assert_eq!(buf[(card.right() - 1, card.y)].symbol(), " ");
    }
}

#[test]
fn slashes_in_machine_names_do_not_split_the_machine_target() {
    for position in NAV_POSITIONS {
        let mut h = H::new(&[("prod/db", &["api"], None)]);
        h.draw_at(nav_at(position));
        let row = h.title_row("prod/db");
        let machine = h.half(row, Part::Machine);
        let host = h.half(row, Part::Host);
        assert_eq!(h.symbols(machine), vec!["prod/db"]);
        assert_eq!(h.symbols(host), vec!["tmux"]);
        assert!(h
            .sw
            .mouse_select(&h.plan.clone(), machine.right() - 1, machine.y));
        assert_eq!(
            h.sw.selected_node(),
            Some(crate::model::Node::Machine("prod/db".into()))
        );
    }
}

#[test]
fn filtering_preserves_graphemes_and_machine_target_geometry() {
    for position in NAV_POSITIONS {
        let mut h = H::new(&[("👩\u{200d}💻", &[], None)]);
        h.state.filter = "👩".into();
        h.draw_at(nav_at(position));
        let row = h.card_row(|r| matches!(r, RowRef::Host { .. }));
        let machine = h.half(row, Part::Machine);
        let buf = h.term.backend().buffer();
        assert_eq!(machine.width, 2);
        assert_eq!(buf[(machine.x, machine.y)].symbol(), "👩\u{200d}💻");
        assert_eq!(buf[(machine.right(), machine.y)].symbol(), "/");
    }
}

#[test]
fn enter_mark_does_not_shorten_a_name_that_fits_without_it() {
    let mut h = H::new(&[("web", &["abcde"], None)]);
    h.select("web", "abcde");
    h.draw_at(NavSize::visible(11).with_position(NavPosition::Left));
    let row = h.card_row(|r| matches!(r, RowRef::Session { .. }));
    let text = h.symbols(h.card(row))[0].clone();
    assert!(text.contains("abcde"), "{text:?}");
    assert!(!text.contains(super::render::ENTER_MARK), "{text:?}");
}
