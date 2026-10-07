# Working Notes: /src/display/vt100

## Purpose

This directory is the `vt100` crate, version 0.16.2 by Jesse Luehrs under the MIT
license kept beside it, carried inside xmux as the parser the grid runs a session's
output through. The published crate keeps no OSC 8 link per cell and offers no hook on
printed text that would let the grid attach one from outside, and a crate xmux
publishes cannot depend on a patched copy kept elsewhere, so the parser lives here.

## Differences From the Published Crate

- Every cell keeps the OSC 8 link it was written under, and the screen names the URI
  behind it. A screen keeps at most 4096 distinct URIs; a link opened past that is
  written as plain text.
- An SGR reset leaves an open OSC 8 link open, as a terminal does, because the link is
  not part of the rendition.
- Paths name this module instead of the crate root, the crate's own lint settings and
  documentation example are left out, and the formatting is xmux's.

## Before Editing

- Keep a change to the parser to what xmux needs from it and add it to the list above,
  so the distance from the published crate stays known.
