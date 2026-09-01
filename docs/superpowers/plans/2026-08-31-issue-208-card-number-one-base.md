# Issue 208: Switching Session Card Numbers to 1-Based

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Switch the nav card numbers and jumps to 1-based: the first card is 1 and `prefix 1` jumps to the first card, no card carries 0, and the jump guide, the range flash, the help modal, the cheatsheets, and the documentation follow.

**Architecture:** Every painted number comes from the single `Switcher::card_number`, and the jump is defined by the single `jump_row`. The two are one invariant (number = the 1-based rank among the selectable cards, jump n = the n-th card), so they change together in one commit.

**Tech Stack:** Rust, cargo test. Test harness: the Harness in `src/ui/switcher/tests.rs`.

---

## Leading Zero Decision

**01 is accepted as 1 (not a leading-zero rejection).** The grounds lie in the actual structure of the input state machine:

1. **The jump buffer is a freely edited string.** `handle_input_key` allows Left/Right/Home/End/Delete/Backspace and moves the caret, so text can be inserted or deleted anywhere (the edit branches in `src/ui/switcher/input.rs`). The state machine tracks nothing about how the buffer was typed, so a "leading 0" is not a property of the input phase but merely the spelling of the final string. A rejection rule would have to vet the completed string, and then a live number with the same value dies: "010", reached by pressing Home in "10" and inserting a 0, has the value 10 yet becomes a dead number. This clashes head-on with the live-cursor design of "every edit follows the selection while the number names a card".
2. **`parse::<usize>()` already provides the value semantics.** The accepting side is a single `checked_sub(1)` line, and "every digit is taken as typed" in keybind.md and requirements FR-B10 stays true without exception. Rejection would add one special rule to the parser and one documentation exception apiece.
3. **The only path that starts with 0 is `prefix 0`.** Under 1-based numbering, "01" typed as a continuation in that buffer reads as the value 1, which is what the reader expects. With rejection, after `prefix 0` no card is reachable from the buffer at all, so the input is effectively dead-ended.

Therefore `jump_row` reads by value, and the values no card carries are only 0 and the values past the last card. The decision is stated as current behavior in the `jump_row` comment and in keybind.md (no change-history narration).

## Plan

### Task 1: Switching the Number Definition and Jump to 1-Based (switcher core)

Key facts: every painted number comes from the single `Switcher::card_number` (the `address` closure in render.rs is used for both session cards and host cards; section titles return before it). The jump is defined by the single `jump_row`. The two are one invariant, so they must change together in one commit. Splitting them breaks the existing tests (`a_digit_opens_the_jump_popup_and_lands_on_that_card` among others) in the intermediate state.

- [ ] 1. **(red) tests first** - `src/ui/switcher/tests.rs`:
   - Rename the test at line 3322 from `every_unselected_card_carries_its_0_based_number_beside_its_session` to `every_unselected_card_carries_its_1_based_number_beside_its_session`, and remove `.saturating_sub(1)` from the `num_w` computation inside it:
     ```rust
     let num_w = sw.selectable_count().to_string().len().max(1) as u16;
     ```
     (This test is not red: it verifies round-trip consistency between the painted digits and `card_number`, and the task only aligns the width computation with the new definition.)
   - Fix the `scan_with_sessions` comment at line 335 from "numbered `0..n`" to "numbered `1..=n`".
   - Add the three tests below after `a_jump_walks_into_a_two_digit_number` (line 3551):
     ```rust
     #[tokio::test]
     async fn card_numbers_count_from_1_and_the_last_card_carries_the_count() {
         // The number a card carries is its 1-based rank among the selectable cards:
         // the first card is 1 and the last carries the card count. A section title
         // carries no number, so the ranks count the cards only.
         let mut h = Harness::new(sample());
         let selectable: Vec<usize> = (0..h.sw.rows.len())
             .filter(|&i| h.sw.rows[i].selectable())
             .collect();
         for (rank, &i) in selectable.iter().enumerate() {
             assert_eq!(h.sw.card_number(i), rank + 1, "card {i}'s number is its rank");
         }
         // `prefix 1` lands on the card numbered 1, the first selectable card.
         h.key(KeyCode::Char('1')).await;
         assert_eq!(h.sw.selected, selectable[0], "1 addresses the first card");
     }

     #[tokio::test]
     async fn a_jump_on_0_opens_the_input_and_names_no_card() {
         // No card carries 0: `prefix 0` opens the jump input holding 0 and the
         // selection stays put; Enter flashes the 1-based range and keeps the popup.
         let mut h = Harness::new(sample());
         h.key(KeyCode::End).await; // start far from where 0 used to point
         let start = h.sw.selected;
         h.key(KeyCode::Char('0')).await;
         assert!(h.state.is_inputting(), "0 still opens the jump input");
         assert_eq!(h.input_buffer(), "0", "the input holds the 0");
         assert_eq!(
             h.sw.selected, start,
             "no card carries 0, so the selection stays"
         );
         let bar = h.hint_bar_text();
         assert!(
             bar.contains("jump to a session (1 - 4)"),
             "the guide states the 1-based range: {bar:?}"
         );
         h.key(KeyCode::Enter).await;
         assert!(h.state.is_inputting(), "the popup stays open");
         assert!(
             h.state.chrome.flash.contains("no session 0 (1 - 4)"),
             "the flash names the dead number and the 1-based range: {}",
             h.state.chrome.flash
         );
     }

     #[tokio::test]
     async fn a_leading_zero_names_its_value() {
         // The number is read as its value, spelling included: 01 is 1, the card 1
         // addresses. The dead 0 comes alive the moment the digit giving it its
         // value lands, and Enter closes on the card the value names.
         let mut h = Harness::new(sample());
         h.key(KeyCode::End).await;
         h.key(KeyCode::Char('0')).await;
         h.key(KeyCode::Char('1')).await;
         let first = h.sw.rows.iter().position(Row::selectable).unwrap();
         assert_eq!(h.sw.selected, first, "01 names the card 1 names");
         assert_eq!(h.sw.card_number(h.sw.selected), 1, "which carries number 1");
         h.key(KeyCode::Enter).await;
         assert!(!h.state.is_inputting(), "Enter closes on the card 01 names");
     }
     ```
     `sample()` has 4 selectable cards (2 local + 1 jupiter00 + the db-2 host card), so the range notation "1 - 4" is accurate. All three tests start from `End` so that they do not rely on 0 having pointed at the first card in the past.
- [ ] 2. Confirm red: `cargo test --lib card_numbers_count_from_1` (fails because the first card's `card_number` is currently 0), `cargo test --lib a_jump_on_0` (fails because `prefix 0` currently jumps to the first card and the flash reads `(0 - 3)`), `cargo test --lib a_leading_zero_names_its_value` ("01" currently goes to the second card) - failure is the expected result for all three.
- [ ] 3. **(green) implementation**:
   - `src/ui/switcher/mod.rs:501-505` `card_number`:
     ```rust
     /// The number card `i` addresses: its 1-based position among the selectable
     /// cards, the first card being 1 and the last the selectable count. A section
     /// title has no number; it is never the selection and never a jump target.
     fn card_number(&self, i: usize) -> usize {
         self.rows[..i].iter().filter(|r| r.selectable()).count() + 1
     }
     ```
   - `src/ui/switcher/render.rs:577-582` `number_width`: the maximum is now the card count, so
     ```rust
     fn number_width(&self) -> usize {
         self.selectable_count().to_string().len().max(1)
     }
     ```
     (The comment "the digit count of the highest card number" remains true as is.)
   - `src/ui/switcher/input.rs:159-171` `jump_row`: change only the indexing and rewrite the comment to state current behavior:
     ```rust
     /// The row `number` addresses, or `None` when no card carries it: the number-th
     /// SELECTABLE card, section titles excepted, counting from 1. The buffer is read
     /// as its value, spelling included, so 01 is 1: the values no card carries are 0
     /// alone and everything past the last card. The jump reads it on every edit to
     /// move the selection while the number names a card, and at Enter to decide
     /// whether to land or flash: see [`Switcher::jump_accepts`].
     fn jump_row(&self, number: &str) -> Option<usize> {
         let n = number.trim().parse::<usize>().ok()?;
         self.rows
             .iter()
             .enumerate()
             .filter(|(_, r)| r.selectable())
             .nth(n.checked_sub(1)?)
             .map(|(i, _)| i)
     }
     ```
   - `src/ui/switcher/input.rs:194` `open_jump`: change to `let last = self.selectable_count();` (under 1-based numbering the last number = the card count) and the guide line at line 199 to `format!(" jump to a session (1 - {last})")`.
   - `src/ui/switcher/input.rs:284-286` the Enter branch: compute `last` with the same expression and set the flash to `format!("no session {val} (1 - {last})")`.
   - What not to change: the digit forwarding in `src/app/input.rs` (it stays as is because `prefix 0` still opens the jump input), `jump_accepts`, and the ctl/dump paths (they go through the same paint).
- [ ] 4. **Verification**: `cargo test --lib 1_based`, `cargo test --lib card_number`, `cargo test --lib jump`, `cargo test --lib leading_zero` all pass. The existing jump tests exercise only `card_number` round trips or out-of-range numbers, so they pass without modification (the seed 6 in `a_jump_holds_out_of_range_numbers_and_vets_at_enter` exceeds the `sample()` card count of 4 and is out of range under both bases). Full `cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` pass.
- [ ] 5. **Commit**: `feat(ui): number nav cards from 1`
   Body: "A card's number is its 1-based rank among the selectable cards, so prefix 1 lands on the first card and the last card carries the card count. No card carries 0: prefix 0 opens the jump input holding a number no card carries and leaves the selection alone. The number is read as its value, so 01 names the card 1 names. The jump guide and the out-of-range flash state the range 1 to the card count."

### Task 2: Cheatsheet and Help Modal Notation

- [ ] 1. **(red)** the `src/ui/chrome.rs` tests first: in the order array at line 1229, `"0-9 jump to a session"` to `"1-9 jump to a session"`. The flash samples at lines 1198 and 1201, `"no session 9 (0 - 3)"` to `"no session 9 (1 - 4)"` (this test is not red: it verifies the display of an arbitrary string, and the task only aligns it with the real format).
   Check: `cargo test --lib hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed` fails (the bar still prints 0-9).
- [ ] 2. **(green)**:
   - `src/ui/chrome.rs:883-886`: in the four cheatsheet candidate strings, `0-9` to `1-9` (same width, no effect on the fit order).
   - `src/ui/modal.rs:358`: `format!("{p} 0-9")` to `format!("{p} 1-9")`. The description "jump to a session by its number (keep typing for 10+)" stays as is.
- [ ] 3. **Verification**: `cargo test --lib hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed`, `cargo test --lib hint_bar_shows_a_flash_over_an_open_input`, `cargo test --lib armed_hint_bar_fits_a_narrow_nav`, `cargo test --lib help_lines_reflects_configured_prefix` pass. Full `cargo test`, fmt, and clippy pass.
- [ ] 4. **Commit**: `feat(ui): state the jump keys as 1-9 in the cheatsheets`

### Task 3: Documentation Updates

- [ ] 1. `docs/keybind.md:66`: the table row to `` | `prefix 1`-`prefix 9` | jump to a session by its number | ``.
- [ ] 2. `docs/keybind.md:79-87`, the "Jumping by number" section: rewrite the first paragraph to open with "Every card carries a dim number in its left column, on the same row as the session it names, counted from 1 in the same order the list reads: the first card is 1 and the last is the card count." (the remaining sentences and the `prefix 1` then `2` lands on 12 example stay valid), change the second paragraph's "(0 to the last card)" to "(1 to the last card)", and add one sentence each covering 0 and leading zeros as current behavior: "No card carries 0, so `prefix 0` opens the jump input holding a number no card carries and leaves the selection where it is; 0 matters only inside a longer number (10, 20, 100), and a leading zero is just a spelling (01 is 1)."
- [ ] 3. `docs/requirements.md:168` FR-B10: "a 0-based number" to "a 1-based number". The remaining sentences ("Each edit moves the selection while the number names a card...") already cover the behavior of 0, so they stay.
- [ ] 4. `CONTEXT.md:266`, the address column entry: "the dim 0-based number" to "the dim 1-based number" (one word). The jump entry mentions no base, so it stays.
- [ ] 5. `README.md:91`: `` `prefix 0`-`prefix 9` `` to `` `prefix 1`-`prefix 9` ``. `README.ko.md:90`: in the same table row, change only the key cell to `` `prefix 1`-`prefix 9` `` and leave the description cell as is.
- [ ] 6. **Verification**: `cargo test` passes (only documentation changes, but the full gate is checked). Check the documentation rules: no test, function, or field names quoted, and current behavior only.
- [ ] 7. **Commit**: `docs: state the card numbers and jump keys as 1-based`

### Task 4: Remaining 0-Based Notation Sweep and the Final Gate

- [ ] 1. Sweep (grep, listing even the leftovers unrelated to the change targets):
   - `grep -rn "0-based" src docs CONTEXT.md README.md README.ko.md` → 0 hits related to card numbers. Everything remaining concerns screen and SGR coordinates and is unrelated: `src/app/input.rs` (drag height, mouse SGR conversion), `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`, `src/display/grid.rs`, `src/display/attachment.rs`, `src/ui/switcher/mouse.rs` (screen coordinates, not card indexes), `src/ui/switcher/columns.rs` (flow column numbers).
   - `grep -rn "0-9" src docs README.md README.ko.md` → only `[a-z0-9-]` in `src/cli/mod.rs` (the instance name character set, unrelated) should remain.
   - `grep -rnF "(0 - " src docs README.md README.ko.md` and `grep -rn "prefix 0" docs README.md README.ko.md` → 0 hits. (The "prefix 0 opens the jump popup" test comment in `src/app/input.rs` stays because it is still true.)
- [ ] 2. If the sweep finds card-number leftovers, fix them in the same spirit and commit `docs: sweep remaining 0-based card number references`. If none, no commit.
- [ ] 3. **Final gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and full `cargo test` pass. App and display are heavily coupled, so run the whole suite (the repository verification rule).

## Files to Modify

- `src/ui/switcher/mod.rs` - `card_number` returns a 1-based rank (+1), comment updated
- `src/ui/switcher/render.rs` - `number_width` uses the digit count of the highest number (= the card count)
- `src/ui/switcher/input.rs` - `jump_row` 1-based indexing (`checked_sub(1)`), range notation `(1 - {last})` in the `open_jump` guide line and the Enter flash
- `src/ui/switcher/tests.rs` - rename the render-consistency test and update its num_w, the `scan_with_sessions` comment, 3 new pegging tests
- `src/ui/chrome.rs` - the four cheatsheet strings `0-9` → `1-9`, 2 test literals
- `src/ui/modal.rs` - the help modal jump row `{p} 0-9` → `{p} 1-9`
- `docs/keybind.md` - the table row, the 0-based wording, the range notation, the 0 and leading-zero sentences
- `docs/requirements.md` - FR-B10 0-based → 1-based
- `CONTEXT.md` - the address column entry 0-based → 1-based
- `README.md`, `README.ko.md` - `prefix 0`-`prefix 9` → `prefix 1`-`prefix 9` in the nav keys table

## New Files (if any)

None.

## Risks

- **Column width of a list with exactly 10 cards**: with 10 selectable cards, `number_width` grows from 1 to 2 and each card becomes one cell wider. The portrait tests using `scan_with_sessions(10)` (tests.rs 2220, 2297) make only structural assertions such as connector presence and offsets and read the rect the paint recorded, so they are expected to pass; if any breaks, do not hardcode absolute coordinates but re-read the geometry the paint recorded to decide.
- **Meaning shift in the existing jump tests**: `a_jump_past_the_last_card_is_inert` and the like pass unchanged because their value assertions are `card_number` round trips, but the card a test lands on differs from the 0-based case (the first card, among others). The behavior stays identical, so they are left untouched (surgical changes).
- **Degenerate notation for a count of 0**: with no cards at all, the range becomes "(1 - 0)". Host cards are always selectable, so the state is hard to reach in practice, and the existing code degenerates to "(0 - 0)" as well, so no special case is added.
- **AS-IS wording**: the `jump_row` and `card_number` comments and all documentation state current behavior only ("was 0-based" phrasing banned). Documentation quotes no test or function names (docs/AGENTS.md rule).
- **Commit rules**: no trailers, no em/en dashes, conventional prefix. When editing README.ko.md, keep the Korean prose of the description cell as is and change only the key cell.
