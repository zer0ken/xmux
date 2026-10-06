# Working Notes: /src/mux/herdr

## Purpose

`mux/herdr` owns both herdr-facing paths: JSON session enumeration and the display
driver that attaches a real herdr client for the selected session. herdr has one
persistent server per session and no control stream, so inventory changes arrive only
on an asked-for poll. Creating a session is completed by its first display attachment:
the create operation runs the listing as a prompt health check and reselects the
requested session, and `herdr session attach <name>` starts it.

## Module Seams

- The implementation root owns identity, command plans, JSON parsing, and enumeration.
- The display driver owns reattachment.

## Invariants

- Enumeration issues exactly one `herdr session list --json` command.
- Only running entries without a connection error become session cards.
- Every display selection creates a fresh attachment; no in-place switch is claimed.
- An attachment ending is not reported as a session ending.

## Common Pitfalls

- Do not invent window or attachment metadata that herdr does not report.
- Do not strip every `HERDR_` variable; user configuration must survive.

## Verification

- Exercise running, stopped, connection-error, unknown-field, and malformed listings,
  and confirm a poll emits one session event and no per-session query.
