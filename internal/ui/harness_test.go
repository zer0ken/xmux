package ui

import (
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
	"github.com/zer0ken/xmux/internal/session"
)

// --- headless harness -------------------------------------------------------
//
// The switcher is driven without app.Run(): primitives are built by
// newSwitcher, keys are dispatched through the exact tview path the run loop
// uses, and the flex is drawn to a tcell SimulationScreen after every key (the
// per-key redraw settles TreeView's internal node cache, matching live).

type harness struct {
	s      *switcher
	screen tcell.SimulationScreen
}

func newHarness(t *testing.T, groups []Group, ops SwitcherOps) *harness {
	t.Helper()
	screen := tcell.NewSimulationScreen("UTF-8")
	if err := screen.Init(); err != nil {
		t.Fatalf("screen init: %v", err)
	}
	screen.SetSize(90, 24)
	s := newSwitcher(groups, ops)
	s.app.SetScreen(screen)
	h := &harness{s: s, screen: screen}
	h.draw()
	return h
}

func (h *harness) draw() {
	w, hgt := h.screen.Size()
	h.screen.Clear()
	h.s.flex.SetRect(0, 0, w, hgt)
	h.s.flex.Draw(h.screen)
	h.screen.Show()
}

func (h *harness) press(ev *tcell.EventKey) {
	if cap := h.s.app.GetInputCapture(); cap != nil {
		ev = cap(ev)
		if ev == nil {
			h.draw()
			return
		}
	}
	if handler := h.s.app.GetFocus().InputHandler(); handler != nil {
		handler(ev, func(p tview.Primitive) { h.s.app.SetFocus(p) })
	}
	h.draw()
}

func (h *harness) rune(r rune)     { h.press(tcell.NewEventKey(tcell.KeyRune, r, tcell.ModNone)) }
func (h *harness) key(k tcell.Key) { h.press(tcell.NewEventKey(k, 0, tcell.ModNone)) }

func (h *harness) text() string {
	cells, w, hgt := h.screen.GetContents()
	var b strings.Builder
	for y := 0; y < hgt; y++ {
		for x := 0; x < w; x++ {
			rs := cells[y*w+x].Runes
			if len(rs) == 0 || rs[0] == 0 {
				b.WriteByte(' ')
			} else {
				b.WriteRune(rs[0])
			}
		}
		b.WriteString("\n")
	}
	return b.String()
}

func noopOps() SwitcherOps {
	return SwitcherOps{
		Panes:   func(session.Session) ([]session.WindowPanes, error) { return nil, nil },
		Refresh: func() ([]Group, error) { return nil, nil },
		New:     func(string, string) (session.Session, error) { return session.Session{}, nil },
		Kill:    func(session.Session) error { return nil },
		Rename:  func(session.Session, string) error { return nil },
	}
}

func switcherSample() []Group {
	return []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "editor", Windows: 3, Attached: true, LastAttached: 200},
			{Source: "local", Name: "build", Windows: 1, LastAttached: 100},
		}},
		{Source: "jupiter00", Sessions: []session.Session{
			{Source: "jupiter00", Name: "inference", Windows: 3, LastAttached: 300},
		}},
		{Source: "db-2", Err: errUnreachable},
	}
}

// errUnreachable is a stand-in for a scan error.
var errUnreachable = &scanErr{"connection timed out"}

type scanErr struct{ s string }

func (e *scanErr) Error() string { return e.s }

func TestRendersHostsAndSessions(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	out := h.text()
	for _, want := range []string{"local", "editor", "build", "jupiter00", "inference", "db-2", "unreachable"} {
		if !strings.Contains(out, want) {
			t.Errorf("rendered tree missing %q\n---\n%s", want, out)
		}
	}
}

func TestPreselectsGloballyMostRecentSession(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	// jupiter00/inference has the highest LastAttached (300) ⇒ it is the current node.
	cur := h.s.tree.GetCurrentNode()
	if cur == nil {
		t.Fatal("no current node")
	}
	ref, ok := cur.GetReference().(swSessionRef)
	if !ok || ref.S.Name != "inference" {
		t.Fatalf("preselected node = %+v, want session inference", cur.GetReference())
	}
}
