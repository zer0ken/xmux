# Working Notes: /src/mux/tuios

## Purpose

`mux/tuios` owns both tuios-facing paths: JSON session enumeration and the display
driver that attaches a real tuios client for the selected session.

## Mental Model

tuios has one daemon per user and that daemon owns every session. Its display behavior
still has per-session semantics: no command can name and retarget a particular client,
so every selection is shown with a fresh `tuios attach <name>` client.

One `tuios ls --json` command is the complete metadata answer. The listing carries each
session's window count and attachment state. Saved records are not live sessions and are
not offered. There is no control stream, so inventory changes arrive only on an
asked-for poll.

Ending an attachment does not end its daemon session. A detach or client quit therefore
has no session-death push; a later asked-for poll supplies the current inventory.

## Module Seams

- The implementation root owns identity, command plans, JSON parsing, and enumeration.
- The display driver owns reattachment and stays behind the mux-agnostic driver seam.
- The transport decides whether commands run locally, through ssh, or through WSL.
- The per-session server model describes display behavior here, not daemon topology.

## Invariants

- Enumeration issues exactly one `tuios ls --json` command.
- Exit 3 means no live daemon and produces an empty live-session list.
- Saved records never become session cards.
- Every display selection creates a fresh attachment; no in-place switch is claimed.
- An attachment ending is not reported as a session ending.

## Common Pitfalls

- Do not query windows per session; the aggregate listing already carries the needed
  count and another command would violate the one-command poll.
- Do not use `-V`; tuios identifies itself with `--version`.
- Do not expose `TUIOS_SESSION` as display truth. The client does not rewrite it when it
  moves between sessions.
- Do not model tuios as shared display behavior merely because its daemon is shared.

## Before Editing

- Decide whether the change affects JSON enumeration, a command plan, or display
  reattachment.
- Keep transport wrapping out of the mux implementation.
- Preserve the one-command enumeration boundary.

## Verification

- Pin identity, attach, create, and list command plans.
- Exercise live JSON, saved records, exit 3, and real failures.
- Confirm a poll emits one session event and no per-session query.
- Set `XMUX_LOG=xmux::mux::tuios=debug` to trace display decisions.
