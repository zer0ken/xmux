# Working Notes: /src/mux/herdr

## Purpose

`mux/herdr` owns both herdr-facing paths: JSON session enumeration and the display
driver that attaches a real herdr client for the selected session. herdr has one
persistent server per session and no control stream, so inventory changes arrive only
on an asked-for poll. Explicit creation starts a detached server and waits for readiness.
An attachment validates the current session listing, resumes an existing stopped
session, and uses `herdr --session <name> client` to connect to its server.

## Module Seams

- The implementation root owns identity, command plans, JSON parsing, and enumeration.
- The display driver owns reattachment.
- The client query owns where xmux's own client is: herdr moves a client between saved
  SSH machines from inside it and records the choice in one endpoint selection per user
  and machine, so the query reads that selection together with the user's herdr
  processes.

## Invariants

- Enumeration issues exactly one `herdr session list --json` command.
- Every entry without a connection error becomes a session, a stopped entry a stopped
  session, except the stopped reserved `default` entry, which herdr lists on every
  machine.
- A session change xmux makes creates a fresh attachment; no in-place switch is claimed.
  herdr sizes a session by the client that last had input and has no way to leave one
  out.
  An attachment whose client the user already moved to the selected session is kept.
- The endpoint selection is attributed to xmux's client only while it is the user's one
  herdr client on that machine. A saved machine on the client's own machine is one of
  the host's sessions; any other is a place no card covers.
- An attachment ending is not reported as a session ending.
- A missing session fails attachment with a notification and no server creation.
  Creation belongs to the explicit new-session action; a saved stopped session can
  be resumed when its card is executed. Preparation stays off the runtime thread.

## Common Pitfalls

- Do not invent window or attachment metadata that herdr does not report.
- Do not strip every `HERDR_` variable; user configuration must survive.

## Verification

- Exercise running, stopped, connection-error, unknown-field, and malformed listings,
  and confirm a poll emits one session event and no per-session query.
