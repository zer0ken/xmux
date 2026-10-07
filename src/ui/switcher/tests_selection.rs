//! The one look of the hard selection: on every surface that has one, the selected item's
//! cells are painted on the theme's accent with the theme's text-on-accent slot, and no
//! other item's are. The highlight keeps one cell of padding before and after the
//! item's text wherever the layout leaves that cell blank. The harness paints the default
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

    /// From where `text` is painted in `popup` to the popup's right border: the whole
    /// row an entry's selection covers, its key, its words, and its padding.
    fn row_in(&self, popup: Rect, text: &str) -> Rect {
        let at = self.find_in(popup, text);
        Rect::new(at.x, at.y, popup.right() - 1 - at.x, 1)
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
    // The `/` before the host half is the title's own text, so the padding takes only
    // the blank cell after it.
    assert!(h.selected_look(Rect::new(host.right(), host.y, 1, 1)));
    assert!(h.plain(Rect::new(host.x - 1, host.y, 1, 1)));
}

#[test]
fn a_selected_screen_link_is_highlighted_and_no_other_link_is() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(h.selected_look(h.link_rect(0)));
    // The headline's machine link is followed by the `/` of its host, the headline's own
    // text, so only the cell before it takes the padding.
    let link = h.link_rect(0);
    assert!(h.selected_look(Rect::new(link.x - 1, link.y, link.width + 1, 1)));
    assert!(h.plain(Rect::new(link.right(), link.y, 1, 1)));
    assert!(h.plain(h.link_rect(1)));
}

#[test]
fn a_selected_landing_link_is_highlighted_and_no_other_link_is() {
    let h = landed();
    assert!(h.selected_look(landing_link(&h, session("gpu", "train"))));
    assert!(h.padded(landing_link(&h, session("gpu", "train"))));
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
    // The cell after the value is the caret, where the terminal's cursor stands, and it
    // is the highlight's right padding.
    assert!(h.padded(value));
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
    /// Puts the open popup's hard selection on `index`: the help's tab, or the row of the
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

// The highlight grows outward into the blank cells beside the text and never moves the
// text to make room: each test below paints one surface with the selection on one item
// and then on another, and the characters of the surface stay in the same cells.

#[test]
fn selecting_a_nav_card_or_a_title_half_moves_no_text() {
    let mut h = fleet();
    h.select("web", "api");
    let nav = h.plan.nav_inner;
    let on_api = h.symbols(nav);
    h.select("web", "deploy");
    assert_eq!(h.symbols(nav), on_api, "a card selected");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.symbols(nav), on_api, "a title's host half selected");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.symbols(nav), on_api, "a title's machine half selected");
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
    h.sw.link = 1;
    h.draw();
    assert!(h.selected_look(h.link_rect(1)));
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
