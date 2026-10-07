# Working Notes: /src/mux/tuios

## Purpose

`mux/tuios` owns both tuios-facing paths: JSON session enumeration and the display
driver that attaches a real tuios client for the selected session. One tuios daemon per
user owns every session, but no command can name and retarget a particular client, so
every selection is shown with a fresh `tuios attach <name>` client. There is no control
stream, so inventory changes arrive only on an asked-for poll.

## Module Seams

- The implementation root owns identity, command plans, JSON parsing, and enumeration.
- The display driver owns reattachment.
- The per-session server model describes display behavior here, not daemon topology.

## Invariants

- Enumeration issues exactly one `tuios ls --json` command.
- Exit 3 means no live daemon. Its stdout still lists the sessions the daemon saved,
  and each saved record becomes a stopped session; a stdout that is no listing lists
  nothing.
- Every display selection creates a fresh attachment; no in-place switch is claimed.
- No attach flag keeps a client from sizing a session; `daemon.window_size` alone decides.
- An attachment ending is not reported as a session ending.

## Common Pitfalls

- Do not query windows per session; the aggregate listing already carries the count.
- Do not expose `TUIOS_SESSION` as display truth; the client does not rewrite it when
  it moves between sessions.
- Do not model tuios as shared display behavior merely because its daemon is shared.

## Verification

- Exercise live JSON, saved records, exit 3, and real failures, and confirm a poll
  emits one session event and no per-session query.
