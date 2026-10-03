//! Runtime-owned chrome data and its plain field updates.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};

/// The tree and terminal view border's three colours. `active` marks nav focus,
/// `inactive` marks terminal focus, and `hover` is the drag-resize grab cue.
///
/// The border says which VIEW holds focus, which is a fact about xmux and not about
/// the selected mux. Configuration can replace each role without changing that meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewBorderColors {
    pub active: Color,
    pub inactive: Color,
    pub hover: Color,
}

/// What a flash is about, which decides how the bar paints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum FlashKind {
    /// A refused action or a failure: the error bar and warning mark.
    #[default]
    Error,
    /// Information that is not a failure: the notice style and no mark.
    Notice,
}

/// How xmux reaches one source, in the words the unreachable screen prints.
///
/// Resolved once at startup from that source's own config, because how a source is
/// REACHED cannot change under a run. Every field is already words: rendering prints
/// them and branches on none of them, which keeps this data blind to machine and mux
/// implementations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceReach {
    /// The command a session listing spawns, spelled so it can be run by hand.
    pub probe: String,
    /// The machine and how it is addressed, with the connect budget that applies to it.
    pub machine: String,
    /// The mux binary asked for on that machine.
    pub mux: String,
    /// What that mux is CALLED, which is the name every surface shows.
    pub kind: String,
    /// The socket or ControlMaster path. Empty means the screen omits the row.
    pub socket: String,
}

/// How long a flash stays up with nothing pressed. A refusal is about something that
/// already happened, so holding it forever would hide the nav's normal help text.
/// Ten seconds leaves enough time to read a wrapped line twice.
pub(crate) const FLASH_TTL: Duration = Duration::from_secs(10);

/// Runtime-owned chrome data read by border, hint bar, and host-screen rendering.
pub struct Chrome {
    pub(crate) flash: String,
    /// When the flash stops showing itself, or `None` when nothing is flashing.
    pub(crate) flash_until: Option<Instant>,
    /// Whether the current flash is an error or a notice.
    pub(crate) flash_kind: FlashKind,
    /// Auto-hide-tree mode. It drives the view border glyph that cues the mode.
    pub(crate) auto_hide: bool,
    /// Whether the mouse is hovering the view border, used as the resize grab cue.
    pub(crate) view_border_hovered: bool,
    /// Session addresses currently connecting or awaiting first output.
    pub(crate) spinner: HashSet<String>,
    pub(crate) spinner_frame: usize,
    pub(crate) login_defaults: HashMap<String, crate::provision::env::LoginDefaults>,
    pub(crate) ssh_stanzas: HashMap<String, String>,
    /// What offered each host to the roster, already reduced to words to print.
    pub(crate) roster_providers: HashMap<String, String>,
    /// How xmux reaches each source, keyed by source id.
    pub(crate) source_reach: HashMap<String, SourceReach>,
    /// The log file every dispatched command and its result is written to.
    pub(crate) log_path: String,
    /// The human-readable prefix string shown in the help and hint surfaces.
    pub(crate) ui_prefix: String,
    /// Whether the prefix is pressed and the app is waiting for a command key.
    pub(crate) armed: bool,
    /// The side the nav is attached to in the current frame.
    pub(crate) nav_position: crate::model::NavPosition,
    /// The view border colours resolved from the active palette and configuration.
    pub(crate) colors: ViewBorderColors,
    /// The hint bar style resolved from the active palette and configuration.
    pub(crate) hint_bar_style: Style,
}

impl Chrome {
    /// Sets the transient error flash shown in the nav's hint bar. The next tree key
    /// or [`FLASH_TTL`] clears it so the normal help/status hint bar returns.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.show_flash(msg.into(), FlashKind::Error);
    }

    /// Sets a transient notice with the same lifetime as an error flash.
    pub(crate) fn notice(&mut self, msg: impl Into<String>) {
        self.show_flash(msg.into(), FlashKind::Notice);
    }

    fn show_flash(&mut self, msg: String, kind: FlashKind) {
        self.flash = msg;
        self.flash_kind = kind;
        self.flash_until = Some(Instant::now() + FLASH_TTL);
    }

    /// Takes the flash down, however its lifetime ended.
    pub(crate) fn clear_flash(&mut self) {
        self.flash.clear();
        self.flash_until = None;
    }

    /// Drops an expired flash and reports whether the bar changed.
    pub(crate) fn expire_flash(&mut self, now: Instant) -> bool {
        match self.flash_until {
            Some(until) if now >= until => {
                self.clear_flash();
                true
            }
            _ => false,
        }
    }

    /// Replaces the sessions currently connecting or awaiting first output.
    pub(crate) fn set_spinner(&mut self, addresses: HashSet<String>) {
        self.spinner = addresses;
    }

    /// Sets the braille spinner frame derived from elapsed wall-clock time.
    pub(crate) fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }

    /// Sets auto-hide-nav mode, which the view border glyph reflects.
    pub(crate) fn set_auto_hide(&mut self, on: bool) {
        self.auto_hide = on;
    }

    /// Sets the view border hover cue used for drag resizing.
    pub(crate) fn set_view_border_hovered(&mut self, on: bool) {
        self.view_border_hovered = on;
    }

    #[cfg(test)]
    pub(crate) fn set_view_border_colors(&mut self, colors: ViewBorderColors) {
        self.colors = colors;
    }

    /// Sets the prefix string shown in the help modal.
    pub(crate) fn set_ui_prefix(&mut self, prefix: String) {
        self.ui_prefix = prefix;
    }

    /// Sets whether the prefix is pressed and awaiting its command key.
    pub(crate) fn set_armed(&mut self, armed: bool) {
        self.armed = armed;
    }

    /// Sets the nav attachment side used by focus hints.
    pub(crate) fn set_nav_position(&mut self, position: crate::model::NavPosition) {
        self.nav_position = position;
    }

    /// Sets the provider label for every host in the assembled roster.
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

    /// Returns the effective ssh address, port, and user for the login pane.
    pub(crate) fn login_defaults(&self, source: &str) -> (String, String, String) {
        let host = crate::session::machine_of(source);
        self.login_defaults
            .get(host)
            .cloned()
            .unwrap_or_else(|| (host.to_string(), "22".into(), String::new()))
    }

    /// Returns the display name of the mux on `source`.
    pub(crate) fn source_mux<'a>(&'a self, source: &'a str) -> &'a str {
        match self.source_reach.get(source) {
            Some(reach) if !reach.kind.is_empty() => &reach.kind,
            _ => crate::session::mux_of(source),
        }
    }

    /// Formats `source` with the shared `{host}/{mux}` grammar.
    pub(crate) fn source_label(&self, source: &str) -> String {
        crate::session::source_label(crate::session::machine_of(source), self.source_mux(source))
    }

    /// Formats the source while omitting an unconfirmed mux name.
    pub(crate) fn source_label_when(&self, source: &str, answered: bool) -> String {
        let mux = if crate::session::mux_may_be_named(source, answered) {
            self.source_mux(source)
        } else {
            ""
        };
        crate::session::source_label(crate::session::machine_of(source), mux)
    }

    /// Sets the resolved reach description for every source.
    pub(crate) fn set_source_reach(&mut self, reach: HashMap<String, SourceReach>) {
        self.source_reach = reach;
    }

    /// Sets the log path named by the unreachable screen.
    pub(crate) fn set_log_path(&mut self, path: String) {
        self.log_path = path;
    }
}
