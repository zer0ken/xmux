# Keybindings

`prefix ?` opens the in-app help with every key and glyph. This document lists the
keys and describes what the help does not show: how the nav is placed and walked, what
the longer commands do, the prefix indicator, popups, toasts, the mouse, and
automation.

## Everyday Keys

The nav takes these keys while it holds focus:

| Key                      | Action                                                                   |
| ------------------------ | ------------------------------------------------------------------------ |
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends)                                       |
| `←` / `→` (or `h` / `l`) | previous / next `machine/mux` host section, the machine cards counting as one |
| `Home` / `End`           | jump to the first / last card                                            |
| `PageUp` / `PageDown`    | jump ten cards                                                           |
| `Enter`                  | move focus into the selected session's terminal view                     |
| `prefix 1`-`prefix 9`    | jump to card number (keep typing for 10+)                                |
| `prefix n`               | new session on the selected host                                         |
| `prefix /`               | filter cards (fuzzy)                                                     |
| `prefix r`               | rescan this machine: the selected card's machine and its hosts           |
| `prefix R`               | rescan all machines: refresh which machines exist, and every host's sessions |
| `prefix L`               | log out of this machine (an SSH machine)                                 |

xmux has its own prefix, like tmux's `set -g prefix`. The default is `Ctrl-g`,
and `[ui] prefix` replaces it. A chord is the prefix followed by one key:

| Chord        | Action                                                  |
| ------------ | ------------------------------------------------------- |
| `prefix q`   | quit xmux                                               |
| `prefix ?`   | help and glyphs (type to search)                        |
| `prefix m`   | message history of results and events                   |
| `prefix Tab` | toggle focus between the nav and the terminal view      |
| `prefix p`   | place nav on the next side of the view                  |

Pressing the prefix opens a box beside the prefix indicator that lists every key it
unlocks. A click on a card selects it, and a click on the terminal view focuses it.
The first key pressed after installation briefly points out the configured prefix and
help key. xmux records that the introduction has been shown.

## The Prefix

The prefix is read only from `[ui] prefix` in `~/.config/xmux/config.toml`, with no
environment-variable override. It accepts `C-<letter>` (for example `C-g`, `C-b`,
`C-a`) and `C-Space`, and anything unrecognised falls back to `C-g`. The prefix is a
single control byte, so it never collides with typed text, and a prefix pasted as data
(bracketed paste) passes through untouched.

## Other Keys

| Key | Action |
|---|---|
| `Ctrl-↑` / `Ctrl-↓` | move up a level (session, host, machine) / back down to the child |
| `prefix i` (bare `i` in the nav) | select the current host and show its screen |
| `prefix r` | rescan this machine: the selected card's machine alone |
| `prefix h` | open the table of machine problems |
| `prefix :` | open the command palette |
| `prefix t` | toggle auto-hide-nav |
| `prefix z` | collapse or expand the nav |
| `prefix Ctrl-←` / `prefix Ctrl-→` | move a side nav's view border, then bare `Ctrl-←` / `Ctrl-→` keep resizing for a moment |
| `prefix Ctrl-↑` / `prefix Ctrl-↓` | move a band's view border, then bare `Ctrl-↑` / `Ctrl-↓` keep resizing for a moment |
| the prefix arrow pair facing the terminal | focus the terminal view |
| the other prefix arrow pair | focus the nav |
| `prefix prefix` | send one literal prefix byte to the focused pane |
| `d` on an unreachable screen | unfold the full failure diagnostic |

When the terminal view has focus, every key that is not a prefix chord reaches the
session's active pane unchanged.

## Nav Placement

The nav rides on one of four sides of the terminal view: a left or right column, or a
top or bottom band. `[ui] nav-position` (`left`, `top`, `right`, or `bottom`; an unknown
word falls back to `left`) names the default, and `prefix p` pins the next side
clockwise in `~/.xmux/nav_position` until the key cycles back to the default. The nav
never moves on its own.

The layout inside the nav is identical at every side: a right column is the same list
as a left one, and a bottom band is the same down-then-right flow as a top one, a whole
section per column. A band column that continues a split section repeats its title on
its top row followed by `…`, and a band one row tall runs titles and cards along its row
and scrolls sideways.

The prefix arrow pair facing the terminal's side focuses the terminal: `prefix →` and
`prefix ↓` with the nav on the left or above, `prefix ←` and `prefix ↑` with it on the
right or below. The resize keys move the view border the way they point, so the nav grows
on a left or top nav and shrinks on a right or bottom one. An expanded side nav is never
narrower than a card's indent, a two-digit number, and eight cells of name; a band is at
least one row.

`prefix z` collapses the nav to its prefix indicator: a side nav keeps a column as wide
as the prefix, with the view border running down its edge, and a band keeps only its
view border row. Dragging the view border past the minimum collapses the nav too, and
dragging back out in the same drag expands it. A click anywhere on the collapsed nav, or
focusing the nav by keyboard, expands it. Auto-hide takes the whole nav away and returns
it in the state it left.

A nav that would leave the terminal view smaller than 24 columns by 4 rows, the size xmux
draws in at all, hides the way auto-hide hides it whenever the terminal view holds the
focus, whatever auto-hide is set to. In a small window the view the user works in then
has the whole window: focusing the nav or pressing the prefix brings the nav back.

## Walking the Nav

The nav is a list of numbered cards in sections, not a tree. `←`/`→` step one section:
a host with sessions, entered at its first session, or the whole band of machine cards
at once, entered at its first card, so a run of idle machines is one stop rather than a
long walk. Both steps wrap, and neither depends on where a card sits on screen, so they
mean the same thing in a column and in a band. The card step never stops on a section
title, and a title takes no number.

The hierarchy of sessions, hosts, and machines is reached three ways: `Ctrl-↑`/`Ctrl-↓`,
the two parts of a section title, and the links on a machine's or a host's screen. A
host is selected on the `{mux}` part of its title, or on its card when it has no
sessions; a machine on the `{machine}` part, or on its card when none of its hosts
connected. `Ctrl-↓` returns to the child the walk came from, else the first host by
name or the first session in card order. From a title part, `↑`/`↓` go to the adjacent
card and `←`/`→` to the adjacent section. A bare `Ctrl-↑`/`Ctrl-↓` right after
`prefix Ctrl-↑`/`prefix Ctrl-↓` still resizes the band.

The terminal view shows a machine screen for a machine and a host screen for one mux
on it, each headed by its level and path, such as `machine db-01` or `host db-01/tmux`.
A machine screen states how the machine is reached and logged in to; a host screen
states the host's sessions and how they stay current. While the terminal view shows
either screen, `↑`/`↓` (and `Tab`) step through its links and `Enter` opens the
selected one. A machine screen links each of its hosts whose mux is confirmed, and
none while no mux is; a host screen links its machine and each of its sessions. While
a machine screen shows the login pane, the pane takes those keys and its links answer
only a click.

At launch the terminal view shows the landing screen in place of a session: how many
machines the scan has reached, and every nav card under its number as a
`machine/mux/session` path. The landing list and the nav share one selection, which only
highlights there. The first execution (`Enter`, a click on a nav card or a landing card,
a landed `prefix <digit>` jump, or a ctl `switch`) closes the landing screen for the
rest of the run, opens the chosen card, and focuses the terminal view.

## Commands

`prefix n` starts a session on the host of the selected card, a host's card or a
session card alike, and is refused under an unreachable machine or host. An empty name
is assigned by the mux where it names its own sessions, otherwise by xmux as an
`<adjective>-<noun>` no session on that host holds.

`prefix r` asks the selected card's machine again and nothing else: its reachability
probe, then every host it serves. Its cards keep their sessions and numbers meanwhile,
and it reports in one toast titled `rescan machine <machine>`. It is refused while that
machine is still scanning and while another re-scan has not reported; a `prefix R`
pressed meanwhile takes over.

`prefix L` asks for `logout` typed in full. It removes the lines of the machine's key files
that hold this PC's public key and carry the `xmux-registered` mark. A matching line
without the mark opens a second confirmation, where `remove` typed in full removes it too
and anything else keeps it. The ssh config stanza a login saved for the machine goes
next. Any other ssh config `Host` entry naming the machine is listed in that same second
confirmation, and `remove` takes the machine off it too: an entry naming only this machine
goes, and an entry naming others too keeps them. The held password and the connections are cleared after
that; a machine that cannot be reached or a removal that fails still logs out, with a toast
saying what remains and why.

`prefix <digit>` opens the jump popup holding the digit, so a number past 9 is typed out
(`prefix 1` then `2` lands on 12). The selection follows the number while it names a
card and stays put while it does not; `prefix 0` therefore moves nothing, and a leading
zero is only a spelling. `Enter` closes the popup on a card's number and otherwise states
in the popup that no card carries it, with the range in its top border; `Esc` returns to
where the jump started.

## Prefix Indicator and Hint Bar

At rest the prefix sits on the bottom row of a side column, or at the right end of a
band's view border row, as a label sized to its text. A side list that overflows thickens
the stretch of the view border beside the cards on screen to `┃`; a band writes `‹ 5` and
`7 ›` on its view border row, counting the cards scrolled off each side.

Pressing the prefix opens the key list from the indicator toward the terminal view,
floating over it without moving a card. A terminal view too narrow beside a side column
lends the box the window's whole width. When the keys do not fit, the box shortens every
description, then gives up the keys needed least behind `+N more`, never the jump, help,
or quit keys. The box closes when the function the prefix started ends: at once for most
keys, when Enter or Esc closes an input, or when a resize's repeat window lapses. A focus
switch or any mouse action outside the box cancels the prefix; dragging the box moves it.
A held prefix sends one literal per repeat and blinks the box, because a terminal sends
no key-up.

The hint bar shows one thing at a time, in this order: a refusal, the prefix alone while
the key list or an input is open, the hint after a selection move, the scan progress,
the active filter, and the resting prefix. A refusal opens into the terminal view in the
error colour, wraps instead of clipping, and goes away on the next key or after ten
seconds. The hint after a selection move names the card's next keys and one fact about
it for three seconds; when the terminal view holds focus it offers only prefix keys, and
a selection xmux was told to make raises none. With the nav auto-hidden, a prefix
interaction brings the nav back until it ends, and the key list and the bar open over
the window's bottom left.

## Popups

Every popup opens where the key list opens and is moved by dragging it. Its rows wrap
to its width rather than being cut, and only a text field stays on one row. In a popup
with items to pick, the arrows move its selection, the pointer underlines an item without
moving that selection, and a click executes the item as `Enter` would.

- **Help** (`prefix ?`): typing searches it, ignoring case; `Backspace` shortens the
  query and `Ctrl-U` clears it. A row of tabs names the sections: `←`/`→` or a click
  scrolls to one, and a hovered tab shows its section until the pointer leaves the tab
  row. `↑`/`↓`, `PgUp`/`PgDn`, and `Home`/`End` scroll. `Esc` or `prefix ?` closes it.
- **History** (`prefix m`): newest first, with how long ago each record happened.
  Opening it takes every toast down. `q`, `Esc`, or `prefix m` closes it.
- **Machine problems** (`prefix h`): `Enter` or a click selects the machine's card, filtering
  to its name when it has no card, and focuses its login pane when it needs a login.
- **Filter** (`prefix /`): the list re-filters as you type, and the selection moves to
  the nearest visible card related to its card when that card is hidden. `Enter` keeps
  the filter, `Esc` restores the one the popup opened with, and `Esc` in the nav clears
  an applied filter.
- **Logout** (`prefix L`): states the machine it logs out of, whichever of its cards is
  selected, the observed SSH login, what happens to a held password, to this PC's
  key, and to the ssh config entry xmux saved, and the machine whose connections close.
  Typing `logout` and `Enter` confirms it, and `Esc` cancels. When the window is too short
  for these rows, `↑`/`↓` and `PgUp`/`PgDn` scroll them above the field, and the key
  removal confirm that can follow scrolls the same way.

A terminal smaller than 24 columns by 4 rows shows the required and current size in
place of the split view.

## Toasts

The result of work you started floats as a toast in the terminal view's corner nearest
the hint, at most 40% of the window wide, up to three at once. A toast of successes and
facts leaves after five seconds, with the time left drawn on its bottom border. One that
carries a warning (`▲`) or a failure (`✗`) stays until a click on it or opening the
history takes it down, except a login result, which leaves after five seconds since the
login pane and the history keep it. Something nobody asked about, such as a machine
that stops answering, goes to the history only. `[ui] notifications = false` turns
toasts off; the history still records every result.

## Mouse

| Gesture | Action |
|---|---|
| left-click a card or title part | open it: select it and focus the terminal view |
| point at a card or title part | preview it in the terminal view without moving the selection |
| left-click a screen link | open the screen it names |
| point at or left-click a landing card | underline it, or open it as `Enter` does, from either focus |
| left-click a view | focus that view |
| left-click a collapsed nav | expand the nav |
| left-click `‹ 5` or `7 ›` | select the nearest card scrolled off that side |
| wheel over the nav | move the selection |
| drag the view border | resize the nav, collapsing it past the minimum |
| drag the key list or a popup | move it |
| left-click a popup item | execute it, as `Enter` would |
| left-click a toast | dismiss it |

There is no context menu. While the terminal view is focused, mouse events over it reach
the pane, which needs the mux's own mouse mode to use them.

## Automation

A running instance listens on `ctl-<name>.sock`. Its verbs are `ping`, `status`,
`dump`, `rescan`, `switch <host> <session>`, `focus <nav|terminal>`, `width <delta>`
(a signed column delta), `toggle-auto-hide`, `quit`, and `new-session <host> [name]`.
There are no kill, rename, or window verbs, because the mux owns editing a session.

```
xmux instances
xmux send amber-otter switch prod api
xmux send am focus terminal          # any unambiguous name prefix works
xmux send - dump                     # `-` when exactly one is running
printf 'switch prod api\nfocus terminal\n' | xmux send amber-otter
```

`--name` takes lowercase letters, digits, and `-`, up to 32 characters. With no command,
`send` reads commands from stdin, one per line. A refused command exits non-zero, and a
`switch` to a host or session the inventory does not list replies `err:` naming the
missing half. The `raw:` namespace (`raw:key`, `raw:keys`, `raw:text`) injects
keystrokes or bytes and is not part of the supported surface.
