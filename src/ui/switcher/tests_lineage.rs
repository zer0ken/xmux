//! The two selection rules over every path that changes the list: a card that
//! disappears moves the selection along its lineage, and a card that appears takes the
//! selection only when it is the user's interest.

use super::*;
use crate::model::OpResult;
use crate::state::State;

fn sess(host: &str, name: &str) -> Session {
    Session {
        host: host.into(),
        name: name.into(),
        windows: 1,
        ..Default::default()
    }
}

fn launch(hosts: &[&str]) -> (Switcher, State) {
    let mut state = State::from_hosts(hosts.iter().map(|s| s.to_string()).collect());
    let sw = Switcher::from_hosts(&mut state);
    (sw, state)
}

fn answer(sw: &mut Switcher, state: &mut State, host: &str, names: &[&str]) {
    let sessions = names.iter().map(|n| sess(host, n)).collect();
    sw.apply_host_result(host.into(), sessions, None, state);
}

fn fail(sw: &mut Switcher, state: &mut State, host: &str, reason: &str) {
    sw.apply_host_result(host.into(), Vec::new(), Some(reason.into()), state);
}

/// The selected card as a message for a failed assertion.
fn picked(sw: &Switcher) -> String {
    match sw.selected_card() {
        Some(RowRef::Session { sess }) => format!("session {}", sess.address().display()),
        Some(RowRef::Section { host }) => format!("section {host}"),
        Some(RowRef::Host { host, .. }) => format!("host {host}"),
        Some(RowRef::Machine { machine, .. }) => format!("machine {machine}"),
        None => "nothing".into(),
    }
}

fn on_session(sw: &Switcher, host: &str, name: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Session { sess }) if sess.host == host && sess.name == name)
}

fn on_section(sw: &Switcher, host: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Section { host: s }) if s == host)
}

fn on_host(sw: &Switcher, host: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Host { host: s, .. }) if s == host)
}

/// Whether the selection is on the one card of a machine that is down.
fn on_machine(sw: &Switcher, machine: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Machine { machine: m, .. }) if m == machine)
}

fn machine(machine: &str) -> Option<Node> {
    Some(Node::Machine(machine.into()))
}

/// The issue's walk up to the point the user is on the unreachable machine card: `local`
/// lists a session (the launch preselect lands there), `mars`, whose muxes are not known
/// yet, cannot be reached, and the user selects mars's card.
fn on_unreachable_machine() -> (Switcher, State) {
    let mut state = State::from_roster(vec!["local".into()], vec!["local".into(), "mars".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    answer(&mut sw, &mut state, "local", &["work"]);
    sw.apply_machine_result("mars", Some("connection refused".into()), &mut state);
    sw.open_host("mars", &mut state);
    assert!(on_machine(&sw, "mars"), "{}", picked(&sw));
    (sw, state)
}

/// The machine answers with two muxes: each becomes a host of its own, and the card
/// that stood for the machine goes as they join the list.
fn resolve_into_two_muxes(sw: &mut Switcher, state: &mut State) {
    sw.add_hosts(vec!["mars:tmux".into(), "mars:screen".into()], state);
}

#[test]
fn a_resolved_machine_card_keeps_the_selection_on_its_machine() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    assert_eq!(sw.selected_node(), machine("mars"), "{}", picked(&sw));
    assert!(
        on_host(&sw, "mars:screen"),
        "the machine half of its first card"
    );
    // The hosts answer with sessions; the machine keeps the selection on the machine half
    // of its first title, and its screen links both hosts rather than showing one of them.
    answer(&mut sw, &mut state, "mars:tmux", &["t1"]);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    assert_eq!(sw.selected_node(), machine("mars"), "{}", picked(&sw));
    assert!(on_section(&sw, "mars:screen"), "{}", picked(&sw));
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::Machine));
    assert_eq!(
        sw.screen_links(&Node::Machine("mars".into()), &state)
            .into_iter()
            .filter_map(|l| l.node().cloned())
            .collect::<Vec<_>>(),
        vec![
            Node::Host("mars:screen".into()),
            Node::Host("mars:tmux".into())
        ]
    );
    assert!(sw.current_attach_target(&state).is_none());
}

#[test]
fn a_created_session_takes_the_selection_and_its_end_returns_it_to_the_section() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    assert!(on_section(&sw, "mars:screen"));

    sw.note_create("mars:screen");
    sw.apply_op_result(
        OpResult::Created {
            session: sess("mars:screen", "fresh"),
        },
        &mut state,
    );
    assert!(on_session(&sw, "mars:screen", "fresh"));

    // The session ends: the next listing lacks it, and the selection goes to the section.
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    assert!(on_section(&sw, "mars:screen"), "{}", picked(&sw));
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::Host));
}

#[test]
fn the_last_session_ending_leaves_the_selection_on_its_host_card() {
    let (mut sw, mut state) = launch(&["local"]);
    answer(&mut sw, &mut state, "local", &["only"]);
    assert!(on_session(&sw, "local", "only"));
    answer(&mut sw, &mut state, "local", &[]);
    assert!(on_host(&sw, "local"), "{}", picked(&sw));
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::Empty));
}

#[test]
fn a_vanished_host_goes_to_its_machine() {
    let (mut sw, mut state) = launch(&["prod", "prod:zellij", "zeta"]);
    answer(&mut sw, &mut state, "prod", &["a"]);
    answer(&mut sw, &mut state, "prod:zellij", &["z"]);
    answer(&mut sw, &mut state, "zeta", &["q"]);
    assert!(sw.select_address(&Address::new("prod:zellij", "z")));
    sw.remove_host("prod:zellij", &mut state);
    assert_eq!(sw.selected_node(), machine("prod"));
    assert!(
        on_section(&sw, "prod"),
        "the machine half of its remaining title"
    );
}

#[test]
fn a_vanished_host_goes_to_its_machine_on_the_machines_remaining_title() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    answer(&mut sw, &mut state, "mars:tmux", &["t1"]);
    assert!(on_section(&sw, "mars:screen"));
    sw.remove_host("mars:screen", &mut state);
    assert_eq!(sw.selected_node(), machine("mars"));
    assert!(on_section(&sw, "mars:tmux"), "{}", picked(&sw));
}

/// Whether the selection names nothing and the terminal view shows the landing list,
/// attaching nothing.
fn names_nothing(sw: &Switcher, state: &State) -> bool {
    sw.selected_node().is_none()
        && sw.selection_row().is_none()
        && sw.current_view_screen(state) == Some(ViewScreen::Landing)
        && sw.current_attach_target(state).is_none()
}

#[test]
fn a_removed_machine_leaves_the_selection_naming_nothing() {
    let (mut sw, mut state) = launch(&["alpha", "beta", "gamma"]);
    for (machine, name) in [("alpha", "a"), ("beta", "b"), ("gamma", "g")] {
        answer(&mut sw, &mut state, machine, &[name]);
    }
    sw.select_address(&Address::new("beta", "b"));
    assert!(on_session(&sw, "beta", "b"));
    sw.remove_host("beta", &mut state);
    assert!(names_nothing(&sw, &state), "{}", picked(&sw));
    // The roster names the machine again and it answers: a background return takes
    // nothing back.
    sw.add_host("beta".into(), &mut state);
    answer(&mut sw, &mut state, "beta", &["b"]);
    assert!(names_nothing(&sw, &state), "{}", picked(&sw));
    // The next arrow key starts on the card standing where the lost card stood.
    sw.move_selection(1);
    assert!(on_session(&sw, "gamma", "g"), "{}", picked(&sw));
}

#[test]
fn a_machine_serving_no_mux_leaves_the_selection_naming_nothing() {
    let mut state = State::from_roster(vec!["local".into()], vec!["local".into(), "mars".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    answer(&mut sw, &mut state, "local", &["work"]);
    sw.open_host("mars", &mut state);
    assert_eq!(sw.selected_node(), machine("mars"));
    sw.settle_muxless("mars", &mut state);
    assert!(names_nothing(&sw, &state), "{}", picked(&sw));
}

#[test]
fn a_logout_moves_the_selection_to_the_machines_own_card() {
    let (mut sw, mut state) = launch(&["local", "prod"]);
    answer(&mut sw, &mut state, "local", &["work"]);
    answer(&mut sw, &mut state, "prod", &["api"]);
    assert!(sw.select_address(&Address::new("prod", "api")));
    fail(
        &mut sw,
        &mut state,
        "prod",
        "SSH password no longer held; log in again",
    );
    assert!(on_machine(&sw, "prod"), "{}", picked(&sw));
    assert_eq!(sw.selected_node(), machine("prod"));
    assert!(sw.current_attach_target(&state).is_none());
}

#[test]
fn a_filter_hiding_the_whole_machine_leaves_the_selection_naming_nothing() {
    let (mut sw, mut state) = launch(&["alpha", "beta", "gamma"]);
    answer(&mut sw, &mut state, "alpha", &["red"]);
    answer(&mut sw, &mut state, "beta", &["blue"]);
    answer(&mut sw, &mut state, "gamma", &["green"]);
    sw.select_address(&Address::new("beta", "blue"));
    assert!(on_session(&sw, "beta", "blue"));
    state.filter = "re".into(); // red and green match, blue does not
    sw.rebuild(&mut state);
    assert!(names_nothing(&sw, &state), "{}", picked(&sw));
    answer(&mut sw, &mut state, "beta", &["blue", "red2"]);
    assert!(
        names_nothing(&sw, &state),
        "an answer listing the machine again moves nothing: {}",
        picked(&sw)
    );
    // Clearing the filter is the user's own action, and it lists the node the user was
    // on again, so the selection returns to it.
    state.filter.clear();
    sw.rebuild(&mut state);
    assert!(on_session(&sw, "beta", "blue"), "{}", picked(&sw));
}

#[test]
fn a_rescan_waits_on_the_host_card_and_returns_only_to_the_awaited_session() {
    let (mut sw, mut state) = launch(&["alpha", "beta"]);
    answer(&mut sw, &mut state, "alpha", &["a"]);
    answer(&mut sw, &mut state, "beta", &["b"]);
    assert!(sw.select_address(&Address::new("beta", "b")));
    sw.request_rescan(&mut state);
    assert!(on_host(&sw, "beta"), "{}", picked(&sw));
    // Another host answering first is unrelated to the interest.
    answer(&mut sw, &mut state, "alpha", &["a"]);
    assert!(on_host(&sw, "beta"));
    answer(&mut sw, &mut state, "beta", &["b"]);
    assert!(on_session(&sw, "beta", "b"));
}

#[test]
fn a_rescan_whose_session_did_not_return_stays_on_the_section() {
    let (mut sw, mut state) = launch(&["alpha"]);
    answer(&mut sw, &mut state, "alpha", &["a", "gone"]);
    assert!(sw.select_address(&Address::new("alpha", "gone")));
    sw.request_rescan(&mut state);
    answer(&mut sw, &mut state, "alpha", &["a"]);
    assert!(on_section(&sw, "alpha"), "{}", picked(&sw));
    // The interest ended with the answer, so the name coming back later is a new card
    // unrelated to it.
    answer(&mut sw, &mut state, "alpha", &["a", "gone"]);
    assert!(on_section(&sw, "alpha"));
}

#[test]
fn a_preselected_session_that_ends_during_the_scan_holds_its_host_card() {
    let (mut sw, mut state) = launch(&["alpha", "beta"]);
    answer(&mut sw, &mut state, "beta", &["b"]);
    assert!(on_session(&sw, "beta", "b"), "the launch preselect");
    answer(&mut sw, &mut state, "beta", &[]);
    assert!(on_host(&sw, "beta"));
    answer(&mut sw, &mut state, "alpha", &["a"]);
    assert!(on_host(&sw, "beta"), "a later answer does not take it");
}

#[test]
fn a_rescanned_session_the_filter_hides_is_still_awaited() {
    let (mut sw, mut state) = launch(&["prod"]);
    answer(&mut sw, &mut state, "prod", &["work", "edit"]);
    assert!(on_session(&sw, "prod", "edit"), "the launch preselect");
    sw.request_rescan(&mut state);
    state.filter = "work".into();
    sw.rebuild(&mut state);
    answer(&mut sw, &mut state, "prod", &["work", "edit"]);
    assert!(on_section(&sw, "prod"), "{}", picked(&sw));
    state.filter.clear();
    sw.rebuild(&mut state);
    assert!(on_session(&sw, "prod", "edit"), "{}", picked(&sw));
}

#[test]
fn a_created_session_the_filter_hides_takes_the_selection_once_shown() {
    let (mut sw, mut state) = launch(&["prod"]);
    answer(&mut sw, &mut state, "prod", &["work"]);
    state.filter = "work".into();
    sw.rebuild(&mut state);
    sw.note_create("prod");
    sw.apply_op_result(
        OpResult::Created {
            session: sess("prod", "edit"),
        },
        &mut state,
    );
    state.filter.clear();
    sw.rebuild(&mut state);
    assert!(on_session(&sw, "prod", "edit"), "{}", picked(&sw));
}

/// The context the user is looking at: the node the selection names, the link selected
/// on its screen, and the screen the terminal view shows.
fn context(sw: &Switcher, state: &State) -> (Option<Node>, Option<Node>, Option<ViewScreen>) {
    (
        sw.selected_node(),
        sw.link_selection_node
            .clone()
            .and_then(|target| match target {
                crate::ui::chrome::LinkTarget::Node(node) => Some(node),
                crate::ui::chrome::LinkTarget::Action(_) => None,
            }),
        sw.current_view_screen(state),
    )
}

#[test]
fn background_answers_leave_the_users_context_alone() {
    let mut state = State::from_roster(
        vec!["prod".into(), "zeta".into()],
        vec!["prod".into(), "zeta".into(), "mars".into()],
    );
    let mut sw = Switcher::from_hosts(&mut state);
    answer(&mut sw, &mut state, "prod", &["a", "b"]);
    answer(&mut sw, &mut state, "zeta", &["q"]);
    // The user opens the host prod and selects the link of its second session.
    sw.note_user_move();
    sw.select_node(Node::Host("prod".into()));
    sw.link_selection = 1;
    sw.link_selection_node = Some(crate::ui::chrome::LinkTarget::Node(Node::Session(
        Address::new("prod", "b"),
    )));
    let before = context(&sw, &state);
    assert_eq!(before.2, Some(ViewScreen::Host));

    // Discovery adds a host whose cards sort above prod's, a poll adds sessions, a
    // machine joins the roster and fails, and prod itself answers a poll.
    sw.add_hosts(vec!["mars:tmux".into()], &mut state);
    assert_eq!(context(&sw, &state), before, "discovery");
    answer(&mut sw, &mut state, "mars:tmux", &["m1", "m2"]);
    assert_eq!(context(&sw, &state), before, "a scan answer above");
    answer(&mut sw, &mut state, "zeta", &["p", "q", "r"]);
    assert_eq!(context(&sw, &state), before, "a poll below");
    sw.add_machine("venus".into(), &mut state);
    sw.apply_machine_result("venus", Some("connection refused".into()), &mut state);
    assert_eq!(
        context(&sw, &state),
        before,
        "a machine joining and failing"
    );
    answer(&mut sw, &mut state, "prod", &["a", "b", "c"]);
    assert_eq!(context(&sw, &state), before, "a poll of the selected host");
}

#[test]
fn a_lost_session_moves_up_and_its_return_does_not_pull_the_selection_down() {
    let (mut sw, mut state) = launch(&["prod", "zeta"]);
    answer(&mut sw, &mut state, "prod", &["work"]);
    answer(&mut sw, &mut state, "zeta", &["q"]);
    sw.select_address(&Address::new("prod", "work"));
    assert!(on_session(&sw, "prod", "work"));
    fail(&mut sw, &mut state, "prod", "connection refused");
    assert_eq!(sw.selected_node(), machine("prod"), "{}", picked(&sw));
    answer(&mut sw, &mut state, "prod", &["work"]);
    assert_eq!(
        sw.selected_node(),
        machine("prod"),
        "the recovery leaves the selection where the loss put it: {}",
        picked(&sw)
    );
    assert!(sw.current_attach_target(&state).is_none());
}

#[test]
fn a_created_session_whose_host_failed_meanwhile_does_not_pull_the_selection_down() {
    let (mut sw, mut state) = launch(&["prod", "zeta"]);
    answer(&mut sw, &mut state, "prod", &["work"]);
    answer(&mut sw, &mut state, "zeta", &["q"]);
    sw.select_address(&Address::new("prod", "work"));
    assert!(on_session(&sw, "prod", "work"));
    sw.note_create("prod");
    // The host stops answering while the create runs; a timed-out scan keeps the
    // sessions it last listed beside the failure.
    let group = state.groups.iter_mut().find(|g| g.host == "prod").unwrap();
    group.err = Some("scan timed out after 10s".into());
    sw.rebuild(&mut state);
    assert_eq!(sw.selected_node(), machine("prod"));
    sw.apply_op_result(
        OpResult::Created {
            session: sess("prod", "fresh"),
        },
        &mut state,
    );
    answer(&mut sw, &mut state, "prod", &["fresh", "work"]);
    assert_eq!(
        sw.selected_node(),
        machine("prod"),
        "the lost host's return moves nothing down: {}",
        picked(&sw)
    );
}

#[test]
fn a_move_made_while_a_create_runs_stands_when_it_finishes() {
    let (mut sw, mut state) = launch(&["prod"]);
    answer(&mut sw, &mut state, "prod", &["a", "b"]);
    sw.select_address(&Address::new("prod", "a"));
    assert!(on_session(&sw, "prod", "a"));
    sw.note_create("prod");
    sw.move_selection(1);
    assert!(on_session(&sw, "prod", "b"));
    sw.apply_op_result(
        OpResult::Created {
            session: sess("prod", "fresh"),
        },
        &mut state,
    );
    answer(&mut sw, &mut state, "prod", &["a", "b", "fresh"]);
    assert!(on_session(&sw, "prod", "b"), "{}", picked(&sw));
}
