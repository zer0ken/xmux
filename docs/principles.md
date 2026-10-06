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
died keeps its last frame, and an unreachable card stays unreachable, each until the
user asks again.

## Minimal Persistent Surface

The always-visible nav carries names, numbers, one state glyph per card, the selection
mark, and the resting prefix, and nothing else. A long name keeps its beginning and end
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
reverse video, bold, and dim attributes, and the terminal resolves them into hues. Only
a colour the user names may leave the sixteen slots, and a colour a child program emits
passes through untouched.

A hue xmux chose would be chosen for somebody else's terminal and would be wrong on
every theme it was not chosen for. Slots let the whole UI recolour with whatever scheme
the user runs, and xmux never fights a theme it cannot see. What the slots cannot say is
said with an attribute: the selected card is reverse video, the terminal swapping its
own pair, because a computed surface needs the terminal's background colour and a
terminal is free to answer no colour query at all.

## Terminal-Safe Shape Vocabulary

Persistent UI symbols are conventional one-cell glyphs that OS-default terminal fonts
render without emoji presentation: `❯`, `✓`, `✗`, braille spinner frames led by `⠋`,
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
up its lineage that has one, and a card that appears takes the selection only when the
interest names it. A machine keeps a target while any card of it is listed, its own card
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
nothing, so nothing attaches before the user has chosen.

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
