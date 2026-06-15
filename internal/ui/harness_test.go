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
// per-key redraw settles TreeView's node cache, matching live). The live
// preview poller is NOT started here (it belongs to RunSwitcher), so preview
// CONTENT is verified live; the harness verifies the preview TARGET selection.

type harness struct {
	s      *switcher
	screen tcell.SimulationScreen
}

func newHarness(t *testing.T, scan Scan, ops SwitcherOps) *harness {
	t.Helper()
	screen := tcell.NewSimulationScreen("UTF-8")
	if err := screen.Init(); err != nil {
		t.Fatalf("screen init: %v", err)
	}
	screen.SetSize(100, 30)
	s := newSwitcher(scan, ops)
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

// treeRowOf returns the first screen row where text appears within the tree pane.
func (h *harness) treeRowOf(text string) int {
	cells, w, ht := h.screen.GetContents()
	runes := []rune(text)
	limit := treeWidth
	if limit > w {
		limit = w
	}
	for y := 0; y < ht; y++ {
		for x := 0; x+len(runes) <= limit; x++ {
			match := true
			for i, r := range runes {
				rs := cells[y*w+x+i].Runes
				if len(rs) == 0 || rs[0] != r {
					match = false
					break
				}
			}
			if match {
				return y
			}
		}
	}
	return -1
}

func (h *harness) treeRowReversed(y int) bool {
	cells, w, _ := h.screen.GetContents()
	limit := treeWidth
	if limit > w {
		limit = w
	}
	for x := 0; x < limit; x++ {
		_, _, attr := cells[y*w+x].Style.Decompose()
		if attr&tcell.AttrReverse != 0 {
			return true
		}
	}
	return false
}

// treeFgOf returns the foreground colour of the first cell of text in the tree.
func (h *harness) treeFgOf(text string) tcell.Color {
	cells, w, ht := h.screen.GetContents()
	runes := []rune(text)
	limit := treeWidth
	if limit > w {
		limit = w
	}
	for y := 0; y < ht; y++ {
		for x := 0; x+len(runes) <= limit; x++ {
			match := true
			for i, r := range runes {
				rs := cells[y*w+x+i].Runes
				if len(rs) == 0 || rs[0] != r {
					match = false
					break
				}
			}
			if match {
				fg, _, _ := cells[y*w+x].Style.Decompose()
				return fg
			}
		}
	}
	return tcell.ColorDefault
}

func curRef(h *harness) interface{} {
	n := h.s.tree.GetCurrentNode()
	if n == nil {
		return nil
	}
	return n.GetReference()
}

func noopOps() SwitcherOps {
	return SwitcherOps{
		Panes:   func(session.Session) ([]session.WindowPanes, error) { return nil, nil },
		Capture: func(string, string) (string, error) { return "", nil },
		Refresh: func() (Scan, error) { return Scan{}, nil },
		New:     func(src, name string) (session.Session, error) { return session.Session{Source: src, Name: name, Windows: 1}, nil },
		Kill:    func(session.Session) error { return nil },
		Rename:  func(session.Session, string) error { return nil },
	}
}

var errUnreachable = &scanErr{"connection timed out"}

type scanErr struct{ s string }

func (e *scanErr) Error() string { return e.s }

// switcherSample is a fully-populated Scan: local (editor, build) + jupiter00
// (inference) with windows/panes, and an unreachable db-2.
func switcherSample() Scan {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "editor", Windows: 2, Attached: true, LastAttached: 200},
			{Source: "local", Name: "build", Windows: 1, LastAttached: 100},
		}},
		{Source: "jupiter00", Sessions: []session.Session{
			{Source: "jupiter00", Name: "inference", Windows: 1, LastAttached: 300},
		}},
		{Source: "db-2", Err: errUnreachable},
	}
	panes := map[string][]session.WindowPanes{
		"local/editor": {
			{Index: 1, Name: "shell", Active: true, Panes: []session.Pane{{Index: 1, Active: true, Command: "bash"}}},
			{Index: 2, Name: "logs", Panes: []session.Pane{{Index: 1, Command: "tail"}}},
		},
		"local/build": {
			{Index: 1, Name: "make", Active: true, Panes: []session.Pane{{Index: 1, Active: true, Command: "make"}}},
		},
		"jupiter00/inference": {
			{Index: 1, Name: "train", Active: true, Panes: []session.Pane{{Index: 1, Active: true, Command: "python"}}},
		},
	}
	return Scan{Groups: groups, Panes: panes}
}
