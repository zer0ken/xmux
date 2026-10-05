//! The host / source / session hierarchy in the nav and the terminal view: the two halves
//! of a section title, the card step and the level step, the host's and the source's
//! screens and their links, the soft selection under the pointer, and a host none of
//! whose sources connected standing as one card.

use super::*;
use crate::model::Node;
use crate::state::State;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;

const LOGGED_OUT: &str = "logged out; log in again or re-scan";

fn sess(source: &str, name: &str) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        windows: 1,
        ..Default::default()
    }
}

fn host(machine: &str) -> Option<Node> {
    Some(Node::Host(machine.into()))
}

fn source(id: &str) -> Option<Node> {
    Some(Node::Source(id.into()))
}

fn session(id: &str, name: &str) -> Option<Node> {
    Some(Node::Session(Address::new(id, name)))
}

/// The mux a test source is reached through: the one its id names, else tmux.
fn mux_named(id: &str) -> &str {
    match crate::session::mux_of(id) {
        "" => "tmux",
        mux => mux,
    }
}

/// A switcher over `groups`, each `(source, sessions, failure)`, painted on a 140x30
/// screen with the nav on the left. Every source is reached over ssh through tmux, so its
/// title and its card read `{host}/tmux` and its host's screen states the ssh facts.
struct H {
    sw: Switcher,
    state: State,
    plan: RenderPlan,
    term: Terminal<TestBackend>,
    terminal_focused: bool,
}

impl H {
    fn new(groups: &[(&str, &[&str], Option<&str>)]) -> Self {
        let groups: Vec<Group> = groups
            .iter()
            .map(|(id, names, err)| Group {
                source: id.to_string(),
                err: err.map(str::to_string),
                sessions: names.iter().map(|n| sess(id, n)).collect(),
            })
            .collect();
        let reach = groups
            .iter()
            .map(|g| {
                (
                    g.source.clone(),
                    crate::state::SourceReach {
                        ssh: true,
                        kind: mux_named(&g.source).into(),
                        mux: mux_named(&g.source).into(),
                        refresh: "live updates".into(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let mut state = State::from_scan(Scan { groups });
        state.chrome.set_source_reach(reach);
        let sw = Switcher::new(&mut state);
        let mut h = H {
            sw,
            state,
            plan: RenderPlan::default(),
            term: Terminal::new(TestBackend::new(140, 30)).unwrap(),
            terminal_focused: false,
        };
        h.draw();
        h
    }

    fn draw(&mut self) {
        self.sw.sync_view_focus(self.terminal_focused);
        let (sw, state, previous) = (&self.sw, &self.state, self.plan.clone());
        let focused = self.terminal_focused;
        let mut next = None;
        self.term
            .draw(|f| {
                let nav = super::tests_support::auto_nav(NAV_WIDTH, f.area());
                let plan = sw.layout(f.area(), nav, state, &previous);
                sw.render(f, None, focused, state, &plan);
                next = Some(plan);
            })
            .unwrap();
        self.plan = next.unwrap();
    }

    fn press(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.sw
            .handle_key(KeyEvent::new(code, modifiers), &mut self.state);
        self.draw();
    }

    fn key(&mut self, code: KeyCode) {
        self.press(code, KeyModifiers::NONE);
    }

    fn ctrl(&mut self, code: KeyCode) {
        self.press(code, KeyModifiers::CONTROL);
    }

    fn node(&self) -> Option<Node> {
        self.sw.selected_node()
    }

    fn select(&mut self, id: &str, name: &str) {
        self.sw.select_address(&Address::new(id, name));
        self.draw();
    }

    fn cells(&self, rect: Rect) -> String {
        let buf = self.term.backend().buffer();
        (rect.x..rect.right())
            .map(|x| buf[(x, rect.y)].symbol().to_string())
            .collect()
    }

    fn reversed(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        (rect.x..rect.right()).all(|x| buf[(x, rect.y)].modifier.contains(Modifier::REVERSED))
    }

    fn underlined(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        (rect.x..rect.right()).all(|x| buf[(x, rect.y)].modifier.contains(Modifier::UNDERLINED))
    }

    fn view(&self) -> String {
        let buf = self.term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in NAV_WIDTH + 1..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn title_row(&self, id: &str) -> usize {
        self.sw
            .rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Section { source } if source == id))
            .expect("the source has a title")
    }

    fn card_row(&self, pick: impl Fn(&RowRef) -> bool) -> usize {
        self.sw
            .rows
            .iter()
            .position(|r| pick(&r.reference))
            .expect("the card is on the list")
    }

    fn half(&self, row: usize, part: Part) -> Rect {
        self.plan
            .nav_parts
            .iter()
            .find(|(i, p, _)| *i == row && *p == part)
            .map(|(_, _, rect)| *rect)
            .expect("the row paints that half")
    }

    fn card(&self, row: usize) -> Rect {
        self.plan
            .nav_cells
            .iter()
            .find(|(i, _)| *i == row)
            .map(|(_, rect)| *rect)
            .expect("the card is painted")
    }

    fn link_rect(&self, index: usize) -> Rect {
        self.plan
            .view_links
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, rect)| *rect)
            .expect("the screen paints that link")
    }
}

/// `gpu` and `web` serve sessions, `idle` serves none, and `db` refused every login.
fn fleet() -> H {
    H::new(&[
        ("gpu", &["train"], None),
        ("web", &["api", "deploy"], None),
        ("idle", &[], None),
        ("db", &[], Some(LOGGED_OUT)),
    ])
}

#[test]
fn a_section_title_is_two_targets_and_paints_only_the_selected_half() {
    let mut h = fleet();
    let title = h.title_row("web");
    let (host_half, source_half) = (h.half(title, Part::Host), h.half(title, Part::Source));
    assert_eq!(h.cells(host_half), "web");
    assert_eq!(h.cells(source_half), "tmux");

    assert!(h.sw.mouse_select(&h.plan.clone(), host_half.x, host_half.y));
    h.draw();
    assert_eq!(h.node(), host("web"));
    let (host_half, source_half) = (h.half(title, Part::Host), h.half(title, Part::Source));
    assert!(h.reversed(host_half), "the host half is the selection");
    assert!(!h.reversed(source_half), "the source half is not");
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Host));

    assert!(h
        .sw
        .mouse_select(&h.plan.clone(), source_half.x, source_half.y));
    h.draw();
    assert_eq!(h.node(), source("web"));
    let (host_half, source_half) = (h.half(title, Part::Host), h.half(title, Part::Source));
    assert!(h.reversed(source_half) && !h.reversed(host_half));
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::HostInfo)
    );
}

#[test]
fn the_card_step_never_stops_on_a_title_and_does_stop_on_cards_without_sessions() {
    let mut h = fleet();
    h.key(KeyCode::Home);
    let mut seen = vec![h.node()];
    for _ in 0..5 {
        h.key(KeyCode::Down);
        seen.push(h.node());
    }
    assert_eq!(
        seen,
        vec![
            session("gpu", "train"),
            session("web", "api"),
            session("web", "deploy"),
            source("idle"),
            host("db"),
            session("gpu", "train"),
        ],
        "titles are skipped; an empty source and a down host each take a step"
    );
}

#[test]
fn the_card_step_from_a_title_half_goes_to_the_neighbouring_card() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up); // the source half of web's title
    h.key(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("web", "api"),
        "↓ reaches the first card under it"
    );
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up); // the host half
    assert_eq!(h.node(), host("web"));
    h.key(KeyCode::Up);
    assert_eq!(
        h.node(),
        session("gpu", "train"),
        "↑ reaches the card above it"
    );
}

#[test]
fn ctrl_up_walks_session_source_host_and_ctrl_down_returns_where_it_came_from() {
    let mut h = fleet();
    h.select("web", "deploy");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), source("web"));
    assert_eq!(h.sw.part, Part::Source);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("web"));
    assert_eq!(
        (h.sw.selected, h.sw.part),
        (h.title_row("web"), Part::Host),
        "the host half of the same title"
    );
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("web"), "a host is the top");
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), source("web"));
    h.ctrl(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("web", "deploy"),
        "the session the walk came up from, not the first one"
    );
    h.ctrl(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("web", "deploy"),
        "a session is the bottom"
    );
}

#[test]
fn ctrl_down_without_a_trail_takes_the_first_child_by_name() {
    let mut h = H::new(&[
        ("box:zellij", &["scratch"], None),
        ("box:tmux", &["notes", "editor"], None),
    ]);
    let title = h.title_row("box:zellij");
    let half = h.half(title, Part::Host);
    h.sw.mouse_select(&h.plan.clone(), half.x, half.y);
    assert_eq!(h.node(), host("box"));
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), source("box:tmux"), "sources by name");
    h.ctrl(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("box:tmux", "editor"),
        "sessions in card order"
    );
}

#[test]
fn a_source_card_reads_as_two_targets_too() {
    let mut h = fleet();
    let idle = h.card_row(|r| matches!(r, RowRef::Host { source, .. } if source == "idle"));
    h.sw.set_selected(idle);
    h.draw();
    assert_eq!(h.node(), source("idle"));
    let half = h.half(idle, Part::Host);
    assert_eq!(h.cells(half), "idle");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("idle"));
    assert!(h.reversed(h.half(idle, Part::Host)));
    assert!(
        !h.reversed(h.card(idle)),
        "only the host half is the selection"
    );
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), source("idle"));
}

#[test]
fn the_host_screen_states_the_machine_and_links_its_sources() {
    let mut h = H::new(&[
        ("box:tmux", &["notes", "editor"], None),
        ("box:zellij", &[], None),
    ]);
    h.state
        .auth_methods
        .insert("box".into(), crate::model::AuthMethod::Password);
    h.select("box:tmux", "editor");
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("box"));
    let view = h.view();
    for want in [
        "reachable",
        "address",
        "port",
        "user",
        "SSH login",
        "username and password",
        "re-scan this host",
        "log out of this host",
        "sources",
        "tmux  2 sessions",
        "zellij  no sessions",
    ] {
        assert!(
            view.contains(want),
            "the host screen states {want:?}:\n{view}"
        );
    }
    assert!(!view.contains("start a new session"), "{view}");
}

#[test]
fn the_source_screen_links_its_host_and_its_sessions_and_leaves_the_login_to_the_host() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    let view = h.view();
    assert!(view.contains("web/tmux"), "{view}");
    for want in [
        "sessions",
        "updates",
        "start a new session",
        "api",
        "deploy",
    ] {
        assert!(
            view.contains(want),
            "the source screen states {want:?}:\n{view}"
        );
    }
    for gone in ["SSH login", "log out of this host", "public key"] {
        assert!(
            !view.contains(gone),
            "the host states {gone:?}, not its source:\n{view}"
        );
    }
    assert_eq!(
        h.cells(h.link_rect(0)),
        "web",
        "the host half of the path is a link"
    );
    assert_eq!(h.cells(h.link_rect(1)), "api");
    assert_eq!(h.cells(h.link_rect(2)), "deploy");
}

#[test]
fn screen_links_take_the_arrows_and_enter_in_the_terminal_view() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(
        h.reversed(h.link_rect(0)),
        "the first link is the hard selection"
    );
    h.sw.step_link(1, &h.state);
    h.sw.step_link(1, &h.state);
    h.draw();
    assert!(h.reversed(h.link_rect(2)) && !h.reversed(h.link_rect(0)));
    h.sw.step_link(5, &h.state);
    h.draw();
    assert!(
        h.reversed(h.link_rect(2)),
        "the arrows stop at the last link"
    );

    assert!(h.sw.open_selected_link(&h.state));
    h.draw();
    assert_eq!(h.node(), session("web", "deploy"));
    assert_eq!(
        h.sw.terminal_view_target().target,
        "deploy",
        "the opened session is what the terminal view shows"
    );

    // Up the path: the source screen, then its host's, which selects the source it came
    // from among its links.
    h.ctrl(KeyCode::Up);
    assert!(h.sw.open_link(0, &h.state));
    h.draw();
    assert_eq!(h.node(), host("web"));
    let links = h.sw.screen_links(&Node::Host("web".into()), &h.state);
    assert_eq!(links[h.sw.link].node, Node::Source("web".into()));
    assert!(h.reversed(h.link_rect(h.sw.link)));
}

#[test]
fn a_link_is_drawn_selected_only_while_the_terminal_view_holds_the_focus() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    assert!(!h.reversed(h.link_rect(0)), "the nav holds the focus");
    h.terminal_focused = true;
    h.draw();
    assert!(h.reversed(h.link_rect(0)));
}

#[test]
fn hovering_a_nav_target_shows_its_screen_without_moving_the_hard_selection() {
    let mut h = fleet();
    h.select("gpu", "train");
    let deploy = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "deploy"));
    let rect = h.card(deploy);
    assert!(h.sw.mouse_hover(&h.plan.clone(), rect.x, rect.y));
    h.draw();
    assert_eq!(
        h.node(),
        session("gpu", "train"),
        "the hard selection stays"
    );
    assert_eq!(
        h.sw.terminal_view_target().target,
        "deploy",
        "the hovered card's session is shown"
    );
    assert!(
        h.underlined(h.card(deploy)),
        "the soft selection is underlined"
    );
    assert!(!h.reversed(h.card(deploy)));

    let title = h.title_row("web");
    let half = h.half(title, Part::Host);
    h.sw.mouse_hover(&h.plan.clone(), half.x, half.y);
    h.draw();
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Host));
    assert!(h.view().contains("sources"), "{}", h.view());

    // Off every target the hard selection's screen comes back.
    assert!(h.sw.mouse_hover(&h.plan.clone(), 100, 10));
    h.draw();
    assert_eq!(h.sw.terminal_view_target().target, "train");
    assert_eq!(h.sw.current_view_screen(&h.state), None);
}

#[test]
fn the_nav_takes_no_soft_selection_while_the_terminal_view_holds_the_focus() {
    let mut h = fleet();
    h.select("gpu", "train");
    h.terminal_focused = true;
    h.draw();
    let deploy = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "deploy"));
    let rect = h.card(deploy);
    assert!(!h.sw.mouse_hover(&h.plan.clone(), rect.x, rect.y));
    assert_eq!(h.sw.terminal_view_target().target, "train");
}

#[test]
fn a_hovered_link_is_underlined_in_the_terminal_view() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    let rect = h.link_rect(2);
    assert!(h.sw.link_hover_at(&h.plan.clone(), rect.x, rect.y));
    h.draw();
    assert!(h.underlined(h.link_rect(2)));
    assert_eq!(h.node(), source("web"), "hovering a link opens nothing");
}

#[test]
fn a_logout_gathers_the_selection_onto_the_hosts_one_card() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &["pg-primary"], None),
        ("db:zellij", &["scratch"], None),
    ]);
    h.select("db:tmux", "pg-primary");
    for id in ["db:tmux", "db:zellij"] {
        h.sw.apply_source_result(id.into(), vec![], Some(LOGGED_OUT.into()), &mut h.state);
    }
    h.draw();
    let machine_cards =
        h.sw.rows
            .iter()
            .filter(|r| matches!(r.reference, RowRef::Machine { .. }))
            .count();
    assert_eq!(machine_cards, 1, "one card for the host, none per source");
    assert!(
        !h.sw.rows.iter().any(
            |r| matches!(&r.reference, RowRef::Host { source, .. } if source.starts_with("db"))
        ),
        "no source of the host keeps a card"
    );
    assert_eq!(h.node(), host("db"));
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Login));
    assert!(h.sw.login_pane_shown(&h.state));
    let view = h.view();
    assert!(
        view.contains("Log in"),
        "the login form is on the host's screen:\n{view}"
    );
    assert!(view.contains("tmux  login needed"), "{view}");
}

#[test]
fn a_host_card_that_logs_back_in_hands_the_selection_to_its_first_source() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:zellij", &[], Some(LOGGED_OUT)),
        ("db:tmux", &[], Some(LOGGED_OUT)),
    ]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    assert_eq!(h.node(), host("db"));
    h.sw.mark_machine_scanning("db", &mut h.state);
    assert_eq!(h.node(), source("db:tmux"), "the first source by name");
}

#[test]
fn a_link_opens_a_source_the_nav_has_no_card_for() {
    let mut h = H::new(&[("gpu", &["train"], None), ("db", &[], Some(LOGGED_OUT))]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    h.terminal_focused = true;
    h.draw();
    assert!(
        !h.sw.login_pane_shown(&h.state) || h.view().contains("sources"),
        "the host's screen lists its sources"
    );
    let links = h.sw.screen_links(&Node::Host("db".into()), &h.state);
    assert_eq!(links[0].node, Node::Source("db".into()));
    assert!(h.sw.open_link(0, &h.state));
    h.draw();
    assert_eq!(
        h.node(),
        source("db"),
        "the source is selected without a card"
    );
    assert_eq!(h.sw.selected, card, "the nav stands on its host's card");
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Login));
    assert!(
        !h.sw.login_pane_shown(&h.state),
        "a source states its failure and leaves the login to its host"
    );
    let view = h.view();
    assert!(
        view.contains("login needed") && view.contains("reason"),
        "{view}"
    );
    assert!(!view.contains("Log in ]"), "{view}");

    // The selection holds while the inventory lists the source, and lands on the source's
    // own card once it has one.
    h.sw.rebuild(&mut h.state);
    assert_eq!(h.node(), source("db"));
    h.sw.apply_source_result("db".into(), vec![sess("db", "pg")], None, &mut h.state);
    assert_eq!(h.node(), source("db"));
    assert!(h.sw.deep.is_none());
    assert_eq!(h.sw.part, Part::Source);
}

#[test]
fn a_selected_source_whose_host_goes_down_goes_to_the_hosts_card() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), source("web"));
    h.sw.apply_source_result(
        "web".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    assert_eq!(h.node(), host("web"));
    assert!(matches!(h.sw.current_ref(), Some(RowRef::Machine { .. })));
}

#[test]
fn a_click_on_a_nav_target_selects_it_and_reports_the_hit() {
    let mut h = fleet();
    let title = h.title_row("gpu");
    let rect = h.card(title);
    let blank = rect.right() - 1;
    assert!(
        !h.sw.mouse_select(&h.plan.clone(), blank, rect.y),
        "a title's blank tail is no target"
    );
    let half = h.half(title, Part::Source);
    assert!(h.sw.mouse_select(&h.plan.clone(), half.x, half.y));
    assert_eq!(h.node(), source("gpu"));
}
