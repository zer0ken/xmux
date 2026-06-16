package config

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func writeSSHConfig(t *testing.T, content string) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "config")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write ssh config: %v", err)
	}
	return path
}

func TestSSHHostAliasesMissingFile(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "no-such-config")
	got := SSHHostAliases(missing)
	if got != nil {
		t.Fatalf("missing file should yield nil, got %v", got)
	}
}

func TestSSHHostAliases(t *testing.T) {
	path := writeSSHConfig(t, `
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
`)
	got := SSHHostAliases(path)
	want := []string{"alpha", "beta", "gamma", "realhost", "indented"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("SSHHostAliases = %v, want %v", got, want)
	}
}
