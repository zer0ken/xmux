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
    pub exclude: Vec<String>,
    #[serde(default)]
    pub ui: UiConfig,
}

/// Configures the mux used on the local machine.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalConfig {
    #[serde(default)]
    pub mux: String,
}

/// The optional `[ui]` table: xmux's own prefix.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// xmux's prefix spec (e.g. `C-g`, `C-Space`), config-only like tmux's
    /// `set -g prefix`. Parsed by `proxy::term::parse_prefix`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// The INITIAL state of the auto-hide-tree mode (toggled live with `prefix t`,
    /// then persisted to `~/.xmux/auto_hide_tree`, which wins over this on later
    /// runs). When the mode is on, focusing the mux hides the tree and gives the mux
    /// the full terminal width; the tree returns when focus returns to it. While
    /// hidden the tree has no column to click, so focus returns via the prefix keys
    /// (`prefix Tab`/`←`/`Esc`). Default false keeps the tree shown in both focus states.
    #[serde(rename = "auto-hide-tree", default)]
    pub auto_hide_tree: bool,
    /// The tree|mux divider colours, named after tmux's pane-border options so the
    /// experience matches tmux: the focused side uses `pane-active-border-style`, the
    /// unfocused side `pane-border-style`, and the drag-hover cue `pane-border-hover-style`.
    /// Values use tmux's colour vocabulary (parsed by [`crate::ui::switcher::map_color`]);
    /// the defaults mirror tmux's own (`green` / `default` / `yellow`).
    #[serde(rename = "pane-active-border-style", default = "default_active_border")]
    pub pane_active_border_style: String,
    #[serde(rename = "pane-border-style", default = "default_border")]
    pub pane_border_style: String,
    #[serde(rename = "pane-border-hover-style", default = "default_hover_border")]
    pub pane_border_hover_style: String,
}

fn default_prefix() -> String {
    "C-g".to_string()
}

fn default_active_border() -> String {
    "green".to_string()
}
fn default_border() -> String {
    "default".to_string()
}
fn default_hover_border() -> String {
    "yellow".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            prefix: default_prefix(),
            auto_hide_tree: false,
            pane_active_border_style: default_active_border(),
            pane_border_style: default_border(),
            pane_border_hover_style: default_hover_border(),
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
    pub mux: String,
}

/// A resolved remote host: its ssh alias and the mux binary to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
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
    /// Returns the mux binary to run on the local machine for the given `os`. An
    /// empty or `"auto"` setting picks psmux on Windows and tmux elsewhere; any
    /// other value is returned verbatim.
    pub fn local_bin(&self, os: &str) -> String {
        if self.local.mux.is_empty() || self.local.mux == "auto" {
            if os == "windows" {
                return "psmux".to_string();
            }
            return "tmux".to_string();
        }
        self.local.mux.clone()
    }

    /// xmux's configured prefix spec.
    pub fn ui_prefix(&self) -> &str {
        &self.ui.prefix
    }

    /// The initial auto-hide-tree mode from config (default false). The live toggle's
    /// persisted state, when present, overrides this — see `state::load_auto_hide_tree`.
    pub fn ui_auto_hide_tree(&self) -> bool {
        self.ui.auto_hide_tree
    }

    /// Merges ssh-config discovery with the config file. Discovered aliases come
    /// first in their original order (each deduped and skipping any in
    /// `exclude`), with the mux taken from a matching `hosts` override or
    /// defaulting to `"tmux"`. Config-only hosts (`hosts` entries whose ssh alias
    /// was not discovered) are appended afterwards. Config augments discovery; it
    /// never replaces it.
    pub fn host_specs(&self, ssh_aliases: &[String]) -> Vec<HostSpec> {
        use std::collections::HashSet;

        let excluded: HashSet<&str> = self.exclude.iter().map(String::as_str).collect();

        let mut override_mux: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for h in &self.hosts {
            if h.ssh.is_empty() {
                continue;
            }
            // First entry wins; a later duplicate with an empty mux must never
            // clobber an explicit one already recorded for the same alias.
            let replace = match override_mux.get(h.ssh.as_str()) {
                None => true,
                Some(existing) => existing.is_empty() && !h.mux.is_empty(),
            };
            if replace {
                override_mux.insert(h.ssh.as_str(), h.mux.as_str());
            }
        }

        let mut specs = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        // "local" is reserved for the local mux source; pre-seeding it makes both
        // the discovered-alias and config-host loops skip any host named "local"
        // so a remote can never shadow the local source.
        seen.insert(crate::session::LOCAL_SOURCE);

        for alias in ssh_aliases {
            if excluded.contains(alias.as_str()) || seen.contains(alias.as_str()) {
                continue;
            }
            let mut bin = "tmux";
            if let Some(&m) = override_mux.get(alias.as_str()) {
                if !m.is_empty() {
                    bin = m;
                }
            }
            specs.push(HostSpec {
                alias: alias.clone(),
                bin: bin.to_string(),
            });
            seen.insert(alias.as_str());
        }

        for h in &self.hosts {
            if h.ssh.is_empty()
                || excluded.contains(h.ssh.as_str())
                || seen.contains(h.ssh.as_str())
            {
                continue;
            }
            let bin = if h.mux.is_empty() { "tmux" } else { &h.mux };
            specs.push(HostSpec {
                alias: h.ssh.clone(),
                bin: bin.to_string(),
            });
            seen.insert(h.ssh.as_str());
        }

        specs
    }
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
        assert_eq!(cfg.local.mux, "");
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
        assert_eq!(cfg.local.mux, "tmux");
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].ssh, "prod");
        assert_eq!(cfg.hosts[0].mux, "psmux");
        assert_eq!(cfg.hosts[1].ssh, "stage");
        assert_eq!(cfg.hosts[1].mux, "");
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
        assert_eq!(cfg.local.mux, "");
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
        assert_eq!(cfg.local.mux, "tmux");
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
                alias: "prod".into(),
                bin: "psmux".into(),
            },
            HostSpec {
                alias: "stage".into(),
                bin: "tmux".into(),
            },
            HostSpec {
                alias: "extra".into(),
                bin: "zellij".into(),
            },
            HostSpec {
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
                    mux: String::new(),
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
    fn local_bin_cases() {
        let cases: &[(&str, &str, &str)] = &[
            ("", "windows", "psmux"),
            ("", "linux", "tmux"),
            ("auto", "windows", "psmux"),
            ("auto", "linux", "tmux"),
            ("zellij", "windows", "zellij"),
            ("zellij", "linux", "zellij"),
        ];
        for &(mux, os, want) in cases {
            let c = Config {
                local: LocalConfig { mux: mux.into() },
                ..Default::default()
            };
            assert_eq!(c.local_bin(os), want, "mux={mux:?} os={os:?}");
        }
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
        // Missing file → tmux's own code defaults (green / default / yellow).
        let missing = std::env::temp_dir().join("xmux-border-absent-xyz.toml");
        let cfg = load(&missing).unwrap();
        assert_eq!(cfg.ui.pane_active_border_style, "green");
        assert_eq!(cfg.ui.pane_border_style, "default");
        assert_eq!(cfg.ui.pane_border_hover_style, "yellow");

        // [ui] present but border keys missing → still the defaults.
        let path = write_temp("[ui]\nprefix = \"C-g\"\n", "border-missing.toml");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.pane_active_border_style, "green");
        assert_eq!(cfg.ui.pane_border_style, "default");
    }

    #[test]
    fn ui_border_styles_override_via_tmux_option_names() {
        let path = write_temp(
            "[ui]\npane-active-border-style = \"blue\"\npane-border-style = \"white\"\npane-border-hover-style = \"fg=red\"\n",
            "border-override.toml",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.ui.pane_active_border_style, "blue");
        assert_eq!(cfg.ui.pane_border_style, "white");
        assert_eq!(cfg.ui.pane_border_hover_style, "fg=red");
    }

    #[test]
    fn ui_auto_hide_tree_round_trip() {
        // Missing file → false.
        let missing = std::env::temp_dir().join("xmux-autohide-absent-xyz.toml");
        assert!(!load(&missing).unwrap().ui_auto_hide_tree());

        // [ui] present but key missing → false; prefix still loads.
        let path = write_temp("[ui]\nprefix = \"C-g\"\n", "autohide-missing.toml");
        let cfg = load(&path).unwrap();
        assert!(!cfg.ui_auto_hide_tree());
        assert_eq!(cfg.ui_prefix(), "C-g");

        // Explicit true.
        let path = write_temp("[ui]\nauto-hide-tree = true\n", "autohide-true.toml");
        let cfg = load(&path).unwrap();
        assert!(cfg.ui_auto_hide_tree());
        assert_eq!(cfg.ui_prefix(), "C-g"); // prefix unaffected, still defaults

        // Explicit false.
        let path = write_temp("[ui]\nauto-hide-tree = false\n", "autohide-false.toml");
        assert!(!load(&path).unwrap().ui_auto_hide_tree());
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
