# Configurable frame rate cap

## Purpose

xmux limits how often it paints its complete terminal view. A user can raise that
limit to display more of the updates produced by a fast attached mux client,
including TUIOS, without changing how xmux attaches to or enumerates that mux.
The limit is an upper bound on xmux's draws, not a promise that the outer terminal
will display every source update.

## Configuration

`[ui] max-fps` is an integer from 10 through 120, inclusive. Its default is 30.
For example:

```toml
[ui]
max-fps = 120
```

The setting applies to the whole xmux view for every mux and every focus mode.
It does not read or change a mux's own frame rate setting. A user who wants to
see TUIOS output above 30 frames per second must set xmux's cap high enough as
well as configure TUIOS for that output rate. A higher cap cannot create frames
that TUIOS did not send.

An absent value uses 30. A non-integer or out-of-range value is a configuration
error. At startup, xmux reports the error through its normal configuration
diagnostics. During live reload, an invalid edit leaves the last valid cap in
effect and reports the configuration error without replacing the valid setting.
Removing the key in a valid edit restores the default. A valid live edit takes
effect without restarting xmux or reconnecting a mux client.

## Draw scheduling

The maximum draw rate is set by a minimum interval of one second divided by
`max-fps` between draw attempts. Time is measured with a monotonic
clock and enough precision that 30, 60, 90, and 120 do not require rounded
millisecond intervals. A pending draw is eligible when the interval has
elapsed. Input, PTY output, source events, and animation may mark the view for
drawing, but a timer wake alone does not paint a frame. An active animation can
mark the view for drawing on its own cadence.

When output arrives faster than xmux can draw, xmux renders the newest available
grid and does not queue intermediate screen images. Missed deadlines do not
trigger catch-up draws. The cap also applies when drawing itself is slow; xmux
does not start a second draw while one is in progress. Each attachment's output
wakeups remain bounded, and the event loop continues to service input and source
events during an output flood.

The 120 ms navigation spinner phase remains based on elapsed time rather than the
number of draws. Other animations may be displayed less often when the user sets
a lower cap, without changing their elapsed-time phase. Rendering reads the
application model and writes only the frame. Configuration changes enter through
the application's update transition; the runtime uses the resulting cap to pace
draws.

## Boundaries

The cap controls xmux's outer-terminal draws. It does not throttle a mux daemon,
change a PTY client's output rate, alter session inventory polling, or introduce
another connection to a host. The same display attachment and last confirmed
grid remain in use while a selection or scan is in progress.

The effective visible rate can be lower than the cap because the mux, PTY, xmux
rendering, transport, and outer terminal each have finite throughput. In
particular, a 120 fps cap means xmux can attempt to display changes at that rate
when they arrive and drawing keeps up; it does not guarantee 120 visible frames
per second.

## Acceptance criteria

- The default preserves the existing approximately 30 fps draw limit, and
  configured limits of 10, 30, 60, 90, and 120 are accepted. Values outside the
  stated range are rejected.
- A sustained source of distinct PTY screen states can lead to more than 30 xmux
  draws per second at a 60 or 120 fps cap, while draws never exceed the configured
  pacing limit because of timer rounding or missed deadlines.
- Under a burst faster than the cap, xmux draws the latest grid, does not grow an
  unbounded frame backlog, and keeps input responsive.
- A valid live edit changes the pacing without reattaching the selected mux
  client. An invalid edit retains the last valid cap. Removing the key restores
  the default.
- Idle views do not redraw solely because the frame timer wakes. Navigation
  spinner phase and render purity remain independent of the cap.
- The behavior is the same for TUIOS and the other supported muxes because the
  cap belongs to xmux's common display path.
