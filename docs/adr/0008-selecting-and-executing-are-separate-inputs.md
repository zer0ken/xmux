# ADR 0008: Selecting and Executing Are Separate Inputs

## Status

Accepted

## Context

Everything the user can pick in xmux is either looked at or acted on: a nav card, a
link on a view screen, a popup item, a help tab. When one input does both, the user
cannot look at a thing without also acting on it, and cannot predict which inputs
are safe to try. A click that both selects a card and moves the focus, or an arrow
that both moves the cursor and runs the item under it, makes every exploration a
commitment.

## Decision

Choosing a target and acting on it are two separate inputs, on the keyboard and on
the mouse alike.

| Input | Selects | Executes |
| --- | --- | --- |
| Keyboard | the arrow keys | Enter |
| Mouse | hovering the pointer over the target | clicking the target |

**Selecting** marks a target and changes nothing else. A selection may show what the
target is: the terminal view shows the selected card's screen, and a selected help
tab scrolls the help body to its section. It never moves the focus, runs a command,
or changes a host, a mux, or a session.

**Executing** acts on the target: it opens the screen the target names and gives it
the focus, or runs the command the target stands for. Executing a target that is not
yet selected selects it first, so a click acts on exactly what the pointer was over.

A hover highlights the target under the pointer and is drawn apart from the keyboard
selection. Moving the pointer never moves the keyboard selection, so the pointer
drifting across the nav never changes the terminal view.

Gestures that are neither a pick nor an action keep their own meaning: a drag moves
or resizes what it grabs, the wheel scrolls, and typing edits a text field.

## Consequences

Every pickable surface defines both a selected look and a hovered look, and routes
the arrow keys and hovering to selection, Enter and clicking to execution. A
surface that has no action has only a selection, and Enter and a click on it do
nothing beyond selecting.

A new input binding states which of the two it is. A binding that would select and
execute at once is split, or it is a deliberate shortcut that names its target
directly (a digit jump, a prefix chord), never an arrow key or a hover.
