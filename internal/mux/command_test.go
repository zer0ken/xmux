package mux

import (
	"reflect"
	"testing"
)

func TestSessionFormat(t *testing.T) {
	want := "#{session_windows}\t#{session_attached}\t#{session_last_attached}\t#{session_name}"
	if SessionFormat != want {
		t.Errorf("SessionFormat = %q, want %q", SessionFormat, want)
	}
}

func TestPaneFormat(t *testing.T) {
	want := "#{window_index}\t#{window_active}\t#{pane_index}\t#{pane_active}\t#{pane_current_command}\t#{window_name}"
	if PaneFormat != want {
		t.Errorf("PaneFormat = %q, want %q", PaneFormat, want)
	}
}

func TestListSessions(t *testing.T) {
	got := ListSessions("tmux")
	want := []string{"tmux", "list-sessions", "-F", SessionFormat}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ListSessions = %#v, want %#v", got, want)
	}
}

func TestListPanes(t *testing.T) {
	got := ListPanes("psmux", "work")
	want := []string{"psmux", "list-panes", "-s", "-t", "work", "-F", PaneFormat}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ListPanes = %#v, want %#v", got, want)
	}
}

func TestAttach(t *testing.T) {
	got := Attach("tmux", "main")
	want := []string{"tmux", "attach", "-t", "main"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("Attach = %#v, want %#v", got, want)
	}
}

func TestSwitchClient(t *testing.T) {
	got := SwitchClient("tmux", "main")
	want := []string{"tmux", "switch-client", "-t", "main"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("SwitchClient = %#v, want %#v", got, want)
	}
}

func TestDetachClient(t *testing.T) {
	got := DetachClient("tmux")
	want := []string{"tmux", "detach-client"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("DetachClient = %#v, want %#v", got, want)
	}
}

func TestNewSessionNamed(t *testing.T) {
	got := NewSession("tmux", "dev")
	want := []string{"tmux", "new-session", "-A", "-d", "-P", "-F", "#{session_name}", "-s", "dev"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("NewSession named = %#v, want %#v", got, want)
	}
}

func TestNewSessionAutoName(t *testing.T) {
	got := NewSession("tmux", "")
	want := []string{"tmux", "new-session", "-A", "-d", "-P", "-F", "#{session_name}"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("NewSession auto = %#v, want %#v", got, want)
	}
}

func TestKillSession(t *testing.T) {
	got := KillSession("tmux", "old")
	want := []string{"tmux", "kill-session", "-t", "old"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("KillSession = %#v, want %#v", got, want)
	}
}

func TestRenameSession(t *testing.T) {
	got := RenameSession("tmux", "old", "new")
	want := []string{"tmux", "rename-session", "-t", "old", "new"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("RenameSession = %#v, want %#v", got, want)
	}
}
