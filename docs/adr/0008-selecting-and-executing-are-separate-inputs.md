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

Choosing a target and acting on it are two separate inputs. There are two kinds of
selection and one kind of execution.

| Input | Selects | Executes |
| --- | --- | --- |
| Keyboard | the arrow keys move the hard selection | Enter executes the hard selection |
| Mouse | hovering sets the soft selection | clicking executes the soft selection |

**A selection shows its target.** Either kind may show what the target is: the
terminal view shows the selected card's screen, and a selected help tab scrolls the
help body to its section. A selection never moves the focus, runs a command, or
changes a host, a mux, or a session.

**The hard selection** is the keyboard's target. There is one per surface, and it
stays where the keys left it.

**The soft selection** is the target under the pointer, drawn apart from the hard
selection. While it exists, it is what the surface shows: hovering a nav card shows
that card's screen in the terminal view. It never moves the hard selection, and when
the pointer leaves the target the surface shows the hard selection again.

**Executing** is the same act whichever input starts it: Enter on the hard selection
and a click on the soft selection do the same thing to their target. Executing opens
the screen the target names and gives it the focus, or runs the command the target
stands for. The executed target becomes the hard selection.

Gestures that are neither a pick nor an action keep their own meaning: a drag moves
or resizes what it grabs, the wheel scrolls, and typing edits a text field.

## Consequences

Every pickable surface defines a hard-selected look and a soft-selected look, routes
the arrow keys to the hard selection and hovering to the soft selection, and gives
Enter and a click one shared execution. A surface that has no action has only a
hard selection, and executing on it only makes the target the hard selection.

A new input binding states which of the three it is. A binding that would select and
execute at once is split, or it is a deliberate shortcut that names its target
directly (a digit jump, a prefix chord), never an arrow key or a hover.
