//! Loads xmux's optional TOML configuration and merges it with ssh-config
//! discovery to produce the set of hosts and mux binaries to use.

use std::path::Path;

use crate::ui::switcher::{NavPosition, NavPositionSetting};
use serde::Deserialize;

/// The on-disk `config.toml` structure. All fields are optional.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub local: LocalConfig,
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
    #[serde(default)]
    pub wsl: Vec<WslConfig>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
}

/// The optional `[discovery]` table: which providers contribute ssh targets to the
/// roster (see [`crate::provision::roster`]).
///
/// Every provider is ON by default, so a machine xmux can reach is a machine xmux
/// offers with nothing to configure. Each flag is how a user narrows that: `ssh-config`
/// off for someone who keeps no ssh config, `tailscale` off for someone who does not
/// want the roster to depend on an external CLI. A provider that cannot run costs an
/// empty list, not an error, so leaving one on is safe on a machine without it.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryConfig {
    /// Read host aliases from `~/.ssh/config`.
    #[serde(rename = "ssh-config", default = "default_true")]
    pub ssh_config: bool,
    /// Offer the online peers of this machine's tailnet, by their DNS label.
    #[serde(default = "default_true")]
    pub tailscale: bool,
    /// Offer this machine's WSL distributions, by the name `wsl.exe` lists them under.
    #[serde(default = "default_true")]
    pub wsl: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        DiscoveryConfig {
            ssh_config: true,
            tailscale: true,
            wsl: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Configures the mux used on the local machine.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalConfig {
    #[serde(default)]
    pub mux: MuxSpec,
}

/// The `mux` value of a machine: ONE mux or SEVERAL. A machine can run more than one
/// mux at a time (a Windows box with psmux and zellij both up), and each is its own
/// source, so the value is a list as readily as a name. A bare string stays valid and
/// means exactly one.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum MuxSpec {
    One(String),
    Many(Vec<String>),
}

impl Default for MuxSpec {
    fn default() -> Self {
        MuxSpec::One(String::new())
    }
}

impl From<&str> for MuxSpec {
    fn from(s: &str) -> Self {
        MuxSpec::One(s.to_string())
    }
}

impl From<Vec<&str>> for MuxSpec {
    fn from(v: Vec<&str>) -> Self {
        MuxSpec::Many(v.into_iter().map(str::to_string).collect())
    }
}

impl MuxSpec {
    /// The mux names this value asks for, in order, without the empty entries a
    /// hand-written list picks up. An unset value yields nothing, which is what lets the
    /// caller apply its own default.
    pub fn names(&self) -> Vec<String> {
        let raw: Vec<&String> = match self {
            MuxSpec::One(s) => vec![s],
            MuxSpec::Many(v) => v.iter().collect(),
        };
        let mut out: Vec<String> = Vec::new();
        for name in raw {
            let name = name.trim();
            if !name.is_empty() && !out.iter().any(|k| k == name) {
                out.push(name.to_string());
            }
        }
        out
    }

    /// True when this value names NO mux, so the caller's default applies.
    pub fn is_unset(&self) -> bool {
        self.names().is_empty()
    }

    /// True when this value asks xmux to decide: unset, or exactly `"auto"`. A list
    /// that names muxes is never auto, even if `"auto"` is one of the entries - a
    /// written name is a name the user meant.
    pub fn is_auto(&self) -> bool {
        let names = self.names();
        names.is_empty() || (names.len() == 1 && names[0] == "auto")
    }
}

/// The optional `[ui]` table: xmux's own prefix.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// The built-in colour theme, named by [`crate::ui::palette`]: `auto-dark` (the
    /// default) or `auto-light`, each painting only ANSI slots so the TERMINAL theme
    /// resolves the actual hues. An unknown name falls back to `auto-dark` and the
    /// doctor reports the resolution. Selecting a theme does not pick colours - the
    /// theme IS the ANSI-slot mapping; see `Colour ownership` in `CONTEXT.md`.
    #[serde(rename = "theme", default = "default_theme")]
    pub theme: String,
    /// xmux's prefix spec (e.g. `C-g`, `C-Space`), config-only like tmux's
    /// `set -g prefix`. Parsed by `display::term::parse_prefix`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// The INITIAL state of the auto-hide-nav mode (toggled live with `prefix t`,
    /// then persisted to `~/.xmux/auto_hide_nav`, which wins over this on later
    /// runs). When the mode is on, focusing the terminal view hides the tree and gives it
    /// the full terminal width; the tree returns when focus returns to it. While
    /// hidden the tree has no column to click, so focus returns via the prefix keys
    /// (`prefix Tab`/`←`). Default false keeps the tree shown in both focus states.
    #[serde(rename = "auto-hide-nav", default)]
    pub auto_hide_nav: bool,
    /// Whether the nav's attachment follows the wide/narrow turnover. False pins the
    /// `force-nav-position` (or the wide default) regardless of the aspect.
    #[serde(rename = "auto-nav-position", default = "default_true")]
    pub auto_nav_position: bool,
    /// The nav placement when the terminal view is the wider (the column layout):
    /// `left` | `top` | `right` | `bottom`. An unknown word falls back to `left`.
    #[serde(rename = "wide-nav-position", default = "default_wide_nav_position")]
    pub wide_nav_position: String,
    /// The nav placement when the turnover picks the band layout. An unknown word
    /// falls back to `top`.
    #[serde(
        rename = "narrow-nav-position",
        default = "default_narrow_nav_position"
    )]
    pub narrow_nav_position: String,
    /// The nav placement while `auto-nav-position` is off. Empty (default) = unset,
    /// which falls back to the wide placement.
    #[serde(rename = "force-nav-position", default)]
    pub force_nav_position: String,
    /// The tree|terminal view border colour OVERRIDES, named after tmux's pane-border
    /// options: the focused side is `view-active-border-style`, the unfocused side
    /// `view-border-style`, the drag-hover cue `view-border-hover-style`. Values use
    /// tmux's colour syntax (parsed by [`crate::ui::chrome::map_color`]). Each
    /// defaults to EMPTY (unset), leaving that side at xmux's own colour
    /// — see [`crate::ui::chrome::ViewBorderColors::resolve`].
    #[serde(rename = "view-active-border-style", default)]
    pub view_active_border_style: String,
    #[serde(rename = "view-border-style", default)]
    pub view_border_style: String,
    #[serde(rename = "view-border-hover-style", default)]
    pub view_border_hover_style: String,
    /// The hint bar's colour as a tmux `status-style` string (`bg=…,fg=…`, tmux colour
    /// colour syntax parsed by [`crate::ui::chrome::parse_hint_bar_style`]). Empty (default)
    /// = the built-in tmux default (themegreen/themeblack → yellowgreen / gray5).
    #[serde(rename = "hint-bar-style", default)]
    pub hint_bar_style: String,
    /// The selected card's background, in the same colour slots as the view border
    /// (`bg=<colour>`, or a bare colour token). Empty (default) means the surface comes
    /// from the terminal's reported background, and NOTHING is painted when the terminal
    /// does not report one - see [`crate::ui::palette`]. This is how a user on a
    /// terminal that answers no colour query (Windows Terminal answers none) gets a
    /// selection surface at all.
    #[serde(rename = "selection-style", default)]
    pub selection_style: String,
    /// Per-role colour OVERRIDES for the chosen theme, named after the palette roles
    /// (see [`crate::ui::palette`]): `primary`, `secondary`, `accent`, `decoration`,
    /// `warning`, `error`, `disabled`, and the hint bar's `bar-bg`, `bar-fg`,
    /// `bar-accent`. Values use the same colour vocabulary as the view border
    /// (parsed by [`crate::ui::chrome::map_color`]): a named ANSI colour, `bright*`,
    /// `colourN`, `#RRGGBB`, or `default`. Each defaults to EMPTY (unset), leaving that
    /// role at the theme's own slot.
    #[serde(rename = "primary", default)]
    pub primary: String,
    #[serde(rename = "secondary", default)]
    pub secondary: String,
    #[serde(rename = "accent", default)]
    pub accent: String,
    #[serde(rename = "decoration", default)]
    pub decoration: String,
    #[serde(rename = "warning", default)]
    pub warning: String,
    #[serde(rename = "error", default)]
    pub error: String,
    #[serde(rename = "disabled", default)]
    pub disabled: String,
    #[serde(rename = "bar-bg", default)]
    pub bar_bg: String,
    #[serde(rename = "bar-fg", default)]
    pub bar_fg: String,
    #[serde(rename = "bar-accent", default)]
    pub bar_accent: String,
}

fn default_prefix() -> String {
    "C-g".to_string()
}

fn default_wide_nav_position() -> String {
    "left".to_string()
}

fn default_narrow_nav_position() -> String {
    "top".to_string()
}

impl UiConfig {
    /// The nav-position settings the per-frame resolution starts from. Each placement
    /// word is parsed here and an unknown one falls back to its own default, so the
    /// resolver never sees a garbage value. An empty force means none is forced.
    pub fn nav_position_setting(&self) -> NavPositionSetting {
        NavPositionSetting {
            auto: self.auto_nav_position,
            wide: NavPosition::parse(&self.wide_nav_position)
                .unwrap_or(NavPositionSetting::default().wide),
            narrow: NavPosition::parse(&self.narrow_nav_position)
                .unwrap_or(NavPositionSetting::default().narrow),
            force: NavPosition::parse(&self.force_nav_position),
        }
    }
}

fn default_theme() -> String {
    crate::ui::palette::AUTO_DARK.to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: default_theme(),
            prefix: default_prefix(),
            auto_hide_nav: false,
            auto_nav_position: true,
            wide_nav_position: default_wide_nav_position(),
            narrow_nav_position: default_narrow_nav_position(),
            // Empty = unset: the force falls back to the wide placement.
            force_nav_position: String::new(),
            // Empty = unset: the effective colour is ViewBorderColors::default().
            view_active_border_style: String::new(),
            view_border_style: String::new(),
            view_border_hover_style: String::new(),
            // Empty = the built-in tmux default hint bar style (see
            // crate::ui::chrome::hint_bar_default_style).
            hint_bar_style: String::new(),
            // Empty = no selection surface of xmux's own choosing (see
            // crate::ui::palette).
            selection_style: String::new(),
            // Empty = unset: that role keeps the theme's own slot (see
            // crate::ui::palette::Overrides).
            primary: String::new(),
            secondary: String::new(),
            accent: String::new(),
            decoration: String::new(),
            warning: String::new(),
            error: String::new(),
            disabled: String::new(),
            bar_bg: String::new(),
            bar_fg: String::new(),
            bar_accent: String::new(),
        }
    }
}

/// Overrides the mux for a discovered ssh alias, or adds a host that ssh-config
/// discovery did not surface.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HostConfig {
    #[serde(default)]
    pub ssh: String,
    #[serde(default)]
    pub mux: MuxSpec,
}

/// Overrides the mux for a WSL distribution, or names one `[discovery] wsl` is not
/// listing. `distro` is the bare name `wsl.exe` reports (`Ubuntu-24.04`); the machine it
/// becomes carries the WSL prefix, so `exclude` names it as `wsl.Ubuntu-24.04`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WslConfig {
    #[serde(default)]
    pub distro: String,
    #[serde(default)]
    pub mux: MuxSpec,
}

/// A resolved remote SOURCE: one mux on one machine. `id` is the source id the rest of
/// the app keys everything by, `alias` is the ssh destination it is reached at (several
/// sources share it when a machine runs several muxes), and `bin` is the mux binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub id: String,
    pub alias: String,
    pub bin: String,
}

/// Reads `config.toml` from `path`. A missing file yields a zero [`Config`] and
/// no error; a parse error is returned to the caller (treated as fatal).
pub fn load(path: &Path) -> anyhow::Result<Config> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e.into()),
    };
    Ok(toml::from_str(&content)?)
}

/// Behaves like [`load`] but also returns human-readable warnings for any keys
/// present in the file that did not decode into [`Config`] (typos, removed or
/// unsupported options). A missing file yields no warnings and no error.
pub fn load_verbose(path: &Path) -> anyhow::Result<(Config, Vec<String>)> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Config::default(), Vec::new()))
        }
        Err(e) => return Err(e.into()),
    };
    let mut warnings = Vec::new();
    let de = toml::de::Deserializer::parse(&content)?;
    let cfg: Config = serde_ignored::deserialize(de, |path| {
        warnings.push(format!("unknown key {:?}", path.to_string()));
    })?;
    Ok((cfg, warnings))
}

impl Config {
    /// The mux binaries to run on the local machine, in order.
    ///
    /// A written value is taken verbatim, and a LIST yields one source per entry: a name
    /// the user wrote is a name they meant, even if it is not installed (it then shows as
    /// unreachable rather than vanishing). A written name no kind owns is dropped here
    /// and warned at load: an unknown name is never decoded to a kind that does exist.
    ///
    /// An unset or `"auto"` value means "whatever this machine actually has", so `installed`
    /// (from `mux::installed_muxes`) becomes the list, with the `os`'s conventional mux
    /// first so a single-mux box reads exactly as it always did. A box where discovery
    /// finds nothing yields an empty list: the local sources name what the box actually
    /// has, so nothing installed means no local source, not a mux that is not there.
    pub fn local_muxes(&self, os: &str, installed: &[String]) -> Vec<String> {
        if !self.local.mux.is_auto() {
            return self
                .local
                .mux
                .names()
                .into_iter()
                .filter(|n| crate::mux::is_recognized(n))
                .collect();
        }
        let conventional = if os == "windows" { "psmux" } else { "tmux" };
        let mut out: Vec<String> = Vec::new();
        if installed.iter().any(|m| m == conventional) {
            out.push(conventional.to_string());
        }
        out.extend(
            installed
                .iter()
                .filter(|m| m.as_str() != conventional)
                .cloned(),
        );
        out
    }

    /// Advisory warnings for `mux` values that DECODE but name no mux xmux knows
    /// (e.g. a `"tmuxx"` typo), which would otherwise silently run as tmux. Emitted
    /// through the existing `cfg_warnings` channel (surfaced by `xmux doctor`). The
    /// documented defaults `""`/`"auto"` never warn.
    pub fn value_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for name in self.local.mux.names() {
            if name != "auto" && !crate::mux::is_recognized(&name) {
                warnings.push(format!(
                    "local mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen); no source is created for it"
                ));
            }
        }
        for h in &self.hosts {
            for name in h.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "host {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen); no source is created for it",
                        h.ssh
                    ));
                }
            }
        }
        for w in &self.wsl {
            for name in w.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "wsl {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen); no source is created for it",
                        w.distro
                    ));
                }
            }
        }
        warnings
    }

    /// Whether `machine`'s mux list is xmux's to decide: unset, or exactly `"auto"`.
    /// A machine that named its muxes is never probed - a written name is taken verbatim,
    /// and probing could only add ones the user did not ask for.
    pub fn mux_is_auto(&self, machine: &str) -> bool {
        if machine == crate::session::LOCAL_SOURCE {
            return self.local.mux.is_auto();
        }
        if let Some(distro) = crate::session::wsl_distro_of(machine) {
            return self
                .wsl
                .iter()
                .find(|w| w.distro == distro)
                .is_none_or(|w| w.mux.is_auto());
        }
        // First entry wins, mirroring `host_specs`; no entry at all is auto.
        self.hosts
            .iter()
            .find(|h| h.ssh == machine)
            .is_none_or(|h| h.mux.is_auto())
    }

    /// xmux's configured prefix spec.
    pub fn ui_prefix(&self) -> &str {
        &self.ui.prefix
    }

    /// The initial auto-hide-nav mode from config (default false). The live toggle's
    /// persisted state, when present, overrides this — see `state::load_auto_hide_nav`.
    pub fn ui_auto_hide_nav(&self) -> bool {
        self.ui.auto_hide_nav
    }

    /// Merges ssh-config discovery with the config file. Discovered aliases come
    /// first in their original order (each deduped and skipping any in
    /// `exclude`), with the mux taken from a matching `hosts` override or
    /// defaulting to `"tmux"`. Config-only hosts (`hosts` entries whose ssh alias
    /// was not discovered) are appended afterwards. Config augments discovery; it
    /// never replaces it.
    ///
    /// A machine configured with SEVERAL muxes yields one spec per mux, all sharing the
    /// ssh alias and each carrying its own qualified source id. `exclude` names
    /// MACHINES, so excluding one drops every mux on it.
    pub fn host_specs(&self, ssh_aliases: &[String]) -> Vec<HostSpec> {
        let configured: Vec<(&str, &MuxSpec)> = self
            .hosts
            .iter()
            .map(|h| (h.ssh.as_str(), &h.mux))
            .collect();
        merge_specs(
            ssh_aliases,
            &configured,
            &self.excluded(),
            is_reserved_alias,
        )
    }

    /// The WSL sources: one spec per mux on each distribution, merged the same way
    /// [`host_specs`](Self::host_specs) merges ssh hosts. `distro_machines` are the
    /// MACHINE names `[discovery] wsl` listed (`wsl.Ubuntu-24.04`); a `[[wsl]]` entry
    /// names its distribution bare and is prefixed here, so both halves key alike.
    ///
    /// `exclude` names MACHINES here too, which for this kind is the prefixed name.
    pub fn wsl_specs(&self, distro_machines: &[String]) -> Vec<HostSpec> {
        let prefixed: Vec<String> = self
            .wsl
            .iter()
            .map(|w| {
                if w.distro.is_empty() {
                    String::new()
                } else {
                    format!("{}{}", crate::session::WSL_PREFIX, w.distro)
                }
            })
            .collect();
        let configured: Vec<(&str, &MuxSpec)> = prefixed
            .iter()
            .map(String::as_str)
            .zip(self.wsl.iter().map(|w| &w.mux))
            .collect();
        // Nothing to reserve: every name here already carries the WSL prefix, so it can
        // collide with neither `local` nor an ssh alias `host_specs` accepted.
        merge_specs(distro_machines, &configured, &self.excluded(), |_| false)
    }

    /// The machines `exclude` names, as a lookup.
    fn excluded(&self) -> std::collections::HashSet<&str> {
        self.exclude.iter().map(String::as_str).collect()
    }
}

/// The machine names the ssh kind may not claim: `local` is this machine's own, and a
/// `wsl.`-prefixed name is a WSL distribution's. Either would otherwise be built as an
/// ssh destination and shadow the machine that owns the name, so an ssh alias spelled
/// either way is dropped rather than served ambiguously.
fn is_reserved_alias(machine: &str) -> bool {
    machine == crate::session::LOCAL_SOURCE || crate::session::wsl_distro_of(machine).is_some()
}

/// The merge every machine kind's spec list follows: `discovered` names first, in the
/// order their provider gave them, then the `configured` entries that were not
/// discovered. Config augments discovery; it never replaces it.
///
/// A name that is excluded, reserved, or already taken is skipped, and a machine's mux
/// list is its config override or the conventional `tmux`. A machine configured with
/// SEVERAL muxes yields one spec per mux, all sharing the machine and each carrying its
/// own qualified source id.
fn merge_specs(
    discovered: &[String],
    configured: &[(&str, &MuxSpec)],
    excluded: &std::collections::HashSet<&str>,
    is_reserved: impl Fn(&str) -> bool,
) -> Vec<HostSpec> {
    use std::collections::HashSet;

    let mut override_mux: std::collections::HashMap<&str, &MuxSpec> =
        std::collections::HashMap::new();
    for (machine, mux) in configured {
        if machine.is_empty() {
            continue;
        }
        // First entry wins; a later duplicate with an unset mux must never
        // clobber an explicit one already recorded for the same machine.
        let replace = match override_mux.get(machine) {
            None => true,
            Some(existing) => existing.is_unset() && !mux.is_unset(),
        };
        if replace {
            override_mux.insert(machine, mux);
        }
    }

    let mut specs = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for machine in discovered {
        let machine = machine.as_str();
        if is_reserved(machine) || excluded.contains(machine) || !seen.insert(machine) {
            continue;
        }
        let muxes = override_mux
            .get(machine)
            .map(|m| m.names())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| vec!["tmux".to_string()]);
        specs.extend(host_specs_for(machine, &muxes));
    }

    for (machine, mux) in configured {
        if machine.is_empty()
            || is_reserved(machine)
            || excluded.contains(machine)
            || !seen.insert(machine)
        {
            continue;
        }
        let muxes = if mux.is_unset() {
            vec!["tmux".to_string()]
        } else {
            mux.names()
        };
        specs.extend(host_specs_for(machine, &muxes));
    }

    specs
}

/// One [`HostSpec`] per mux on `alias`. The id is qualified only when the machine
/// serves more than one, so a single-mux host keeps the bare alias it always had.
fn host_specs_for(alias: &str, muxes: &[String]) -> Vec<HostSpec> {
    // A written name no kind owns is dropped (warned at load), never decoded to a
    // kind that does exist; the qualified-id count reads the names that survive.
    let muxes: Vec<&String> = muxes
        .iter()
        .filter(|bin| crate::mux::is_recognized(bin.as_str()))
        .collect();
    let qualified = muxes.len() > 1;
    muxes
        .iter()
        .map(|bin| HostSpec {
            id: crate::session::source_id(alias, bin, qualified),
            alias: alias.to_string(),
            bin: (*bin).clone(),
        })
        .collect()
}

/// Parses an OpenSSH client config at `path` and returns the concrete host
/// aliases declared by `Host` lines, in first-seen order and deduplicated. Glob
/// patterns (containing `*`, `?`, or `[...]`) and negations (starting with `!`)
/// are skipped, as are comments, blank lines, and non-`Host` directives.
/// Backslash line continuations and `Include` directives are honored: an
/// `Include` glob is expanded (relative to the including file, with `~` expanded
/// to the home the shell ssh uses) and the included files are parsed in turn,
/// with include cycles broken. `Match` blocks declare no aliases of their own —
/// they only apply options to hosts named elsewhere — so they contribute nothing
/// here; `host_stanza` still shows them for display. A missing file yields an
/// empty list.
pub fn ssh_host_aliases(path: &Path) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = Vec::new();
    collect_ssh_aliases(path, &mut aliases, &mut seen, &mut stack);
    aliases
}

/// Recursively reads `path`'s `Host` aliases into `aliases`, expanding `Include`
/// directives. `stack` holds the canonical include chain so a cycle (A includes B
/// includes A) terminates instead of looping.
fn collect_ssh_aliases(
    path: &Path,
    aliases: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
    stack: &mut Vec<std::path::PathBuf>,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canonical) {
        return;
    }
    stack.push(canonical);

    for line in logical_lines(&content) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(directive) = fields.next() else {
            continue;
        };
        if directive.eq_ignore_ascii_case("Include") {
            for pattern in fields {
                for included in expand_include(pattern, path) {
                    collect_ssh_aliases(&included, aliases, seen, stack);
                }
            }
            continue;
        }
        if !directive.eq_ignore_ascii_case("Host") {
            continue;
        }
        for pattern in fields {
            if pattern.starts_with('!') || has_glob(pattern) {
                continue;
            }
            if seen.insert(pattern.to_string()) {
                aliases.push(pattern.to_string());
            }
        }
    }

    stack.pop();
}

/// Splits `content` into logical config lines, joining a line that ends in a
/// backslash with the following line (OpenSSH's continuation syntax). The
/// backslash and newline collapse to a single space.
fn logical_lines(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for raw in content.lines() {
        let trimmed = raw.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            current.push_str(stripped.trim_end());
            current.push(' ');
        } else {
            current.push_str(raw);
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Expands an `Include` pattern to the files it names: a leading `~` becomes the
/// home the shell ssh uses, and a relative pattern is resolved against the
/// directory of the including config file. Glob metacharacters are then matched
/// against the filesystem.
fn expand_include(pattern: &str, from: &Path) -> Vec<std::path::PathBuf> {
    let expanded = if pattern == "~" {
        crate::provision::env::ssh_home()
    } else if let Some(rest) = pattern.strip_prefix("~/") {
        crate::provision::env::ssh_home().join(rest)
    } else {
        std::path::PathBuf::from(pattern)
    };
    let full = if expanded.is_absolute() {
        expanded
    } else {
        from.parent().unwrap_or(from).join(expanded)
    };
    glob_walk(&full)
}

/// Walks `pattern`, treating `*`, `?`, and `[...]` as globs and resolving literal
/// segments as paths. Returns the matching files in sorted order.
fn glob_walk(pattern: &std::path::Path) -> Vec<std::path::PathBuf> {
    use std::path::Component;
    let mut matches = Vec::new();
    let mut base = std::path::PathBuf::new();
    let mut segs: Vec<String> = Vec::new();
    for comp in pattern.components() {
        match comp {
            Component::Prefix(p) => base.push(p.as_os_str()),
            Component::RootDir => base.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => base.push(".."),
            Component::Normal(s) => segs.push(s.to_string_lossy().into_owned()),
        }
    }
    if segs.is_empty() {
        return matches;
    }
    let last = segs.len() - 1;
    let mut dirs = vec![base];
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i == last;
        let mut next = Vec::new();
        for dir in &dirs {
            if has_glob(seg) {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if !glob_match(seg, &name.to_string_lossy()) {
                        continue;
                    }
                    let p = dir.join(&name);
                    let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
                    if is_last {
                        if is_file {
                            matches.push(p);
                        }
                    } else if !is_file {
                        next.push(p);
                    }
                }
            } else {
                let p = dir.join(seg);
                if is_last {
                    if p.is_file() {
                        matches.push(p);
                    }
                } else if p.is_dir() {
                    next.push(p);
                }
            }
        }
        dirs = next;
    }
    matches.sort();
    matches
}

fn has_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Matches `name` against the glob `pat`, supporting `*`, `?`, and `[...]`
/// character classes (with `!`/`^` negation and `a-z` ranges).
fn glob_match(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    fn rec(p: &[char], n: &[char]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            '*' => {
                if rec(&p[1..], n) {
                    return true;
                }
                !n.is_empty() && rec(p, &n[1..])
            }
            '?' => !n.is_empty() && rec(&p[1..], &n[1..]),
            '[' => {
                if n.is_empty() {
                    return false;
                }
                match parse_class(p, n[0]) {
                    Some((matched, rest)) => matched && rec(rest, &n[1..]),
                    None => false,
                }
            }
            c => !n.is_empty() && n[0] == c && rec(&p[1..], &n[1..]),
        }
    }
    rec(&p, &n)
}

/// Parses a `[...]` character class at the head of `p`. Returns whether `c` is in
/// the class and the remaining pattern past the closing `]`; `None` for an
/// unterminated class.
fn parse_class(p: &[char], c: char) -> Option<(bool, &[char])> {
    let mut i = 1;
    let negate = i < p.len() && (p[i] == '!' || p[i] == '^');
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        let ch = p[i];
        if ch == ']' && !first {
            return Some((matched != negate, &p[i + 1..]));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            let (lo, hi) = (ch, p[i + 2]);
            if lo <= c && c <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if ch == c {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// Returns the raw ssh-config stanza(s) that name `alias`: every `Host`/`Match`
/// block whose header line lists `alias` as a whitespace token, joined with a blank
/// line between blocks. A stanza runs from its `Host`/`Match` header to the next
/// header (or EOF). Display text only — Match-resolved values (e.g. an exec-chosen
/// HostName) are NOT computed; the literal config lines are shown. Empty when no
/// block names the alias.
pub fn host_stanza(config_text: &str, alias: &str) -> String {
    let is_header = |l: &str| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("Host") || w.eq_ignore_ascii_case("Match"))
    };
    let names_alias = |l: &str| l.split_whitespace().skip(1).any(|tok| tok == alias);

    let lines = logical_lines(config_text);
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_header(&lines[i]) && names_alias(&lines[i]) {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.push(lines[i].trim_end().to_string());
            i += 1;
            while i < lines.len() && !is_header(&lines[i]) {
                out.push(lines[i].trim_end().to_string());
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::switcher::{NavPosition, NavPositionSetting};
    use std::io::Write;

    fn write_temp(content: &str, name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xmux-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Unique per-name file so parallel tests do not collide.
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn load_missing_file() {
        let missing = std::env::temp_dir().join("xmux-does-not-exist-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert!(cfg.hosts.is_empty());
        assert!(cfg.exclude.is_empty());
        assert!(cfg.local.mux.is_unset());
    }

    #[test]
    fn load_round_trip() {
        let path = write_temp(
            r#"
exclude = ["foo", "bar"]

[local]
mux = "tmux"

[[hosts]]
ssh = "prod"
mux = "psmux"

[[hosts]]
ssh = "stage"
"#,
            "round-trip.toml",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.local.mux.names(), vec!["tmux"]);
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].ssh, "prod");
        assert_eq!(cfg.hosts[0].mux.names(), vec!["psmux"]);
        assert_eq!(cfg.hosts[1].ssh, "stage");
        assert!(cfg.hosts[1].mux.is_unset());
        assert_eq!(cfg.exclude, vec!["foo", "bar"]);
    }

    #[test]
    fn load_malformed() {
        let path = write_temp("this is = = not valid toml [[[", "malformed.toml");
        assert!(load(&path).is_err());
    }

    #[test]
    fn load_verbose_missing_file() {
        let missing = std::env::temp_dir().join("xmux-nope-xyz.toml");
        let (cfg, warnings) = load_verbose(&missing).unwrap();
        assert!(warnings.is_empty());
        assert!(cfg.local.mux.is_unset());
    }

    #[test]
    fn load_verbose_unknown_key() {
        let path = write_temp(
            r#"
[local]
mux = "tmux"
bogus = "nope"
"#,
            "unknown-key.toml",
        );
        let (cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(cfg.local.mux.names(), vec!["tmux"]);
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert_eq!(warnings[0], r#"unknown key "local.bogus""#);
    }

    #[test]
    fn host_specs_merge() {
        let cfg = Config {
            hosts: vec![
                HostConfig {
                    ssh: "prod".into(),
                    mux: "psmux".into(),
                },
                HostConfig {
                    ssh: "extra".into(),
                    mux: "zellij".into(),
                },
                HostConfig {
                    ssh: "noMuxOnly".into(),
                    mux: "".into(),
                },
                HostConfig {
                    ssh: "".into(),
                    mux: "ignored".into(),
                },
            ],
            exclude: vec!["banned".into()],
            ..Default::default()
        };
        let ssh_aliases: Vec<String> = ["prod", "banned", "stage", "prod"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let got = cfg.host_specs(&ssh_aliases);
        let want = vec![
            HostSpec {
                id: "prod".into(),
                alias: "prod".into(),
                bin: "psmux".into(),
            },
            HostSpec {
                id: "stage".into(),
                alias: "stage".into(),
                bin: "tmux".into(),
            },
            HostSpec {
                id: "extra".into(),
                alias: "extra".into(),
                bin: "zellij".into(),
            },
            HostSpec {
                id: "noMuxOnly".into(),
                alias: "noMuxOnly".into(),
                bin: "tmux".into(),
            },
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn host_specs_duplicate_empty_mux_does_not_clobber() {
        // A later [[hosts]] for the same ssh with an empty mux must not erase the
        // explicit mux recorded earlier.
        let cfg = Config {
            hosts: vec![
                HostConfig {
                    ssh: "prod".into(),
                    mux: "psmux".into(),
                },
                HostConfig {
                    ssh: "prod".into(),
                    mux: MuxSpec::default(),
                },
            ],
            ..Default::default()
        };
        let got = cfg.host_specs(&["prod".to_string()]);
        let prod = got
            .iter()
            .find(|s| s.alias == "prod")
            .expect("prod present");
        assert_eq!(
            prod.bin, "psmux",
            "explicit mux must survive a later empty dup"
        );
    }

    #[test]
    fn host_specs_excludes_reserved_local_alias() {
        // "local" is reserved for the local mux source; an ssh alias or a config
        // host named "local" must never shadow it.
        let cfg = Config {
            hosts: vec![HostConfig {
                ssh: "local".into(),
                mux: "psmux".into(),
            }],
            ..Default::default()
        };
        let ssh_aliases: Vec<String> = ["local", "prod"].iter().map(|s| s.to_string()).collect();
        let got = cfg.host_specs(&ssh_aliases);
        assert!(
            !got.iter().any(|s| s.alias == "local"),
            "reserved 'local' alias must be excluded: {got:?}"
        );
        assert!(got.iter().any(|s| s.alias == "prod"));
    }

    #[test]
    fn host_specs_excludes_config_only() {
        let cfg = Config {
            hosts: vec![HostConfig {
                ssh: "secret".into(),
                mux: "psmux".into(),
            }],
            exclude: vec!["secret".into()],
            ..Default::default()
        };
        assert!(cfg.host_specs(&[]).is_empty());
    }

    #[test]
    fn a_written_mux_is_taken_verbatim_and_auto_is_what_the_box_has() {
        // A name the user wrote wins over anything discovered, on either OS. `auto` and
        // unset take the discovered list instead - that is the whole point of the default.
        let installed = vec!["tmux".to_string(), "zellij".to_string()];
        let cases: &[(&str, &str, &[&str])] = &[
            ("", "windows", &["tmux", "zellij"]),
            ("", "linux", &["tmux", "zellij"]),
            ("auto", "windows", &["tmux", "zellij"]),
            ("auto", "linux", &["tmux", "zellij"]),
            ("zellij", "windows", &["zellij"]),
            ("zellij", "linux", &["zellij"]),
        ];
        for &(mux, os, want) in cases {
            let c = Config {
                local: LocalConfig { mux: mux.into() },
                ..Default::default()
            };
            assert_eq!(c.local_muxes(os, &installed), want, "mux={mux:?} os={os:?}");
        }
    }

    #[test]
    fn the_conventional_mux_leads_the_discovered_list() {
        // The order decides which source paints first and reads as this machine's main one,
        // so a Windows box that has both psmux and tmux leads with psmux and a unix box
        // leads with tmux, exactly as a single-mux box always did.
        let c = Config::default();
        let installed = vec![
            "tmux".to_string(),
            "psmux".to_string(),
            "zellij".to_string(),
        ];
        assert_eq!(
            c.local_muxes("windows", &installed),
            vec!["psmux", "tmux", "zellij"]
        );
        assert_eq!(
            c.local_muxes("linux", &installed),
            vec!["tmux", "psmux", "zellij"]
        );
    }

    #[test]
    fn a_box_where_nothing_answered_offers_no_local_source() {
        // Discovery finding nothing is a box with no mux installed: the source list must
        // say so rather than fabricate the conventional mux. A mux that is not there
        // must not appear as a local source.
        let c = Config::default();
        assert!(c.local_muxes("windows", &[]).is_empty());
        assert!(c.local_muxes("linux", &[]).is_empty());
    }

    #[test]
    fn only_a_machine_that_named_no_mux_is_xmuxs_to_decide() {
        // Discovery probes a machine only when the config left the choice open. A written
        // name is verbatim, so probing it could only add muxes nobody asked for.
        let cfg = Config {
            local: LocalConfig { mux: "auto".into() },
            hosts: vec![
                HostConfig {
                    ssh: "written".into(),
                    mux: "zellij".into(),
                },
                HostConfig {
                    ssh: "blank".into(),
                    mux: MuxSpec::default(),
                },
            ],
            ..Default::default()
        };
        assert!(cfg.mux_is_auto("local"), "unset/auto local");
        assert!(cfg.mux_is_auto("blank"), "an entry with no mux");
        assert!(cfg.mux_is_auto("never-configured"), "no entry at all");
        assert!(!cfg.mux_is_auto("written"), "a written mux is not probed");

        let explicit_local = Config {
            local: LocalConfig {
                mux: vec!["psmux", "zellij"].into(),
            },
            ..Default::default()
        };
        assert!(!explicit_local.mux_is_auto("local"));
    }

    #[test]
    fn a_machine_can_be_given_several_muxes() {
        // The point of the list: one machine, several muxes, each its own source. The
        // ids say which mux, and they all reach the same ssh destination.
        let cfg = Config {
            local: LocalConfig {
                mux: vec!["psmux", "zellij"].into(),
            },
            hosts: vec![HostConfig {
                ssh: "prod".into(),
                mux: vec!["tmux", "zellij"].into(),
            }],
            ..Default::default()
        };
        assert_eq!(cfg.local_muxes("windows", &[]), vec!["psmux", "zellij"]);
        let got = cfg.host_specs(&["prod".to_string()]);
        assert_eq!(
            got,
            vec![
                HostSpec {
                    id: "prod:tmux".into(),
                    alias: "prod".into(),
                    bin: "tmux".into(),
                },
                HostSpec {
                    id: "prod:zellij".into(),
                    alias: "prod".into(),
                    bin: "zellij".into(),
                },
            ]
        );
    }

    #[test]
    fn one_mux_on_a_machine_keeps_the_bare_id() {
        // A single-mux machine must be spelled exactly as before, whether it was named
        // with a bare string or a one-entry list: the id is what the user types and what
        // saved state is keyed by.
        for spec in [MuxSpec::from("zellij"), MuxSpec::from(vec!["zellij"])] {
            let cfg = Config {
                hosts: vec![HostConfig {
                    ssh: "prod".into(),
                    mux: spec.clone(),
                }],
                ..Default::default()
            };
            let got = cfg.host_specs(&["prod".to_string()]);
            assert_eq!(got.len(), 1, "spec={spec:?}");
            assert_eq!(got[0].id, "prod", "spec={spec:?}");
            assert_eq!(got[0].bin, "zellij", "spec={spec:?}");
        }
        // Same for this machine.
        let cfg = Config {
            local: LocalConfig {
                mux: "zellij".into(),
            },
            ..Default::default()
        };
        assert_eq!(cfg.local_muxes("windows", &[]), vec!["zellij"]);
    }

    #[test]
    fn excluding_a_machine_drops_every_mux_on_it() {
        // `exclude` names MACHINES, so it cannot half-exclude one.
        let cfg = Config {
            exclude: vec!["prod".into()],
            hosts: vec![HostConfig {
                ssh: "prod".into(),
                mux: vec!["tmux", "zellij"].into(),
            }],
            ..Default::default()
        };
        assert!(cfg.host_specs(&["prod".to_string()]).is_empty());
    }

    #[test]
    fn a_mux_list_parses_from_toml_beside_a_bare_name() {
        let path = write_temp(
            r#"
[local]
mux = ["psmux", "zellij"]

[[hosts]]
ssh = "prod"
mux = "tmux"
"#,
            "mux-list.toml",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.local.mux.names(), vec!["psmux", "zellij"]);
        assert_eq!(cfg.hosts[0].mux.names(), vec!["tmux"]);
    }

    #[test]
    fn a_mux_list_drops_blanks_and_repeats() {
        // A hand-written list picks up empty entries and duplicates; neither may become
        // a source (a duplicate would collide on its own id).
        let spec = MuxSpec::from(vec!["tmux", "", "  ", "tmux", "zellij"]);
        assert_eq!(spec.names(), vec!["tmux", "zellij"]);
        assert!(MuxSpec::from(vec!["", " "]).is_unset());
        assert!(MuxSpec::from("").is_unset());
    }

    #[test]
    fn value_warnings_flags_unrecognized_mux() {
        // Documented defaults and recognized muxes never warn.
        for mux in ["", "auto", "tmux", "psmux", "zellij"] {
            let c = Config {
                local: LocalConfig { mux: mux.into() },
                ..Default::default()
            };
            assert!(c.value_warnings().is_empty(), "mux={mux:?} must not warn");
        }
        // An unrecognized local mux warns exactly once and names the value.
        let c = Config {
            local: LocalConfig {
                mux: "byobu".into(),
            },
            ..Default::default()
        };
        let w = c.value_warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("byobu"), "{w:?}");
        // A recognized host mux is silent; an unrecognized one warns once and names
        // both the host alias and the bad value.
        let c = Config {
            hosts: vec![
                HostConfig {
                    ssh: "prod".into(),
                    mux: "psmux".into(),
                },
                HostConfig {
                    ssh: "bad".into(),
                    mux: "kitty".into(),
                },
            ],
            ..Default::default()
        };
        let w = c.value_warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("bad") && w[0].contains("kitty"), "{w:?}");
    }

    #[test]
    fn wsl_specs_merge_listed_distributions_with_config_entries() {
        // The same merge as `host_specs`: listed machines first in the order `wsl.exe`
        // gave them, then a `[[wsl]]` entry that was not listed. The default mux is tmux,
        // because a distribution is a Linux machine.
        let cfg = Config {
            wsl: vec![
                WslConfig {
                    distro: "Ubuntu-24.04".into(),
                    mux: vec!["tmux", "zellij"].into(),
                },
                WslConfig {
                    distro: "Alpine".into(),
                    mux: MuxSpec::default(),
                },
            ],
            ..Config::default()
        };
        let listed: Vec<String> = ["wsl.Ubuntu-24.04", "wsl.docker-desktop"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got: Vec<(String, String, String)> = cfg
            .wsl_specs(&listed)
            .into_iter()
            .map(|s| (s.id, s.alias, s.bin))
            .collect();
        assert_eq!(
            got,
            vec![
                // Two muxes on one distribution, so both ids name theirs.
                (
                    "wsl.Ubuntu-24.04:tmux".to_string(),
                    "wsl.Ubuntu-24.04".to_string(),
                    "tmux".to_string()
                ),
                (
                    "wsl.Ubuntu-24.04:zellij".to_string(),
                    "wsl.Ubuntu-24.04".to_string(),
                    "zellij".to_string()
                ),
                // Listed, not configured: the conventional mux, and a bare id.
                (
                    "wsl.docker-desktop".to_string(),
                    "wsl.docker-desktop".to_string(),
                    "tmux".to_string()
                ),
                // Configured, not listed: appended, so one distribution is served
                // without listing every one of them.
                (
                    "wsl.Alpine".to_string(),
                    "wsl.Alpine".to_string(),
                    "tmux".to_string()
                ),
            ]
        );
    }

    #[test]
    fn exclude_names_a_wsl_machine_by_its_prefixed_name() {
        // `exclude` names MACHINES, and a distribution's machine name carries the WSL
        // prefix — which is how the Docker Desktop distributions are dropped.
        let cfg = Config {
            exclude: vec!["wsl.docker-desktop".into()],
            ..Config::default()
        };
        let listed: Vec<String> = ["wsl.Ubuntu", "wsl.docker-desktop"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ids: Vec<String> = cfg.wsl_specs(&listed).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["wsl.Ubuntu"]);
    }

    #[test]
    fn an_ssh_alias_may_not_claim_a_wsl_machine_name() {
        // A `wsl.`-prefixed name belongs to the WSL kind, and `kind_for` reads the
        // kind out of the name. An ssh alias spelled that way would be built as a WSL
        // machine, so it is dropped instead of served as the wrong kind.
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "wsl.internal", "local"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ids: Vec<String> = cfg.host_specs(&aliases).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["prod"], "only the plain alias is served");
    }

    #[test]
    fn mux_is_auto_reads_the_wsl_table_for_a_wsl_machine() {
        // The async mux discovery asks this per MACHINE. A distribution that named its
        // muxes must not be probed, and one with no entry is xmux's to decide.
        let cfg = Config {
            wsl: vec![WslConfig {
                distro: "Ubuntu".into(),
                mux: "zellij".into(),
            }],
            ..Config::default()
        };
        assert!(!cfg.mux_is_auto("wsl.Ubuntu"));
        assert!(cfg.mux_is_auto("wsl.Alpine"));
    }

    #[test]
    fn an_unrecognized_wsl_mux_warns() {
        let cfg = Config {
            wsl: vec![WslConfig {
                distro: "Ubuntu".into(),
                mux: "tmuxx".into(),
            }],
            ..Config::default()
        };
        let warnings = cfg.value_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("tmuxx"), "{warnings:?}");
        assert!(warnings[0].contains("Ubuntu"), "{warnings:?}");
    }

    #[test]
    fn every_roster_provider_answers_by_default() {
        // No provider waits to be asked for. One that cannot run on this machine costs an
        // empty list rather than an error, so being on where there is nothing to say
        // costs nothing.
        let d = DiscoveryConfig::default();
        assert!(d.ssh_config && d.tailscale && d.wsl);
    }

    #[test]
    fn a_partial_discovery_table_leaves_the_others_on() {
        // The default is per KEY, not per table: a config that names one provider to
        // narrow the roster must not silently drop the ones it did not mention.
        let path = write_temp(
            "[discovery]
tailscale = false
",
            "partial-discovery.toml",
        );
        let d = load(&path).unwrap().discovery;
        assert!(!d.tailscale, "the key that was written is honoured");
        assert!(d.ssh_config && d.wsl, "the keys left out stay on");
    }

    #[test]
    fn ui_hint_bar_style_defaults_empty_and_parses() {
        // Missing key ⇒ empty (the app then uses the built-in tmux default).
        let missing = std::env::temp_dir().join("xmux-hintbar-absent-xyz.toml");
        assert_eq!(load(&missing).unwrap().ui.hint_bar_style, "");
        // An explicit value round-trips as the raw tmux-style string.
        let path = write_temp(
            "[ui]\nhint-bar-style = \"bg=blue,fg=white\"\n",
            "ui-hintbar.toml",
        );
        assert_eq!(load(&path).unwrap().ui.hint_bar_style, "bg=blue,fg=white");
    }

    #[test]
    fn ui_table_defaults_and_overrides() {
        // Missing [ui] → default prefix "C-g".
        let missing = std::env::temp_dir().join("xmux-ui-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui_prefix(), "C-g");

        // Explicit [ui] overrides prefix.
        let path = write_temp(
            r#"
[ui]
prefix = "C-Space"
"#,
            "ui-override.toml",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui_prefix(), "C-Space");
    }

    #[test]
    fn ui_theme_defaults_to_auto_dark_and_parses_any_name() {
        // `[ui] theme` names a built-in theme; missing → `auto-dark`. Any string is
        // stored (an unknown name is a resolution/fallback concern of the palette,
        // not a config error), so the config test only pins the default and the round
        // trip.
        let missing = std::env::temp_dir().join("xmux-theme-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.theme, crate::ui::palette::AUTO_DARK);
        let path = write_temp("[ui]\ntheme = \"auto-light\"\n", "ui-theme.toml");
        let (cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(cfg.ui.theme, "auto-light");
        assert!(warnings.is_empty(), "theme is a known key: {warnings:?}");
    }

    #[test]
    fn ui_unknown_key_still_warns() {
        // serde_ignored must still surface a typo'd key under [ui].
        let path = write_temp(
            r#"
[ui]
prefix = "C-g"
bogus = "nope"
"#,
            "ui-unknown.toml",
        );
        let (cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(cfg.ui_prefix(), "C-g");
        assert_eq!(warnings, vec![r#"unknown key "ui.bogus""#.to_string()]);
    }

    #[test]
    fn ui_table_keeps_prefix_drops_keep_cap() {
        // keep_cap is no longer a known field; writing it in TOML produces an
        // unknown-key warning while prefix still loads correctly.
        let path = write_temp(
            "[ui]\nprefix = \"C-Space\"\nkeep_cap = 10\n",
            "ui-no-keepcap.toml",
        );
        let (cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(cfg.ui_prefix(), "C-Space");
        assert!(
            warnings.iter().any(|w| w.contains("ui.keep_cap")),
            "keep_cap is now an unknown key: {warnings:?}"
        );
    }

    #[test]
    fn ui_border_styles_default_to_unset() {
        // The keys are OVERRIDE-only, so unset → empty. The effective visual default
        // comes from ViewBorderColors::default() via ViewBorderColors::resolve, not
        // from these raw config values.
        let missing = std::env::temp_dir().join("xmux-border-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.view_active_border_style, "");
        assert_eq!(cfg.ui.view_border_style, "");
        assert_eq!(cfg.ui.view_border_hover_style, "");

        // [ui] present but border keys missing → still unset (empty).
        let path = write_temp("[ui]\nprefix = \"C-g\"\n", "border-missing.toml");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.view_active_border_style, "");
        assert_eq!(cfg.ui.view_border_style, "");
    }

    #[test]
    fn ui_border_styles_override_via_tmux_option_names() {
        let path = write_temp(
            "[ui]\nview-active-border-style = \"blue\"\nview-border-style = \"white\"\nview-border-hover-style = \"fg=red\"\n",
            "border-override.toml",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.view_active_border_style, "blue");
        assert_eq!(cfg.ui.view_border_style, "white");
        assert_eq!(cfg.ui.view_border_hover_style, "fg=red");
    }

    #[test]
    fn ui_role_overrides_default_to_unset_and_round_trip() {
        // The role keys are OVERRIDE-only, so missing → empty (the palette keeps the
        // theme's own slot). An explicit value is stored raw for map_color to parse.
        let missing = std::env::temp_dir().join("xmux-role-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.primary, "");
        assert_eq!(cfg.ui.secondary, "");
        assert_eq!(cfg.ui.bar_bg, "");
        let path = write_temp(
            "[ui]\nprimary = \"brightwhite\"\naccent = \"#ff0000\"\nbar-bg = \"colour235\"\n",
            "ui-role-override.toml",
        );
        let (cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(cfg.ui.primary, "brightwhite");
        assert_eq!(cfg.ui.accent, "#ff0000");
        assert_eq!(cfg.ui.bar_bg, "colour235");
        assert!(warnings.is_empty(), "role keys are known: {warnings:?}");
    }

    #[test]
    fn ui_nav_position_setting() {
        // Missing file → the defaults: auto on, left wide, top narrow, no force.
        let missing = std::env::temp_dir().join("xmux-navpos-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.nav_position_setting(), NavPositionSetting::default());

        // All four keys parsed.
        let path = write_temp(
            "[ui]\nauto-nav-position = false\nwide-nav-position = \"right\"\nnarrow-nav-position = \"bottom\"\nforce-nav-position = \"top\"\n",
            "navpos-all.toml",
        );
        let cfg = load(&path).unwrap();
        let s = cfg.ui.nav_position_setting();
        assert!(!s.auto);
        assert_eq!(s.wide, NavPosition::Right);
        assert_eq!(s.narrow, NavPosition::Bottom);
        assert_eq!(s.force, Some(NavPosition::Top));

        // Unknown words fall back to their own defaults; an empty force means none.
        let path = write_temp(
            "[ui]\nwide-nav-position = \"diagonal\"\nnarrow-nav-position = \"side\"\nforce-nav-position = \"\"\n",
            "navpos-garbage.toml",
        );
        let cfg = load(&path).unwrap();
        let s = cfg.ui.nav_position_setting();
        assert_eq!(s.wide, NavPositionSetting::default().wide);
        assert_eq!(s.narrow, NavPositionSetting::default().narrow);
        assert_eq!(s.force, None);
    }

    #[test]
    fn ui_auto_hide_nav_round_trip() {
        // Missing file → false.
        let missing = std::env::temp_dir().join("xmux-autohide-absent-xyz.toml");
        assert!(!load(&missing).unwrap().ui_auto_hide_nav());

        // [ui] present but key missing → false; prefix still loads.
        let path = write_temp("[ui]\nprefix = \"C-g\"\n", "autohide-missing.toml");
        let cfg = load(&path).unwrap();
        assert!(!cfg.ui_auto_hide_nav());
        assert_eq!(cfg.ui_prefix(), "C-g");

        // Explicit true.
        let path = write_temp("[ui]\nauto-hide-nav = true\n", "autohide-true.toml");
        let cfg = load(&path).unwrap();
        assert!(cfg.ui_auto_hide_nav());
        assert_eq!(cfg.ui_prefix(), "C-g"); // prefix unaffected, still defaults

        // Explicit false.
        let path = write_temp("[ui]\nauto-hide-nav = false\n", "autohide-false.toml");
        assert!(!load(&path).unwrap().ui_auto_hide_nav());
    }

    #[test]
    fn host_stanza_extracts_matching_blocks() {
        let cfg = "Match originalhost jupiter00 exec \"probe 1.2.3.4\"\n    HostName 1.2.3.4\n\nHost jupiter00\n    HostName 143.248.140.120\n    User hrlee\n\nHost other\n    HostName 9.9.9.9\n";
        let s = host_stanza(cfg, "jupiter00");
        assert!(
            s.contains("HostName 143.248.140.120"),
            "Host block included: {s}"
        );
        assert!(
            s.contains("HostName 1.2.3.4"),
            "Match block also included: {s}"
        );
        assert!(s.contains("User hrlee"));
        assert!(!s.contains("9.9.9.9"), "unrelated host excluded: {s}");
        // Empty config / unknown alias → empty.
        assert!(host_stanza("", "jupiter00").is_empty());
        assert!(host_stanza(cfg, "nope").is_empty());
    }

    #[test]
    fn ssh_host_aliases_missing_file() {
        let missing = std::env::temp_dir().join("xmux-no-such-ssh-config");
        assert!(ssh_host_aliases(&missing).is_empty());
    }

    #[test]
    fn ssh_host_aliases_parsing() {
        let path = write_temp(
            r#"
# a comment line
Host alpha beta gamma
    HostName 10.0.0.1
    User me

Host *
    ForwardAgent yes

Host prod-*
    User deploy

Host !skipme realhost
    Port 2222

  Host indented
    HostName 10.0.0.2

Host alpha
    Port 2200
"#,
            "ssh-config",
        );
        let got = ssh_host_aliases(&path);
        assert_eq!(got, vec!["alpha", "beta", "gamma", "realhost", "indented"]);
    }

    #[test]
    fn ssh_host_aliases_line_continuation() {
        // OpenSSH joins a line ending in a backslash with the next line, so a
        // `Host` header split across lines must still yield all its aliases.
        let path = write_temp(
            r#"
Host alpha \
     beta
    HostName 10.0.0.1

Host gamma
    HostName 10.0.0.2
"#,
            "ssh-config-cont",
        );
        let got = ssh_host_aliases(&path);
        assert_eq!(got, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn ssh_host_aliases_expands_include() {
        let inc = write_temp("Host inc-host\n    HostName 10.0.0.9\n", "ssh-inc-a");
        let dir = inc.parent().unwrap();
        let sub = dir.join("inc-d");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("extra.conf"), "Host extra-host\n    Port 2200\n").unwrap();
        std::fs::write(sub.join("skip.txt"), "Host not-included\n").unwrap();

        // A direct include plus a glob include restricted to *.conf.
        let glob_pat = sub.join("*.conf").to_string_lossy().into_owned();
        let include_line = format!(
            "Include {}\nInclude {glob_pat}\n\nHost main\n    HostName 10.0.0.1\n",
            inc.display()
        );
        let path = write_temp(&include_line, "ssh-main");
        let got = ssh_host_aliases(&path);
        assert_eq!(got, vec!["inc-host", "extra-host", "main"]);
    }

    #[test]
    fn ssh_host_aliases_include_cycle_terminates() {
        // A includes B and B includes A: the cycle must not loop forever, and
        // aliases from both files still come through exactly once.
        let dir = std::env::temp_dir().join(format!("xmux-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a_path = dir.join("ssh-cycle-a");
        let b_path = dir.join("ssh-cycle-b");
        std::fs::write(
            &a_path,
            format!("Include {}\nHost a-host\n", b_path.display()),
        )
        .unwrap();
        std::fs::write(
            &b_path,
            format!("Include {}\nHost b-host\n", a_path.display()),
        )
        .unwrap();
        let mut got = ssh_host_aliases(&a_path);
        got.sort();
        assert_eq!(got, vec!["a-host", "b-host"]);
    }
}
