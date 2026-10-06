# Working Notes: /src/ui/braille_x

## Purpose

`braille_x/` holds the precomputed frame atlas of the Braille X animation that the
initial scan and the view screens show. The frames are sampled once and committed as
data, so the TUI loop never rasterizes a font or meets an emoji fallback.

## Module Seams

- The atlas is raw bytes: one byte per terminal cell, each the dot pattern of one
  Braille glyph, laid out frame after frame at a fixed frame size.
- The `ui` animation module embeds the atlas at build time and owns its frame size,
  frame count, timing, and placement; nothing else reads it.

## Invariants

- The atlas holds every complete frame the animation indexes; its length is the frame
  count times the frame size, and the frame order is the approved symbol order.
- The frames are monochrome Braille: the paint writes glyphs only, never a colour.
