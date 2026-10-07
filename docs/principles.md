# Design Principles

Every screen and every module of xmux follows these principles. Each principle is
stated here once, and `CONTEXT.md` defines the terms they use.

## Honesty

xmux shows only what it can back with an answer, and it says so when it cannot. A mux
is named only once it is confirmed, an answer that has not arrived shows as in flight,
a failure shows as a failure, and a card states what something is while its screen
states why.

A value shown before it is known reads exactly like a fact, so a guessed mux, a
placeholder, or a failure dressed as an empty list sends the user to act on something
that is not true. Honesty is therefore checked before colour, before layout, and before
any value on a card. The reason belongs on the screen because a card is only as wide as
the nav: it could carry no more than a cut-down copy of a tool's diagnostic, while the
screen keeps it whole.

## Asked-for Requests

xmux reaches a machine only when something asked it to: the launch scan, a user action
(a re-scan, a login, selecting a card, managing machine access, or an operation on a
session), or a push stream that is already open. No failure raises its own retry, and a machine is
asked one thing at a time.

A request that answers a failed request cannot stop. A machine that refuses one
connection refuses the next identically, so a client that reconnects on every refusal
reconnects without end, and the machine's own defences are built to read exactly that
as an attack. A machine also counts the connections that have not authenticated yet and
drops those past its limit, so work fans out across machines and never within one.
xmux is a guest on every machine it reaches: recovering is the user's to ask for, which
costs a keystroke and is the only kind of recovery that stops. What the user sees
follows from this deliberately: a dropped channel stays dropped, a display whose client
died keeps its last frame, a display that failed to start is not started again, and an
unreachable card stays unreachable, each until the user asks again by selecting the card,
executing it, or re-scanning.

One exception answers a defect in zellij 0.45. zellij can give an attaching client the
id that its own session probe has just released, and the probe's late cleanup then
removes the new client while the session stays up (zellij-org/zellij#5270,
zellij-org/zellij#5546). zellij runs that probe inside the attach itself, so no ordering
on xmux's side avoids it. When the display client of a mux with this defect ends within
two seconds of attaching while its session is still selected, xmux attaches it once
more. The bound is one reattach per selection: a second early end shows the ordinary
ended display, so a session that is really gone costs one extra connection and never a
loop.

## Minimal Persistent Surface

The always-visible nav carries names, numbers, one state glyph per card, the highlighted
selection, and the resting prefix, and nothing else. A long name keeps its beginning and end
around a middle ellipsis rather than displacing a state or navigation cell.

The nav is read at a glance between tasks, so every cell it spends on a hint or a
reason is a cell taken from the names the user is scanning for. Anything the user needs
only while acting has a surface that appears while they act.

## Helpful Interaction Surface

A surface the user is interacting with spends its room on state words, counts, the next
key, and complete reasons or solutions. The one exception at rest is a nav with no card:
its body says in one line why it is empty and which key answers it.

While the user acts, the question is what to do next, and a surface that answers it
saves a trip to the help. So the selected card names its state, an open filter counts
its matches, a live prefix names every key it unlocks, a selection move names the
selected card's next keys for three seconds, and a machine screen keeps the failure
reason whole. Every one of these reads the one key table, so a surface never names a key that
does something else.

## Action Names and Keys

An action name states the action and its object, such as `rescan this machine` or
`place nav`, and an object at one level of the hierarchy is named by that level,
machine, host, or session. A key is the first letter of its action's name where that letter is free. Where
one letter serves two actions that differ only in scope, the lowercase key runs the
smaller-scope action and the uppercase form of the same letter runs the larger-scope
one: `prefix r` rescans this machine and `prefix R` rescans all machines.

A name that states only an object, such as `history`, or only a place, such as `side`,
leaves the user to guess what pressing the key does. A key that starts its action's name
is recalled from the name the help, the key list, and the hints already show, so
learning the names is learning the keys. The lowercase key is the easier one to press
and to press by mistake, so it runs the action that asks the least of the machines
xmux reaches. A machine and a mux on it share a name and look alike on screen, so a
word both levels could answer to leaves the user unable to tell which one a key acts on;
`host` therefore always names the mux level and never the machine, and each screen
opens with its level.

## Terminal-Owned Colour

The terminal theme owns every colour xmux paints: xmux names ANSI-16 slots and the
reverse video, bold, dim, and underline attributes, and the terminal resolves them into
hues. Only
a colour the user names may leave the sixteen slots, and a colour a child program emits
passes through untouched.

A hue xmux chose would be chosen for somebody else's terminal and would be wrong on
every theme it was not chosen for. Slots let the whole UI recolour with whatever scheme
the user runs, and xmux never fights a theme it cannot see. What the slots cannot say is
said with an attribute or with a slot as a background: the selection is the accent slot
behind a text slot picked to read on it, because a computed surface needs the
terminal's background colour and a terminal is free to answer no colour query at all.

## Terminal-Safe Shape Vocabulary

Persistent UI symbols are conventional one-cell glyphs that OS-default terminal fonts
render without emoji presentation: `✓`, `✗`, braille spinner frames led by `⠋`,
box drawing led by `╭`, `▲`, `?`, and `…`.

xmux runs in whatever terminal and font the user has, on every OS it supports. A glyph
some font draws as a colour emoji, or as two cells, breaks the column a card is aligned
on and changes what the symbol means at a glance. ASCII alone would lose shapes every
terminal already renders.

## Four-Position Grammar

The nav uses the same card, status, selection, filter, and key grammar on the left,
top, right, and bottom. Placement changes geometry, never vocabulary or interaction.

A user who moves the nav keeps everything they learned about reading and driving it.
A grammar that differed per side would make the placement a second thing to learn
rather than a matter of screen shape.

## Cards and Sections

The nav is a list of numbered cards grouped in sections: one host's session cards
under its `{machine}/{mux}` title, or the cards of machines with no session to show. `↑`/`↓`
move between numbered cards and `←`/`→` between sections, and neither step depends on
the hierarchy of machines, hosts, and sessions, which is reached only through
`Ctrl+↑`/`Ctrl+↓`, the two parts of a section title, and the links on the machine and
host screens.

Cards and sections are what the user sees and counts, so stepping through them must
mean the same thing in a column and in a band, whatever hosts the list holds. Folding
the hierarchy into the ordinary step would turn the list into a tree whose stops change
with the inventory. Keeping the hierarchy behind its own inputs leaves the card step
predictable and still lets every machine and host be opened.

## Hierarchy Separator

Wherever the hierarchy is shown, its levels (machine, host, session, window) are parted
by `/` and by nothing else. A host label always shows both halves, `{machine}/{mux}`,
even for a machine serving a single mux.

An id is typed and a label is read. The id keeps its own separator because it is what
the user types and what xmux is sent; a label parts its levels the way the rest of an
address on screen does, so one grammar covers every level. Both halves stay because a
machine that appears with its mux on one title and without it on the next reads as two
machines.

## Selection by Interest

The selection names what the user is interested in, a machine, a host, or a session,
never a position in the list. Every path that changes the list resolves the selection
from that interest by one lineage: a node that loses its card moves to the nearest node
up its lineage that has one, or names nothing when no node up its lineage has one, and a
card that appears takes the selection only when the interest names it. A machine keeps a target while any card of it is listed, its own card
or the machine part of a title or card of one of its hosts, so a machine whose card gives
way to the cards of the hosts found on it, as after a login, stays selected and its screen
lists those hosts as links.

The list changes under the user without the user doing anything: a scan answers machine
by machine, a machine goes down, a logout closes connections, a session ends, a filter
narrows the list. If each of those paths chose a fallback of its own, where the cursor
landed would depend on which path ran, and because the terminal view follows the
selection, an unrelated session could appear on screen. One lineage keeps the result
predictable, so the terminal view never shows a session the user neither chose, pointed
at, nor was led to. A change to how the selection moves is a change to the lineage or to
what counts as interest, never a special case in one path.

## User-Owned Context

The context the user is looking at, which is the selection, the terminal view, the focus,
and the open screen or popup, changes only when the user acts or as the direct, expected
result of the user's own action. A background event, such as a scan or poll answer, a
discovery, a login, create, or logout step finishing, a reconnect, or a recovery, never
moves it. A session switch the user makes inside a mux client is the user's own action,
so the selection follows it.

The one automatic move is upward. When the context itself is lost, as when a logout or a
network failure drops a session or a host, the selection and the view move up to the
nearest level that still exists, session to host and host to machine. When nothing of the
machine is left, because the roster dropped it, it serves no mux, or the filter hides all
of it, no level is left to move to, so the selection names nothing and the terminal view
shows the landing list and attaches nothing until the user picks a card. They never move
down or sideways, and a lost context that returns through a background event does not
take the selection back. A filter is the user's own, so a filter edit that lists the
node the user was on again returns the selection to that node. The upward move is the
lineage of Selection by Interest, so the two principles name one rule for the selection.

The user reads the screen to choose the next input, so a context that moves on its own
turns a key the user already decided on into an action on something else. A machine
whose login reveals its hosts therefore stays selected with its screen in view, a
created session takes the selection only while the user has not moved it since asking,
the link selected on a screen stays on the node it names while the list changes around
it, and a question an operation asks waits behind a popup the user opened. Moving up is
the one exception because a lost node leaves nothing to show; its nearest surviving
ancestor is the context that contained what the user was looking at, so it is the least
surprising place to stand.

## Separate Selection and Execution

Choosing a target and acting on it are separate inputs. The arrow keys move the hard
selection and Enter executes it; the pointer sets the soft selection and a click
executes it, with the same effect as Enter.

When one input both looks and acts, the user cannot look at a thing without acting on
it and cannot tell which inputs are safe to try. A selection may show its target, as a
hovered card previews its screen, but it never moves the focus, runs a command, or
changes a machine, a mux, or a session, and the soft selection never moves the hard one.
Executing opens the screen the target names and gives it the focus, or runs the command
the target stands for. A binding that selects and executes at once is a deliberate
shortcut that names its target directly, such as a digit jump or a prefix chord, never
an arrow key or a hover. Gestures that are neither keep their own meaning: a drag moves
or resizes, the wheel scrolls, and typing edits a text field.

The landing screen is the one surface on which a selection shows nothing: from launch
until the first execution the hard selection only highlights and a hover previews
nothing, so nothing attaches before the user has chosen. The target the first execution
opens is the user's choice from then on, so a session card that appears later does not
take the selection from it.

## Results as Notifications

The hint bar carries only short advice that fits the current state: the prefix, the
selected card's next keys, the scan progress, and the active filter. Every result of an
action the user took, whether the action was done, refused, failed, or had nothing to
do, is a toast titled by the action, and the history keeps it.

Advice and a result answer different questions. Advice says what the user can do next
and changes with the state it describes, so the hint bar keeps it current. A result says
what the last action did and stays true after the state moves on, so it belongs to the
action and to a record the user can open again. A result written into the hint bar
would push the advice off the bar while it lasted, could leave on the next key before
it was read, and would be missing from the history. One surface for every result also
lets a refusal and a failure read alike, so the user looks for an answer in one place.

Feedback on a value typed into a popup is not a result. When a popup cannot accept what
was typed, such as a number no card carries or a confirming word that is not the one
asked for, the popup states that beside its field and stays open. The value has not
become an action yet: the user is still in the popup and still correcting it, so the
answer belongs where their eyes and the caret already are. A toast would report it in a
corner away from the field, would close nothing and decide nothing, and would leave a
typo in the history beside the real results.

## Accent Selection

A selected item has the theme's accent colour as its background, with the theme's text
colour for the accent on it, and nothing else marks it as selected. Every cell of the
item takes that one pair: its number, its name, its key tokens, and its glyphs alike, so
no colour or dimming of the item survives inside the highlight. Every surface with a
hard selection paints it in this one look: the nav's cards and the halves of a section
title, the links on the landing, machine, and host screens, the rows of a popup list,
the help's tabs, and the focused stop of the login pane. The highlight keeps one cell of
padding before and after the item's text, taken from a blank cell the layout already
leaves there; where that cell holds other text or lies outside the surface, that side
goes without, because the padding never moves text or wraps a row. The caret of a
focused login field is the padding cell after its value. The soft selection under the
pointer is an underline, which reads apart from the highlight and lies on top of it when
both mark one item. A colour the user names in `[ui] selection-style` replaces the
accent background on every one of these surfaces alike, and the item keeps its own text
colours on it. With no accent to paint, under `NO_COLOR`, the selection is reverse
video.

The hard selection is where the next key lands, so it has to be found at a glance on
whatever surface the user has moved to. The accent is the colour the theme already
spends on what is interactive, so the highlight reads as the place to act, and it reads
as one colour on every theme instead of whatever the item's own colours turn into when
swapped. The padding cell keeps the first and last character off the highlight's edge,
so the highlight reads as a block around the item rather than as coloured letters. One
look learned on the nav then reads on every popup and screen, while a surface that marks
its selection with a colour, a weight, or a glyph of its own is one more thing to learn
and, beside a highlighted surface, reads as a different state. A marker glyph beside a
highlighted item says nothing the highlight does not, and takes a cell from the number
or the name the item carries.
