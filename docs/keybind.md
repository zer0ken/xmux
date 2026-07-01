# App global picker hotkey

Under the app (entered via `xmux`), a built-in prefix hotkey opens the
cross-environment picker — from ANY session, local or remote, with nothing
installed on the remote host.

**Default binding:** `Ctrl-g` then `s` (mnemonic: switch).

    Ctrl-g s    open the cross-environment picker
    Ctrl-g g    send one literal Ctrl-g byte through to the inner application

Press the prefix twice (`Ctrl-g Ctrl-g`) to send one literal prefix byte through
to the inner application. A lone prefix not followed by a recognised action key
is also forwarded after a brief timeout, so a reflexive `Ctrl-g` still reaches
readline/emacs.

**Change the prefix:** set `XMUX_PREFIX` to a `C-<x>` spec before launching:

    XMUX_PREFIX=C-Space xmux

Accepted specs: `C-g`, `C-Space`, `C-b`, and any `C-<printable>`.

## What the picker does

- **Peek and cancel** — `Esc` closes the picker; you are back in your session,
  untouched.
- **Same-server teleport** — pick a local session: xmux switches the client
  in-place (`switch-client`), no detach.
- **Cross-host re-attach** — pick a remote session: xmux tears down the current
  attach and opens a new one to the remote host, all within the same terminal
  window — no manual detach and no `xmux` command needed on the remote.
