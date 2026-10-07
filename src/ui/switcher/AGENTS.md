# Working Notes: /src/ui/switcher

## Purpose

`switcher/` is the interactive session switcher. The UI is immediate-mode, so this
directory owns its state machine: the flattened card model, the selection and the
interest it is resolved from, key and mouse handling, and one render pass that draws
to the live terminal or to the headless backend behind the ctl `dump`.

## Module Seams

- The switcher state holds the cards, the hard and soft selections, the interest, and
  the hierarchy trail; key handling, mouse handling, and rendering each extend it.
- Card geometry is pure and backend-free: the side list places cards by their heights,
  and the band places them in column flow. Each returns rects that the paint, the mouse
  hit-test, and the tests read.
- The test suites drive the switcher through keys, mouse, and a test backend, split by
  concern: position independence, the hierarchy, and the selection lineage.

## Invariants

- Paint and hit-test read the same geometry answer; rendering never computes a second.
- Every rebuild resolves the selection from the one interest; no path picks a fallback
  card of its own.
- A background event moves the selection only up its lineage, when the node it names
  is lost, and never down or sideways; with nothing of its machine left, the selection
  names nothing.
- A screen's links are its children, then its actions, then its link up, and the arrows
  cycle through them. An action link runs through the key its row writes, so a link and
  its key cannot do two different things.
- Every card, screen, screen link, and lineage step derives from the three levels: a
  machine with no host has a card and a screen of its own and links to no host, and the
  hosts found on it take over its card while the selection stays on the machine.
