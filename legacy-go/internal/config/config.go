// Package config loads xmux's optional TOML configuration and merges it with
// ssh-config discovery to produce the set of hosts and mux binaries to use.
package config

import (
	"fmt"
	"os"

	"github.com/BurntSushi/toml"
)

// Config is the on-disk config.toml structure. All fields are optional.
type Config struct {
	Local   LocalConfig  `toml:"local"`
	Hosts   []HostConfig `toml:"hosts"`
	Exclude []string     `toml:"exclude"`
}

// LocalConfig configures the mux used on the local machine.
type LocalConfig struct {
	Mux string `toml:"mux"`
}

// HostConfig overrides the mux for a discovered ssh alias, or adds a host that
// ssh-config discovery did not surface.
type HostConfig struct {
	SSH string `toml:"ssh"`
	Mux string `toml:"mux"`
}

// HostSpec is a resolved remote host: its ssh alias and the mux binary to run.
type HostSpec struct {
	Alias, Bin string
}

// Load reads config.toml from path. A missing file yields a zero Config and no
// error; a parse error is returned to the caller (treated as fatal).
func Load(path string) (Config, error) {
	cfg, _, err := load(path)
	return cfg, err
}

// LoadVerbose behaves like Load but also returns human-readable warnings for
// any keys present in the file that did not decode into Config (typos, removed
// or unsupported options). A missing file yields nil warnings and no error.
func LoadVerbose(path string) (Config, []string, error) {
	cfg, md, err := load(path)
	if err != nil || md == nil {
		return cfg, nil, err
	}
	var warnings []string
	for _, key := range md.Undecoded() {
		warnings = append(warnings, fmt.Sprintf("unknown key %q", key.String()))
	}
	return cfg, warnings, nil
}

// LocalBin returns the mux binary to run on the local machine for the given
// GOOS. An empty or "auto" setting picks psmux on Windows and tmux elsewhere;
// any other value is returned verbatim.
func (c Config) LocalBin(goos string) string {
	if c.Local.Mux == "" || c.Local.Mux == "auto" {
		if goos == "windows" {
			return "psmux"
		}
		return "tmux"
	}
	return c.Local.Mux
}

// HostSpecs merges ssh-config discovery with the config file. Discovered
// aliases come first in their original order (each deduped and skipping any in
// Exclude), with the mux taken from a matching Hosts override or defaulting to
// "tmux". Config-only hosts (Hosts entries whose ssh alias was not discovered)
// are appended afterwards. Config augments discovery; it never replaces it.
func (c Config) HostSpecs(sshAliases []string) []HostSpec {
	excluded := make(map[string]bool, len(c.Exclude))
	for _, e := range c.Exclude {
		excluded[e] = true
	}

	override := make(map[string]string, len(c.Hosts))
	for _, h := range c.Hosts {
		if h.SSH != "" {
			override[h.SSH] = h.Mux
		}
	}

	var specs []HostSpec
	seen := make(map[string]bool)

	for _, alias := range sshAliases {
		if excluded[alias] || seen[alias] {
			continue
		}
		bin := "tmux"
		if m, ok := override[alias]; ok && m != "" {
			bin = m
		}
		specs = append(specs, HostSpec{Alias: alias, Bin: bin})
		seen[alias] = true
	}

	for _, h := range c.Hosts {
		if h.SSH == "" || excluded[h.SSH] || seen[h.SSH] {
			continue
		}
		bin := h.Mux
		if bin == "" {
			bin = "tmux"
		}
		specs = append(specs, HostSpec{Alias: h.SSH, Bin: bin})
		seen[h.SSH] = true
	}

	return specs
}

func load(path string) (Config, *toml.MetaData, error) {
	var cfg Config
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return cfg, nil, nil
	}
	md, err := toml.DecodeFile(path, &cfg)
	if err != nil {
		return Config{}, nil, err
	}
	return cfg, &md, nil
}
