package ui

import (
	"fmt"
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
	"github.com/zer0ken/xmux/internal/session"
)

// navigate from the preselected inference up to the local "editor" session.
func focusEditor(h *harness) {
	h.key(tcell.KeyUp) // jupiter00 host
	h.key(tcell.KeyUp) // build
	h.key(tcell.KeyUp) // editor
}

func TestDetailCacheInvalidatedOnKill(t *testing.T) {
	calls := map[string]int{}
	r, ops := recordingOps()
	ops.Panes = func(s session.Session) ([]session.WindowPanes, error) {
		calls[s.Address()]++
		return []session.WindowPanes{{Index: 1, Name: fmt.Sprintf("gen%d", calls[s.Address()])}}, nil
	}
	h := newHarness(t, switcherSample(), ops)
	focusEditor(h)
	if _, ok := h.s.detailByAddr["local/editor"]; !ok {
		t.Fatalf("editor detail should be cached after focus")
	}
	h.rune('x')
	h.rune('y') // kill editor
	if len(r.killed) == 0 {
		t.Fatal("kill not invoked")
	}
	if _, ok := h.s.detailByAddr["local/editor"]; ok {
		t.Fatalf("kill must invalidate the detail cache for local/editor (else a recreated session shows stale windows)")
	}
}

func TestRenameRejectsLeadingDash(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	h.rune('R') // rename inference
	h.s.input.SetText("-bad")
	h.key(tcell.KeyEnter)
	if len(r.renamed) != 0 {
		t.Fatalf("rename to a '-'-leading name must be refused (mux silently no-ops it): %v", r.renamed)
	}
	if !strings.Contains(h.text(), "inference") {
		t.Fatalf("model must not optimistically show the rejected name:\n%s", h.text())
	}
}

func TestCreateOnUnreachableHostRefused(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	h.key(tcell.KeyDown) // inference → db-2 (unreachable host)
	ref, ok := curRef(h).(swHostRef)
	if !ok || !ref.Unreachable {
		t.Fatalf("expected unreachable host focused, got %+v", curRef(h))
	}
	h.rune('n')
	if !strings.Contains(strings.ToLower(h.s.flash), "unreachable") {
		t.Fatalf("create on an unreachable host should flash 'unreachable', got %q", h.s.flash)
	}
	// even if the user types and commits, nothing is created
	h.s.input.SetText("ghost")
	h.key(tcell.KeyEnter)
	if len(r.created) != 0 {
		t.Fatalf("must not create on an unreachable host: %v", r.created)
	}
}

func TestCreateSelectsNewSession(t *testing.T) {
	_, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	// inference preselected ⇒ create targets jupiter00.
	h.rune('n')
	h.s.input.SetText("scratch")
	h.key(tcell.KeyEnter)
	ref, ok := curRef(h).(swSessionRef)
	if !ok || ref.S.Name != "scratch" {
		t.Fatalf("cursor should land on the just-created session, got %+v", curRef(h))
	}
}
