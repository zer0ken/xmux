//! The one look of the hard selection: on every surface that has one, the selected item's
//! cells are painted on the theme's accent with the theme's text-on-accent slot, and no
//! other item's are. The harness paints the default `auto-dark` theme: Black on
//! LightGreen.

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
    assert!(h.plain(h.card(deploy)));
}

#[test]
fn a_selected_section_title_half_is_highlighted_and_the_other_half_is_not() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    let title = h.title_row("web");
    assert!(h.selected_look(h.half(title, Part::Host)));
    assert!(h.plain(h.half(title, Part::Machine)));
}

#[test]
fn a_selected_screen_link_is_highlighted_and_no_other_link_is() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(h.selected_look(h.link_rect(0)));
    assert!(h.plain(h.link_rect(1)));
}

#[test]
fn a_selected_landing_link_is_highlighted_and_no_other_link_is() {
    let h = landed();
    assert!(h.selected_look(landing_link(&h, session("gpu", "train"))));
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
    assert!(h.plain(h.find_in(screen, "alice")));
    // The caret cell after the value is the accent pair swapped, so it reads inside the
    // highlight without a colour of its own.
    let caret = &h.term.backend().buffer()[(value.right(), value.y)];
    assert_eq!((caret.fg, caret.bg), (Color::Black, Color::LightGreen));
    assert!(caret.modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_focused_login_choice_is_highlighted_and_no_other_choice_is() {
    let h = login(crate::state::LoginFocus::AfterNothing);
    let screen = h.screen();
    assert!(h.selected_look(h.find_in(screen, "do nothing")));
    assert!(h.plain(h.find_in(screen, "register my public key")));
    assert!(h.plain(h.find_in(screen, "10.0.0.9")));
}

#[test]
fn the_focused_login_button_is_highlighted() {
    let h = login(crate::state::LoginFocus::Submit);
    let screen = h.screen();
    assert!(h.selected_look(h.find_in(screen, "Log in")));
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
