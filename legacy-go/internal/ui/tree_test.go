package ui

import (
	"errors"
	"testing"

	"github.com/zer0ken/xmux/internal/session"
)

func TestSortByRecency(t *testing.T) {
	// LastAttached descending; name ascending tiebreak; stable.
	in := []session.Session{
		{Source: "local", Name: "beta", LastAttached: 100},
		{Source: "local", Name: "alpha", LastAttached: 200},
		{Source: "local", Name: "gamma", LastAttached: 100},
		{Source: "local", Name: "delta", LastAttached: 0},
	}
	SortByRecency(in)
	got := []string{in[0].Name, in[1].Name, in[2].Name, in[3].Name}
	want := []string{"alpha", "beta", "gamma", "delta"}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("position %d: got %q, want %q (full order %v)", i, got[i], want[i], got)
		}
	}
}

func TestFuzzyMatch(t *testing.T) {
	cases := []struct {
		pattern, s string
		want       bool
	}{
		{"if", "jupiter00/inference", true},   // subsequence, non-contiguous
		{"xyz", "abc", false},                 // no match
		{"", "anything", true},                // empty pattern always matches
		{"", "", true},                        // empty pattern, empty string
		{"abc", "abc", true},                  // exact
		{"abc", "a-b-c", true},                // gaps allowed
		{"cba", "abc", false},                 // order matters
		{"ABC", "xaybzc", true},               // case-insensitive pattern upper
		{"abc", "XAYBZC", true},               // case-insensitive subject upper
		{"abcd", "abc", false},                // pattern longer than match
		{"local", "local/web", true},          // address-style
		{"web", "local/web", true},            // tail subsequence
	}
	for _, c := range cases {
		if got := FuzzyMatch(c.pattern, c.s); got != c.want {
			t.Errorf("FuzzyMatch(%q, %q) = %v, want %v", c.pattern, c.s, got, c.want)
		}
	}
}

func sampleGroups() []Group {
	return []Group{
		{
			Source: "jupiter00",
			Sessions: []session.Session{
				{Source: "jupiter00", Name: "inference"},
				{Source: "jupiter00", Name: "training"},
			},
		},
		{
			Source: "local",
			Sessions: []session.Session{
				{Source: "local", Name: "web"},
				{Source: "local", Name: "db"},
			},
		},
		{
			Source: "deadhost",
			Err:    errors.New("dial: connection refused"),
			Sessions: []session.Session{
				{Source: "deadhost", Name: "ghost"}, // ignored when Err != nil
			},
		},
	}
}

func TestFilterGroupsEmptyPatternPassthrough(t *testing.T) {
	in := sampleGroups()
	got := FilterGroups(in, "")
	if len(got) != len(in) {
		t.Fatalf("empty pattern changed group count: got %d, want %d", len(got), len(in))
	}
	for i := range in {
		if got[i].Source != in[i].Source {
			t.Fatalf("group %d source: got %q, want %q", i, got[i].Source, in[i].Source)
		}
		if len(got[i].Sessions) != len(in[i].Sessions) {
			t.Fatalf("group %d session count changed: got %d, want %d", i, len(got[i].Sessions), len(in[i].Sessions))
		}
	}
}

func TestFilterGroupsSourceMatchKeepsAllSessions(t *testing.T) {
	in := sampleGroups()
	// "jptr" is a subsequence of "jupiter00" -> source matches, keep all its sessions.
	got := FilterGroups(in, "jptr")
	if len(got) != 1 {
		t.Fatalf("expected 1 group, got %d", len(got))
	}
	if got[0].Source != "jupiter00" {
		t.Fatalf("expected jupiter00, got %q", got[0].Source)
	}
	if len(got[0].Sessions) != 2 {
		t.Fatalf("source match should keep all sessions, got %d", len(got[0].Sessions))
	}
}

func TestFilterGroupsSessionOnlyMatchKeepsMatchingSessions(t *testing.T) {
	in := sampleGroups()
	// "inference" matches address "jupiter00/inference" but NOT "jupiter00/training",
	// and source "jupiter00" is not itself matched as a whole by this address-only filter.
	got := FilterGroups(in, "jupiter00/inference")
	if len(got) != 1 {
		t.Fatalf("expected 1 group, got %d (%+v)", len(got), got)
	}
	if got[0].Source != "jupiter00" {
		t.Fatalf("expected jupiter00, got %q", got[0].Source)
	}
	if len(got[0].Sessions) != 1 || got[0].Sessions[0].Name != "inference" {
		t.Fatalf("expected only [inference], got %+v", got[0].Sessions)
	}
}

func TestFilterGroupsUnreachableKeptOnlyOnSourceMatch(t *testing.T) {
	in := sampleGroups()

	// Source matches "deadhost" -> unreachable group kept.
	got := FilterGroups(in, "dead")
	if len(got) != 1 || got[0].Source != "deadhost" {
		t.Fatalf("expected deadhost kept on source match, got %+v", got)
	}
	if got[0].Err == nil {
		t.Fatalf("expected Err preserved on kept unreachable group")
	}

	// "ghost" would match the ignored session but NOT the source -> dropped.
	got2 := FilterGroups(in, "ghost")
	for _, g := range got2 {
		if g.Source == "deadhost" {
			t.Fatalf("unreachable group must not be kept by session match; got %+v", g)
		}
	}
}

func TestFilterGroupsPreservesOrder(t *testing.T) {
	in := sampleGroups()
	// "e" appears in jupiter00 (inference), local (web), deadhost (source has 'e').
	got := FilterGroups(in, "e")
	var order []string
	for _, g := range got {
		order = append(order, g.Source)
	}
	// jupiter00 (via session inference), local (via web), deadhost (via source).
	want := []string{"jupiter00", "local", "deadhost"}
	if len(order) != len(want) {
		t.Fatalf("got order %v, want %v", order, want)
	}
	for i := range want {
		if order[i] != want[i] {
			t.Fatalf("order[%d]: got %q, want %q (full %v)", i, order[i], want[i], order)
		}
	}
}

func TestFilterGroupsDoesNotMutateInput(t *testing.T) {
	in := sampleGroups()
	origLen := len(in[0].Sessions)
	origFirst := in[0].Sessions[0].Name
	_ = FilterGroups(in, "jupiter00/inference") // would shrink group 0 to 1 session if mutated
	if len(in[0].Sessions) != origLen {
		t.Fatalf("input group session count mutated: got %d, want %d", len(in[0].Sessions), origLen)
	}
	if in[0].Sessions[0].Name != origFirst {
		t.Fatalf("input group session content mutated: got %q, want %q", in[0].Sessions[0].Name, origFirst)
	}
}

func TestAddSessionNewGroup(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{{Source: "local", Name: "web"}}},
	}
	got := AddSession(groups, session.Session{Source: "remote", Name: "build"})
	if len(got) != 2 {
		t.Fatalf("expected 2 groups, got %d", len(got))
	}
	last := got[len(got)-1]
	if last.Source != "remote" || len(last.Sessions) != 1 || last.Sessions[0].Name != "build" {
		t.Fatalf("new group not appended correctly: %+v", last)
	}
}

func TestAddSessionAppendToExistingAndResort(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web", LastAttached: 50},
		}},
	}
	got := AddSession(groups, session.Session{Source: "local", Name: "db", LastAttached: 100})
	if len(got) != 1 {
		t.Fatalf("expected 1 group, got %d", len(got))
	}
	s := got[0].Sessions
	if len(s) != 2 {
		t.Fatalf("expected 2 sessions, got %d", len(s))
	}
	// db (100) is more recent than web (50) -> db first after resort.
	if s[0].Name != "db" || s[1].Name != "web" {
		t.Fatalf("not re-sorted by recency: %v", []string{s[0].Name, s[1].Name})
	}
}

func TestAddSessionDedupByNameReplaces(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web", Windows: 1, LastAttached: 10},
			{Source: "local", Name: "db", LastAttached: 5},
		}},
	}
	// Same Name "web" -> replace, not duplicate. New Windows=9, LastAttached bumps it first.
	got := AddSession(groups, session.Session{Source: "local", Name: "web", Windows: 9, LastAttached: 100})
	s := got[0].Sessions
	if len(s) != 2 {
		t.Fatalf("dedup failed, expected 2 sessions, got %d (%+v)", len(s), s)
	}
	// Find web and assert it was replaced.
	var web session.Session
	found := false
	for _, x := range s {
		if x.Name == "web" {
			web = x
			found = true
		}
	}
	if !found {
		t.Fatalf("web session missing after replace")
	}
	if web.Windows != 9 || web.LastAttached != 100 {
		t.Fatalf("web not replaced with new value: %+v", web)
	}
	if s[0].Name != "web" {
		t.Fatalf("expected replaced web first after resort, got %q", s[0].Name)
	}
}

func TestAddSessionDoesNotMutateInput(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{{Source: "local", Name: "web"}}},
	}
	origLen := len(groups[0].Sessions)
	_ = AddSession(groups, session.Session{Source: "local", Name: "db"})
	if len(groups[0].Sessions) != origLen {
		t.Fatalf("input mutated: group session count %d, want %d", len(groups[0].Sessions), origLen)
	}
}

func TestRemoveSessionDropsSession(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web"},
			{Source: "local", Name: "db"},
		}},
	}
	got := RemoveSession(groups, "local/web")
	if len(got) != 1 {
		t.Fatalf("expected group kept, got %d groups", len(got))
	}
	if len(got[0].Sessions) != 1 || got[0].Sessions[0].Name != "db" {
		t.Fatalf("web not removed: %+v", got[0].Sessions)
	}
}

func TestRemoveSessionKeepsEmptyGroup(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web"},
		}},
	}
	got := RemoveSession(groups, "local/web")
	if len(got) != 1 {
		t.Fatalf("empty group must remain a valid create target, got %d groups", len(got))
	}
	if got[0].Source != "local" {
		t.Fatalf("group source lost: %q", got[0].Source)
	}
	if len(got[0].Sessions) != 0 {
		t.Fatalf("expected empty session slice, got %+v", got[0].Sessions)
	}
}

func TestRemoveSessionDoesNotMutateInput(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web"},
			{Source: "local", Name: "db"},
		}},
	}
	origLen := len(groups[0].Sessions)
	_ = RemoveSession(groups, "local/web")
	if len(groups[0].Sessions) != origLen {
		t.Fatalf("input mutated: count %d, want %d", len(groups[0].Sessions), origLen)
	}
}

func TestRenameSessionRenamesAndResorts(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "alpha", LastAttached: 100},
			{Source: "local", Name: "zeta", LastAttached: 100},
		}},
	}
	// Equal LastAttached -> name tiebreak. Rename "alpha" to "zzz" should reorder
	// so zeta (z < zzz) comes first after resort.
	got := RenameSession(groups, "local/alpha", "zzz")
	s := got[0].Sessions
	if len(s) != 2 {
		t.Fatalf("expected 2 sessions, got %d", len(s))
	}
	if s[0].Name != "zeta" || s[1].Name != "zzz" {
		t.Fatalf("rename+resort wrong: got %v", []string{s[0].Name, s[1].Name})
	}
}

func TestRenameSessionNoOpWhenMissing(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web"},
		}},
	}
	got := RenameSession(groups, "local/nonexistent", "newname")
	if len(got) != 1 || len(got[0].Sessions) != 1 {
		t.Fatalf("unexpected shape: %+v", got)
	}
	if got[0].Sessions[0].Name != "web" {
		t.Fatalf("no-op violated: name changed to %q", got[0].Sessions[0].Name)
	}
}

func TestRenameSessionDoesNotMutateInput(t *testing.T) {
	groups := []Group{
		{Source: "local", Sessions: []session.Session{
			{Source: "local", Name: "web"},
		}},
	}
	_ = RenameSession(groups, "local/web", "renamed")
	if groups[0].Sessions[0].Name != "web" {
		t.Fatalf("input mutated: name is %q, want %q", groups[0].Sessions[0].Name, "web")
	}
}

func TestSortByRecencyStableForEqualKeys(t *testing.T) {
	// Equal LastAttached AND equal name: original relative order preserved.
	in := []session.Session{
		{Source: "h1", Name: "x", LastAttached: 50},
		{Source: "h2", Name: "x", LastAttached: 50},
		{Source: "h3", Name: "x", LastAttached: 50},
	}
	SortByRecency(in)
	want := []string{"h1", "h2", "h3"}
	for i := range want {
		if in[i].Source != want[i] {
			t.Fatalf("position %d: got source %q, want %q", i, in[i].Source, want[i])
		}
	}
}
