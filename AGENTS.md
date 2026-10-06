# Working Notes: /

## Purpose

xmux is a Rust terminal multiplexer switcher: the app owns the terminal, keeps mux
display attachments alive, renders the split view, and serves `ctl-<name>.sock`. Two
orthogonal axes, `Transport` (HOST) and `Mux` (MUX), describe every connection, and argv
is composed from a source's own transport and mux, so neither knows the other.
Vocabulary is in `CONTEXT.md` and design principles in `docs/principles.md`.

## Module Seams

- `src/app/` - the application model and its update transition, the runtime loop, the
  ctl server, and preference persistence.
- `src/cli/` - argument parsing and command dispatch, behind one public entry.
- `src/provision/` - the config, the roster, the source probe, and the resolved view.
- `src/transport/` - the HOST axis: the `Transport` trait and its implementations.
- `src/mux/` - the MUX axis: the `Mux` trait and one directory per mux.
- `src/model/` - runtime domain values, the action and command sets, the operation port.
- `src/display/` - the display path: PTY attachment, the grid, and terminal input.
- `src/link/` - the metadata path: per-source channels, mux operations, ctl protocol.
- `src/ui/` - nav rows, off-loop operations, interaction, and rendering.
- `src/state/` - domain state and the action reducer the update transition uses.

## Invariants

- **Layer Direction.** Every `crate::<top>` edge follows this table; a file belongs to
  the directory directly below `src/` or to the root module its `.rs` file names. The
  architecture check covers `#[cfg(test)]` modules and has no known exceptions.

| Importing module | Allowed target modules |
| --- | --- |
| `display`, `driver`, `link`, `logging`, `model`, `mux`, `provision`, `session`, `transport` | the same set |
| `state` | the same set plus `state` |
| `app`, `cli`, `lib`, `main`, `ui` | every module |

- **View Purity.** Rendering reads the application model and writes only the frame,
  through an immutable `RenderPlan` that paint and hit-testing both consume and neither
  mutates.
- **Single Update Owner.** Only the update transition mutates application state. Every
  input except raw terminal bytes is a message to it, and it emits one effect type that
  one exhaustive executor runs in order.
- Nothing above the driver seam branches on a mux kind.
- Blocking process, PTY, and pipe work whose duration another party sets stays off the
  single-threaded runtime.
- Public ctl verbs resolve to domain actions; raw key injection stays behind `raw:`.

## Before Editing

- Follow the existing seam before widening it.
- Check the change against the glossary, the principles, and every touched directory's
  Working Notes; where docs and code disagree, the docs are the intent, and a doc
  changes only with the maintainer's sign-off.
- Write commits, pull requests, issues, and release notes in English
  (`docs/AGENTS.md`); a pull request title becomes a release note line.

## Verification

- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and the whole
  test suite, and exercise the touched behavior from a key and from the ctl verb.
