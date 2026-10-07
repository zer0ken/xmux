# Context

The glossary: one concept, one word. The design principles these words serve are in
`docs/principles.md`, the architecture is in the root `AGENTS.md`, and the behavior of
each surface is in `docs/keybind.md` and `docs/requirements.md`.

## Documentation

- **Working Notes** - the `AGENTS.md` of a directory, titled `Working Notes: <path>`:
  why the code there exists, its module seams, and what must hold inside it.
- **Module Seam** - where a module's interface lives: what callers may rely on, what
  the module hides, and which dependencies may cross into it.

## Machines and Hosts

- **machine** - a computer xmux can reach (`db-01`, `local`, `wsl.Ubuntu`); the roster
  decides the set. A sentence about a machine must stay true when one machine serves
  two muxes; one that does not is about a host.
- **host** - one mux on one machine (`db-01/tmux`), and what every session address
  names. Its id is the bare machine name when the machine serves one mux and
  `<machine>:<mux>` when it serves several. A host exists only for a mux the machine is
  confirmed to serve, one the config writes or mux discovery found, so a machine whose
  muxes are not known yet has none. Every surface writes it as its host label.
- **session** - one session of a host's mux (`db-01/tmux/pg-primary`).
- **level words** - `machine`, `host`, and `session`, the three levels a session lives
  in, one word for each level in the code, the docs, and every surface: `host` always
  names one mux on a machine and never the machine.
- **interfaces** - the config keys, CLI flags, ctl verbs, and their argument formats are
  accepted unchanged under these words: a `[[hosts]]` entry configures a machine, and
  an argument that names a session's host (`xmux attach <host> <name>`, `switch <host>
  <session>`) takes the host id.
- **`Transport`** - the MACHINE axis trait (local, ssh, WSL): where a command runs and
  how its argv is executed. A transport reaches a machine; it is not one.
- **`Mux`** - the MUX axis trait (tmux, psmux, zellij, abduco, screen, tuios, herdr):
  the per-mux metadata, command plans, and display driver.
- **`MuxDriver`** - a mux's display driver, built by the mux itself.
- **window** - a mux's subdivision of a session. xmux uses one set of words for every
  mux, so a zellij tab is a window and a mux's own naming stops at its implementation.
- **pane** - a mux window's terminal split, never a screen region.
- **roster** - which machines xmux offers, assembled from providers that `[discovery]`
  switches: ssh config aliases, neighbours, and WSL distributions.
- **neighbour** - a machine this box reaches in one hop, found in the operating
  system's own network state, that answers ssh.
- **reachability** - a machine's connect state from one bounded probe: `connected`,
  `blocked`, or `unreachable`. Only a connected machine is asked anything further.
- **mux discovery** - asking a machine that names no mux which supported muxes it has;
  each one that answers becomes a host.
- **discovery** - scanning a host for its sessions.
- **blocked** - a machine ssh refused for a reason the submitted login can answer: an
  authentication refusal, or a first-seen host key under an `ask` policy.
- **nesting** - xmux running inside a mux session. It is allowed because mux clients
  are PTY children, and it costs only the own session.
- **own session** - the mux session xmux itself runs in, the one address the terminal
  view refuses to mirror.
- **instance name** - a running app's `<adjective>-<noun>` (or `--name`) identity,
  owning `ctl-<name>.sock` for its lifetime.

## Screen Regions

- **the app** - the runtime that owns the terminal: its loop, its focus, and its input
  routing.
- **split view** - the whole two-region layout.
- **nav view** - the region holding the cards, attached as a left or right column or a
  top or bottom band. Never the "sidebar" or the "tree".
- **terminal view** - the other region: the selected session's grid or a view screen.
- **`ViewFocus`** - which region holds focus, the nav or the terminal view.
- **view border** - the line between the nav and the terminal view. Its colour states
  which view holds focus, and dragging it resizes the nav.
- **view border lines** - `│` or `─` by default, `║` or `═` with auto-hide, and `┃` or
  `━` while hovered.
- **seam thumb** - the heavy stretch of a side column's view border beside the cards on
  screen when the list overflows.
- **offscreen counts** - the `‹ 5` and `7 ›` a band writes on its view border row,
  counting the cards scrolled off each side.
- **chrome** - the furniture around the two views: the view border, the hint bar, and
  the view screens. Never a "status surface".
- **nav size** - the nav's live geometry as one value: the set width, the on-screen
  width, the band height, the attached side, and the collapsed state.
- **collapsed nav** - the nav reduced to its prefix indicator, keeping its natural size
  for when it expands.
- **column flow** - how a band lays out its rows: down a column, then right, a whole
  section per column.
- **grid** - xmux's in-memory cell mirror of the attached session's screen, drawn in the
  terminal view.
- **cursor** - the terminal's text cursor, on the grid or on a focused field's caret.
  Never the nav selection.

## Nav Content

- **card** - one numbered nav entry: a session, a host's host-state card, or one card
  for a machine that has no host yet or none of whose hosts connected. A card states
  what something is, never why.
- **section** - one host's session cards under its section title, or the band of
  host-state and machine cards.
- **section title** - the `{machine}/{mux}` header over a host's session cards. It is
  not a card; its `{machine}` part stands for the machine and its `{mux}` part for the
  host.
- **card and section navigation** - how the nav itself is walked: `↑`/`↓` between
  numbered cards, `←`/`→` between sections, independent of the hierarchy.
- **host label** - `{machine}/{mux}`, how a machine and its mux are shown wherever the
  pair is read. Both halves always, parted by `/`, never by the id's separator.
- **nav groups** - the session cards, then reachable hosts with no sessions, then hosts
  and machines whose connection or inventory is unresolved.
- **card order** - local, then WSL, then remote, each by host id, sessions by name.
  A re-enumeration reproduces it exactly.
- **address column** - the leftmost cells of every card: its number.
- **card number** - the number `prefix <digit>` jumps to; `[ui] renumbering` decides
  whether it follows list position or stays with the card.
- **status** - a host-state card's one-cell glyph: `?` login needed, `▲` unreachable,
  `✗` listing failure, blank for an empty host, the spinner while scanning. Never a
  "hint".
- **alert mark** - the `!` after a session card's name: the session rang its bell or
  sent a desktop notification while its grid was not on screen, and has not been shown
  since.
- **level colour** - the palette role of each part of a card. The accent goes to the
  lowest level the card shows.
- **spinner** - the braille glyph marking work in flight, one frame counter for the whole
  UI.

## Selection

- **hierarchy** - session, host, and machine, the levels a session lives in. It is
  reached through `Ctrl+↑`/`Ctrl+↓`, the parts of a section title, and screen links,
  never through the card step.
- **selection** - the hard selection: the node (machine, host, or session) the arrows
  and execution move and the terminal view shows.
- **soft selection** - the target under the pointer. It previews without moving the
  selection and paints as an underline.
- **interest** - what the user is on or asked for, the one value the selection is
  resolved from on every rebuild.
- **selection lineage** - session, host, machine: the chain a selection walks up when its
  node loses its card.
- **selection highlight** - the theme's accent as the background and its on-accent
  text colour over the cells of the item the hard selection is on and one blank cell on
  each side of its text, the one look every surface gives its selection.
- **card focus** - what selecting a card changes: the selection highlight, and on a
  machine or host card its state word. Its height, its number, and its name's column
  never move.

## Interaction Surfaces

- **prefix** - xmux's own key chord leader, `C-g` unless `[ui] prefix` names another.
- **ready** - the state while a prefix interaction is live, from the prefix key until
  the function it started ends or is cancelled.
- **key table** - the one table of every key xmux binds and the words naming it; every
  surface that names a key reads it.
- **hint bar** - the prefix indicator and what takes its place: the selection hint, the
  scan indicator, the active filter. Advice for the current state, never a result.
- **status row fill** - how much of its row the hint bar paints: the resting indicator
  its text, a floating bar the whole row.
- **key list** - the box a live prefix opens, naming every key it unlocks.
- **selection hint** - what the hint bar says for three seconds after the user moves the
  selection: the card's next keys and one fact about it.
- **scan indicator** - the `scanning hosts n/m…` progress in the hint bar, counting
  hosts and the machines that have no host yet.
- **toast** - the result of an action the user took, whether it was done, refused, or
  failed, in a box in the terminal view's corner. Never a "notice", and never feedback on
  a value typed into a popup, which that popup states beside its field.
- **history** - the bounded record of every toast and background event (`prefix m`).
- **`Modal`** - the one focus-grabbing UI a prefix key opens: an input, the command
  palette, the help, the machine problems, or the history.
- **popup** - the rounded box every modal draws, opened where the key list opens.
- **jump** - the digits-only input `prefix <digit>` opens, which moves the selection
  while its number names a card.
- **filter** - the type-to-filter input over the nav list (`prefix /`).
- **machine problems** - the table of hosts and machines in a problem state, grouped by cause
  (`prefix h`).
- **command palette** - the searchable list of named actions (`prefix :`).
- **one-machine re-scan** - `prefix r`: the selected card's machine asked again alone.
- **view screen** - what the terminal view shows in place of a grid: a machine screen,
  a host screen, a scanning or settled state, the own session, or the landing screen.
- **machine screen** - the view screen headed `machine {machine}`: how the machine is
  reached and logged in to, its login pane, and a link to each of its hosts whose mux is
  confirmed. A machine with no confirmed mux links nowhere.
- **host screen** - the view screen headed `host {machine}/{mux}`: the host's sessions,
  how they stay current, and a link to its machine and to each session.
- **landing screen** - the view screen from launch until the first execution: the scan
  progress and every nav card as a link, sharing the one hard selection.
- **screen link** - a selectable link on a machine or host screen that opens another
  node's screen. While the login pane is open it owns the keyboard, so a link there
  holds no selection and answers only the pointer.
- **switcher screen** - the rendered split view as a whole. Never an "overlay".

## Login

- **login pane** - the form on the machine screen of a blocked or chosen unreachable
  machine: address, port, username, an optional masked password, and what to do once it
  works.
- **running a login** - one bounded ssh command whose password is reachable only through
  xmux's private credential broker.
- **SSH authentication record** - the method OpenSSH reported for a connection: public
  key, password, or `not observed`.
- **recording a login** - writing an xmux-marked stanza with the working values at the
  top of `~/.ssh/config`.
- **registering a key** - appending this PC's public key, marked `xmux-registered`, to
  the machine's key files, then proving a key-only login works.
- **logging out** - `prefix L`: removing this PC's key from the machine and the
  stanza a login recorded, then forgetting the held password and closing the machine's
  connections.
