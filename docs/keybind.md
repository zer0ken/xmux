# Cockpit global overlay hotkey

Under the cockpit (entered via `xmux`), a built-in prefix hotkey opens the
cross-environment picker as a fast LOCAL overlay over your current pane — from
ANY session, local or remote, with nothing installed on the remote host.

**Default binding:** `Ctrl-g` then `s` (mnemonic: switch).

    Ctrl-g s    open the cross-environment picker overlay
    Ctrl-g g    send one literal Ctrl-g byte through to the inner application

Press the prefix twice (`Ctrl-g Ctrl-g`) to send one literal prefix byte through
to the inner application. A lone prefix not followed by a recognised action key
is also forwarded after a brief timeout, so a reflexive `Ctrl-g` still reaches
readline/emacs.

**Change the prefix:** set `XMUX_PREFIX` to a `C-<x>` spec before launching:

    XMUX_PREFIX=C-Space xmux

Accepted specs: `C-g`, `C-Space`, `C-b`, and any `C-<printable>`.

## What the cockpit overlay does

- **Peek and cancel** — `Esc` closes the overlay; you are back in your pane,
  untouched.
- **Same-server teleport** — pick a local session: xmux switches the client
  in-place (`switch-client`), no detach.
- **Cross-host re-attach** — pick a remote session: xmux tears down the current
  attach and opens a new one to the remote host, all within the same terminal
  window — no manual detach and no `xmux` command needed on the remote.

---

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
