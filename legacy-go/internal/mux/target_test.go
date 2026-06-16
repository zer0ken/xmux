package mux

import (
	"reflect"
	"testing"
)

func TestWindowTarget(t *testing.T) {
	if got := WindowTarget("editor", 2); got != "editor:2" {
		t.Errorf("WindowTarget = %q, want editor:2", got)
	}
}

func TestPaneTarget(t *testing.T) {
	if got := PaneTarget("editor", 2, 1); got != "editor:2.1" {
		t.Errorf("PaneTarget = %q, want editor:2.1", got)
	}
}

func TestCapturePane(t *testing.T) {
	got := CapturePane("psmux", "editor:1.0")
	// -e includes the pane's ANSI colour escapes so the preview can reproduce them.
	want := []string{"psmux", "capture-pane", "-p", "-e", "-t", "editor:1.0"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("CapturePane = %v, want %v", got, want)
	}
}

func TestSelectWindow(t *testing.T) {
	got := SelectWindow("tmux", "if:3")
	want := []string{"tmux", "select-window", "-t", "if:3"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("SelectWindow = %v, want %v", got, want)
	}
}

func TestSelectPane(t *testing.T) {
	got := SelectPane("tmux", "if:3.2")
	want := []string{"tmux", "select-pane", "-t", "if:3.2"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("SelectPane = %v, want %v", got, want)
	}
}
