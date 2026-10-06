//! Runtime-owned chrome data and its plain field updates.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};

/// The tree and terminal view border's three colours. `active` marks nav focus,
/// `inactive` marks terminal focus, and `hover` is the drag-resize grab cue.
///
/// The defaults are xmux's own and the same on every host: the palette's `primary`
/// for nav focus, `disabled` for terminal focus, and `accent` for the grab cue. The
/// border says which VIEW holds focus, which is a fact about xmux and not about the mux
/// on the other side of it, so a border that changed hue as the selection moved between
/// hosts was reading as a state change where there was none.
///
/// [`Self::resolve`] layers one tier over that: a `[ui] view-*-border-style` value the
/// user named. Their terminal, their choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewBorderColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

/// How xmux reaches one host, in the words the unreachable screen prints.
///
/// Resolved once at startup from that host's own config, because how a host is
/// REACHED cannot change under a run - only whether it answers can. Every field is
/// already words: the screen prints them and nothing branches on any of them, which is
/// what keeps this layer blind to which machine kind or which mux a host is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostReach {
    /// Whether the machine is reached over SSH and can use machine access actions.
    pub ssh: bool,
    /// The command a session listing spawns, spelled so it can be run by hand. The one
    /// datum that turns "it failed" into something the user can reproduce outside xmux.
    pub probe: String,
    /// The machine and how it is addressed, with the connect budget that applies to it.
    pub machine: String,
    /// The mux binary asked for on that machine.
    pub mux: String,
    /// What that mux is CALLED, which is the name every surface shows: the binary above is
    /// what was asked for, and the two part company wherever a binary is an alias or a
    /// path. One spelling, so a card and the screen reached from it cannot name one mux two
    /// ways.
    pub kind: String,
    /// The socket / ControlMaster path the mux is addressed through. Empty ⇒ no row,
    /// which is the honest answer for a machine addressed without one.
    pub socket: String,
    /// How the host's session list becomes current.
    pub refresh: String,
}

/// How long a flash stays up with nothing pressed. A refused key is about something that
/// already happened, so a bar holding one forever keeps the nav's own help text off
/// screen over a message that has stopped being news. Ten seconds reads a wrapped line
/// twice over.
pub(crate) const FLASH_TTL: Duration = Duration::from_secs(10);

/// How long the hint after a selection move stays up with nothing pressed. Long enough to
/// read a key and a fact, short enough that the resting indicator is back before the next
/// glance.
pub(crate) const SELECTION_HINT_TTL: Duration = Duration::from_secs(3);

/// One key a hint offers: its keys as written, its full description, and its short one.
pub(crate) type HintKey = (String, String, String);

/// What the hint bar says about the card the selection just moved to: its most relevant
/// keys and one fact about it, each already in words, until `until`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectionHint {
    pub(crate) keys: Vec<HintKey>,
    pub(crate) fact: String,
    pub(crate) until: Instant,
}

/// Runtime-owned chrome data read by border, hint bar, and host-screen rendering.
pub struct Chrome {
    /// A refused key's reason, shown in the hint bar until the next key or its own life
    /// ends. Empty when nothing is flashing.
    pub(crate) flash: String,
    /// When the flash stops showing itself, or `None` when nothing is flashing.
    pub(crate) flash_until: Option<Instant>,
    /// The hint about the card the selection moved to, while it lasts.
    pub(crate) selection_hint: Option<SelectionHint>,
    /// The first interactive key without a saved preference introduces the prefix.
    pub(crate) first_key_seen: bool,
    pub(crate) first_key_notice: bool,
    /// Auto-hide-tree mode (set by the app each frame). Drives the view border glyph:
    /// ║ (double) when on, │ (single) when off - the only on-screen cue, since while
    /// the mode is on but the tree is focused the tree still shows.
    pub(crate) auto_hide: bool,
    /// True while the mouse is hovering the view border rule - the app sets this from
    /// idle motion so the view border highlights as a grab cue for drag-resize.
    pub(crate) view_border_hovered: bool,
    /// Session addresses currently connecting / awaiting first output - a braille
    /// spinner glyph renders right of their name in the tree.
    pub(crate) spinner: HashSet<String>,
    pub(crate) spinner_frame: usize,
    /// Milliseconds since this run's animation origin, supplied by the app update.
    pub(crate) animation_ms: u64,
    /// Whether the central Braille X animation is painted on view screens.
    pub(crate) braille_animation: bool,
    pub(crate) login_defaults: HashMap<String, crate::provision::env::LoginDefaults>,
    pub(crate) ssh_stanzas: HashMap<String, String>,
    /// What offered each machine to the roster, keyed by MACHINE name and already reduced
    /// to the words to print (set once by the app). The unreachable machine screen names it.
    /// Empty in tests, where the row is then absent rather than blank.
    pub(crate) roster_providers: HashMap<String, String>,
    /// How xmux reaches each host, keyed by HOST id (set once by the app). The
    /// unreachable screen states it: a host that failed is worth little without what was
    /// asked of it and how. See [`HostReach`].
    pub(crate) host_reach: HashMap<String, HostReach>,
    /// The log file every dispatched command and its result is written to (set once by the
    /// app). The unreachable screen names the path, so the full history of what was run
    /// is findable rather than being something the user has to know about.
    pub(crate) log_path: String,
    /// The human-readable prefix string (e.g. `"C-g"`, `"C-Space"`) - set once by
    /// the app from config so the help modal reflects the active binding.
    pub(crate) ui_prefix: String,
    /// True while the prefix has been pressed and the app is waiting for the command
    /// key (set by the app each frame from the live input state, in either focus). The
    /// indicator shows the prefix alone either way; while this is set the prefix key list
    /// opens beside it, so the keys appear exactly when they are needed and never compete
    /// with the cards for room.
    pub(crate) armed: bool,
    /// The side the nav is attached to this frame (set by the app each frame from the
    /// runtime's resolved position). The key list's focus rows name the arrow pair the
    /// placement makes active.
    pub(crate) nav_position: crate::model::NavPosition,
    /// The view border colours resolved from the active palette and configuration.
    pub(crate) colors: ViewBorderColors,
    /// The hint bar style resolved from the active palette and configuration.
    pub(crate) hint_bar_style: Style,
}

impl Chrome {
    /// Sets the flash shown in the nav's hint bar: why a key was refused. The next tree
    /// key clears it (the switcher's `handle_key`), and [`FLASH_TTL`] clears it for a user
    /// who presses nothing, so the normal hint bar returns either way.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.flash = msg.into();
        self.flash_until = Some(Instant::now() + FLASH_TTL);
    }

    /// Takes the flash down, however its lifetime ended.
    pub(crate) fn clear_flash(&mut self) {
        self.flash.clear();
        self.flash_until = None;
    }

    /// Drops a flash that has been up for its whole life, and says whether the bar
    /// changed, so a caller repaints only when it did.
    pub(crate) fn expire_flash(&mut self, now: Instant) -> bool {
        match self.flash_until {
            Some(until) if now >= until => {
                self.clear_flash();
                true
            }
            _ => false,
        }
    }

    /// Shows the hint about the card the selection just moved to, replacing any earlier
    /// one, for [`SELECTION_HINT_TTL`] from `now`.
    pub(crate) fn show_selection_hint(&mut self, keys: Vec<HintKey>, fact: String, now: Instant) {
        self.selection_hint = Some(SelectionHint {
            keys,
            fact,
            until: now + SELECTION_HINT_TTL,
        });
    }

    /// Takes the selection hint down, however its life ended.
    pub(crate) fn clear_selection_hint(&mut self) {
        self.selection_hint = None;
    }

    pub(crate) fn key_read(&mut self) -> bool {
        if !self.first_key_seen {
            self.first_key_seen = true;
            self.first_key_notice = true;
            let prefix = self.ui_prefix.clone();
            self.show_selection_hint(
                vec![
                    (prefix.clone(), "prefix".into(), "prefix".into()),
                    (format!("{prefix} ?"), "help".into(), "help".into()),
                ],
                String::new(),
                Instant::now(),
            );
            true
        } else {
            self.first_key_notice = false;
            self.clear_selection_hint();
            false
        }
    }

    /// Drops a selection hint whose time is up, and says whether the bar changed.
    pub(crate) fn expire_selection_hint(&mut self, now: Instant) -> bool {
        if self.selection_hint.as_ref().is_some_and(|h| now >= h.until) {
            self.selection_hint = None;
            self.first_key_notice = false;
            return true;
        }
        false
    }

    /// Replaces the set of session addresses currently connecting / awaiting
    /// first output. The tree draws a braille spinner right of each matching
    /// session name.
    pub(crate) fn set_spinner(&mut self, addresses: HashSet<String>) {
        self.spinner = addresses;
    }

    /// Sets the braille spinner frame index. The app derives it from elapsed
    /// wall-clock time, so the spinner animates on every render rather than once
    /// per animation tick (which can starve under a `%output` flood).
    pub(crate) fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    /// Sets auto-hide-nav mode, which the view border glyph reflects.
    pub(crate) fn set_auto_hide(&mut self, on: bool) {
        self.auto_hide = on;
    }

    /// Sets whether the mouse is hovering the view border (the app derives it from
    /// idle motion); when set, the view border highlights as a drag-resize grab cue.
    pub(crate) fn set_view_border_hovered(&mut self, on: bool) {
        self.view_border_hovered = on;
    }

    #[cfg(test)]
    pub(crate) fn set_view_border_colors(&mut self, colors: ViewBorderColors) {
        self.colors = colors;
    }

    /// Sets the prefix string shown in the help modal. The app calls this once
    /// at startup so the help modal reflects the binding from config's `[ui] prefix`.
    pub(crate) fn set_ui_prefix(&mut self, prefix: String) {
        self.ui_prefix = prefix;
    }

    /// Sets whether the prefix is armed (pressed, awaiting its command key). The app
    /// calls this each frame from the live input state; while it is set the prefix key
    /// list opens beside the indicator.
    pub(crate) fn set_armed(&mut self, armed: bool) {
        self.armed = armed;
    }

    /// Sets the nav's attachment side. The app calls this each frame from the runtime's
    /// resolved position; the key list and the help read it to name the active arrow pair.
    pub(crate) fn set_nav_position(&mut self, position: crate::model::NavPosition) {
        self.nav_position = position;
    }

    /// Sets what offered each machine to the roster. The app calls this once at startup
    /// with the assembled roster; a machine missing from the map simply shows no such row,
    /// which is the honest answer for one nothing recorded.
    pub(crate) fn set_roster_providers(&mut self, providers: HashMap<String, String>) {
        self.roster_providers = providers;
    }

    /// Sets the resolved login values and matching ssh stanzas the chrome renders.
    pub(crate) fn set_login_defaults(
        &mut self,
        defaults: HashMap<String, crate::provision::env::LoginDefaults>,
        stanzas: HashMap<String, String>,
    ) {
        self.login_defaults = defaults;
        self.ssh_stanzas = stanzas;
    }

    /// What ssh WOULD use to reach `host`, as the login pane's starting values: the
    /// address, the port, and the username, as provisioning resolved them.
    ///
    /// An effective ssh address, port, or user wins when present. Missing values fall back
    /// to the provider address or host name, port 22, and this machine's account name.
    /// A pane that opened on a failure therefore opens showing the effective connection
    /// values, and the user changes the part that was wrong.
    pub(crate) fn login_defaults(&self, host: &str) -> crate::provision::env::LoginDefaults {
        let machine = crate::session::machine_of(host);
        self.login_defaults
            .get(machine)
            .cloned()
            .unwrap_or_else(|| crate::provision::env::LoginDefaults::fallback(machine))
    }

    /// What the mux on `host` is CALLED. The resolved reach answers it; a host id that
    /// carries its own mux (a machine serving several) is the fallback, for the paths that
    /// have a list of hosts and no resolved reach yet. Empty while neither knows, which
    /// is the state a card turns a spinner for.
    pub(crate) fn host_mux<'a>(&'a self, host: &'a str) -> &'a str {
        match self.host_reach.get(host) {
            Some(reach) if !reach.kind.is_empty() => &reach.kind,
            _ => crate::session::mux_of(host),
        }
    }

    /// Formats `host` with the shared `{machine}/{mux}` grammar.
    pub(crate) fn host_label(&self, host: &str) -> String {
        crate::session::host_label(crate::session::machine_of(host), self.host_mux(host))
    }

    /// The mux `host` is NAMED by, for a surface that knows whether the host ANSWERED: its
    /// mux, or empty while no answer confirmed one, so a label never puts a guess where
    /// every other one carries a fact.
    pub(crate) fn named_mux<'a>(&'a self, host: &'a str, answered: bool) -> &'a str {
        if crate::session::mux_may_be_named(host, answered) {
            self.host_mux(host)
        } else {
            ""
        }
    }

    /// The host's label with the mux [`Self::named_mux`] names.
    pub(crate) fn host_label_when(&self, host: &str, answered: bool) -> String {
        crate::session::host_label(
            crate::session::machine_of(host),
            self.named_mux(host, answered),
        )
    }

    /// Sets how xmux reaches each host, keyed by host id. The app calls this from the
    /// host registry whenever the hosts change; a host missing from the map shows
    /// the rows it has and no blanks for the rest.
    pub(crate) fn set_host_reach(&mut self, reach: HashMap<String, HostReach>) {
        self.host_reach = reach;
    }

    /// Sets the log file path the unreachable screen names. The app calls this once at
    /// startup with the file logging actually opened.
    pub(crate) fn set_log_path(&mut self, path: String) {
        self.log_path = path;
    }
}
