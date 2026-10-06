//! The two selection rules over every path that changes the list: a card that
//! disappears moves the selection along its lineage, and a card that appears takes the
//! selection only when it is the user's interest.

use super::*;
use crate::model::OpResult;
use crate::state::State;

fn sess(source: &str, name: &str) -> Session {
    Session {
        source: source.into(),
        name: name.into(),
        windows: 1,
        ..Default::default()
    }
}

fn launch(sources: &[&str]) -> (Switcher, State) {
    let mut state = State::from_sources(sources.iter().map(|s| s.to_string()).collect());
    let sw = Switcher::from_sources(&mut state);
    (sw, state)
}

fn answer(sw: &mut Switcher, state: &mut State, source: &str, names: &[&str]) {
    let sessions = names.iter().map(|n| sess(source, n)).collect();
    sw.apply_source_result(source.into(), sessions, None, state);
}

fn fail(sw: &mut Switcher, state: &mut State, source: &str, reason: &str) {
    sw.apply_source_result(source.into(), Vec::new(), Some(reason.into()), state);
}

/// The selected card as a message for a failed assertion.
fn picked(sw: &Switcher) -> String {
    match sw.selected_card() {
        Some(RowRef::Session { sess }) => format!("session {}", sess.address().display()),
        Some(RowRef::Section { source }) => format!("section {source}"),
        Some(RowRef::Host { source, .. }) => format!("host {source}"),
        Some(RowRef::Machine { machine, .. }) => format!("machine {machine}"),
        None => "nothing".into(),
    }
}

fn on_session(sw: &Switcher, source: &str, name: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Session { sess }) if sess.source == source && sess.name == name)
}

fn on_section(sw: &Switcher, source: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Section { source: s }) if s == source)
}

fn on_host(sw: &Switcher, source: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Host { source: s, .. }) if s == source)
}

/// Whether the selection is on the one card of a host that is down.
fn on_machine(sw: &Switcher, machine: &str) -> bool {
    matches!(sw.selected_card(), Some(RowRef::Machine { machine: m, .. }) if m == machine)
}

fn host(machine: &str) -> Option<Node> {
    Some(Node::Host(machine.into()))
}

/// The issue's walk up to the point the user is on the unreachable machine card: `local`
/// lists a session (the launch preselect lands there), `mars`, whose muxes are not known
/// yet, cannot be reached, and the user selects mars's card.
fn on_unreachable_machine() -> (Switcher, State) {
    let mut state = State::from_roster(vec!["local".into()], vec!["local".into(), "mars".into()]);
    let mut sw = Switcher::from_sources(&mut state);
    answer(&mut sw, &mut state, "local", &["work"]);
    sw.apply_machine_result("mars", Some("connection refused".into()), &mut state);
    sw.open_host("mars", &mut state);
    assert!(on_machine(&sw, "mars"), "{}", picked(&sw));
    (sw, state)
}

/// The machine answers with two muxes: each becomes a source of its own, and the card
/// that stood for the machine goes as they join the list.
fn resolve_into_two_muxes(sw: &mut Switcher, state: &mut State) {
    sw.add_sources(vec!["mars:tmux".into(), "mars:screen".into()], state);
}

#[test]
fn a_resolved_machine_card_hands_the_selection_to_its_first_source_card() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    assert!(on_host(&sw, "mars:screen"), "{}", picked(&sw));
    // The sources answer; the selected source gains sessions and keeps the selection as
    // its section title, whose screen is the source's information, not a session grid.
    answer(&mut sw, &mut state, "mars:tmux", &["t1"]);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    assert!(on_section(&sw, "mars:screen"), "{}", picked(&sw));
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::HostInfo));
    assert!(sw.current_attach_target(&state).is_none());
}

#[test]
fn a_created_session_takes_the_selection_and_its_end_returns_it_to_the_section() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    assert!(on_section(&sw, "mars:screen"));

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
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::HostInfo));
}

#[test]
fn the_last_session_ending_leaves_the_selection_on_its_source_card() {
    let (mut sw, mut state) = launch(&["local"]);
    answer(&mut sw, &mut state, "local", &["only"]);
    assert!(on_session(&sw, "local", "only"));
    answer(&mut sw, &mut state, "local", &[]);
    assert!(on_host(&sw, "local"), "{}", picked(&sw));
    assert_eq!(sw.current_view_screen(&state), Some(ViewScreen::Empty));
}

#[test]
fn a_vanished_source_goes_to_its_host() {
    let (mut sw, mut state) = launch(&["prod", "prod:zellij", "zeta"]);
    answer(&mut sw, &mut state, "prod", &["a"]);
    answer(&mut sw, &mut state, "prod:zellij", &["z"]);
    answer(&mut sw, &mut state, "zeta", &["q"]);
    assert!(sw.select_address(&Address::new("prod:zellij", "z")));
    sw.remove_source("prod:zellij", &mut state);
    assert_eq!(sw.selected_node(), host("prod"));
    assert!(
        on_section(&sw, "prod"),
        "the host half of its remaining title"
    );
}

#[test]
fn a_vanished_source_goes_to_its_host_on_the_hosts_remaining_title() {
    let (mut sw, mut state) = on_unreachable_machine();
    resolve_into_two_muxes(&mut sw, &mut state);
    answer(&mut sw, &mut state, "mars:screen", &["s1"]);
    answer(&mut sw, &mut state, "mars:tmux", &["t1"]);
    assert!(on_section(&sw, "mars:screen"));
    sw.remove_source("mars:screen", &mut state);
    assert_eq!(sw.selected_node(), host("mars"));
    assert!(on_section(&sw, "mars:tmux"), "{}", picked(&sw));
}

#[test]
fn a_removed_machine_hands_the_selection_to_the_card_in_its_place() {
    let (mut sw, mut state) = launch(&["alpha", "beta", "gamma"]);
    for (source, name) in [("alpha", "a"), ("beta", "b"), ("gamma", "g")] {
        answer(&mut sw, &mut state, source, &[name]);
    }
    assert!(sw.select_address(&Address::new("beta", "b")));
    sw.remove_source("beta", &mut state);
    assert!(on_session(&sw, "gamma", "g"), "the next card");
    sw.remove_source("gamma", &mut state);
    assert!(
        on_session(&sw, "alpha", "a"),
        "the previous card at the end"
    );
}

#[test]
fn a_logout_moves_the_selection_to_the_hosts_own_card() {
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
    assert_eq!(sw.selected_node(), host("prod"));
    assert!(sw.current_attach_target(&state).is_none());
}

#[test]
fn a_filter_hiding_the_whole_source_lands_on_the_neighbouring_visible_card() {
    let (mut sw, mut state) = launch(&["alpha", "beta", "gamma"]);
    answer(&mut sw, &mut state, "alpha", &["red"]);
    answer(&mut sw, &mut state, "beta", &["blue"]);
    answer(&mut sw, &mut state, "gamma", &["green"]);
    assert!(sw.select_address(&Address::new("beta", "blue")));
    state.filter = "re".into(); // red and green match, blue does not
    sw.rebuild(&mut state);
    assert!(on_session(&sw, "gamma", "green"), "{}", picked(&sw));
    state.filter.clear();
    sw.rebuild(&mut state);
    assert!(on_session(&sw, "gamma", "green"), "clearing holds it");
}

#[test]
fn a_rescan_waits_on_the_source_card_and_returns_only_to_the_awaited_session() {
    let (mut sw, mut state) = launch(&["alpha", "beta"]);
    answer(&mut sw, &mut state, "alpha", &["a"]);
    answer(&mut sw, &mut state, "beta", &["b"]);
    assert!(sw.select_address(&Address::new("beta", "b")));
    sw.request_rescan(&mut state);
    assert!(on_host(&sw, "beta"), "{}", picked(&sw));
    // Another source answering first is unrelated to the interest.
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
fn a_preselected_session_that_ends_during_the_scan_holds_its_source_card() {
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
