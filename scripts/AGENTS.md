# Working Notes: /scripts

## Purpose

Developer tools that regenerate checked-in artifacts or test the app from outside.
Nothing here runs inside xmux.

## Module Seams

- `install/` holds the install scripts each release publishes.
- `demo/` regenerates the README demo GIFs in an isolated container.
- `e2e/` runs the end-to-end suite against Docker hosts.
- `braille_x_prototype/` is the HTML source of the Braille animation atlas, and the
  generator script at this level renders it into `src/ui/braille_x/`.

## Invariants

- A regenerated artifact is committed together with any change to its source.
