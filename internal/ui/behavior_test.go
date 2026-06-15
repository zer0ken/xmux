package ui

import (
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
	"github.com/zer0ken/xmux/internal/session"
)

type recordOps struct {
	killed  []session.Session
	created []string
	renamed []string
}

func recordingOps() (*recordOps, SwitcherOps) {
	r := &recordOps{}
	ops := noopOps()
	ops.New = func(src, name string) (session.Session, error) {
		r.created = append(r.created, src+"/"+name)
		return session.Session{Source: src, Name: name, Windows: 1}, nil
	}
	ops.Kill = func(s session.Session) error { r.killed = append(r.killed, s); return nil }
	ops.Rename = func(s session.Session, nn string) error {
		r.renamed = append(r.renamed, s.Address()+"->"+nn)
		return nil
	}
	return r, ops
}

func TestRendersFourLevelTree(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	out := h.text()
	for _, want := range []string{
		"local", "editor", "window 1: shell", "pane 1  bash", "window 2: logs",
		"build", "jupiter00", "inference", "window 1: train", "pane 1  python",
		"db-2", "unreachable",
	} {
		if !strings.Contains(out, want) {
			t.Errorf("tree missing %q\n---\n%s", want, out)
		}
	}
}

func TestPreselectsMostRecentSession(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	ref, ok := curRef(h).(swSessionRef)
	if !ok || ref.S.Name != "inference" {
		t.Fatalf("preselected = %+v, want session inference (highest recency)", curRef(h))
	}
}

func TestPanesAreNotSelectable(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.key(tcell.KeyHome)
	sawWindow := false
	for i := 0; i < 14; i++ {
		ref := curRef(h)
		if ref == nil {
			t.Fatalf("cursor landed on a non-selectable node (a pane) at step %d", i)
		}
		if _, ok := ref.(swWindowRef); ok {
			sawWindow = true
		}
		h.key(tcell.KeyDown)
	}
	if !sawWindow {
		t.Error("navigation should reach window nodes")
	}
}

func TestPreviewTargetFollowsCursor(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())

	// host node ⇒ its most-recent session's active window.
	h.key(tcell.KeyHome)
	if _, ok := curRef(h).(swHostRef); !ok {
		t.Fatalf("Home should focus the first host, got %+v", curRef(h))
	}
	if got := h.s.previewTgt; got.Source != "local" || got.Target != "editor" {
		t.Errorf("host preview target = %+v, want {local editor}", got)
	}

	// session node ⇒ that session.
	h.key(tcell.KeyDown) // editor
	if got := h.s.previewTgt; got.Source != "local" || got.Target != "editor" {
		t.Errorf("session preview target = %+v, want {local editor}", got)
	}

	// window node ⇒ session:window.
	h.key(tcell.KeyDown) // window 1: shell
	if _, ok := curRef(h).(swWindowRef); !ok {
		t.Fatalf("expected a window node, got %+v", curRef(h))
	}
	if got := h.s.previewTgt; got.Source != "local" || got.Target != "editor:1" {
		t.Errorf("window preview target = %+v, want {local editor:1}", got)
	}
}

func TestSelectedNodeRendersReverseVideo(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	sel := h.treeRowOf("inference")
	other := h.treeRowOf("editor")
	if sel < 0 || other < 0 {
		t.Fatalf("rows not found")
	}
	if !h.treeRowReversed(sel) {
		t.Error("the selected row must render reverse video (visible on any theme)")
	}
	if h.treeRowReversed(other) {
		t.Error("a non-selected row must not be reversed")
	}
}

func TestEnterAttachesSession(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.key(tcell.KeyEnter) // inference preselected
	if h.s.result.Chosen == nil || h.s.result.Chosen.Name != "inference" || h.s.result.Window != -1 {
		t.Fatalf("Enter on session = %+v, want inference window -1", h.s.result)
	}
}

func TestEnterAttachesWindow(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.key(tcell.KeyDown) // inference → window 1: train
	ref, ok := curRef(h).(swWindowRef)
	if !ok {
		t.Fatalf("expected window node, got %+v", curRef(h))
	}
	h.key(tcell.KeyEnter)
	if h.s.result.Chosen == nil || h.s.result.Chosen.Name != ref.S.Name || h.s.result.Window != ref.Window {
		t.Fatalf("Enter on window = %+v, want %s window %d", h.s.result, ref.S.Name, ref.Window)
	}
}

func TestEnterOnHostAttachesRecentSession(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.key(tcell.KeyHome) // local host
	h.key(tcell.KeyEnter)
	if h.s.result.Chosen == nil || h.s.result.Chosen.Name != "editor" || h.s.result.Window != -1 {
		t.Fatalf("Enter on host = %+v, want local's recent session editor", h.s.result)
	}
}

func TestFilterNarrows(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.rune('/')
	for _, r := range "infer" {
		h.rune(r)
	}
	h.key(tcell.KeyEnter)
	out := h.text()
	if !strings.Contains(out, "inference") {
		t.Fatalf("filter should keep inference:\n%s", out)
	}
	if strings.Contains(out, "editor") || strings.Contains(out, "build") {
		t.Fatalf("filter should drop non-matches:\n%s", out)
	}
	if !strings.Contains(out, "filter: infer") {
		t.Fatalf("active filter should show in the title:\n%s", out)
	}
}

func TestKillRemovesSessionAndCache(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	if _, ok := h.s.panes["jupiter00/inference"]; !ok {
		t.Fatal("inference windows should be cached up front")
	}
	h.rune('x') // arm
	if !strings.Contains(h.text(), "kill jupiter00/inference?") {
		t.Fatalf("expected inline kill confirm:\n%s", h.text())
	}
	h.rune('y')
	if len(r.killed) != 1 || r.killed[0].Name != "inference" {
		t.Fatalf("Kill not called for inference: %+v", r.killed)
	}
	if _, ok := h.s.panes["jupiter00/inference"]; ok {
		t.Error("kill must invalidate the windows cache for the dead session")
	}
	if strings.Contains(h.text(), "inference") {
		t.Errorf("killed session must disappear:\n%s", h.text())
	}
}

func TestCreateAddsAndSelects(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	// inference preselected ⇒ create on jupiter00.
	h.rune('n')
	h.s.input.SetText("scratch")
	h.key(tcell.KeyEnter)
	if len(r.created) != 1 || r.created[0] != "jupiter00/scratch" {
		t.Fatalf("New mis-called: %+v", r.created)
	}
	ref, ok := curRef(h).(swSessionRef)
	if !ok || ref.S.Name != "scratch" {
		t.Fatalf("cursor should land on the created session, got %+v", curRef(h))
	}
}

func TestRenameRejectsLeadingDash(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	h.rune('R')
	h.s.input.SetText("-bad")
	h.key(tcell.KeyEnter)
	if len(r.renamed) != 0 {
		t.Fatalf("rename to a '-'-leading name must be refused: %v", r.renamed)
	}
}

func TestCreateOnUnreachableHostRefused(t *testing.T) {
	r, ops := recordingOps()
	h := newHarness(t, switcherSample(), ops)
	// from inference (preselected): Down → its window, Down → the unreachable db-2 host.
	h.key(tcell.KeyDown)
	h.key(tcell.KeyDown)
	ref, ok := curRef(h).(swHostRef)
	if !ok || !ref.Unreachable {
		t.Fatalf("expected to reach the unreachable db-2 host, got %+v", curRef(h))
	}
	h.rune('n')
	if !strings.Contains(strings.ToLower(h.s.flash), "unreachable") {
		t.Fatalf("create on unreachable host should flash 'unreachable', got %q", h.s.flash)
	}
	if len(r.created) != 0 {
		t.Fatalf("must not create on an unreachable host: %v", r.created)
	}
}

func TestQuitLeavesNoChoice(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	h.rune('q')
	if h.s.result.Chosen != nil {
		t.Fatalf("quit must leave no choice, got %+v", h.s.result.Chosen)
	}
}
