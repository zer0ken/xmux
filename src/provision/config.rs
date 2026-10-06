//! Loads xmux's optional TOML configuration and merges it with ssh-config
//! discovery to produce the set of hosts and mux binaries to use.

use std::path::Path;

use crate::model::NavPosition;
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
    #[serde(default)]
    pub update: UpdateConfig,
}

/// The optional `[update]` table.
///
/// One key, because there is one thing to decide: whether xmux may ask the release
/// feed which version is newest. It is on by default, since a user who never hears
/// that a release exists stays on an old build without choosing to. Turning it off is
/// for a machine that must reach nothing but the hosts it was told about.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConfig {
    /// Whether xmux may ask GitHub for the newest released version. The answer is
    /// cached for a day and the request runs off the app's own path, so this is at
    /// most one request a day and never a wait.
    #[serde(rename = "check", default = "default_update_check")]
    pub check: bool,
}

fn default_update_check() -> bool {
    true
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check: default_update_check(),
        }
    }
}

/// The optional `[discovery]` table: which providers contribute ssh targets to the
/// roster (see [`crate::provision::roster`]).
///
/// Every provider is ON by default, so a machine xmux can reach is a machine xmux
/// offers with nothing to configure. Each flag is how a user narrows that: `ssh-config`
/// off for someone who keeps no ssh config, `neighbors` off for someone who wants only
/// the machines they wrote down. A provider that cannot run costs an empty list, not an
/// error, so leaving one on is safe on a machine without it.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryConfig {
    /// Read host aliases from `~/.ssh/config`.
    #[serde(rename = "ssh-config", default = "default_true")]
    pub ssh_config: bool,
    /// Offer the machines the OS already reaches in one hop - a tunnel's peers and the
    /// machines on this link - that answer ssh.
    #[serde(default = "default_true")]
    pub neighbors: bool,
    /// Offer this machine's WSL distributions, by the name `wsl.exe` lists them under.
    #[serde(default = "default_true")]
    pub wsl: bool,
    /// The maximum number of discovery tasks - the roster resolve, each machine's
    /// reachability probe, the mux discovery a connected machine runs, and each source's
    /// mux detection - that may run at once. The pool bounds CONCURRENCY only, never
    /// which task runs. Defaults to 6, hard-capped at [`SCAN_CONCURRENCY_MAX`].
    #[serde(rename = "scan-concurrency", default = "default_scan_concurrency")]
    pub scan_concurrency: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        DiscoveryConfig {
            ssh_config: true,
            neighbors: true,
            wsl: true,
            scan_concurrency: 6,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_scan_concurrency() -> usize {
    6
}

/// The hard upper bound on `[discovery] scan-concurrency`: more concurrent discovery
/// tasks than this would flood the network and the machines it probes.
pub const SCAN_CONCURRENCY_MAX: usize = 8;

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
    /// hand-written list picks up. An unset value yields nothing.
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
    /// Maximum complete terminal draws per second (10 through 120).
    #[serde(
        rename = "max-fps",
        default = "default_max_fps",
        deserialize_with = "deserialize_max_fps"
    )]
    pub max_fps: u16,
    /// The built-in colour theme: `auto-dark` (the default) or `auto-light`, each
    /// painting only ANSI slots so the terminal theme resolves the actual hues. An
    /// unknown name falls back to `auto-dark` and the doctor reports the resolution.
    /// Selecting a theme names the ANSI-slot mapping; see the palette module doc.
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
    /// Whether card numbers follow the current sorted list (default true). When off,
    /// cards keep their numbers until a full scan deals them again.
    #[serde(rename = "renumbering", default = "default_renumbering")]
    pub renumbering: bool,
    /// Whether the result of work the user started floats as a toast over the terminal
    /// view (default true). Off, results still land in the `prefix m` history. A config
    /// edit applies it live.
    #[serde(rename = "notifications", default = "default_notifications")]
    pub notifications: bool,
    /// Whether the central Braille X animation appears while scanning and beneath
    /// settled host content (default true). A config edit applies it live.
    #[serde(rename = "braille-animation", default = "default_braille_animation")]
    pub braille_animation: bool,
    /// The nav placement when nothing is pinned by `prefix p`: `left` | `top` | `right`
    /// | `bottom`. An unknown word falls back to `left`. The nav never moves on its own;
    /// `prefix p` pins a side (persisted to `~/.xmux/nav_position`) and this default
    /// applies while no pin is set.
    #[serde(rename = "nav-position", default = "default_nav_position")]
    pub nav_position: String,
    /// The tree|terminal view border colour OVERRIDES, named after tmux's pane-border
    /// options: the focused side is `view-active-border-style`, the unfocused side
    /// `view-border-style`, the drag-hover cue `view-border-hover-style`. Values use
    /// tmux's colour syntax. Each defaults to empty (unset), leaving that side at
    /// xmux's own colour.
    #[serde(rename = "view-active-border-style", default)]
    pub view_active_border_style: String,
    #[serde(rename = "view-border-style", default)]
    pub view_border_style: String,
    #[serde(rename = "view-border-hover-style", default)]
    pub view_border_hover_style: String,
    /// The hint bar's colour as a tmux `status-style` string (`bg=…,fg=…`, tmux colour
    /// colour syntax). Empty means the built-in default.
    #[serde(rename = "hint-bar-style", default)]
    pub hint_bar_style: String,
    /// The selected card's background, in the same colour slots as the view border
    /// (`bg=<colour>`, or a bare colour token). Empty (default) paints the selected card
    /// in reverse video, which needs no colour from the terminal; a named colour gives
    /// it a surface instead.
    #[serde(rename = "selection-style", default)]
    pub selection_style: String,
    /// Per-role colour overrides for the chosen theme: `primary`, `secondary`,
    /// `accent`, `decoration`, `warning`, `error`, `disabled`, and the hint bar's
    /// `bar-bg`, `bar-fg`, `bar-accent`. Values use the same colour vocabulary as the
    /// view border: a named ANSI colour, `bright*`, `colourN`, `#RRGGBB`, or `default`.
    /// Each defaults to empty, leaving that role at the theme's own slot.
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

pub const DEFAULT_MAX_FPS: u16 = 30;

fn default_max_fps() -> u16 {
    DEFAULT_MAX_FPS
}

fn deserialize_max_fps<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let fps = u16::deserialize(deserializer)?;
    if (10..=120).contains(&fps) {
        Ok(fps)
    } else {
        Err(serde::de::Error::custom(
            "max-fps must be between 10 and 120",
        ))
    }
}

fn default_notifications() -> bool {
    true
}

fn default_renumbering() -> bool {
    true
}

fn default_braille_animation() -> bool {
    true
}

fn default_nav_position() -> String {
    "left".to_string()
}

impl UiConfig {
    /// The default nav placement when nothing is pinned. Parsed here so the runtime
    /// never sees a garbage value; an unknown word falls back to `left`.
    pub fn nav_position(&self) -> NavPosition {
        NavPosition::parse(&self.nav_position).unwrap_or(NavPosition::Left)
    }
}

/// The built-in theme `[ui] theme` falls back to when it is unset or names no theme.
pub(crate) const DEFAULT_THEME: &str = "auto-dark";

fn default_theme() -> String {
    DEFAULT_THEME.to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            max_fps: default_max_fps(),
            theme: default_theme(),
            prefix: default_prefix(),
            auto_hide_nav: false,
            renumbering: default_renumbering(),
            notifications: default_notifications(),
            braille_animation: default_braille_animation(),
            nav_position: default_nav_position(),
            // Empty leaves the view border at its theme role.
            view_active_border_style: String::new(),
            view_border_style: String::new(),
            view_border_hover_style: String::new(),
            // Empty leaves the hint bar at its built-in style.
            hint_bar_style: String::new(),
            // Empty selects reverse video instead of a named surface colour.
            selection_style: String::new(),
            // Empty leaves each role at the theme's own slot.
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
                    "local mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen/tuios/herdr); no source is created for it"
                ));
            }
        }
        for h in &self.hosts {
            for name in h.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "host {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen/tuios/herdr); no source is created for it",
                        h.ssh
                    ));
                }
            }
        }
        for w in &self.wsl {
            for name in w.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "wsl {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij/abduco/screen/tuios/herdr); no source is created for it",
                        w.distro
                    ));
                }
            }
        }
        warnings
    }

    /// Whether `machine`'s mux list is xmux's to decide: no entry for it writes one
    /// (unset, or exactly `"auto"`). A machine that named its muxes is never probed - a
    /// written name is taken verbatim, and probing could only add ones the user did not
    /// ask for. Answers exactly what the spec merge reads, so a machine is either built
    /// from its written list or asked, never both.
    pub fn mux_is_auto(&self, machine: &str) -> bool {
        if machine == crate::session::LOCAL_SOURCE {
            return self.local.mux.is_auto();
        }
        if let Some(distro) = crate::session::wsl_distro_of(machine) {
            return self
                .wsl
                .iter()
                .all(|w| w.distro != distro || w.mux.is_auto());
        }
        self.hosts
            .iter()
            .all(|h| h.ssh != machine || h.mux.is_auto())
    }

    /// xmux's configured prefix spec.
    pub fn ui_prefix(&self) -> &str {
        &self.ui.prefix
    }

    /// The initial auto-hide-nav mode from config (default false). The live toggle's
    /// persisted state, when present, overrides this - see `state::load_auto_hide_nav`.
    pub fn ui_auto_hide_nav(&self) -> bool {
        self.ui.auto_hide_nav
    }

    /// The sources of the ssh hosts whose muxes are WRITTEN: a matching `hosts` entry
    /// names them. Discovered aliases come first in their original order (each deduped
    /// and skipping any in `exclude`), then the config-only hosts. A host that names no
    /// mux yields no source here: which muxes it serves is asked of the host itself
    /// ([`auto_hosts`](Self::auto_hosts)), never assumed.
    ///
    /// A machine configured with SEVERAL muxes yields one spec per mux, all sharing the
    /// ssh alias and each carrying its own qualified source id. `exclude` names
    /// MACHINES, so excluding one drops every mux on it.
    pub fn host_specs(&self, ssh_aliases: &[String]) -> Vec<HostSpec> {
        written_specs(self.merged_ssh_hosts(ssh_aliases))
    }

    /// The ssh hosts and WSL distributions on the roster whose mux list is xmux's to
    /// decide, in roster order: every one [`host_specs`](Self::host_specs) and
    /// [`wsl_specs`](Self::wsl_specs) build no source for. The local machine is not one
    /// of them; its muxes are resolved before the roster is.
    pub fn auto_hosts(&self, ssh_aliases: &[String], distro_machines: &[String]) -> Vec<String> {
        self.merged_ssh_hosts(ssh_aliases)
            .into_iter()
            .chain(self.merged_wsl_hosts(distro_machines))
            .filter(|(_, written)| written.is_none())
            .map(|(machine, _)| machine)
            .collect()
    }

    fn merged_ssh_hosts(&self, ssh_aliases: &[String]) -> Vec<(String, Option<Vec<String>>)> {
        let configured: Vec<(&str, &MuxSpec)> = self
            .hosts
            .iter()
            .map(|h| (h.ssh.as_str(), &h.mux))
            .collect();
        merge_hosts(
            ssh_aliases,
            &configured,
            &self.excluded(),
            is_reserved_alias,
        )
    }

    /// The WSL sources: one spec per written mux on each distribution, merged the same way
    /// [`host_specs`](Self::host_specs) merges ssh hosts. `distro_machines` are the
    /// MACHINE names `[discovery] wsl` listed (`wsl.Ubuntu-24.04`); a `[[wsl]]` entry
    /// names its distribution bare and is prefixed here, so both halves key alike.
    ///
    /// `exclude` names MACHINES here too, which for this kind is the prefixed name.
    pub fn wsl_specs(&self, distro_machines: &[String]) -> Vec<HostSpec> {
        written_specs(self.merged_wsl_hosts(distro_machines))
    }

    fn merged_wsl_hosts(&self, distro_machines: &[String]) -> Vec<(String, Option<Vec<String>>)> {
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
        merge_hosts(distro_machines, &configured, &self.excluded(), |_| false)
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

/// The merge every machine kind's roster follows: `discovered` names first, in the
/// order their provider gave them, then the `configured` entries that were not
/// discovered. Config augments discovery; it never replaces it.
///
/// A name that is excluded, reserved, or already taken is skipped. Each machine carries
/// the mux list its config WROTE, or `None` when it wrote none (unset or `"auto"`): the
/// first entry for a machine that writes muxes decides, and entries that leave the list
/// to xmux never override one that wrote it.
fn merge_hosts(
    discovered: &[String],
    configured: &[(&str, &MuxSpec)],
    excluded: &std::collections::HashSet<&str>,
    is_reserved: impl Fn(&str) -> bool,
) -> Vec<(String, Option<Vec<String>>)> {
    use std::collections::HashSet;

    let mut written: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for (machine, mux) in configured {
        if machine.is_empty() || mux.is_auto() {
            continue;
        }
        written.entry(machine).or_insert_with(|| mux.names());
    }

    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let names = discovered
        .iter()
        .map(String::as_str)
        .chain(configured.iter().map(|(machine, _)| *machine));
    for machine in names {
        if machine.is_empty()
            || is_reserved(machine)
            || excluded.contains(machine)
            || !seen.insert(machine)
        {
            continue;
        }
        out.push((machine.to_string(), written.get(machine).cloned()));
    }
    out
}

/// The sources of the machines in `merged` whose mux list is written.
fn written_specs(merged: Vec<(String, Option<Vec<String>>)>) -> Vec<HostSpec> {
    merged
        .iter()
        .filter_map(|(machine, written)| Some(host_specs_for(machine, written.as_ref()?)))
        .flatten()
        .collect()
}

/// One [`HostSpec`] per mux on `alias`. The id is qualified only when the machine
/// serves more than one, so a single-mux host keeps the bare alias it always had.
pub fn host_specs_for(alias: &str, muxes: &[String]) -> Vec<HostSpec> {
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
/// with include cycles broken. `Match` blocks declare no aliases of their own -
/// they only apply options to hosts named elsewhere - so they contribute nothing
/// here; `host_stanza` still shows them for display. A missing file yields an
/// empty list.
pub fn ssh_host_aliases(path: &Path) -> Vec<String> {
    read_ssh_config(path).1
}

/// Reads an OpenSSH client config and returns its root text with every concrete alias.
pub fn read_ssh_config(path: &Path) -> (String, Vec<String>) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut aliases = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = Vec::new();
    collect_ssh_aliases_from_text(path, &content, &mut aliases, &mut seen, &mut stack);
    (content, aliases)
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
    collect_ssh_aliases_from_text(path, &content, aliases, seen, stack);
}

fn collect_ssh_aliases_from_text(
    path: &Path,
    content: &str,
    aliases: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
    stack: &mut Vec<std::path::PathBuf>,
) {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canonical) {
        return;
    }
    stack.push(canonical);

    for line in logical_lines(content) {
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
/// header (or EOF). Display text only - Match-resolved values (e.g. an exec-chosen
/// HostName) are NOT computed; the literal config lines are shown. Empty when no
/// block names the alias.
/// The marker that opens a stanza xmux wrote, naming the host it is for.
fn managed_marker(alias: &str) -> String {
    format!("# xmux: {alias}")
}

/// `config_text` with the xmux-managed stanza for `alias` replaced by one naming
/// `login`'s values, or added when there is none.
///
/// The stanza goes at the TOP of the file, because ssh keeps the FIRST value it obtains
/// for a keyword: a stanza appended after one the user already wrote would be read and
/// then ignored. It carries a marker naming its host, which is what makes a second write
/// replace the first instead of stacking, and what tells a reader which lines are xmux's
/// to delete.
///
/// Nothing the user wrote is touched. Only lines between a marker and the end of the
/// stanza it opens are replaced, and a file that never held one is only prepended to.
pub fn upsert_managed_stanza(
    config_text: &str,
    alias: &str,
    login: &crate::transport::Login,
) -> String {
    let marker = managed_marker(alias);
    let mut out = String::new();
    out.push_str(&marker);
    out.push('\n');
    out.push_str(&format!("Host {alias}\n"));
    if let Some(address) = &login.address {
        out.push_str(&format!("    HostName {address}\n"));
    }
    if let Some(port) = login.port {
        out.push_str(&format!("    Port {port}\n"));
    }
    if let Some(user) = &login.user {
        out.push_str(&format!("    User {user}\n"));
    }
    out.push('\n');
    out.push_str(
        strip_managed(config_text, &marker)
            .0
            .trim_start_matches(['\r', '\n']),
    );
    out
}

/// `config_text` without the xmux-managed stanza for `alias`, or `None` when it holds
/// none. What the user wrote stays byte for byte, so a logout takes back only what the
/// login recorded.
pub fn remove_managed_stanza(config_text: &str, alias: &str) -> Option<String> {
    let (rest, found) = strip_managed(config_text, &managed_marker(alias));
    found.then_some(rest)
}

/// `config_text` without the stanza `marker` opens, and whether there was one.
///
/// The stanza is the marker line, the `Host` line under it, and the directive and blank
/// lines after it. A comment line ends it as a header does: a comment above the next
/// header, another host's marker among them, belongs to that next stanza. Every other
/// line is carried across with its own line ending.
fn strip_managed(config_text: &str, marker: &str) -> (String, bool) {
    let is_header = |l: &str| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("Host") || w.eq_ignore_ascii_case("Match"))
    };
    let is_comment = |l: &str| l.trim_start().starts_with('#');
    let mut out = String::with_capacity(config_text.len());
    let mut found = false;
    let mut lines = config_text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if line.trim() != marker {
            out.push_str(line);
            continue;
        }
        found = true;
        // The marker's own stanza header, then its body up to the next header or comment.
        if lines.peek().is_some_and(|l| is_header(l)) {
            lines.next();
        }
        while lines
            .peek()
            .is_some_and(|l| !is_header(l) && !is_comment(l))
        {
            lines.next();
        }
    }
    (out, found)
}

/// The `User` an `~/.ssh/config` stanza names for `alias`, or `None` when none does.
///
/// Read from the same stanza the host screen shows, which is the one whose header names
/// the alias exactly. A stanza reached only through a pattern is not consulted, so a
/// value this returns is one the user wrote against this host by name.
pub fn stanza_user(config_text: &str, alias: &str) -> Option<String> {
    let mut exact_host = false;
    for line in host_stanza(config_text, alias).lines() {
        let key = line.split_whitespace().next();
        if key.is_some_and(|key| key.eq_ignore_ascii_case("Host")) {
            exact_host = true;
        } else if key.is_some_and(|key| key.eq_ignore_ascii_case("Match")) {
            exact_host = false;
        } else if exact_host {
            if let Some((_, value)) =
                ssh_directive(line).filter(|(key, _)| key.eq_ignore_ascii_case("User"))
            {
                return Some(value);
            }
        }
    }
    None
}

/// The connection values the named ssh-config stanza supplies. OpenSSH keeps the first
/// value obtained for each keyword, so later matching blocks fill only missing values.
pub fn stanza_login(config_text: &str, alias: &str) -> crate::transport::Login {
    let mut login = crate::transport::Login::default();
    for line in host_stanza(config_text, alias).lines() {
        let Some((key, value)) = ssh_directive(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("HostName") && login.address.is_none() {
            login.address = Some(value);
        } else if key.eq_ignore_ascii_case("Port") && login.port.is_none() {
            login.port = value.parse().ok();
        } else if key.eq_ignore_ascii_case("User") && login.user.is_none() {
            login.user = Some(value);
        }
    }
    login
}

/// The address and port the login pane starts with. Only an exact host stanza supplies
/// the username; without one, the user enters it.
///
/// A field counts as set by ssh config when the stanza naming the host sets it, or when
/// OpenSSH's effective value differs from what OpenSSH fills in for a host no block
/// configures (the alias as the host name, port 22). `ssh -G` reports every field for
/// any name, so its output alone cannot tell a configured value from a default. Where
/// both set a field, OpenSSH's effective value wins. A field ssh config does not set is
/// the provider's address, else the host's own name, and port 22.
pub fn login_defaults(
    alias: &str,
    provider_address: Option<&str>,
    effective: Option<&crate::transport::Login>,
    config_text: &str,
) -> crate::provision::env::LoginDefaults {
    let stanza = stanza_login(config_text, alias);
    let effective = effective.cloned().unwrap_or_default();
    let user = stanza_user(config_text, alias);
    let configured = crate::transport::Login {
        address: match stanza.address {
            Some(address) => effective.address.or(Some(address)),
            None => effective.address.filter(|address| address != alias),
        },
        port: match stanza.port {
            Some(port) => effective.port.or(Some(port)),
            None => effective.port.filter(|port| *port != 22),
        },
        user: user.clone(),
    };
    let address = configured
        .address
        .clone()
        .or_else(|| provider_address.map(str::to_string))
        .unwrap_or_else(|| alias.to_string());
    let port = configured.port.unwrap_or(22).to_string();
    crate::provision::env::LoginDefaults {
        address: crate::provision::env::LoginValue {
            value: address,
            provenance: if configured.address.is_some() {
                "from ssh config"
            } else if provider_address.is_some() {
                "from discovery"
            } else {
                "host name"
            },
        },
        port: crate::provision::env::LoginValue {
            value: port,
            provenance: if configured.port.is_some() {
                "from ssh config"
            } else {
                "default"
            },
        },
        username: crate::provision::env::LoginValue {
            provenance: if user.is_some() {
                "from ssh config"
            } else {
                ""
            },
            value: user.unwrap_or_default(),
        },
        configured,
    }
}

fn ssh_directive(line: &str) -> Option<(&str, String)> {
    let line = line.trim();
    let (key, value) = if let Some((key, value)) = line.split_once('=') {
        (key.trim(), value.trim())
    } else {
        let mut fields = line.splitn(2, char::is_whitespace);
        (fields.next()?, fields.next()?.trim())
    };
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value);
    (!key.is_empty() && !value.is_empty()).then(|| (key, value.to_string()))
}

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

    fn login(address: &str, port: u16, user: &str) -> crate::transport::Login {
        crate::transport::Login {
            address: Some(address.into()),
            port: Some(port),
            user: Some(user.into()),
        }
    }

    #[test]
    fn upsert_puts_the_managed_stanza_first_so_ssh_reads_it() {
        // ssh keeps the FIRST value it obtains for a keyword, so a stanza appended after
        // one the user wrote would be read and then ignored.
        let text = "Host jupiter00\n    User someone\n";
        let got = upsert_managed_stanza(text, "jupiter00", &login("100.88.0.0", 22, "hrlee"));
        let first = got.lines().next().unwrap();
        assert_eq!(first, "# xmux: jupiter00");
        assert!(got.contains("    HostName 100.88.0.0"), "{got}");
        assert!(got.contains("    User hrlee"), "{got}");
        assert!(
            got.contains("    User someone"),
            "what the user wrote is untouched:\n{got}"
        );
    }

    #[test]
    fn upsert_replaces_its_own_stanza_rather_than_stacking() {
        let once = upsert_managed_stanza("", "jupiter00", &login("100.88.0.0", 22, "hrlee"));
        let twice = upsert_managed_stanza(&once, "jupiter00", &login("100.88.0.6", 2222, "bob"));
        assert_eq!(
            twice.matches("# xmux: jupiter00").count(),
            1,
            "one marker, not two:\n{twice}"
        );
        assert!(twice.contains("HostName 100.88.0.6"), "{twice}");
        assert!(
            !twice.contains("100.88.0.0"),
            "the old values are gone:\n{twice}"
        );
        assert!(!twice.contains("User hrlee"), "{twice}");
    }

    #[test]
    fn upsert_leaves_another_hosts_managed_stanza_alone() {
        let a = upsert_managed_stanza("", "jupiter00", &login("100.88.0.0", 22, "hrlee"));
        let b = upsert_managed_stanza(&a, "mars01", &login("100.77.0.1", 22, "hrlee"));
        assert!(b.contains("# xmux: jupiter00"), "{b}");
        assert!(b.contains("# xmux: mars01"), "{b}");
        assert!(b.contains("HostName 100.88.0.0"), "{b}");
        assert!(b.contains("HostName 100.77.0.1"), "{b}");
    }

    #[test]
    fn upsert_writes_only_the_values_the_login_names() {
        let got = upsert_managed_stanza(
            "",
            "prod",
            &crate::transport::Login {
                user: Some("hrlee".into()),
                ..Default::default()
            },
        );
        assert!(got.contains("    User hrlee"), "{got}");
        assert!(!got.contains("HostName"), "{got}");
        assert!(!got.contains("Port"), "{got}");
    }

    /// A logout takes back only what the login recorded: the rest of the file, its own
    /// comments, its own stanza for the same host, and its line endings, comes back byte
    /// for byte.
    #[test]
    fn removing_the_managed_stanza_restores_the_file_the_login_recorded_into() {
        for user_text in [
            "# my hosts\r\nHost db-01\r\n    User admin\r\n\r\nHost *\r\n    ServerAliveInterval 30",
            "# my hosts\nHost db-01\n    User admin\n",
            "",
        ] {
            let recorded = upsert_managed_stanza(user_text, "db-01", &login("10.0.0.5", 22, "dev"));
            assert_eq!(
                remove_managed_stanza(&recorded, "db-01").as_deref(),
                Some(user_text),
                "{recorded:?}"
            );
        }
    }

    #[test]
    fn removing_one_hosts_managed_stanza_keeps_another_hosts() {
        let a = upsert_managed_stanza("Host web\n", "jupiter00", &login("100.88.0.0", 22, "hrlee"));
        let b = upsert_managed_stanza(&a, "mars01", &login("100.77.0.1", 22, "hrlee"));
        assert_eq!(
            remove_managed_stanza(&b, "mars01").as_deref(),
            Some(a.as_str())
        );
        let relogin = upsert_managed_stanza(&b, "mars01", &login("100.77.0.2", 22, "hrlee"));
        assert!(
            relogin.contains("# xmux: jupiter00\nHost jupiter00\n"),
            "{relogin}"
        );
    }

    #[test]
    fn a_file_without_the_managed_stanza_has_nothing_to_remove() {
        let user_text = "Host db-01\n    User admin\n";
        assert_eq!(remove_managed_stanza(user_text, "db-01"), None);
        let other = upsert_managed_stanza(user_text, "web-01", &login("10.0.0.6", 22, "dev"));
        assert_eq!(remove_managed_stanza(&other, "db-01"), None);
        assert_eq!(remove_managed_stanza("", "db-01"), None);
    }

    #[test]
    fn stanza_user_reads_the_user_of_the_named_host() {
        let text = "Host prod\n    User hrlee\n    Port 22\n\nHost other\n    User bob\n";
        assert_eq!(stanza_user(text, "prod").as_deref(), Some("hrlee"));
        assert_eq!(stanza_user(text, "other").as_deref(), Some("bob"));
        assert_eq!(stanza_user(text, "absent"), None);
        assert_eq!(stanza_user("Host prod\n    Port 22\n", "prod"), None);
    }

    #[test]
    fn login_username_ignores_patterns_and_match_blocks() {
        let text = "Host *\n    User wildcard\nMatch originalhost prod\n    User matched\nHost prod\n    Port 2222\n";
        let effective = crate::transport::Login {
            user: Some("wildcard".into()),
            ..Default::default()
        };
        let defaults = login_defaults("prod", None, Some(&effective), text);
        assert!(defaults.username.value.is_empty());
        assert!(defaults.username.provenance.is_empty());

        let text = format!("Host prod\n    User = 'recorded'\n{text}");
        let defaults = login_defaults("prod", None, Some(&effective), &text);
        assert_eq!(defaults.username.value, "recorded");
        assert_eq!(defaults.username.provenance, "from ssh config");
    }

    #[test]
    fn remembered_user_prefills_the_next_login() {
        let config = upsert_managed_stanza(
            "Host *\n    User fallback\n",
            "prod",
            &crate::transport::Login {
                user: Some("alice".into()),
                ..Default::default()
            },
        );
        let defaults = login_defaults("prod", None, None, &config);
        assert_eq!(defaults.username.value, "alice");
        assert_eq!(defaults.username.provenance, "from ssh config");
    }

    #[test]
    fn stanza_login_reads_the_effective_connection_fields() {
        let text = "Host e2e-box\n    HostName 127.0.0.1\n    Port 2222\n    User dev\n";
        assert_eq!(
            stanza_login(text, "e2e-box"),
            crate::transport::Login {
                address: Some("127.0.0.1".into()),
                port: Some(2222),
                user: Some("dev".into()),
            }
        );
    }

    #[test]
    fn stanza_login_accepts_equals_and_quoted_values() {
        let text = "Host box\n HostName = \"127.0.0.1\"\n Port=2222\n User 'dev'\n";
        assert_eq!(
            stanza_login(text, "box"),
            crate::transport::Login {
                address: Some("127.0.0.1".into()),
                port: Some(2222),
                user: Some("dev".into()),
            }
        );
    }

    #[test]
    fn login_defaults_resolve_before_the_view_receives_them() {
        let text = "Host prod\n    HostName stanza.example\n    Port 2200\n    User stanza-user\n";
        let effective = crate::transport::Login {
            address: Some("effective.example".into()),
            port: Some(2222),
            user: Some("effective-user".into()),
        };

        let defaults = login_defaults("prod", Some("192.0.2.10"), Some(&effective), text);
        assert_eq!(defaults.address.value, "effective.example");
        assert_eq!(defaults.port.value, "2222");
        assert_eq!(defaults.username.value, "stanza-user");
        assert_eq!(defaults.address.provenance, "from ssh config");
        let defaults = login_defaults("prod", None, None, text);
        assert_eq!(defaults.address.value, "stanza.example");
        assert_eq!(defaults.port.value, "2200");
        assert_eq!(defaults.username.value, "stanza-user");
    }

    #[test]
    fn login_defaults_preserve_provider_and_ssh_fallback_order() {
        let effective = crate::transport::Login {
            address: Some("prod".into()),
            ..Default::default()
        };
        let defaults = login_defaults("prod", Some("192.0.2.10"), Some(&effective), "");
        assert_eq!(defaults.address.value, "192.0.2.10");
        assert_eq!(defaults.address.provenance, "from discovery");
        assert_eq!(defaults.port.value, "22");
        assert!(defaults.username.value.is_empty());
        let defaults = login_defaults("prod", None, Some(&effective), "");
        assert_eq!(defaults.address.value, "prod");
        assert_eq!(defaults.address.provenance, "host name");
    }

    #[test]
    fn username_has_no_automatic_value_or_provenance() {
        let effective = crate::transport::Login {
            address: Some("prod".into()),
            port: Some(22),
            user: Some("local-user".into()),
        };
        let defaults = login_defaults("prod", None, Some(&effective), "");
        assert_eq!(defaults.address.provenance, "host name");
        assert_eq!(defaults.port.provenance, "default");
        assert!(defaults.username.value.is_empty());
        assert!(defaults.username.provenance.is_empty());
    }
    use crate::model::NavPosition;
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
        assert!(cfg.local.mux.names().is_empty());
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
        assert!(cfg.hosts[1].mux.names().is_empty());
        assert_eq!(cfg.exclude, vec!["foo", "bar"]);
    }

    #[test]
    fn load_malformed() {
        let path = write_temp("this is = = not valid toml [[[", "malformed.toml");
        assert!(load(&path).is_err());
    }

    #[test]
    fn max_fps_accepts_supported_values_and_rejects_invalid_values() {
        assert_eq!(toml::from_str::<Config>("[ui]").unwrap().ui.max_fps, 30);
        for fps in [10, 30, 60, 90, 120] {
            let config: Config = toml::from_str(&format!("[ui]\nmax-fps = {fps}")).unwrap();
            assert_eq!(config.ui.max_fps, fps);
        }
        for value in ["9", "121", "-1", "60.0", "\"60\""] {
            assert!(toml::from_str::<Config>(&format!("[ui]\nmax-fps = {value}")).is_err());
        }
    }

    #[test]
    fn load_verbose_missing_file() {
        let missing = std::env::temp_dir().join("xmux-nope-xyz.toml");
        let (cfg, warnings) = load_verbose(&missing).unwrap();
        assert!(warnings.is_empty());
        assert!(cfg.local.mux.names().is_empty());
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
                id: "extra".into(),
                alias: "extra".into(),
                bin: "zellij".into(),
            },
        ];
        assert_eq!(got, want);
        // A host that writes no mux has no source until it answers which it serves:
        // nothing is assumed for it, and it is on the roster as a host xmux asks.
        assert_eq!(
            cfg.auto_hosts(&ssh_aliases, &[]),
            vec!["stage".to_string(), "noMuxOnly".to_string()]
        );
    }

    #[test]
    fn a_host_written_as_auto_is_asked_and_never_dropped() {
        // `"auto"` is the written form of leaving the list to xmux, so the host is asked
        // exactly as an unset one is, and an entry that leaves it to xmux never overrides
        // one that wrote the list.
        let cfg = Config {
            hosts: vec![
                HostConfig {
                    ssh: "box".into(),
                    mux: "auto".into(),
                },
                HostConfig {
                    ssh: "prod".into(),
                    mux: "auto".into(),
                },
                HostConfig {
                    ssh: "prod".into(),
                    mux: "zellij".into(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(cfg.auto_hosts(&[], &[]), vec!["box".to_string()]);
        assert!(cfg.mux_is_auto("box"));
        assert!(!cfg.mux_is_auto("prod"), "a written list decides");
        let specs = cfg.host_specs(&[]);
        assert_eq!(specs.len(), 1);
        assert_eq!(
            (specs[0].id.as_str(), specs[0].bin.as_str()),
            ("prod", "zellij")
        );
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
        assert_eq!(cfg.auto_hosts(&ssh_aliases, &[]), vec!["prod".to_string()]);
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
        assert!(MuxSpec::from(vec!["", " "]).names().is_empty());
        assert!(MuxSpec::from("").names().is_empty());
    }

    #[test]
    fn value_warnings_flags_unrecognized_mux() {
        // Documented defaults and recognized muxes never warn.
        for mux in ["", "auto"]
            .into_iter()
            .chain(crate::mux::supported_muxes())
        {
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
        // gave them, then a `[[wsl]]` entry that was not listed. A distribution that
        // writes no mux has no source until it answers which it serves.
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
            ]
        );
        // Listed but not configured, then configured but not listed: appended, so one
        // distribution is asked without listing every one of them.
        assert_eq!(
            cfg.auto_hosts(&[], &listed),
            vec!["wsl.docker-desktop".to_string(), "wsl.Alpine".to_string()]
        );
    }

    #[test]
    fn exclude_names_a_wsl_machine_by_its_prefixed_name() {
        // `exclude` names MACHINES, and a distribution's machine name carries the WSL
        // prefix - which is how the Docker Desktop distributions are dropped.
        let cfg = Config {
            exclude: vec!["wsl.docker-desktop".into()],
            ..Config::default()
        };
        let listed: Vec<String> = ["wsl.Ubuntu", "wsl.docker-desktop"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(cfg.auto_hosts(&[], &listed), vec!["wsl.Ubuntu".to_string()]);
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
        assert_eq!(
            cfg.auto_hosts(&aliases, &[]),
            vec!["prod".to_string()],
            "only the plain alias is served"
        );
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
        assert!(d.ssh_config && d.neighbors && d.wsl);
    }

    /// A key that named a provider xmux no longer has is reported, not ignored. Someone
    /// who turned that provider off said something about their network, and finding out
    /// it stopped applying beats discovering it from a roster that grew overnight.
    #[test]
    fn a_key_for_a_provider_that_is_gone_is_reported() {
        let path = write_temp(
            "[discovery]
tailscale = false
",
            "retired-discovery-key.toml",
        );
        let (_cfg, warnings) = load_verbose(&path).unwrap();
        assert_eq!(warnings, vec![r#"unknown key "discovery.tailscale""#]);
    }

    #[test]
    fn a_partial_discovery_table_leaves_the_others_on() {
        // The default is per KEY, not per table: a config that names one provider to
        // narrow the roster must not silently drop the ones it did not mention.
        let path = write_temp(
            "[discovery]
neighbors = false
",
            "partial-discovery.toml",
        );
        let d = load(&path).unwrap().discovery;
        assert!(!d.neighbors, "the key that was written is honoured");
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
        assert_eq!(cfg.ui.theme, DEFAULT_THEME);
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
    fn ui_nav_position() {
        // Missing file → the default (left).
        let missing = std::env::temp_dir().join("xmux-navpos-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.nav_position(), NavPosition::Left);

        // A configured word is parsed.
        let path = write_temp("[ui]\nnav-position = \"right\"\n", "navpos-right.toml");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.nav_position(), NavPosition::Right);

        // An unknown word falls back to the default.
        let path = write_temp("[ui]\nnav-position = \"diagonal\"\n", "navpos-garbage.toml");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.nav_position(), NavPosition::Left);
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
    fn ui_notifications_defaults_true_and_round_trips() {
        let path = write_temp("[ui]\n", "notifications-missing.toml");
        assert!(load(&path).unwrap().ui.notifications);
        let path = write_temp("[ui]\nnotifications = false\n", "notifications-false.toml");
        assert!(!load(&path).unwrap().ui.notifications);
    }

    #[test]
    fn ui_renumbering_defaults_true_and_accepts_false() {
        let config: Config = toml::from_str("[ui]").unwrap();
        assert!(config.ui.renumbering);
        let config: Config = toml::from_str("[ui]\nrenumbering = false").unwrap();
        assert!(!config.ui.renumbering);
    }

    #[test]
    fn ui_braille_animation_defaults_true_and_accepts_false() {
        let config: Config = toml::from_str("[ui]").unwrap();
        assert!(config.ui.braille_animation);
        let config: Config = toml::from_str("[ui]\nbraille-animation = false").unwrap();
        assert!(!config.ui.braille_animation);
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
