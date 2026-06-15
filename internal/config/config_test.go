package config

import (
	"os"
	"path/filepath"
	"testing"
)

func writeTemp(t *testing.T, content string) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write temp config: %v", err)
	}
	return path
}

func TestLoadMissingFile(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "does-not-exist.toml")
	cfg, err := Load(missing)
	if err != nil {
		t.Fatalf("missing file should not error, got %v", err)
	}
	if len(cfg.Hosts) != 0 || len(cfg.Exclude) != 0 || cfg.Local.Mux != "" {
		t.Fatalf("missing file should yield zero Config, got %+v", cfg)
	}
}

func TestLoadRoundTrip(t *testing.T) {
	path := writeTemp(t, `
exclude = ["foo", "bar"]

[local]
mux = "tmux"

[[hosts]]
ssh = "prod"
mux = "psmux"

[[hosts]]
ssh = "stage"
`)
	cfg, err := Load(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg.Local.Mux != "tmux" {
		t.Errorf("Local.Mux = %q, want %q", cfg.Local.Mux, "tmux")
	}
	if len(cfg.Hosts) != 2 {
		t.Fatalf("len(Hosts) = %d, want 2", len(cfg.Hosts))
	}
	if cfg.Hosts[0].SSH != "prod" || cfg.Hosts[0].Mux != "psmux" {
		t.Errorf("Hosts[0] = %+v, want {prod psmux}", cfg.Hosts[0])
	}
	if cfg.Hosts[1].SSH != "stage" || cfg.Hosts[1].Mux != "" {
		t.Errorf("Hosts[1] = %+v, want {stage }", cfg.Hosts[1])
	}
	if len(cfg.Exclude) != 2 || cfg.Exclude[0] != "foo" || cfg.Exclude[1] != "bar" {
		t.Errorf("Exclude = %v, want [foo bar]", cfg.Exclude)
	}
}

func TestLoadMalformed(t *testing.T) {
	path := writeTemp(t, "this is = = not valid toml [[[")
	_, err := Load(path)
	if err == nil {
		t.Fatal("malformed toml should return an error")
	}
}

func TestLoadVerboseMissingFile(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "nope.toml")
	cfg, warnings, err := LoadVerbose(missing)
	if err != nil {
		t.Fatalf("missing file should not error, got %v", err)
	}
	if warnings != nil {
		t.Errorf("missing file should yield nil warnings, got %v", warnings)
	}
	if cfg.Local.Mux != "" {
		t.Errorf("missing file should yield zero Config, got %+v", cfg)
	}
}

func TestLoadVerboseUnknownKey(t *testing.T) {
	path := writeTemp(t, `
[local]
mux = "tmux"
bogus = "nope"
`)
	cfg, warnings, err := LoadVerbose(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg.Local.Mux != "tmux" {
		t.Errorf("Local.Mux = %q, want tmux", cfg.Local.Mux)
	}
	if len(warnings) != 1 {
		t.Fatalf("warnings = %v, want exactly 1", warnings)
	}
	if warnings[0] != `unknown key "local.bogus"` {
		t.Errorf("warning = %q, want %q", warnings[0], `unknown key "local.bogus"`)
	}
}

func TestHostSpecs(t *testing.T) {
	cfg := Config{
		Hosts: []HostConfig{
			{SSH: "prod", Mux: "psmux"}, // override for a discovered alias
			{SSH: "extra", Mux: "zellij"}, // config-only host
			{SSH: "noMuxOnly"},            // config-only host, default bin
			{SSH: "", Mux: "ignored"},     // no ssh => ignored as override/host
		},
		Exclude: []string{"banned"},
	}
	// ssh-config discovery order: prod, banned, stage, prod (dup)
	sshAliases := []string{"prod", "banned", "stage", "prod"}

	got := cfg.HostSpecs(sshAliases)

	want := []HostSpec{
		{Alias: "prod", Bin: "psmux"},     // override applied, ssh-order first
		{Alias: "stage", Bin: "tmux"},     // discovered, default bin (banned excluded, dup prod dropped)
		{Alias: "extra", Bin: "zellij"},   // config-only, after discovery
		{Alias: "noMuxOnly", Bin: "tmux"}, // config-only, default bin
	}

	if len(got) != len(want) {
		t.Fatalf("HostSpecs len = %d (%+v), want %d (%+v)", len(got), got, len(want), want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("HostSpecs[%d] = %+v, want %+v", i, got[i], want[i])
		}
	}
}

func TestHostSpecsExcludesConfigOnly(t *testing.T) {
	cfg := Config{
		Hosts:   []HostConfig{{SSH: "secret", Mux: "psmux"}},
		Exclude: []string{"secret"},
	}
	got := cfg.HostSpecs(nil)
	if len(got) != 0 {
		t.Fatalf("excluded config-only host should not appear, got %+v", got)
	}
}

func TestLocalBin(t *testing.T) {
	tests := []struct {
		name string
		mux  string
		goos string
		want string
	}{
		{"empty windows", "", "windows", "psmux"},
		{"empty linux", "", "linux", "tmux"},
		{"auto windows", "auto", "windows", "psmux"},
		{"auto linux", "auto", "linux", "tmux"},
		{"explicit windows", "zellij", "windows", "zellij"},
		{"explicit linux", "zellij", "linux", "zellij"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			c := Config{Local: LocalConfig{Mux: tc.mux}}
			if got := c.LocalBin(tc.goos); got != tc.want {
				t.Errorf("LocalBin(%q) with mux=%q = %q, want %q", tc.goos, tc.mux, got, tc.want)
			}
		})
	}
}
