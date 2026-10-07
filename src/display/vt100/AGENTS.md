# Working Notes: /src/display/vt100

## Purpose

This directory is the `vt100` crate, version 0.16.2 by Jesse Luehrs under the MIT
license kept beside it, carried inside xmux as the parser the grid runs a session's
output through, so the grid can keep what the published crate does not model: a crate
xmux publishes cannot depend on a patched copy kept elsewhere.

## Differences From the Published Crate

- Paths name this module instead of the crate root, the crate's own lint settings and
  documentation example are left out, and the formatting is xmux's.

## Before Editing

- Keep a change to the parser to what xmux needs from it and add it to the list above,
  so the distance from the published crate stays known.
