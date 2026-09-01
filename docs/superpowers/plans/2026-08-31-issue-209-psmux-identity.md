# Issue 209: Local psmux source is not created (tmux mentions in the help output shadow identity classification)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep psmux's identity decision from being shadowed by the tmux mentions in its own help output, so that local discovery in auto mode (`[local] mux` unspecified) creates a psmux source.

**Architecture:** Identity detection is owned by each mux implementation (`Mux::identity_probes`). `named_mux_excluding` is a shared pure helper that implementations read and use. Direction 1 is chosen, where psmux's classify drops the tmux name to reflect the meaning of its own output.

**Tech Stack:** Rust, cargo test (tokio), the existing `MachineWith`/`ProbeRunner` fake runner injection pattern.

---

## Direction Decision

**Direction 1 (drop the tmux name in psmux's classify) is chosen.** The code shows four pieces of evidence.

1. **Detection ownership belongs to the implementation by design.** The `Mux::identity_probes` comment states "Detection is per implementation: there is no central sequence of shared stages" and deliberately provides no default ("A default probe is a rank"). `named_mux` is only a shared pure helper that implementations read and use, not the judge. Direction 1 stays inside this seam; Direction 2 changes the shared helper's contract and demands re-verification of every mux.
2. **The same mechanism already exists.** tmux's `classify_identity` (`src/mux/tmux/mod.rs:90`) excludes its own name with `named_mux_excluding(help, "tmux")`. psmux's help is the mirror-image situation: it names psmux in its banner and usage lines while mentioning tmux comparatively, as in "(tmux alternative)". psmux interpreting the meaning of its own output is exactly that pattern.
3. **Direction 2 does not remove the defect class.** "Prefer the leading name" leaves the containment fallback in place. zellij's real help banner does not start with its name; it reads "A terminal workspace with batteries included", so if zellij output ever mentions tmux, the same defect recurs under Direction 2 as well. The shared judge would need re-verification across all five implementations even though the fallback still carries the load.
4. **Blast radius.** Direction 1 touches only the psmux implementation and its tests. `named_mux` itself is unchanged, so detection for zellij, abduco, screen, and tmux plus the existing alias-correction tests continue to serve as pins.

Note that this one-line fix also repairs a latent defect beyond the reported discovery defect. Re-detection of a configured psmux source (`Host::detect_and_correct`, `src/model/host.rs:226`) uses the same classify; before the fix, the tmux mentions in `psmux help` output could swap the psmux source to `Tmux { bin: "psmux" }` (Shared model, tmux `-CC` control). After the fix it stays psmux.

## Plan

Test placement check: the identity logic is unit-tested by injecting synthetic probe output, through the `MachineWith` / `ProbeRunner` fake runners in `src/mux/mod.rs` (an `installed_muxes` test and a `detect_backend` test each exist). The psmux implementation's `classify_identity` is callable directly from the tests module (with `use super::*`, the `Mux` trait is in scope). Registry order throughout is tmux, abduco, psmux, zellij, screen, and `probe_identity` lowercases the output.

### Task 1: Add identity detection regression tests and confirm red (no commit)

- [ ] 1. In `src/mux/psmux/mod.rs`, add the two tests inside `#[cfg(test)] mod tests`, after the `psmux()` helper.

```rust
    #[test]
    fn psmux_help_naming_tmux_mentions_still_classifies_psmux() {
        // psmux's help banner names psmux AND mentions tmux ("... for Windows (tmux
        // alternative)"): the mentions are comparative, never an identity claim, so
        // psmux's own name decides.
        let banner = "psmux v3.3.8 - terminal multiplexer for windows (tmux alternative)\n\n\
                      usage: psmux [-S socket-path] command";
        let outs = vec![Some(banner.to_string())];
        assert_eq!(psmux().classify_identity(&outs), Some("psmux"));
    }

    #[test]
    fn psmux_help_naming_only_tmux_is_inconclusive() {
        // An output carrying no psmux name of its own decides nothing: the dropped
        // tmux mentions leave no identity, and None retries on a later scan rather
        // than decoding to another kind.
        let outs = vec![Some("tmux 3.3.8 - a terminal multiplexer".to_string())];
        assert_eq!(psmux().classify_identity(&outs), None);
    }
```

- [ ] 2. In `src/mux/mod.rs`, add two tests to the tests module. The first goes immediately after `a_binary_that_answers_as_another_mux_is_not_that_mux` (discovery level), the second immediately after `detect_backend_classifies_psmux_by_help_marker` (re-detection level).

```rust
    #[tokio::test]
    async fn a_psmux_whose_help_mentions_tmux_is_still_discovered() {
        // psmux's help banner names itself and mentions tmux (it presents itself as
        // a tmux alternative): the mentions are comparative, never an identity claim,
        // so the psmux candidate is confirmed by its own name in its own output.
        let t = crate::transport::local(None);
        let runner = MachineWith {
            present: vec!["psmux"],
            help_marker: Some("psmux v3.3.8 - terminal multiplexer for windows (tmux alternative)"),
        };
        assert_eq!(installed_muxes(&t, &runner).await, vec!["psmux"]);
    }

    #[tokio::test]
    async fn detect_backend_classifies_a_psmux_binary_as_psmux_despite_tmux_mentions() {
        // A configured psmux source re-detects through the same classify: the tmux
        // mentions in psmux's help must not swap it onto a tmux mux over the psmux
        // binary.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(
            Some("psmux v3.3.8 - Terminal multiplexer for Windows (tmux alternative)"),
            Some("tmux 3.3.8"),
        );
        let got = detect_backend(&transport, "psmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "psmux");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(
            got.attach_plan("api"),
            argv(&["psmux", "new-session", "-A", "-s", "api"])
        );
    }
```

- [ ] 3. Confirm red (before the fix, failures are expected; check that the failure shape matches the cause). From the repository root:
  - `cargo test --lib psmux_help`: exactly 2 failures. `psmux_help_naming_tmux_mentions_still_classifies_psmux` fails with left `Some("tmux")` vs right `Some("psmux")`, and `psmux_help_naming_only_tmux_is_inconclusive` with left `Some("tmux")` vs right `None`.
  - `cargo test --lib a_psmux_whose_help_mentions_tmux`: 1 failure, left `[]` vs right `["psmux"]`.
  - `cargo test --lib detect_backend_classifies_a_psmux_binary`: 1 failure, left `"tmux"` vs right `"psmux"`.
  - Everything else passes and compilation is clean. This state is not committed.

### Task 2: Fix classify, update comments, and sync Working Notes (one commit)

- [ ] 1. Replace `classify_identity` in `src/mux/psmux/mod.rs` (around line 50) with the following. The existing comment on `identity_probes` remains true, so leave it as is.

```rust
    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        // psmux's help output names psmux in its banner and usage lines and mentions
        // tmux (it presents itself as a tmux alternative): the mentions are
        // comparative, never an identity claim, so the read drops the tmux name and
        // psmux's own name decides.
        named_mux_excluding(outputs.first()?.as_deref()?, "tmux")
    }
```

- [ ] 2. Replace the doc comment on `named_mux_excluding` in `src/mux/mod.rs` (around line 433) so that it describes both exclusion sites in terms of current behavior (with no change-history narration).

```rust
/// The mux whose name `text` contains, in registry order. `skip` drops one kind's
/// name from the search. tmux's help stage skips itself: real tmux has no `help`
/// command, so a successful help naming a mux names ANOTHER mux - the
/// psmux-behind-a-tmux-alias correction. psmux's help stage skips tmux: psmux's
/// own help output mentions tmux while presenting psmux as a tmux alternative, so
/// those mentions never name the mux.
```

- [ ] 3. Add a bullet at the end of the Module Seams section of `src/mux/psmux/AGENTS.md` (the same place and format as the existing convention where abduco's and screen's AGENTS.md files describe identity detection under Module Seams).

```
- Identity detection is this implementation's own `identity_probes` (one `help`
  question; `-V` mimics tmux's version line so it is never asked) and
  `classify_identity`: psmux's help output names psmux and mentions tmux, so the
  classification drops the tmux name (psmux presents itself as a tmux
  alternative) and psmux's own name decides.
```

- [ ] 4. Documentation check result (explicit): FR-A6/FR-A9 in `docs/requirements.md`, the "mux discovery" item in `CONTEXT.md`, the discovery bullet in `src/mux/AGENTS.md`, and `README.md`/`README.ko.md` describe identity detection at the rule level ("a binary is identified by what it answers as", "each candidate is confirmed only when its own identity probe answers as that candidate") and remain true after the fix. They are therefore left untouched. Only psmux/AGENTS.md described no identity detection at all, which is why the bullet above is added.

- [ ] 5. Confirm green:
  - `cargo test --lib mux::`: all pass. In particular, the four existing alias-correction tests (`where_psmux_answers_a_tmux_alias_is_never_tmux`, `a_machine_serving_tmux_and_psmux_is_offered_both`, `a_binary_that_answers_as_another_mux_is_not_that_mux`, `detect_backend_classifies_psmux_by_help_marker`) must pass unchanged.
  - `cargo test --lib model::host`: the `detect_and_correct` pin tests pass.
  - Run `cargo fmt`, then `cargo fmt --check` is clean.

- [ ] 6. Commit:

```
fix(mux): classify psmux by its own name over the tmux mentions in its help

psmux's help output names psmux and mentions tmux (it presents itself as a
tmux alternative). The identity read matched any registry name anywhere in
the output in registry order, so the tmux mentions classified psmux's own
help as tmux and the psmux candidate never counted as installed. psmux's
classification now drops the tmux name from the search, the way tmux's own
help stage drops itself, so psmux's own name decides. The same classify
serves re-detection of a configured psmux source, which the mentions also
steered onto a tmux mux over the psmux binary.
```

### Task 3: Full CI gate check

- [ ] 1. `cargo fmt --check`: clean.
- [ ] 2. `cargo clippy --all-targets -- -D warnings`: clean. (`named_mux` is no longer called from the psmux module, but it is a glob import and other implementations keep using it, so there is no warning.)
- [ ] 3. `cargo test`: all pass. The psmux enumeration test's local registry read (`~/.psmux`) passes regardless of machine state (with no registry, an empty name set is merged).
- [ ] 4. (Optional, real-machine check) `cargo test --lib mux::tests::detect_backend_live -- --ignored --nocapture`: check by eye that the local `psmux` section prints `Psmux / PerSession`. For the section that needs ssh jupiter00, check the output only.
- [ ] 5. If a gate flags anything, fix it and amend the Task 2 commit (nothing has been pushed yet). If nothing is flagged, this task creates no commit.

## Files to Modify

- `src/mux/psmux/mod.rs` - replace the `classify_identity` body with `named_mux_excluding(out, "tmux")`, add a current-behavior comment to classify, add two classify unit tests to the tests module
- `src/mux/mod.rs` - replace the `named_mux_excluding` doc comment with a current-behavior description covering the two exclusion sites, add two discovery/re-detection level tests to the tests module
- `src/mux/psmux/AGENTS.md` - add one identity detection bullet to Module Seams

## New Files (if any)

None.

## Risks

- **Registry order dependence remains structural.** As the price of not taking Direction 2, if another implementation's output mentions a registry-preceding name (abduco, psmux, and so on), the same defect class can recur. The response pattern then is the same as this one: that implementation's classify drops that name to reflect the meaning of its own output. The ground for the choice was that Direction 2's leading-name preference does not cover the zellij banner for this defect class.
- **A future version whose help contains no "psmux" at all** leaves classify at `None` (inconclusive) and the candidate drops out. That is an honest failure, not a misjudgment; it is a retry target, and no false positive arises.
- **Behavior change scope.** Beyond the discovery defect, this fix also repairs the latent defect where re-detection of a configured (especially remote) psmux source was swapped to `Tmux { bin: "psmux" }`. The test `detect_backend_classifies_a_psmux_binary_as_psmux_despite_tmux_mentions` pins it.
- **The shared judge is unchanged.** `named_mux`, the `known_muxes` order, and the classify of tmux/zellij/abduco/screen are untouched, so the existing alias-correction tests serve as regression pins.
- **Prose conventions.** The comments in the touched files and the Working Notes are all in English convention, so write the additions in English too, and describe current behavior only (no change history, no commit references).
