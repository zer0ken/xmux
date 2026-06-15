# In-mux popup switcher

xmux is the cockpit: you enter every environment through the xmux tree, and an
in-mux popup lets you peek the tree and switch without losing your current pane.

Bind a key your mux does not use by default to open the popup switcher:

    # ~/.tmux.conf (tmux) or ~/.psmux.conf (psmux)
    bind g display-popup -E "xmux popup"

`g` is unused in the default tmux/psmux key table; `e` or `j` work too. Avoid
`w` / `s` / `d` — those are bound (choose-tree window, choose-tree session,
detach). `display-popup -E` runs xmux in an overlay and closes it when xmux
exits; size it with `-w 80% -h 80%` if you want.

## What the popup does

`prefix g` opens the full cross-environment tree as an overlay on top of your
current pane:

- **Peek and cancel** — `Esc` closes the popup; you are back in your pane,
  untouched.
- **Same-server session** — pick it: xmux teleports the client instantly
  (`switch-client`), no detach, no flicker.
- **Another server** — pick it: xmux detaches; the still-running xmux home tree
  re-renders, where you complete the jump. The session keeps running on its
  server.

## Why a popup and not just attach

A mux cannot move a client across servers, and it refuses to nest (attaching a
mux from inside a mux). So the in-mux path switches rather than attaches:
same-server is an instant `switch-client`; cross-server is a plain
`detach-client` back to the home tree.

## Cockpit precondition

The cross-server path needs a home tree underneath, which exists only when you
entered through `xmux`. In a session attached some other way, the popup's peek
and same-server teleport still work, but a cross-server choice detaches to the
shell — run `xmux` to re-enter the cockpit.
