# Working Notes: /src/display/image

## Purpose

The display layer's image protocol handling and terminal painting.

## Module Seams

- The output scanner separates image control strings from the grid's text input.
- Protocol storage associates transmitted images with the attached grid.
- The painters place only image cells that survive frame composition.

## Invariants

- Protocol parsing retains incomplete control strings across output reads.
- Image support follows the outer terminal's confirmed capabilities.
- Kitty transmissions on the outer terminal use xmux-owned image identifiers.
