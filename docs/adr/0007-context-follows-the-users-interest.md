# ADR 0007: Context Follows the User's Interest

## Status

Accepted

## Context

The nav list changes under the user without the user doing anything. A scan
answers host by host, a host becomes reachable or unreachable, a logout closes a
host's connections, a mux session ends, a filter narrows the list. Each change
re-derives the cards, and the card the user was on can vanish or be replaced.

When the selection did not survive such a change it fell to the first card of the
list. The user could not predict where the cursor would land, and because the
display follows the selection, an unrelated session could appear in the terminal
view. At launch the same mechanism attached whichever session answered the scan
first. Neither outcome was chosen by the user, so both read as random.

## Decision

The selection names a thing the user is interested in, never a position in the
list. xmux keeps that interest as one value and moves the selection by two rules
that read from it.

**A card disappears.** When the selected card leaves the list, the selection moves
to the nearest surviving card related to it, along the card's own lineage:

- a session card goes to its source's section title, whose screen shows the source;
- a section title goes to its host's card, or to that host's first surviving section
  title in card order when the host has no card of its own;
- a host-state card that resolves (an unreachable host that answers, a scanning
  host that settles) goes to that host's first section title in card order;
- when nothing of the host survives, the selection goes to the card that now holds
  the vanished card's place in card order.

**A card appears.** A new card takes the selection only when it matches the user's
interest:

- the user asked for it: the session `prefix n` created, the target of a switch, the
  host the user logged into;
- it continues what the user is looking at: the cards a selected host-state card
  resolves into.

A card unrelated to the interest never moves the selection.

Host cards and section titles are reused by identity across every rebuild, so a
selection on one holds for as long as the thing it names exists, and the lineage
always has somewhere to go.

## Consequences

Every path that changes the list (scan, re-scan, poll, logout, a session ending, a
filter, a ctl command) resolves the selection through these two rules. A path does
not choose a fallback of its own, and a fallback to the first card of the list does
not exist.

The display follows the selection, so the terminal view never shows a session the
user neither chose nor was led to by these rules.

A change to how the selection moves is a change to the lineage or to what counts as
interest, made here and in the selection entry of `CONTEXT.md`, never as a special
case in one code path.
