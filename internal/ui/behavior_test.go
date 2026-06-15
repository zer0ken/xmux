package ui

import (
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
	"github.com/zer0ken/xmux/internal/session"
)

// recordOps records calls and lets a test script the side effects.
type recordOps struct {
	SwitcherOps
	killed   []session.Session
	created  []string // "source/name"
	renamed  []string // "addr->new"
}

func recordingOps() (*recordOps, SwitcherOps) {
	r := &recordOps{}
	ops := SwitcherOps{
		Panes:   func(session.Session) ([]session.WindowPanes, error) { return nil, nil },
		Refresh: func() ([]Group, error) { return nil, nil },
		New: func(src, name string) (session.Session, error) {
			r.created = append(r.created, src+"/"+name)
			return session.Session{Source: src, Name: name, Windows: 1}, nil
		},
		Kill: func(s session.Session) error {
			r.killed = append(r.killed, s)
			return nil
		},
		Rename: func(s session.Session, newName string) error {
			r.renamed = append(r.renamed, s.Address()+"->"+newName)
			return nil
		},
	}
	return r, ops
}

func curRef(h *harness) interface{} {
	n := h.s.tree.GetCurrentNode()
	if n == nil {
		return nil
	}
	return n.GetReference()
}

func TestEnterOnSessionChooses(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	// inference is preselected; Enter chooses it.
	h.key(tcell.KeyEnter)
	if h.s.chosen == nil || h.s.chosen.Name != "inference" {
		t.Fatalf("chosen = %+v, want inference", h.s.chosen)
	}
}

func TestEnterOnHostTogglesExpansion(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	// Move up from inference to the jupiter00 host node.
	h.key(tcell.KeyUp)
	ref, ok := curRef(h).(swHostRef)
	if !ok || ref.Source != "jupiter00" {
		t.Fatalf("expected jupiter00 host focused, got %+v", curRef(h))
	}
	if !strings.Contains(h.text(), "▾ jupiter00") {
		t.Fatalf("host should start expanded (▾):\n%s", h.text())
	}
	h.key(tcell.KeyEnter) // collapse
	out := h.text()
	if !strings.Contains(out, "▸ jupiter00") {
		t.Fatalf("after collapse expected ▸ jupiter00:\n%s", out)
	}
	if strings.Contains(out, "inference") {
		t.Fatalf("collapsed host must hide its sessions:\n%s", out)
	}
}

func TestFilterByTypingNarrows(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.rune('/') // open filter input
	for _, r := range "build" {
		h.rune(r)
	}
	h.key(tcell.KeyEnter)
	out := h.text()
	if !strings.Contains(out, "build") {
		t.Fatalf("filter 'build' should keep build:\n%s", out)
	}
	if strings.Contains(out, "editor") || strings.Contains(out, "inference") {
		t.Fatalf("filter 'build' should drop non-matches:\n%s", out)
	}
	if !strings.Contains(out, "filter: build") {
		t.Fatalf("active filter should show in the tree title:\n%s", out)
	}
}

func TestDetailFollowsCursor(t *testing.T) {
	ops := noopOps()
	ops.Panes = func(s session.Session) ([]session.WindowPanes, error) {
		if s.Name == "inference" {
			return []session.WindowPanes{{Index: 1, Name: "train", Active: true, Panes: []session.Pane{{Index: 1, Active: true, Command: "python"}}}}, nil
		}
		return nil, nil
	}
	h := newHarness(t, switcherSample(), ops)
	// inference preselected ⇒ its detail shows.
	if got := h.s.detail.GetText(true); !strings.Contains(got, "python") {
		t.Fatalf("detail for inference should list python pane, got:\n%s", got)
	}
	// Move to a host node ⇒ detail clears.
	h.key(tcell.KeyUp)
	if got := h.s.detail.GetText(true); strings.TrimSpace(got) != "" {
		t.Fatalf("detail should clear on a host node, got:\n%s", got)
	}
}

func TestKillConfirmRemovesSession(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	// inference preselected.
	h.rune('x') // arm confirm
	if !strings.Contains(h.text(), "kill jupiter00/inference?") {
		t.Fatalf("expected inline kill confirm in footer:\n%s", h.text())
	}
	h.rune('y') // confirm
	if len(r.killed) != 1 || r.killed[0].Name != "inference" {
		t.Fatalf("Kill not called for inference: %+v", r.killed)
	}
	if strings.Contains(h.text(), "inference") {
		t.Fatalf("killed session must disappear from the tree:\n%s", h.text())
	}
}

func TestKillConfirmCancel(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	h.rune('x')
	h.rune('n') // cancel
	if len(r.killed) != 0 {
		t.Fatalf("cancel must not kill: %+v", r.killed)
	}
	if !strings.Contains(h.text(), "inference") {
		t.Fatalf("cancelled kill must keep the session:\n%s", h.text())
	}
}

func TestCreateAddsSession(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	// inference preselected ⇒ create targets jupiter00.
	h.rune('n')
	h.s.input.SetText("scratch")
	h.key(tcell.KeyEnter)
	if len(r.created) != 1 || r.created[0] != "jupiter00/scratch" {
		t.Fatalf("New not called for jupiter00/scratch: %+v", r.created)
	}
	if !strings.Contains(h.text(), "scratch") {
		t.Fatalf("created session should appear in the tree:\n%s", h.text())
	}
}

func TestRenameSession(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	h.rune('R')
	h.s.input.SetText("inference2")
	h.key(tcell.KeyEnter)
	if len(r.renamed) != 1 || r.renamed[0] != "jupiter00/inference->inference2" {
		t.Fatalf("Rename not called correctly: %+v", r.renamed)
	}
	out := h.text()
	if !strings.Contains(out, "inference2") || strings.Contains(out, "inference\n") {
		t.Fatalf("tree should show renamed session:\n%s", out)
	}
}

func TestQuitLeavesNoChoice(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.rune('q')
	if h.s.chosen != nil {
		t.Fatalf("quit must leave chosen nil, got %+v", h.s.chosen)
	}
}
