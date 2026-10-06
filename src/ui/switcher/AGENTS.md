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
- Every card, screen, screen link, and lineage step derives from the three levels: a
  host with no source has a card and a screen of its own and links to no host, and the
  first source found on it takes over its card and selection.
