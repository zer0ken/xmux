# Popup Unification Design

## Goal

Every action that needs extra user input is presented as a centered modal
popup. The bottom input pane and the footer kill-confirm are removed. All
popups draw through one shared primitive, so a single fix makes them opaque,
and modal popups can be dragged by their border to reposition.

## Scope

Four changes, all within `src/ui/switcher.rs` plus mouse routing in
`src/cockpit.rs`:

1. **Opaque popup fix.** A popup drawn over a colored grid must not let the
   grid's background bleed through its interior.
2. **Reusable popup primitive.** `render_popup` is already shared by help and
   menu; it becomes the single renderer for help, input, and confirm too.
3. **Input/confirm as popups; remove the bottom pane.** `self.input`
   (filter/rename/new/new-window/split) and `self.pending_kill` render as
   centered popups. The bottom input pane and the footer kill-confirm are
   deleted.
4. **Drag to move.** Modal popups (help, input, confirm) move when their
   border is dragged. The context menu stays anchored (its press-hold-release
   gesture conflicts with a move-drag).

## Current state (for reference)

- `render_popup(frame, area, rect, title, lines)` — opaque bordered box, used
  by `render_help` and `render_menu`.
- Help: `show_help: bool`, centered modal, static key rows.
- Menu: `self.menu`, anchored at the click, press-hold-release.
- Input: `self.input: Option<Input>` rendered as a **bottom full-width pane**
  via a dedicated layout branch in `render()` plus `render_input`,
  `render_input_divider`, `input_desc_lines`. Keys via `handle_input_key`.
  Filter editing also uses this pane.
- Kill confirm: `self.pending_kill` rendered as **red footer text**; keys via
  `resolve_kill`.

## Design

### 1. Opaque popup (root-cause fix in `render_popup`)

Reproduce headlessly first (systematic-debugging): paint a grid cell with a
non-default background, draw a popup over it, assert every interior cell has
`bg == Color::Reset` and `symbol == " "`. If the assertion fails, fix
`render_popup` so it paints a fully opaque fill across the whole `rect` before
the bordered block (e.g. an explicit space-fill with `Style::reset()`), not
relying on `Block::style` alone.

The fix lives in the one shared primitive, so help, menu, and the new
input/confirm popups are all cured at once and cannot regress independently.

### 2. Reusable primitive

Keep `render_popup` as a free function (no new struct/widget — it is already
the reuse point). Callers pass the already-offset `rect`; the drag offset is
applied by the caller via `centered_rect(...)` shifted by `popup_offset` and
clamped to `screen_area`.

### 3. Input and confirm popups; delete the bottom pane

- **Input popup.** When `self.input.is_some()`, draw a centered popup titled
  by the mode (`rename session`, `new window …`, `filter sessions`, …). Body
  lines: the label, an `❯ {buffer}` entry line, and an `Esc to cancel` hint.
  Filter keeps its live behavior (the tree re-filters as the buffer changes);
  the popup is centered and small, so the tree stays visible around it.
- **Confirm popup.** When `self.pending_kill.is_some()`, draw a centered
  popup titled `kill?` with body `kill {addr}?` and `[y]es / [n]o · Esc
  cancel`, styled red.
- **Deletions** (AS-IS removal — git holds the history):
  - the bottom-pane layout branch in `render()`,
  - `render_input`, `render_input_divider`, `input_desc_lines`,
  - the `input_focused` special case in `render_divider`,
  - the `pending_kill` branches in `footer_text` and the red styling in
    `render_footer`.
- **Unchanged:** key routing (`handle_input_key`, `feed_help_key`,
  `resolve_kill`) and the cockpit's `is_inputting()` focus lock. Only the
  render surface moves.

### 4. Drag to move

State on `Switcher`:
- `popup_offset: (i16, i16)` — reset to `(0, 0)` whenever a modal popup opens.
- `popup_drag: Option<{ grab: (u16, u16), origin_offset: (i16, i16) }>`.

A modal popup's drawn rect = `centered_rect(w, h, area)` translated by
`popup_offset`, clamped fully inside `screen_area`.

Mouse routing (`src/cockpit.rs`, `resolve_mouse_chain`): add a modal-popup
gate before the menu and divider-drag gates. When a modal popup is open and a
press lands on a **border cell** of its rect, begin a drag (record grab point
and current offset). Subsequent moves update `popup_offset = origin_offset +
(cursor − grab)`. Release ends the drag. While dragging, all mouse events
route to the popup, mirroring the existing menu/divider-drag gesture handling.

The menu is not draggable.

## Components and boundaries

- `render_popup` — pure draw of a titled opaque box at a rect. No state.
- `Switcher` popup state — `show_help`, `input`, `pending_kill`,
  `popup_offset`, `popup_drag`, plus small helpers: `popup_rect()` (centered +
  offset + clamp), `on_popup_press/move/release`, `is_modal_popup_open()`.
- Cockpit mouse chain — decides whether a mouse event is a popup drag before
  falling through to menu/divider/focus.

## Testing

Headless (`TestBackend`), in `src/ui/switcher.rs`:
- popup interior is opaque over a colored grid (regression for #1),
- input popup renders centered with the prompt and Esc hint,
- confirm popup renders centered, red, with y/n,
- a border-press + move shifts the popup rect; a release ends the drag,
- a press on the interior (not the border) does not start a drag.

Real-terminal visual confirmation (the actual screen handover, drag feel) is a
human gate.

## Out of scope

- Resizing popups (only moving).
- Persisting a moved position across reopens (offset resets each open).
- Making the context menu draggable.
