# Issue 211: hide unreachable hosts in the nav by default

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `[ui] hide-unreachable` setting (default true) so that the nav does not render the cards of settled unreachable hosts. When the filter names a host, the hidden card returns, keeping the entry point to the unreachable screen.

**Architecture:** The hide decision lives in one place in the row pipeline (`tree::flatten`). The interaction between the filter and hiding is resolved by placing the prune before the filter. Selection movement is already handled by the existing recovery paths (`restore_focus` after `apply_source_result`, `fallback_after_removal`), so no new logic is added and only verification tests are added.

**Tech Stack:** Rust, cargo test. Test placement: the `write_temp` helper in `src/provision/config.rs`, the Harness in `src/ui/tree.rs` and `src/ui/switcher/tests.rs`, and `fake_env_with_sources`/`dump_screen` in `src/app/runtime/tests.rs`.

---

## Research Findings

The hide decision lives in one place in the row pipeline: `tree::flatten` (`src/ui/tree.rs`) is the only row-deriving function, and its only call site is `Switcher::rebuild` (`src/ui/switcher/mod.rs:461`). The interaction between the filter and hiding (so the XM-01 no-match fallback does not resurrect a hidden host) is resolved by placing the prune before the filter. Selection movement is already handled by the existing recovery paths: `apply_source_result` calls `restore_focus` after the rebuild, and a vanished card falls to `fallback_after_removal` (the previous selectable card, or the first card if none). There is no new logic; only verification tests are added. The only documents listing the `[ui]` keys are the config example blocks of README.md and README.ko.md (there is no separate CONFIG document; verified, and both are updated in Task 6 with the same change). `docs/requirements.md`, `src/ui/AGENTS.md`, and `CONTEXT.md` are also updated in the same change for their behavior statements.

The worktree is already prepared (`/home/hrlee/xmux-wt/211-hide-unreachable`, branch `feat/211-hide-unreachable`). All paths below are relative to the worktree. The project is Rust, so there are no gitignored files to carry over (cargo builds `target/` fresh).

### Task 1: the `[ui] hide-unreachable` config key (TDD)

- [ ] 1. In the `mod tests` of `src/provision/config.rs`, first add the test right after `ui_auto_hide_nav_round_trip` (following the style and the `write_temp` helper as they are):
```rust
#[test]
fn ui_hide_unreachable_defaults_true_and_round_trips() {
    // Missing file → true.
    let missing = std::env::temp_dir().join("xmux-hideunreachable-absent-xyz.toml");
    assert!(load(&missing).unwrap().ui_hide_unreachable());

    // [ui] present but key missing → true; prefix still loads.
    let path = write_temp("[ui]\nprefix = \"C-g\"\n", "hideunreachable-missing.toml");
    let cfg = load(&path).unwrap();
    assert!(cfg.ui_hide_unreachable());
    assert_eq!(cfg.ui_prefix(), "C-g");

    // Explicit false: the unreachable hosts show as before.
    let path = write_temp("[ui]\nhide-unreachable = false\n", "hideunreachable-false.toml");
    assert!(!load(&path).unwrap().ui_hide_unreachable());

    // Explicit true.
    let path = write_temp("[ui]\nhide-unreachable = true\n", "hideunreachable-true.toml");
    assert!(load(&path).unwrap().ui_hide_unreachable());
}
```
- [ ] 2. Run: `cargo test provision::config::tests::ui_hide_unreachable`. Expected: compile failure (no field or accessor yet).
- [ ] 3. Implement:
  - Add the field after `auto_hide_nav` in `UiConfig`:
```rust
/// Whether the nav hides the cards of hosts no scan has reached (default true).
/// Typing a hidden host's name into the filter brings its card back, which is the
/// one entry to that host's unreachable screen. Config-only: the value applies at
/// startup, like `auto-hide-nav`'s initial state, and there is no live toggle.
#[serde(rename = "hide-unreachable", default = "default_hide_unreachable")]
pub hide_unreachable: bool,
```
  - Add `fn default_hide_unreachable() -> bool { true }` next to `default_prefix`. Add `hide_unreachable: default_hide_unreachable(),` to `impl Default for UiConfig`. Add the accessor next to the `ui_auto_hide_nav` accessor:
```rust
/// Whether the nav hides unreachable hosts' cards (default true). Config-only:
/// no persisted or live-toggle state overrides it.
pub fn ui_hide_unreachable(&self) -> bool {
    self.ui.hide_unreachable
}
```
  - Since serde_ignored warns on unknown keys, adding the field to the struct alone makes the key known. No separate registration is needed (verified).
- [ ] 4. Verify: `cargo test provision::config::tests::ui_hide_unreachable` passes.
- [ ] 5. Commit: `feat(config): add [ui] hide-unreachable (default true)`

### Task 2: the hide transform in the row model (TDD)

- [ ] 1. First add the tests to the `mod tests` of `src/ui/tree.rs`, with one helper, `drop_hidden_setup()` (reusing the existing `sess`, `kind`, and `addr_of` helpers):
```rust
fn drop_hidden_setup() -> Vec<Group> {
    vec![
        Group { source: "local".into(), err: None, sessions: vec![sess("local", "web")] },
        Group { source: "empty".into(), err: None, sessions: vec![] },
        Group { source: "deadhost".into(), err: Some("refused".into()), sessions: vec![] },
    ]
}
```
   Eight tests:
   - `drop_hidden_unreachable_keeps_reachable_and_drops_settled_failures`: with an empty filter, the resulting sources are only `["local", "empty"]`.
   - `drop_hidden_unreachable_never_hides_a_scanning_host`: a source with an `err` stays when it is in `scanning` (consistent with the state in the `a_scanning_host_is_not_yet_a_failure` test).
   - `drop_hidden_unreachable_filter_naming_the_host_keeps_its_group`: with the filter `"dead"`, deadhost stays.
   - `drop_hidden_unreachable_does_not_mutate_input`.
   - `flatten_hides_unreachable_hosts_when_asked`: the kinds of `flatten(&groups, &HashSet::new(), "", true, &mux_of_source)` are `["section", "session", "host"]` (the local section and card, and the empty reachable host's card), and there is no deadhost row. This also pins that the empty host's card stays as it is.
   - `flatten_keeps_the_unreachable_card_when_the_filter_names_it`: with the filter `"dead"` and hide=true, `RowRef::Host { source: "deadhost", unreachable: true, .. }` is present.
   - `flatten_no_match_fallback_does_not_resurrect_a_hidden_host`: with the filter `"zzz"` (matching nothing) and hide=true, there is no deadhost while the other hosts' cards remain. The prune runs before the XM-01 fallback.
   - `flatten_hiding_every_host_leaves_no_rows`: with every host unreachable and hide=true, the rows are empty (without a panic).
- [ ] 2. Fill in the `false` argument at the five existing `flatten` call sites (tree.rs tests at 788, 807, 823, 853, 887). The call site at `src/ui/switcher/mod.rs:461` also takes `false` for now (replaced by the field in Task 3).
- [ ] 3. Run: `cargo test ui::tree`. Expected: compile failure (no function or parameter yet).
- [ ] 4. Implement:
  - Add before `visible_groups`:
```rust
/// The groups the nav may render when unreachable hosts are hidden (`[ui]
/// hide-unreachable`): every reachable group, plus an unreachable group only while
/// the filter names it - the named card is the one entry to that host's unreachable
/// screen, and an empty filter hides every unreachable group. A host still scanning
/// is not unreachable (its card turns the spinner), so it is never hidden, whatever
/// stale error it carries. Inputs are not mutated.
pub(crate) fn drop_hidden_unreachable(
    groups: &[Group],
    scanning: &HashSet<String>,
    filter: &str,
) -> Vec<Group> {
    groups
        .iter()
        .filter(|g| {
            g.err.is_none()
                || scanning.contains(&g.source)
                || fuzzy_match(filter, &g.source)
        })
        .cloned()
        .collect()
}
```
  - Add `hide_unreachable: bool` to `flatten` between `filter` and `mux_of_source`, and, at the top of the body, prune before the filter:
```rust
let groups = if hide_unreachable {
    drop_hidden_unreachable(groups, scanning, filter)
} else {
    groups.to_vec()
};
let groups = visible_groups(&groups, filter);
```
   Since the prune runs before `visible_groups`, the no-match fallback (the group left with only its header) cannot resurrect a hidden host. Add one sentence on this rule to `flatten`'s doc comment.
- [ ] 5. Verify: `cargo test ui::tree` passes.
- [ ] 6. Commit: `feat(ui): drop hidden unreachable hosts from the nav rows`

### Task 3: the Switcher flag and the rebuild wiring (TDD)

- [ ] 1. First add the tests near the unreachable/host-band tests of `src/ui/switcher/tests.rs` (after `a_card_claims_a_mux_only_when_it_is_confirmed`):
   - `hide_unreachable_leaves_no_card_for_the_unreachable_host`: from `Harness::new(sample())`, after `h.sw.set_hide_unreachable(true, &mut h.state); h.draw();`, `nav_text()` has no `"db-2"` and still has `"jupiter00"`.
   - `the_filter_names_a_hidden_unreachable_host_and_its_card_returns`: with the hiding on, `h.ch('/').await; h.ch('d').await; h.ch('b').await;` brings `"db-2"` back into `nav_cards_text()`. Then, after `h.key(KeyCode::Enter).await` (closing the input), the selection stays on the db-2 card or lands on it, and `h.view_text()` contains `"unreachable"`, confirming entry is possible (by the selection-keeping rule, it lands on the first remaining card after the filter).
   - `hide_unreachable_off_brings_the_card_back`: after hiding with true, setting it back to false brings the card back (pins the setter's rebuild).
   - `a_selected_host_going_unreachable_hides_and_the_selection_lands_on_a_remaining_card`: from `Harness::from_sources(&["local", "db-2"])`, apply a session result for local, select the db-2 card with `h.sw.move_to(-1, &h.state)` (user_moved set), then after `h.sw.apply_source_result("db-2".into(), Vec::new(), Some("connection timed out".into()), &mut h.state); h.draw();`, `nav_text()` has no db-2, and `h.sw.current_ref()` is the `local/editor` session card.
   - `a_hidden_unreachable_host_returns_when_its_scan_answers`: after being hidden as unreachable, a successful `apply_source_result` (`err: None`, with sessions) revives the card.
   - `hiding_every_host_leaves_a_tidy_empty_nav`: from `from_sources(&["db-2"])`, after the hiding and applying the err, `nav_cards_text()` holds only empty lines and `hint_bar_text()` still shows the prefix (`C-g`) (a tidy empty nav).
- [ ] 2. Run: the six new tests, including `cargo test ui::switcher::tests::hide`. Expected: compile failure (no setter yet).
- [ ] 3. Implement (`src/ui/switcher/mod.rs`):
  - Add after the `own_session` field:
```rust
/// Whether the nav hides the settled unreachable hosts' cards (`[ui]
/// hide-unreachable`). The app threads it in at construction; there is no live
/// toggle. The filter naming a hidden host keeps its card, which is the
/// unreachable screen's one entry point.
hide_unreachable: bool,
```
  - `hide_unreachable: false,` in `blank()` (the switcher's own default is off. The policy default of true is owned by the config and injected by the app. The construction paths of the roughly 100 existing test sites stay as they are).
  - The setter next to `set_own_session`:
```rust
/// Sets whether the nav hides the settled unreachable hosts' cards, rebuilding the
/// rows since the setting decides which groups render. A no-op when the value is
/// unchanged. The app threads the config value in once at startup.
pub fn set_hide_unreachable(&mut self, on: bool, state: &mut crate::state::State) {
    if self.hide_unreachable == on {
        return;
    }
    self.hide_unreachable = on;
    self.rebuild(state);
}
```
  - In the flatten call of `rebuild`, replace the `false` placeholder with `self.hide_unreachable`.
- [ ] 4. Verify: the whole `cargo test ui::switcher` passes (the existing unreachable card render tests stay as they are because the flag defaults to false).
- [ ] 5. Commit: `feat(ui): hide unreachable hosts in the nav unless the filter names them`

### Task 4: the app injection and the mid-run transition path (TDD)

- [ ] 1. In `src/app/runtime/tests.rs`, first add the tests after `host_exited_before_connect_marks_unreachable` (existing style: a standalone switcher + `note_host_exited` + `dump_screen`; the needed `use` lines go inside the test functions):
```rust
#[test]
fn runtime_threads_hide_unreachable_into_its_switcher() {
    use crate::ui::run::dump_screen;
    // The default roster config: hide-unreachable = true.
    let env = std::sync::Arc::new(fake_env_with_sources(&["jup"]));
    let (mut rt, _io) = Runtime::new(env);
    rt.switcher.apply_source_result(
        "jup".into(),
        Vec::new(),
        Some("no route to host".into()),
        &mut rt.state,
    );
    let out = dump_screen(&mut rt.switcher, None, 80, 24, &rt.state);
    assert!(!out.contains("jup"), "the config default hides the unreachable host:\n{out}");
    rt.switcher.set_hide_unreachable(false, &mut rt.state);
    let out = dump_screen(&mut rt.switcher, None, 80, 24, &rt.state);
    assert!(out.contains("jup"), "hide-unreachable = false shows the card:\n{out}");
}
```
   And the mid-run transition test:
```rust
#[test]
fn hide_unreachable_mid_run_hides_the_card_and_the_selection_lands_on_a_remaining_card() {
    use crate::ui::run::dump_screen;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut state = crate::state::State::from_sources(vec!["local".into(), "jupiter06".into()]);
    let mut switcher = Switcher::from_sources(&mut state);
    switcher.set_hide_unreachable(true, &mut state);
    // Put the selection on the jupiter06 card, then let local answer with a session.
    switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    switcher.apply_source_result(
        "local".into(),
        vec![crate::session::Session {
            source: "local".into(),
            name: "editor".into(),
            ..Default::default()
        }],
        None,
        &mut state,
    );
    // jupiter06's control client dies mid-run: the card hides from that moment.
    assert!(note_host_exited(&mut switcher, &mut state, &mut HashSet::new(), "jupiter06",
        Some("no route to host".into())));
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
    assert!(!out.contains("jupiter06"), "hidden the moment it fails:\n{out}");
    let t = switcher.terminal_view_target();
    assert_eq!((t.source, t.target), ("local".into(), "editor".into()),
        "the selection lands on a remaining card");
    // A later scan answers and the host returns.
    switcher.apply_source_result(
        "jupiter06".into(),
        vec![crate::session::Session {
            source: "jupiter06".into(),
            name: "ops".into(),
            ..Default::default()
        }],
        None,
        &mut state,
    );
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
    assert!(out.contains("jupiter06"), "a successful scan revives the host:\n{out}");
}
```
   The first test is red (Runtime::new does not inject the flag yet, so jup shows), and the second is green right after Task 3 (it pins the production entry point `note_host_exited` and the `restore_focus` path).
- [ ] 2. Implement: in `Runtime::new` of `src/app/runtime/handlers.rs`, right after `switcher.set_own_session(env.own_session.clone());`:
```rust
// [ui] hide-unreachable: the nav drops the settled unreachable hosts' cards. The
// filter naming one brings its card, and its unreachable screen, back.
switcher.set_hide_unreachable(roster.cfg.ui_hide_unreachable(), &mut state);
```
   This key stays out of live re-application: like `auto_hide_nav`'s initial value, it is read once at startup, and `on_config_check` does not touch it. The issue asks for neither a live toggle nor live re-application, so it is out of scope, and the documents do not list it under live re-application either.
- [ ] 3. Verify: both `cargo test app::runtime::tests::runtime_threads_hide_unreachable app::runtime::tests::hide_unreachable_mid_run` pass.
- [ ] 4. Commit: `feat(app): thread [ui] hide-unreachable into the switcher`

### Task 5: the full gate run

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. App and UI changes are strongly coupled, so run the whole suite (the verification rule of the root AGENTS.md). If fmt/clippy fixes arise, include them in the same commit or commit them separately as `style: satisfy fmt and clippy`. Do not attach AI attribution trailers (Co-Authored-By and the like) to commits.

### Task 6: documentation (inside the same change)

The only documents listing the `[ui]` keys are README.md and README.ko.md (verified that there is no separate CONFIG document). Both are updated in this change.

- [ ] 1. `README.md` config example: add after the `auto-hide-nav = false` line, aligned to the same column:
```toml
hide-unreachable = true               # hide hosts no scan has reached (the filter names one to show its card)
```
   The live re-application paragraph below (covering only the theme, role colors, selection-style, hint-bar-style, and view-border styles) stays as it is. This key applies at startup, so it does not go into that list.
- [ ] 2. `README.ko.md` config example: add a Korean comment in the same place (neutral technical written style, no sentence-final endings):
```toml
hide-unreachable = true               # 도달하지 못한 호스트는 nav에서 숨긴다 (필터에 이름을 입력하면 카드가 나타난다)
```
- [ ] 3. `docs/requirements.md`: add FR-B24 after FR-B23 and before "## C. Switching" (English, behavior statements only):
   - **FR-B24**: The nav hides the hosts no scan has reached: an unreachable host takes no card by default, and `[ui] hide-unreachable` (default true) turns the hiding off. The filter naming a hidden host brings its card back, and that named card is the one entry to its unreachable screen. An empty filter hides every unreachable host, and a filter matching nothing does not bring them back through the no-match fallback that shows the other hosts. A reachable host with no sessions keeps its card, and a host still scanning never hides, whatever stale failure it carries. A host that goes unreachable mid-run hides from that result on and returns when a scan answers.
- [ ] 4. `src/ui/AGENTS.md`: add one item to the Invariants (a statement of the current state only):
   - The nav hides a settled unreachable host's card unless the filter names that host (`[ui] hide-unreachable`, default on): the named card is the one entry to its unreachable screen, so the hiding must leave it reachable. A reachable empty host and a host still scanning never hide, and the prune runs before the filter, so the no-match fallback cannot resurrect a host the filter does not name.
- [ ] 5. `CONTEXT.md`: add one sentence at the end of the `filter` entry of the glossary. "A host hidden from the nav (`[ui] hide-unreachable`) shows its card while the filter names it."
- [ ] 6. Verify: re-run `cargo fmt --check` and `cargo test` (the gate holds even when only documents change).
- [ ] 7. Commit: `docs: document [ui] hide-unreachable`

### Task 7: design consistency re-review before merge

- [ ] Re-read `CONTEXT.md`, `docs/requirements.md`, and `src/ui/AGENTS.md` to confirm their statements match the code. In particular: whether the hide decision matches the render's definition of unreachable (a settled failure, scanning excluded), whether the empty host's card is kept, and whether the documents consistently mark the key as not subject to live re-application.

## Files to Modify

- `src/provision/config.rs`: the `hide-unreachable` key in `UiConfig` (serde rename, default true), `Default`, the `ui_hide_unreachable()` accessor, and the round-trip test.
- `src/ui/tree.rs`: add `drop_hidden_unreachable`, the `hide_unreachable` parameter on `flatten`, update the five existing test call sites, and add the eight new tests.
- `src/ui/switcher/mod.rs`: the `hide_unreachable` field, `set_hide_unreachable`, and the updated flatten call in `rebuild`.
- `src/ui/switcher/tests.rs`: six new tests.
- `src/app/runtime/handlers.rs`: `Runtime::new` injects the config value into the switcher.
- `src/app/runtime/tests.rs`: two new tests.
- `README.md`, `README.ko.md`: add hide-unreachable to the `[ui]` key examples.
- `docs/requirements.md`: add FR-B24.
- `src/ui/AGENTS.md`: add the hiding rule to the Invariants.
- `CONTEXT.md`: add one sentence on the hidden-host exception to the `filter` glossary entry.

## New Files

- None.

## Risks

- **Reuse of the mid-run selection movement**: even when hiding removes a card, there is no new recovery logic. The `restore_focus` of `apply_source_result` already provides the vanished-card fallback (the previous selectable card, or the first card if none), so the requirement is satisfied. What needs confirming is whether the tests pin the interaction with the general `rebuild` path (a selected host card is not kept by identity), and the switcher/runtime tests in this plan serve that role.
- **Coexistence of scanning and a stale err**: in production the two states do not arrive together (`request_rescan` clears the err), but `a_scanning_host_is_not_yet_a_failure` allows that state, so the prune, like the render, does not hide a scanning host. Dropping that condition would make the hiding disagree with the render state.
- **The order of the prune and the XM-01 fallback**: placing the prune after the filter lets the no-match fallback resurrect hidden hosts. It must go before the filter. `flatten_no_match_fallback_does_not_resurrect_a_hidden_host` pins this.
- **The `Runtime::new` injection test**: the `test_rt` helper builds the switcher directly and bypasses `Runtime::new`. So the injection is verified by a test that calls `Runtime::new` directly. If the test environment turns up a factor that makes `Runtime::new` unrunnable (terminal size queries and the like are safe through `unwrap_or`), drop that test and verify the injection line through compilation and the whole suite.
- **Preventing over-application in the documents**: this key does not go into the README's live re-application paragraph or into `on_config_check`. Putting it there would be a documentation claim with no behavior behind it, and implementing it would go beyond the issue's scope (a live toggle).
- **Comment and document rules**: all src comments are in English and describe the current state only (no narration of change history). The Korean comment in README.ko.md uses the neutral technical written style. No em-dash or en-dash anywhere.
