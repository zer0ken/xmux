//! The machine / host / session hierarchy in the nav and the terminal view: the two halves
//! of a section title, the card step and the level step, the machine's and the host's
//! screens and their links, the soft selection under the pointer, and a machine none of
//! whose hosts connected standing as one card, and the landing screen above them all.

use super::*;
use crate::model::Node;
use crate::state::State;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

const LOGGED_OUT: &str = crate::model::LOGGED_OUT;

fn sess(host: &str, name: &str) -> Session {
    Session {
        host: host.into(),
        name: name.into(),
        windows: 1,
        ..Default::default()
    }
}

fn machine(machine: &str) -> Option<Node> {
    Some(Node::Machine(machine.into()))
}

fn host(id: &str) -> Option<Node> {
    Some(Node::Host(id.into()))
}

pub(super) fn session(id: &str, name: &str) -> Option<Node> {
    Some(Node::Session(Address::new(id, name)))
}

/// The mux a test host is reached through: the one its id names, else tmux.
fn mux_named(id: &str) -> &str {
    match crate::session::mux_of(id) {
        "" => "tmux",
        mux => mux,
    }
}

/// A switcher over `groups`, each `(host, sessions, failure)`, painted on a 140x30
/// screen with the nav on the left. Every host is reached over ssh through tmux, so its
/// title and its card read `{machine}/tmux` and its machine's screen states the ssh facts.
pub(super) struct H {
    pub(super) sw: Switcher,
    pub(super) state: State,
    pub(super) plan: RenderPlan,
    pub(super) term: Terminal<TestBackend>,
    pub(super) terminal_focused: bool,
}

impl H {
    pub(super) fn new(groups: &[(&str, &[&str], Option<&str>)]) -> Self {
        let groups: Vec<Group> = groups
            .iter()
            .map(|(id, names, err)| Group {
                host: id.to_string(),
                err: err.map(str::to_string),
                sessions: names.iter().map(|n| sess(id, n)).collect(),
            })
            .collect();
        let reach = groups
            .iter()
            .map(|g| {
                (
                    g.host.clone(),
                    crate::state::HostReach {
                        ssh: true,
                        kind: mux_named(&g.host).into(),
                        mux: mux_named(&g.host).into(),
                        refresh: "live".into(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let mut state = State::from_scan(Scan { groups });
        state.chrome.set_host_reach(reach);
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

    pub(super) fn draw(&mut self) {
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

    pub(super) fn key(&mut self, code: KeyCode) {
        self.press(code, KeyModifiers::NONE);
    }

    pub(super) fn ctrl(&mut self, code: KeyCode) {
        self.press(code, KeyModifiers::CONTROL);
    }

    fn node(&self) -> Option<Node> {
        self.sw.selected_node()
    }

    pub(super) fn select(&mut self, id: &str, name: &str) {
        self.sw.select_address(&Address::new(id, name));
        self.draw();
    }

    fn cells(&self, rect: Rect) -> String {
        let buf = self.term.backend().buffer();
        (rect.x..rect.right())
            .map(|x| buf[(x, rect.y)].symbol().to_string())
            .collect()
    }

    fn highlighted(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        (rect.x..rect.right()).all(|x| buf[(x, rect.y)].bg == Color::LightGreen)
    }

    /// Whether every cell of `rect` wears the soft selection's background.
    fn soft_selected(&self, rect: Rect) -> bool {
        let buf = self.term.backend().buffer();
        let bg = crate::ui::palette::soft_selection_style(&self.sw.palette).bg;
        bg.is_some() && (rect.x..rect.right()).all(|x| Some(buf[(x, rect.y)].bg) == bg)
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

    pub(super) fn title_row(&self, id: &str) -> usize {
        self.sw
            .rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Section { host } if host == id))
            .expect("the host has a title")
    }

    pub(super) fn card_row(&self, pick: impl Fn(&RowRef) -> bool) -> usize {
        self.sw
            .rows
            .iter()
            .position(|r| pick(&r.reference))
            .expect("the card is on the list")
    }

    pub(super) fn half(&self, row: usize, part: Part) -> Rect {
        self.plan
            .nav_parts
            .iter()
            .find(|(i, p, _)| *i == row && *p == part)
            .map(|(_, _, rect)| *rect)
            .expect("the row paints that half")
    }

    pub(super) fn card(&self, row: usize) -> Rect {
        self.plan
            .nav_cells
            .iter()
            .find(|(i, _)| *i == row)
            .map(|(_, rect)| *rect)
            .expect("the card is painted")
    }

    pub(super) fn link_rect(&self, index: usize) -> Rect {
        self.plan
            .view_links
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, rect)| *rect)
            .expect("the screen paints that link")
    }
}

/// `gpu` and `web` serve sessions, `idle` serves none, and `db` refused every login.
pub(super) fn fleet() -> H {
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
    let (machine_half, host_half) = (h.half(title, Part::Machine), h.half(title, Part::Host));
    assert_eq!(h.cells(machine_half), "web");
    assert_eq!(h.cells(host_half), "tmux");

    assert!(h
        .sw
        .mouse_select(&h.plan.clone(), machine_half.x, machine_half.y));
    h.draw();
    assert_eq!(h.node(), machine("web"));
    let (machine_half, host_half) = (h.half(title, Part::Machine), h.half(title, Part::Host));
    assert!(
        h.highlighted(machine_half),
        "the machine half is the selection"
    );
    assert!(!h.highlighted(host_half), "the host half is not");
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Machine)
    );

    assert!(h.sw.mouse_select(&h.plan.clone(), host_half.x, host_half.y));
    h.draw();
    assert_eq!(h.node(), host("web"));
    let (machine_half, host_half) = (h.half(title, Part::Machine), h.half(title, Part::Host));
    assert!(h.highlighted(host_half) && !h.highlighted(machine_half));
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Host));
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
            host("idle"),
            machine("db"),
            session("gpu", "train"),
        ],
        "titles are skipped; an empty host and a down machine each take a step"
    );
}

#[test]
fn the_card_step_from_a_title_half_goes_to_the_neighbouring_card() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up); // the host half of web's title
    h.key(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("web", "api"),
        "↓ reaches the first card under it"
    );
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up); // the machine half
    assert_eq!(h.node(), machine("web"));
    h.key(KeyCode::Up);
    assert_eq!(
        h.node(),
        session("gpu", "train"),
        "↑ reaches the card above it"
    );
}

#[test]
fn ctrl_up_walks_session_host_machine_and_ctrl_down_returns_where_it_came_from() {
    let mut h = fleet();
    h.select("web", "deploy");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("web"));
    assert_eq!(h.sw.part, Part::Host);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"));
    assert_eq!(
        (h.sw.selected, h.sw.part),
        (h.title_row("web"), Part::Machine),
        "the machine half of the same title"
    );
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"), "a machine is the top");
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), host("web"));
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
    let half = h.half(title, Part::Machine);
    h.sw.mouse_select(&h.plan.clone(), half.x, half.y);
    assert_eq!(h.node(), machine("box"));
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), host("box:tmux"), "hosts by name");
    h.ctrl(KeyCode::Down);
    assert_eq!(
        h.node(),
        session("box:tmux", "editor"),
        "sessions in card order"
    );
}

#[test]
fn a_host_card_reads_as_two_targets_too() {
    let mut h = fleet();
    let idle = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "idle"));
    h.sw.set_selected(idle);
    h.draw();
    assert_eq!(h.node(), host("idle"));
    let half = h.half(idle, Part::Machine);
    assert_eq!(h.cells(half), "idle");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("idle"));
    assert!(h.highlighted(h.half(idle, Part::Machine)));
    assert!(
        !h.highlighted(h.card(idle)),
        "only the machine half is the selection"
    );
    h.ctrl(KeyCode::Down);
    assert_eq!(h.node(), host("idle"));
}

#[test]
fn the_machine_screen_states_the_machine_and_links_its_hosts() {
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
    assert_eq!(h.node(), machine("box"));
    let view = h.view();
    for want in [
        "reachable",
        "address",
        "port",
        "user",
        "SSH login",
        "username and password",
        "rescan this machine",
        "log out of this machine",
        "hosts",
        "tmux  2 sessions",
        "zellij  no sessions",
    ] {
        assert!(
            view.contains(want),
            "the machine screen states {want:?}:\n{view}"
        );
    }
    assert!(!view.contains("start a new session"), "{view}");
}

#[test]
fn the_machine_and_host_screens_name_their_level_and_keep_to_its_facts() {
    let mut h = H::new(&[("box:tmux", &["notes"], None), ("box:zellij", &[], None)]);
    h.state
        .chrome
        .ssh_stanzas
        .insert("box".into(), "Host box\n    User dev".into());
    h.select("box:tmux", "notes");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("box:tmux"));
    let mux = h.view();
    assert_eq!(
        mux.lines().nth(1).map(str::trim_end),
        Some(" host box/tmux"),
        "{mux}"
    );
    for gone in ["ssh config", "Host box", "address", "SSH login"] {
        assert!(
            !mux.contains(gone),
            "the host screen omits {gone:?}:\n{mux}"
        );
    }
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("box"));
    let machine = h.view();
    assert_eq!(
        machine.lines().nth(1).map(str::trim_end),
        Some(" machine box"),
        "{machine}"
    );
    for want in ["ssh config", "Host box", "User dev", "hosts"] {
        assert!(
            machine.contains(want),
            "the machine screen states {want:?}:\n{machine}"
        );
    }
    for gone in ["start a new session", "last listed", "updates"] {
        assert!(
            !machine.contains(gone),
            "the machine screen omits {gone:?}:\n{machine}"
        );
    }
}

#[test]
fn the_host_screen_links_its_machine_and_its_sessions_and_leaves_the_login_to_the_machine() {
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
            "the host screen states {want:?}:\n{view}"
        );
    }
    for gone in ["SSH login", "log out of this machine", "public key"] {
        assert!(
            !view.contains(gone),
            "the machine states {gone:?}, not its host:\n{view}"
        );
    }
    assert_eq!(h.cells(h.link_rect(0)), "api");
    assert_eq!(h.cells(h.link_rect(1)), "deploy");
    assert_eq!(h.cells(h.link_rect(2)), "start a new session");
    assert_eq!(h.cells(h.link_rect(3)), "rescan this machine");
    assert_eq!(h.cells(h.link_rect(4)), "rescan all machines");
    assert_eq!(
        h.cells(h.link_rect(5)),
        "web",
        "the machine half of the path is the last link"
    );
}

#[test]
fn a_screen_reads_headline_status_children_then_actions() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    let view = h.view();
    let at = |want: &str| {
        view.find(want)
            .unwrap_or_else(|| panic!("the host screen states {want:?}:\n{view}"))
    };
    let order = [
        at("host web/tmux"),
        at("2 sessions"),
        at("updates"),
        at("sessions  api"),
        at("deploy"),
        at("start a new session"),
        at("rescan this machine"),
        at("rescan all machines"),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "headline, status, sessions, actions:\n{view}"
    );

    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"));
    let view = h.view();
    let at = |want: &str| {
        view.find(want)
            .unwrap_or_else(|| panic!("the machine screen states {want:?}:\n{view}"))
    };
    let order = [
        at("machine web"),
        at("hosts"),
        at("rescan this machine"),
        at("rescan all machines"),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "headline, status, hosts, actions:\n{view}"
    );
    let links = h.sw.screen_links(&Node::Machine("web".into()), &h.state);
    assert!(matches!(links[0].node(), Some(Node::Host(_))));
    assert!(
        links[1..].iter().all(|l| l.node().is_none()),
        "a machine screen's actions follow its hosts and nothing follows them"
    );
}

#[test]
fn screen_links_take_the_arrows_and_enter_in_the_terminal_view() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(
        h.highlighted(h.link_rect(0)),
        "a host screen starts on its first session"
    );
    h.sw.step_link(-1, &h.state);
    h.draw();
    assert!(
        h.highlighted(h.link_rect(5)) && !h.highlighted(h.link_rect(0)),
        "a step before the first link wraps to the machine link, the last stop"
    );
    h.sw.step_link(1, &h.state);
    h.draw();
    assert!(
        h.highlighted(h.link_rect(0)) && !h.highlighted(h.link_rect(5)),
        "a step past the last link wraps to the first"
    );
    for (step, want) in [
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
        (1, 5),
        (1, 0),
        (-1, 5),
        (-1, 4),
    ] {
        h.sw.step_link(step, &h.state);
        h.draw();
        assert!(
            h.highlighted(h.link_rect(want)),
            "the arrows cycle to {want}"
        );
    }
    h.sw.step_link(-3, &h.state);

    assert!(h.sw.open_selected_link(&h.state));
    h.draw();
    assert_eq!(h.node(), session("web", "deploy"));
    assert_eq!(
        h.sw.terminal_view_target().target,
        "deploy",
        "the opened session is what the terminal view shows"
    );

    // Up the path: the host screen, then its machine's, which selects the host it came
    // from among its links.
    h.ctrl(KeyCode::Up);
    let up =
        h.sw.screen_links(&Node::Host("web".into()), &h.state)
            .iter()
            .position(|l| l.node() == Some(&Node::Machine("web".into())))
            .unwrap();
    assert!(h.sw.open_link(up, &h.state));
    h.draw();
    assert_eq!(h.node(), machine("web"));
    let links = h.sw.screen_links(&Node::Machine("web".into()), &h.state);
    assert_eq!(links[h.sw.link].node(), Some(&Node::Host("web".into())));
    assert!(h.highlighted(h.link_rect(h.sw.link)));
}

#[test]
fn a_host_opened_from_its_machine_screen_starts_on_its_first_session() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"));
    let host =
        h.sw.screen_links(&Node::Machine("web".into()), &h.state)
            .iter()
            .position(|l| l.node() == Some(&Node::Host("web".into())))
            .unwrap();
    assert!(h.sw.open_link(host, &h.state));
    let links = h.sw.screen_links(&Node::Host("web".into()), &h.state);
    let start = h.sw.link_index(&links);
    assert!(matches!(links[start].node(), Some(Node::Session(_))));
    assert_eq!(start, 0);
}

#[test]
fn a_screen_without_children_starts_on_its_machine_link_and_a_machine_on_its_first_host() {
    let mut h = fleet();
    let idle = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "idle"));
    h.sw.set_selected(idle);
    h.terminal_focused = true;
    h.draw();
    assert_eq!(h.node(), host("idle"));
    let links = h.sw.screen_links(&Node::Host("idle".into()), &h.state);
    let up = links.len() - 1;
    assert_eq!(links[up].node(), Some(&Node::Machine("idle".into())));
    assert!(
        links[..up].iter().all(|l| l.node().is_none()),
        "an empty host lists its actions and no session"
    );
    assert!(
        h.highlighted(h.link_rect(up)),
        "a host with no session starts on its machine link"
    );
    h.sw.step_link(1, &h.state);
    h.draw();
    assert!(
        h.highlighted(h.link_rect(0)),
        "past the machine link the arrows wrap to the first action"
    );

    assert!(h.sw.open_link(up, &h.state));
    h.draw();
    assert_eq!(h.node(), machine("idle"));
    let links = h.sw.screen_links(&Node::Machine("idle".into()), &h.state);
    assert_eq!(
        links[h.sw.link_index(&links)].node(),
        Some(&Node::Host("idle".into())),
        "a step up preselects the host just left"
    );
    h.sw.step_link(-1, &h.state);
    h.draw();
    assert!(
        h.highlighted(h.link_rect(links.len() - 1)),
        "before the first host the arrows wrap to the last action"
    );
}

#[test]
fn a_link_is_drawn_selected_only_while_the_terminal_view_holds_the_focus() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    assert!(!h.highlighted(h.link_rect(0)), "the nav holds the focus");
    h.terminal_focused = true;
    h.draw();
    assert!(h.highlighted(h.link_rect(0)));
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
        h.soft_selected(h.card(deploy)),
        "the soft selection wears its background"
    );
    assert!(!h.highlighted(h.card(deploy)));

    let title = h.title_row("web");
    let half = h.half(title, Part::Machine);
    h.sw.mouse_hover(&h.plan.clone(), half.x, half.y);
    h.draw();
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Machine)
    );
    assert!(h.view().contains("hosts"), "{}", h.view());

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
fn a_hovered_link_wears_the_soft_selection_in_the_terminal_view() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    let rect = h.link_rect(2);
    assert!(h.sw.link_hover_at(&h.plan.clone(), rect.x, rect.y));
    h.draw();
    assert!(h.soft_selected(h.link_rect(2)));
    assert_eq!(h.node(), host("web"), "hovering a link opens nothing");
}

#[test]
fn a_logout_gathers_the_selection_onto_the_machines_one_card() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &["pg-primary"], None),
        ("db:zellij", &["scratch"], None),
    ]);
    h.select("db:tmux", "pg-primary");
    for id in ["db:tmux", "db:zellij"] {
        h.sw.apply_host_result(id.into(), vec![], Some(LOGGED_OUT.into()), &mut h.state);
    }
    h.draw();
    let machine_cards =
        h.sw.rows
            .iter()
            .filter(|r| matches!(r.reference, RowRef::Machine { .. }))
            .count();
    assert_eq!(machine_cards, 1, "one card for the machine, none per host");
    assert!(
        !h.sw
            .rows
            .iter()
            .any(|r| matches!(&r.reference, RowRef::Host { host, .. } if host.starts_with("db"))),
        "no host of the machine keeps a card"
    );
    assert_eq!(h.node(), machine("db"));
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    assert!(
        h.cells(h.card(card)).contains("logged out"),
        "the machine's card states the logout: {}",
        h.cells(h.card(card))
    );
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Login));
    assert!(h.sw.login_pane_shown(&h.state));
    let view = h.view();
    assert!(
        view.contains("Log in"),
        "the login form is on the machine's screen:\n{view}"
    );
    assert!(view.contains("tmux  logged out"), "{view}");
    assert!(
        view.lines().any(|l| l.trim() == "logged out"),
        "the machine screen states the logged-out state:\n{view}"
    );
    assert!(
        !view.contains("ssh failed"),
        "a logout is no failure:\n{view}"
    );
    assert!(
        !view.contains("failures"),
        "a logout starts no failure run:\n{view}"
    );
}

#[test]
fn a_machine_card_that_logs_back_in_keeps_the_selection_on_the_machine() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:zellij", &[], Some(LOGGED_OUT)),
        ("db:tmux", &[], Some(LOGGED_OUT)),
    ]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    assert_eq!(h.node(), machine("db"));
    h.sw.mark_machine_scanning("db", &mut h.state);
    assert!(
        !h.sw
            .rows
            .iter()
            .any(|r| matches!(r.reference, RowRef::Machine { .. })),
        "the machine's card gave way to its hosts' cards"
    );
    assert_eq!(h.node(), machine("db"), "the machine stays selected");
    for id in ["db:tmux", "db:zellij"] {
        h.sw.apply_host_result(id.into(), vec![sess(id, "work")], None, &mut h.state);
    }
    h.terminal_focused = true;
    h.draw();
    assert_eq!(h.node(), machine("db"));
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Machine)
    );
    let view = h.view();
    assert!(view.contains("machine db"), "{view}");
    assert!(view.contains("tmux  1 session"), "{view}");
    assert!(view.contains("zellij  1 session"), "{view}");
}

#[test]
fn an_unresolved_machine_screen_links_to_nothing() {
    let mut h = H::new(&[("gpu", &["train"], None), ("db", &[], Some(LOGGED_OUT))]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    h.terminal_focused = true;
    h.draw();
    assert_eq!(h.node(), machine("db"));
    assert!(
        h.sw.screen_links(&Node::Machine("db".into()), &h.state)
            .iter()
            .all(|l| l.node().is_none()),
        "the placeholder host stands for this machine, so it is no link"
    );
    let view = h.view();
    assert!(
        !view.lines().any(|l| l.trim_start().starts_with("hosts")),
        "no link list without a confirmed mux:\n{view}"
    );
    assert!(view.contains("Log in ]"), "{view}");
}

#[test]
fn the_login_form_keeps_the_keyboard_from_the_screen_links() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &[], Some(LOGGED_OUT)),
    ]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    h.terminal_focused = true;
    h.draw();
    assert!(h.sw.login_pane_shown(&h.state));
    assert_eq!(
        h.sw.link_marks(&h.state).0,
        None,
        "no link holds the hard selection beside the form"
    );
    h.sw.step_link(1, &h.state);
    assert!(
        !h.sw.open_selected_link(&h.state),
        "Enter belongs to the form"
    );
    assert_eq!(h.node(), machine("db"), "the keys moved nothing");
    assert!(h.sw.open_link(0, &h.state), "a click still opens the link");
    assert_eq!(h.node(), host("db:tmux"));
}

#[test]
fn a_link_opens_a_host_the_nav_has_no_card_for() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &[], Some(LOGGED_OUT)),
    ]);
    let card = h.card_row(|r| matches!(r, RowRef::Machine { .. }));
    h.sw.set_selected(card);
    h.terminal_focused = true;
    h.draw();
    assert!(
        h.view().contains("hosts"),
        "the machine's screen lists its hosts:\n{}",
        h.view()
    );
    let links = h.sw.screen_links(&Node::Machine("db".into()), &h.state);
    assert_eq!(links[0].node(), Some(&Node::Host("db:tmux".into())));
    assert!(h.sw.open_link(0, &h.state));
    h.draw();
    assert_eq!(
        h.node(),
        host("db:tmux"),
        "the host is selected without a card"
    );
    assert_eq!(h.sw.selected, card, "the nav stands on its machine's card");
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Login));
    assert!(
        !h.sw.login_pane_shown(&h.state),
        "a host states its failure and leaves the login to its machine"
    );
    let view = h.view();
    assert!(
        view.contains("logged out") && !view.contains("reason"),
        "a logout is the state itself, with no reason under it:\n{view}"
    );
    assert!(!view.contains("Log in ]"), "{view}");

    // The selection holds while the inventory lists the host, and lands on the host's
    // own card once it has one.
    h.sw.rebuild(&mut h.state);
    assert_eq!(h.node(), host("db:tmux"));
    h.sw.apply_host_result(
        "db:tmux".into(),
        vec![sess("db:tmux", "pg")],
        None,
        &mut h.state,
    );
    assert_eq!(h.node(), host("db:tmux"));
    assert!(h.sw.deep.is_none());
    assert_eq!(h.sw.part, Part::Host);
}

#[test]
fn a_selected_host_whose_machine_goes_down_goes_to_the_machines_card() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), host("web"));
    h.sw.apply_host_result(
        "web".into(),
        vec![],
        Some("connection refused".into()),
        &mut h.state,
    );
    assert_eq!(h.node(), machine("web"));
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
    let half = h.half(title, Part::Host);
    assert!(h.sw.mouse_select(&h.plan.clone(), half.x, half.y));
    assert_eq!(h.node(), host("gpu"));
}

#[test]
fn a_cancelled_jump_returns_to_the_half_of_the_title_it_started_on() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"));
    h.key(KeyCode::Char('1'));
    assert_eq!(h.node(), session("gpu", "train"), "the jump moved");
    h.key(KeyCode::Esc);
    assert_eq!(h.node(), machine("web"), "Esc returns to the machine half");
    assert_eq!(h.sw.part, Part::Machine);
}

#[test]
fn the_selected_link_follows_its_node_when_the_links_change() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    h.sw.step_link(1, &h.state);
    assert_eq!(h.sw.link, 1, "deploy, after api");

    // api ends: deploy is still the selected link, one place up.
    h.sw.apply_host_result(
        "web".into(),
        vec![sess("web", "deploy")],
        None,
        &mut h.state,
    );
    h.draw();
    assert_eq!(h.node(), host("web"));
    assert_eq!(h.sw.link, 0);
    assert!(h.highlighted(h.link_rect(0)));

    // deploy ends too: the selection stays on a link the screen still has.
    h.sw.apply_host_result("web".into(), vec![sess("web", "api")], None, &mut h.state);
    h.draw();
    assert_eq!(h.sw.link, 0);
    assert!(h.sw.open_selected_link(&h.state), "Enter opens a link");
    assert_eq!(h.node(), session("web", "api"));
}

#[test]
fn a_screen_scrolls_to_keep_the_selected_link_in_view() {
    let names: Vec<String> = (0..40).map(|i| format!("s{i:02}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut h = H::new(&[("web", &names, None)]);
    h.select("web", "s00");
    h.ctrl(KeyCode::Up);
    h.terminal_focused = true;
    h.draw();
    assert!(
        !h.plan.view_links.iter().any(|(i, _)| *i == 39),
        "the last session's link is below the screen at first"
    );
    h.sw.step_link(39, &h.state);
    h.draw();
    let last = h.link_rect(39);
    assert!(h.highlighted(last), "the selected link is on screen");
    assert_eq!(h.cells(last).trim_end(), "s39");
}

#[test]
fn the_section_step_from_a_title_part_goes_to_the_neighbouring_section() {
    let mut h = fleet();
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.ctrl(KeyCode::Up);
    assert_eq!(h.node(), machine("web"));
    h.key(KeyCode::Left);
    assert_eq!(
        h.node(),
        session("gpu", "train"),
        "← reaches the section above"
    );
    h.select("web", "api");
    h.ctrl(KeyCode::Up);
    h.key(KeyCode::Right);
    assert_eq!(h.node(), host("idle"), "→ reaches the host cards' section");
}

/// The fleet as it stands at launch: the landing screen up, nothing executed yet.
pub(super) fn landed() -> H {
    let mut h = fleet();
    h.sw.open_landing();
    h.draw();
    h
}

/// Where the landing list painted the link for `node`.
pub(super) fn landing_link(h: &H, node: Option<Node>) -> Rect {
    let i =
        h.sw.landing_links(&h.state)
            .iter()
            .position(|l| l.node() == node.as_ref())
            .expect("the landing lists the node");
    h.link_rect(i)
}

#[test]
fn the_landing_lists_every_card_in_nav_order_under_its_number() {
    let h = landed();
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Landing)
    );
    let cards: Vec<(usize, Node)> = (0..h.sw.rows.len())
        .filter(|&i| h.sw.rows[i].selectable())
        .map(|i| {
            (
                h.sw.card_number(i),
                node_of(&h.sw.rows[i].reference, Part::Card),
            )
        })
        .collect();
    let links = h.sw.landing_links(&h.state);
    assert_eq!(
        links
            .iter()
            .map(|l| (l.number.unwrap(), l.node().cloned().unwrap()))
            .collect::<Vec<_>>(),
        cards,
        "the same cards, order and numbers as the nav"
    );
    assert_eq!(
        links.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(),
        [
            "gpu/tmux/train",
            "web/tmux/api",
            "web/tmux/deploy",
            "idle/tmux",
            "db"
        ],
        "each card is written as its path, parted by `/` alone"
    );
    let view = h.view();
    assert!(view.contains(" xmux"), "{view}");
    assert!(view.contains("4 of 4 machines scanned"), "{view}");
    assert!(view.contains("1  gpu/tmux/train"), "{view}");
    assert!(view.contains("5  db  logged out"), "{view}");
}

#[test]
fn the_landing_states_the_scan_progress_with_the_spinner() {
    let mut h = landed();
    h.sw.mark_scanning("idle", &mut h.state);
    h.draw();
    let spinner = crate::ui::spinner_glyph(h.state.chrome.spinner_frame);
    assert!(
        h.view()
            .contains(&format!("{spinner} 3 of 4 machines scanned")),
        "{}",
        h.view()
    );
}

#[test]
fn the_landing_names_a_newer_release_and_the_command_that_installs_it() {
    let mut h = landed();
    assert!(!h.view().contains("xmux update"), "{}", h.view());
    h.state.chrome.update_available = Some("99.0.0".into());
    h.draw();
    let view = h.view();
    let line = view
        .lines()
        .find(|l| l.contains("99.0.0"))
        .unwrap_or_else(|| panic!("{view}"));
    assert!(line.contains("update"), "{line}");
    assert!(line.contains(env!("CARGO_PKG_VERSION")), "{line}");
    assert!(line.contains("`xmux update`"), "{line}");
}

#[test]
fn the_prefix_key_list_names_a_newer_release_beside_the_version() {
    let screen = |h: &H| {
        let buf = h.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let mut h = landed();
    h.state.chrome.armed = true;
    h.draw();
    let version = format!("xmux v{}", env!("CARGO_PKG_VERSION"));
    let row = screen(&h)
        .into_iter()
        .find(|l| l.contains(&version))
        .expect("the key list writes the version");
    assert!(!row.contains("available"), "{row}");
    h.state.chrome.update_available = Some("99.0.0".into());
    h.draw();
    let row = screen(&h)
        .into_iter()
        .find(|l| l.contains(&version))
        .expect("the key list writes the version");
    assert!(
        row.contains(&format!("{version} · v99.0.0 available: xmux update")),
        "{row}"
    );
}

#[test]
fn the_landing_and_the_nav_share_one_selection_that_attaches_nothing() {
    let mut h = landed();
    assert_eq!(h.node(), session("gpu", "train"));
    assert!(h.highlighted(landing_link(&h, session("gpu", "train"))));
    assert_eq!(h.sw.terminal_view_target().target, "");

    h.key(KeyCode::Down);
    assert_eq!(h.node(), session("web", "api"));
    assert!(
        h.highlighted(landing_link(&h, session("web", "api"))),
        "the landing marks the card the nav moved to"
    );
    assert!(!h.highlighted(landing_link(&h, session("gpu", "train"))));
    assert_eq!(
        h.sw.terminal_view_target().target,
        "",
        "the selection highlights and attaches nothing"
    );

    // A nav hover does not preview while the landing is up either.
    let deploy = h.card_row(|r| matches!(r, RowRef::Session { sess } if sess.name == "deploy"));
    let rect = h.card(deploy);
    h.sw.mouse_hover(&h.plan.clone(), rect.x, rect.y);
    h.draw();
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Landing)
    );
    assert_eq!(h.sw.terminal_view_target().target, "");
}

#[test]
fn a_landing_link_takes_the_pointer_from_the_navs_focus() {
    let mut h = landed();
    let rect = landing_link(&h, session("web", "deploy"));
    assert!(h.sw.link_hover_at(&h.plan.clone(), rect.x, rect.y));
    h.draw();
    assert!(
        h.soft_selected(landing_link(&h, session("web", "deploy"))),
        "the soft selection wears its background and survives the frame's focus sync"
    );
    assert_eq!(
        h.node(),
        session("gpu", "train"),
        "hovering selects nothing"
    );
}

#[test]
fn opening_a_landing_link_executes_it_and_the_landing_never_returns() {
    let mut h = landed();
    let i =
        h.sw.landing_links(&h.state)
            .iter()
            .position(|l| l.node() == Some(&Node::Session(Address::new("web", "deploy"))))
            .unwrap();
    assert!(h.sw.open_link(i, &h.state));
    h.draw();
    assert!(!h.sw.landing_open());
    assert_eq!(h.node(), session("web", "deploy"));
    assert_eq!(h.sw.terminal_view_target().target, "deploy");

    h.key(KeyCode::Up);
    h.sw.request_rescan(&mut h.state);
    h.draw();
    assert_ne!(
        h.sw.current_view_screen(&h.state),
        Some(ViewScreen::Landing)
    );
}

#[test]
fn a_landing_link_to_a_machine_that_needs_a_login_opens_its_login_screen() {
    let mut h = landed();
    let i =
        h.sw.landing_links(&h.state)
            .iter()
            .position(|l| l.node() == Some(&Node::Machine("db".into())))
            .unwrap();
    assert!(h.sw.open_link(i, &h.state));
    h.draw();
    assert_eq!(h.node(), machine("db"));
    assert_eq!(h.sw.current_view_screen(&h.state), Some(ViewScreen::Login));
}

#[test]
fn a_landed_jump_closes_the_landing_and_a_cancelled_one_does_not() {
    let mut h = landed();
    h.key(KeyCode::Char('3'));
    assert_eq!(h.node(), session("web", "deploy"));
    h.key(KeyCode::Esc);
    assert!(h.sw.landing_open(), "a cancelled jump executes nothing");

    h.key(KeyCode::Char('3'));
    h.key(KeyCode::Enter);
    assert!(!h.sw.landing_open());
    assert_eq!(h.sw.terminal_view_target().target, "deploy");
}

#[test]
fn a_switch_closes_the_landing_even_onto_the_card_already_selected() {
    let mut h = landed();
    assert_eq!(h.node(), session("gpu", "train"));
    h.sw.select_address(&Address::new("nowhere", "x"));
    assert!(h.sw.landing_open(), "a switch to no card executes nothing");
    h.select("gpu", "train");
    assert!(!h.sw.landing_open());
    assert_eq!(h.sw.terminal_view_target().target, "train");
}

#[test]
fn the_landing_counts_a_machine_serving_several_muxes_once() {
    let mut h = H::new(&[
        ("gpu", &["train"], None),
        ("db:tmux", &["pg-primary"], None),
        ("db:zellij", &["scratch"], None),
    ]);
    h.sw.open_landing();
    assert_eq!(
        h.state.chrome.scan_progress(&h.state),
        "2 of 2 machines scanned"
    );
    h.sw.mark_scanning("db:zellij", &mut h.state);
    let spinner = crate::ui::spinner_glyph(h.state.chrome.spinner_frame);
    assert_eq!(
        h.state.chrome.scan_progress(&h.state),
        format!("{spinner} 1 of 2 machines scanned"),
        "a machine is scanned only once every host of it answered"
    );
}

/// The harness over `groups` plus `machine`, which is on the roster with no host known,
/// in the state its answer left it: `Some` why it could not be asked, `None` still asking.
fn with_unresolved(
    groups: &[(&str, &[&str], Option<&str>)],
    machine: &str,
    err: Option<&str>,
) -> H {
    let mut h = H::new(groups);
    h.state.add_machine(machine.to_string());
    if let Some(err) = err {
        h.sw.apply_machine_result(machine, Some(err.to_string()), &mut h.state);
    } else {
        h.sw.rebuild(&mut h.state);
    }
    h.draw();
    h
}

const REFUSED: &str = "dev@db: Permission denied (publickey,password).";

#[test]
fn a_machine_with_no_host_known_is_a_machine_and_no_host() {
    let mut h = with_unresolved(&[("gpu", &["train"], None)], "db", Some(REFUSED));
    assert!(
        h.state.groups.iter().all(|g| g.host != "db"),
        "no host stands for the machine"
    );
    let card = h.card_row(
        |r| matches!(r, RowRef::Machine { machine, blocked: true, scanning: false, .. } if machine == "db"),
    );
    h.sw.set_selected(card);
    h.terminal_focused = true;
    h.draw();
    assert_eq!(h.node(), machine("db"), "the selection names the machine");
    assert!(
        h.sw.screen_links(&Node::Machine("db".into()), &h.state)
            .iter()
            .all(|l| l.node().is_none()),
        "a machine with no confirmed host links nowhere, least of all to itself"
    );
    let view = h.view();
    assert!(
        view.contains("Log in ]"),
        "its login pane answers it:\n{view}"
    );
    assert!(
        h.sw.login_pane_shown(&h.state),
        "the login pane takes the keys"
    );
    assert_eq!(
        h.sw.link_marks(&h.state).0,
        None,
        "no link holds the keyboard on the login pane"
    );
    assert!(
        h.sw.check_entries(&h.state)
            .iter()
            .any(|e| e.host == "db" && e.kind == crate::model::FailureKind::Blocked),
        "the machine problems list the machine"
    );
}

#[test]
fn a_machine_still_answering_spins_on_its_own_card() {
    let h = with_unresolved(&[("gpu", &["train"], None)], "win", None);
    let row = h.card_row(
        |r| matches!(r, RowRef::Machine { machine, scanning: true, .. } if machine == "win"),
    );
    let landing = h.sw.landing_links(&h.state);
    let link = landing
        .iter()
        .find(|l| l.node() == Some(&Node::Machine("win".into())))
        .expect("the landing lists the machine");
    assert_eq!(link.value, "scanning");
    assert!(h.state.scanning_any());
    assert!(row > 0);
}

#[test]
fn the_first_host_found_takes_the_machines_card_number() {
    let mut h = with_unresolved(&[("gpu", &["train"], None)], "win", None);
    h.sw.set_renumbering(false, &mut h.state);
    let row = h.card_row(|r| matches!(r, RowRef::Machine { machine, .. } if machine == "win"));
    let number = h.sw.card_number(row);
    h.sw.add_hosts(vec!["win".into()], &mut h.state);
    let row = h.card_row(|r| matches!(r, RowRef::Host { host, .. } if host == "win"));
    assert_eq!(h.sw.card_number(row), number, "the card keeps its number");
    assert!(!h.state.machine_scanning.contains("win"));
}

#[test]
fn a_machine_that_serves_no_mux_leaves_the_nav() {
    let mut h = with_unresolved(&[("gpu", &["train"], None)], "win", None);
    h.sw.settle_muxless("win", &mut h.state);
    assert!(!h
        .sw
        .rows
        .iter()
        .any(|r| matches!(&r.reference, RowRef::Machine { machine, .. } if machine == "win")));
    assert!(!h.state.scanning_any(), "nothing is still asking");
    // A failure it reports later is shown again.
    h.sw.apply_machine_result(
        "win",
        Some("ssh: connect to host win: timed out".into()),
        &mut h.state,
    );
    assert!(h
        .sw
        .rows
        .iter()
        .any(|r| matches!(&r.reference, RowRef::Machine { machine, .. } if machine == "win")));
}

#[test]
fn a_filter_keeps_a_machine_by_its_name() {
    let mut h = with_unresolved(&[("gpu", &["train"], None)], "win", None);
    h.state.filter = "wi".into();
    h.sw.rebuild(&mut h.state);
    let machines: Vec<_> =
        h.sw.rows
            .iter()
            .filter(|r| matches!(&r.reference, RowRef::Machine { .. }))
            .collect();
    assert_eq!(machines.len(), 1);
    assert!(
        !h.sw
            .rows
            .iter()
            .any(|r| matches!(&r.reference, RowRef::Session { .. })),
        "gpu's session does not match"
    );
}

#[test]
fn a_scanned_machine_keeps_its_screen_and_reads_scanning() {
    let mut h = fleet();
    h.sw.select_node(Node::Machine("web".into()));
    h.draw();
    let before = h.view();
    h.sw.mark_machine_scanning("web", &mut h.state);
    h.sw.select_node(Node::Machine("web".into()));
    h.draw();
    assert_eq!(
        h.sw.current_view_screen(&h.state),
        Some(crate::model::ViewScreen::Machine)
    );
    let during = h.view();
    assert!(during.contains(" scanning"), "{during}");
    assert!(!during.contains(" reachable"), "{during}");
    for row in [
        "address     web",
        "port        22",
        "ssh config  (no matching entry)",
        "rescan this machine",
        "log out of this machine",
    ] {
        assert!(before.contains(row), "{before}");
        assert!(
            during.contains(row),
            "{row} stays through the scan:
{during}"
        );
    }
}

#[test]
fn the_selected_card_ends_in_the_enter_mark_while_the_nav_holds_the_focus() {
    let enter = super::render::ENTER_MARK;
    let nav_rows = |h: &H| {
        let buf = h.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..NAV_WIDTH)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let mut h = fleet();
    h.select("web", "api");
    let marked: Vec<_> = nav_rows(&h)
        .into_iter()
        .filter(|r| r.contains(enter))
        .collect();
    assert_eq!(marked.len(), 1, "one card carries the mark: {marked:?}");
    assert!(
        marked[0].trim_end().ends_with(&format!("api {enter}")),
        "{marked:?}"
    );
    h.select("web", "deploy");
    let marked: Vec<_> = nav_rows(&h)
        .into_iter()
        .filter(|r| r.contains(enter))
        .collect();
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(
        marked[0].contains("deploy"),
        "the mark follows the selection"
    );
    h.ctrl(KeyCode::Up);
    assert!(
        nav_rows(&h).iter().all(|r| !r.contains(enter)),
        "a title half carries no mark"
    );
    h.sw.select_node(Node::Machine("db".into()));
    h.draw();
    assert!(
        nav_rows(&h).iter().any(|r| r.contains(enter)),
        "a standalone machine card carries the mark"
    );
    h.sw.select_node(Node::Host("idle".into()));
    h.draw();
    assert!(
        nav_rows(&h).iter().any(|r| r.contains(enter)),
        "a standalone host card carries the mark"
    );
    h.ctrl(KeyCode::Up);
    assert!(
        nav_rows(&h).iter().all(|r| !r.contains(enter)),
        "the machine part of a host card carries no mark"
    );
    h.select("web", "deploy");
    h.terminal_focused = true;
    h.draw();
    assert!(
        nav_rows(&h).iter().all(|r| !r.contains(enter)),
        "Enter reaches the pane, so no card carries the mark"
    );
}

#[test]
fn a_resize_box_is_framed_as_a_popup_and_carries_no_version() {
    let rows = |h: &H| {
        let buf = h.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let mut h = fleet();
    h.state.chrome.armed = true;
    h.state.chrome.resizing = true;
    h.state.chrome.update_available = Some("99.0.0".into());
    h.draw();
    let screen = rows(&h);
    let top = screen
        .iter()
        .find(|r| r.contains("╭"))
        .expect("the resize box opens");
    let bottom = screen
        .iter()
        .find(|r| r.contains("╰"))
        .expect("the resize box closes");
    assert!(top.contains("╭ resize ─"), "{top}");
    assert!(bottom.contains(" any other key end ╯"), "{bottom}");
    assert!(
        !screen
            .iter()
            .any(|r| r.contains("xmux v") || r.contains("99.0.0")),
        "the resize box names no version"
    );
}
