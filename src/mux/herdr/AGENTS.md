# Working Notes: /src/mux/herdr

## Purpose

`mux/herdr` owns both herdr-facing paths: JSON session enumeration and the display
driver that attaches a real herdr client for the selected session.

## Mental Model

herdr has one persistent server per session. A stopped listing entry is saved state,
not a live session, and is not offered. A listing entry with a connection error is also
not offered because xmux cannot back it with a reachable live server.

One `herdr session list --json` command is the complete metadata answer. It reports no
window count or attachment state, so offered sessions keep the domain defaults for both.
There is no control stream, so inventory changes arrive only on an asked-for poll.

Creating a session is completed by its first display attachment. The create operation
uses the listing command as a prompt health check, then reselects the requested session;
the normal display path runs `herdr session attach <name>`, which starts a stopped or
missing session before attaching.

## Module Seams

- The implementation root owns identity, command plans, JSON parsing, and enumeration.
- The display driver owns reattachment and stays behind the mux-agnostic driver seam.
- The transport decides whether commands run locally, through ssh, or through WSL.
- The per-session server model describes both server and display behavior.

## Invariants

- Enumeration issues exactly one `herdr session list --json` command.
- Only running entries without a connection error become session cards.
- Every display selection creates a fresh attachment; no in-place switch is claimed.
- The first attachment creates a requested session when no live server exists.
- An attachment ending is not reported as a session ending.

## Common Pitfalls

- Do not offer the always-present stopped `default` entry as a live session.
- Do not invent window or attachment metadata that herdr does not report.
- Do not launch `herdr --session <name> server` as a create command; it does not return.
- Do not strip every `HERDR_` variable; user configuration such as
  `HERDR_CONFIG_PATH` must survive.

## Before Editing

- Decide whether the change affects JSON enumeration, a command plan, or display
  reattachment.
- Keep transport wrapping out of the mux implementation.
- Preserve the one-command enumeration boundary.

## Verification

- Pin identity, attach, create, and list command plans.
- Exercise running, stopped, connection-error, unknown-field, and malformed listings.
- Confirm a poll emits one session event and no per-session query.
- Set `XMUX_LOG=xmux::mux::herdr=debug` to trace display decisions.
