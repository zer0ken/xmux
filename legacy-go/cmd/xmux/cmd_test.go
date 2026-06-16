package main

import (
	"strings"
	"testing"

	"github.com/zer0ken/xmux/internal/discovery"
	"github.com/zer0ken/xmux/internal/session"
)

func TestToGroupsSortsByRecency(t *testing.T) {
	results := []discovery.Result{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "old", LastAttached: 10},
			{Source: "local", Name: "new", LastAttached: 99},
		}},
		{Source: "prod", Err: errSample},
	}
	groups := toGroups(results)
	if len(groups) != 2 {
		t.Fatalf("want 2 groups, got %d", len(groups))
	}
	if groups[0].Sessions[0].Name != "new" {
		t.Errorf("most-recent session must sort first, got %q", groups[0].Sessions[0].Name)
	}
	if groups[1].Err == nil {
		t.Errorf("unreachable group must keep its error")
	}
}

func TestLsLines(t *testing.T) {
	groups := toGroups([]discovery.Result{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "editor", Windows: 3, Attached: true, LastAttached: 5},
		}},
		{Source: "empty"}, // reachable, zero sessions
		{Source: "prod", Err: errSample},
	})
	lines, unreachable, allUnreachable := lsLines(groups)
	if len(lines) != 1 || !strings.HasPrefix(lines[0], "local/editor") {
		t.Fatalf("session line wrong: %v", lines)
	}
	if !strings.Contains(lines[0], "3w") || !strings.Contains(lines[0], "attached=true") {
		t.Errorf("session line missing fields: %q", lines[0])
	}
	if len(unreachable) != 1 || !strings.HasPrefix(unreachable[0], "prod") {
		t.Errorf("unreachable line wrong: %v", unreachable)
	}
	if allUnreachable {
		t.Errorf("a reachable source exists ⇒ not allUnreachable")
	}
}

func TestLsLinesAllUnreachable(t *testing.T) {
	groups := toGroups([]discovery.Result{
		{Source: "a", Err: errSample},
		{Source: "b", Err: errSample},
	})
	_, _, allUnreachable := lsLines(groups)
	if !allUnreachable {
		t.Errorf("every source unreachable ⇒ allUnreachable true")
	}
}

var errSample = &simpleErr{"unreachable"}

type simpleErr struct{ s string }

func (e *simpleErr) Error() string { return e.s }
