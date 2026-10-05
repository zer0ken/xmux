# ADR 0007: Context Follows the User's Interest

## Status

Accepted

## Context

The nav list changes under the user without the user doing anything. A scan
answers host by host, a host becomes reachable or unreachable, a logout closes a
host's connections, a mux session ends, a filter narrows the list. Each change
re-derives the cards, and the card the user was on can vanish or be replaced.

When a path picked the selection's next card on its own, the result depended on
which path ran: one landed on the first card of the list, another on the previous
card, another on a parked host. The user could not predict where the cursor would
land, and because the display follows the selection, an unrelated session could
appear in the terminal view.

## Decision

The selection names what the user is interested in, never a position in the
list: a host, a source, or a session. On the nav it stands on that node's target: a
session's card, a source's card or the source half of its section title, and a host's
card or the host half of a title or source card. xmux keeps that interest as one value
and resolves the selection from it on every rebuild, by two rules.

**A card disappears.** When the selected node loses its target, the selection moves
to the nearest node up its lineage that has one:

- a session goes to its source: the section title, or the source's host-state card
  once the source has no session to show;
- a source goes to its host: the host's card while the host is down, else the host
  half of the row the source stood on, else of the host's first row;
- a card that stood for the whole host (the host's card while it was down, or the
  card of the source named by the host alone) that resolved into sources hands the
  selection to the first of them by name;
- when nothing of the host survives, the selection goes to the card that now holds
  the vanished card's place: the first card after it in the prior card order that
  survived, else the last surviving card before it.

A source keeps one identity whether it shows as a section title or as a host-state
card, so a host-state card that resolves into sessions hands the selection to its
own section title. A host none of whose sources connected shows as one card, so a
logout or an unreachable host gathers a selection on any of its sources or sessions
onto that card.

A node a screen link or a step down opened with no nav target of its own, such as a
source of a host that is down, stays selected while the inventory still lists it. The
nav then stands on the target of its nearest ancestor, and the rules above apply once
the node itself leaves the inventory.

**A card appears.** A new card takes the selection only when the interest names it:

- the session `prefix n` created;
- the session that was under the selection when a full re-scan cleared every
  session, when its source streams it back;
- at launch, before anything is chosen, the first session card to appear. Once one
  appears the interest settles on it, so a session answering later does not take the
  selection.

Any other new card leaves the selection where it is. An awaited session ends as the
interest when the user moves the selection or when its source answers without it.

Selecting a session by address, as `ctl switch` and a mux follow do, is not an
awaited interest. It moves the selection when that session's card is on the list and
does nothing otherwise.

## Consequences

Every path that changes the list (scan, re-scan, poll, logout, a session ending, mux
discovery, a filter) resolves the selection through these two rules. A path does not
choose a fallback of its own. The first card of the list is taken only when the
selection names no card of the prior list or no card of the prior list survives.

The display follows the selection, or the soft selection while the pointer rests on a
nav target (ADR 0008), so the terminal view never shows a session the user neither
chose, pointed at, nor was led to by these rules.

A change to how the selection moves is a change to the lineage or to what counts as
interest, made here and in the selection entry of `CONTEXT.md`, never as a special
case in one code path.
