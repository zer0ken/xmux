//! Loads xmux's optional TOML configuration and merges it with ssh-config
//! discovery to produce the set of hosts and mux binaries to use.

use std::path::Path;

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
/// roster (see [`crate::roster`]).
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
    /// Offer this box's WSL distributions, by the name `wsl.exe` lists them under.
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
    /// xmux's prefix spec (e.g. `C-g`, `C-Space`), config-only like tmux's
    /// `set -g prefix`. Parsed by `display::term::parse_prefix`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// The INITIAL state of the auto-hide-nav mode (toggled live with `prefix t`,
    /// then persisted to `~/.xmux/auto_hide_nav`, which wins over this on later
    /// runs). When the mode is on, focusing the terminal view hides the tree and gives it
    /// the full terminal width; the tree returns when focus returns to it. While
    /// hidden the tree has no column to click, so focus returns via the prefix keys
    /// (`prefix Tab`/`←`/`Esc`). Default false keeps the tree shown in both focus states.
    #[serde(rename = "auto-hide-nav", default)]
    pub auto_hide_nav: bool,
    /// The tree|terminal view border colour OVERRIDES, named after tmux's pane-border
    /// options: the focused side is `view-active-border-style`, the unfocused side
    /// `view-border-style`, the drag-hover cue `view-border-hover-style`. Values use
    /// tmux's colour vocabulary (parsed by [`crate::ui::chrome::map_color`]). Each
    /// defaults to EMPTY (unset): when unset the colour comes from the displayed
    /// host's live mux `pane-*-border-style`, falling back to the stock default
    /// (`green` / terminal-default / `yellow`). A non-empty value here overrides both
    /// — see [`crate::ui::chrome::ViewBorderColors::resolve`]. (`hover` has no live
    /// mux source, so it is this override or the stock default only.)
    #[serde(rename = "view-active-border-style", default)]
    pub view_active_border_style: String,
    #[serde(rename = "view-border-style", default)]
    pub view_border_style: String,
    #[serde(rename = "view-border-hover-style", default)]
    pub view_border_hover_style: String,
    /// The hint bar's colour as a tmux `status-style` string (`bg=…,fg=…`, tmux colour
    /// vocabulary parsed by [`crate::ui::chrome::parse_hint_bar_style`]). Empty (default)
    /// = the built-in tmux default (themegreen/themeblack → yellowgreen / gray5).
    #[serde(rename = "hint-bar-style", default)]
    pub hint_bar_style: String,
    /// The selected card's background, in the same colour vocabulary as the view border
    /// (`bg=<colour>`, or a bare colour token). Empty (default) means the surface comes
    /// from the terminal's reported background, and NOTHING is painted when the terminal
    /// does not report one - see [`crate::ui::palette`]. This is how a user on a
    /// terminal that answers no colour query (Windows Terminal answers none) gets a
    /// selection surface at all.
    #[serde(rename = "selection-style", default)]
    pub selection_style: String,
}

fn default_prefix() -> String {
    "C-g".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            prefix: default_prefix(),
            auto_hide_nav: false,
            // Empty = unset: the effective colour comes from the live mux
            // pane-*-border-style, falling back to ViewBorderColors::default().
            view_active_border_style: String::new(),
            view_border_style: String::new(),
            view_border_hover_style: String::new(),
            // Empty = the built-in tmux default hint bar style (see
            // crate::ui::chrome::hint_bar_default_style).
            hint_bar_style: String::new(),
            // Empty = no selection surface of xmux's own choosing (see
            // crate::ui::palette).
            selection_style: String::new(),
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
/// becomes carries the family prefix, so `exclude` names it as `wsl.Ubuntu-24.04`.
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
    /// unreachable rather than vanishing).
    ///
    /// An unset or `"auto"` value means "whatever this box actually has", so `installed`
    /// (from `mux::installed_muxes`) becomes the list, with the `os`'s conventional mux
    /// first so a single-mux box reads exactly as it always did. A box where discovery
    /// finds nothing still gets the conventional mux, so the nav says the mux is
    /// unreachable instead of showing no sources and no reason.
    pub fn local_muxes(&self, os: &str, installed: &[String]) -> Vec<String> {
        if !self.local.mux.is_auto() {
            return self.local.mux.names();
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
        if out.is_empty() {
            out.push(conventional.to_string());
        }
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
                    "local mux {name:?} is not a recognized mux (tmux/psmux/zellij); treating it as tmux-compatible"
                ));
            }
        }
        for h in &self.hosts {
            for name in h.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "host {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij); treating it as tmux-compatible",
                        h.ssh
                    ));
                }
            }
        }
        for w in &self.wsl {
            for name in w.mux.names() {
                if !crate::mux::is_recognized(&name) {
                    warnings.push(format!(
                        "wsl {:?} mux {name:?} is not a recognized mux (tmux/psmux/zellij); treating it as tmux-compatible",
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
    /// `exclude` names MACHINES here too, which for this family is the prefixed name.
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
        // Nothing to reserve: every name here already carries the family prefix, so it can
        // collide with neither `local` nor an ssh alias `host_specs` accepted.
        merge_specs(distro_machines, &configured, &self.excluded(), |_| false)
    }

    /// The machines `exclude` names, as a lookup.
    fn excluded(&self) -> std::collections::HashSet<&str> {
        self.exclude.iter().map(String::as_str).collect()
    }
}

/// The machine names the ssh family may not claim: `local` is this box's own, and a
/// `wsl.`-prefixed name is a WSL distribution's. Either would otherwise be built as an
/// ssh destination and shadow the machine that owns the name, so an ssh alias spelled
/// either way is dropped rather than served ambiguously.
fn is_reserved_alias(machine: &str) -> bool {
    machine == crate::session::LOCAL_SOURCE || crate::session::wsl_distro_of(machine).is_some()
}

/// The merge every machine family's spec list follows: `discovered` names first, in the
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
    let qualified = muxes.len() > 1;
    muxes
        .iter()
        .map(|bin| HostSpec {
            id: crate::session::source_id(alias, bin, qualified),
            alias: alias.to_string(),
            bin: bin.clone(),
        })
        .collect()
}

/// Parses an OpenSSH client config at `path` and returns the concrete host
/// aliases declared by `Host` lines, in first-seen order and deduplicated. Glob
/// patterns (containing `*` or `?`) and negations (starting with `!`) are
/// skipped, as are comments, blank lines, and non-`Host` directives. `Include`
/// and `Match` directives are not expanded. A missing file yields an empty list.
pub fn ssh_host_aliases(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut aliases = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(directive) = fields.next() else {
            continue;
        };
        if !directive.eq_ignore_ascii_case("Host") {
            continue;
        }
        for pattern in fields {
            if pattern.starts_with('!') || pattern.contains('*') || pattern.contains('?') {
                continue;
            }
            if seen.contains(pattern) {
                continue;
            }
            aliases.push(pattern.to_string());
            seen.insert(pattern.to_string());
        }
    }
    aliases
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

    let lines: Vec<&str> = config_text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_header(lines[i]) && names_alias(lines[i]) {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.push(lines[i].trim_end().to_string());
            i += 1;
            while i < lines.len() && !is_header(lines[i]) {
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
        // unset take the discovered list instead - that is the whole point of the
        // default - and fall back to the OS's conventional mux when nothing answered.
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
        // The order decides which source paints first and reads as this box's main one,
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
    fn a_box_where_nothing_answered_still_offers_its_conventional_mux() {
        // Discovery finding nothing is not the same as having no sources: an empty nav
        // with no host card says nothing at all, while one unreachable card names the
        // mux that is missing.
        let c = Config::default();
        assert_eq!(c.local_muxes("windows", &[]), vec!["psmux"]);
        assert_eq!(c.local_muxes("linux", &[]), vec!["tmux"]);
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
        // Same for this box.
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
        // `exclude` names MACHINES, and a distribution's machine name carries the family
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
        // A `wsl.`-prefixed name belongs to the WSL family, and `kind_for` reads the
        // family out of the name. An ssh alias spelled that way would be built as a WSL
        // machine, so it is dropped instead of served as the wrong family.
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
        // No provider waits to be asked for. One that cannot run on this box costs an
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
    fn ui_border_styles_default_to_tmux_defaults() {
        // The keys are OVERRIDE-only, so unset → empty. The effective visual default
        // (green / terminal-default / yellow) comes from ViewBorderColors::default()
        // via ViewBorderColors::resolve, not from these raw config values.
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
}
