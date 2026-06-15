package ui

import (
	"testing"

	"github.com/gdamore/tcell/v2"
)

// treeRowOf returns the screen row (y) where text first appears within the tree
// pane (x < treeWidth), or -1.
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

// treeRowReversed reports whether any cell on the given row within the tree pane
// is drawn with reverse video.
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

func TestSelectedNodeRendersReverseVideo(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	// inference is preselected (highest recency); editor is not selected.
	sel := h.treeRowOf("inference")
	other := h.treeRowOf("editor")
	if sel < 0 || other < 0 {
		t.Fatalf("rows not found: inference=%d editor=%d\n%s", sel, other, h.text())
	}
	if !h.treeRowReversed(sel) {
		t.Errorf("the selected node row must render with reverse video so the cursor is visible on any theme")
	}
	if h.treeRowReversed(other) {
		t.Errorf("a non-selected node row must NOT be reversed")
	}
}

func TestHighlightFollowsNavigation(t *testing.T) {
	h := newHarness(t, switcherSample(), noopOps())
	// move up to "build"; its row should now be the reversed one.
	h.key(tcell.KeyUp) // jupiter00 host
	h.key(tcell.KeyUp) // build
	build := h.treeRowOf("build")
	inference := h.treeRowOf("inference")
	if build < 0 {
		t.Fatalf("build row not found:\n%s", h.text())
	}
	if !h.treeRowReversed(build) {
		t.Errorf("after navigating to build, its row should be reversed")
	}
	if inference >= 0 && h.treeRowReversed(inference) {
		t.Errorf("inference should no longer be reversed after moving away")
	}
}
