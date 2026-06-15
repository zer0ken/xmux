// Package ui holds the pure tree-model logic for the session switcher: a slice
// of Groups (one per source) each carrying its sessions ordered by recency. The
// functions here are side-effect-free transforms over that model; the
// interactive tview rendering is layered on top separately.
package ui

import (
	"sort"
	"strings"

	"github.com/zer0ken/xmux/internal/session"
)

// Group is the sessions of one source. A non-nil Err means the host was
// unreachable, in which case Sessions carries no meaning.
type Group struct {
	Source   string
	Err      error
	Sessions []session.Session
}

// SortByRecency orders sessions in place with the most recently attached first
// (LastAttached descending), breaking ties by name ascending. The sort is
// stable so sessions equal on both keys keep their original relative order.
func SortByRecency(sessions []session.Session) {
	sort.SliceStable(sessions, func(i, j int) bool {
		a, b := sessions[i], sessions[j]
		if a.LastAttached != b.LastAttached {
			return a.LastAttached > b.LastAttached
		}
		return a.Name < b.Name
	})
}

// FilterGroups keeps the groups whose source matches pattern or that have at
// least one matching session, preserving group order. An empty pattern returns
// the input unchanged. A reachable group whose source matches keeps all its
// sessions; otherwise only the sessions whose address matches are kept. An
// unreachable group (Err != nil) is kept only when its source matches, since
// its sessions carry no meaning. Inputs are never mutated.
func FilterGroups(groups []Group, pattern string) []Group {
	if pattern == "" {
		return groups
	}
	var out []Group
	for _, g := range groups {
		sourceMatch := FuzzyMatch(pattern, g.Source)
		if g.Err != nil {
			if sourceMatch {
				out = append(out, g)
			}
			continue
		}
		if sourceMatch {
			kept := make([]session.Session, len(g.Sessions))
			copy(kept, g.Sessions)
			out = append(out, Group{Source: g.Source, Sessions: kept})
			continue
		}
		var kept []session.Session
		for _, s := range g.Sessions {
			if FuzzyMatch(pattern, s.Address()) {
				kept = append(kept, s)
			}
		}
		if len(kept) > 0 {
			out = append(out, Group{Source: g.Source, Sessions: kept})
		}
	}
	return out
}

// AddSession returns groups with s placed in the group whose source matches
// s.Source, replacing any existing session of the same name (dedup by name) and
// re-sorting that group by recency. If no group has the source, a new group is
// appended. The affected group is copied so inputs are not mutated.
func AddSession(groups []Group, s session.Session) []Group {
	out := make([]Group, len(groups))
	copy(out, groups)
	for i := range out {
		if out[i].Source != s.Source {
			continue
		}
		sessions := make([]session.Session, 0, len(out[i].Sessions)+1)
		replaced := false
		for _, existing := range out[i].Sessions {
			if existing.Name == s.Name {
				sessions = append(sessions, s)
				replaced = true
			} else {
				sessions = append(sessions, existing)
			}
		}
		if !replaced {
			sessions = append(sessions, s)
		}
		SortByRecency(sessions)
		out[i].Sessions = sessions
		return out
	}
	return append(out, Group{Source: s.Source, Sessions: []session.Session{s}})
}

// RemoveSession returns groups with the session at address removed from its
// group. The now-possibly-empty group is kept, since an empty reachable group
// is still a valid create target. Inputs are not mutated.
func RemoveSession(groups []Group, address string) []Group {
	out := make([]Group, len(groups))
	copy(out, groups)
	for i := range out {
		for j, s := range out[i].Sessions {
			if s.Address() != address {
				continue
			}
			sessions := make([]session.Session, 0, len(out[i].Sessions)-1)
			sessions = append(sessions, out[i].Sessions[:j]...)
			sessions = append(sessions, out[i].Sessions[j+1:]...)
			out[i].Sessions = sessions
			return out
		}
	}
	return out
}

// RenameSession returns groups with the session at address renamed to newName
// and its group re-sorted by recency. It is a no-op if no session matches.
// Inputs are not mutated.
func RenameSession(groups []Group, address, newName string) []Group {
	out := make([]Group, len(groups))
	copy(out, groups)
	for i := range out {
		for j, s := range out[i].Sessions {
			if s.Address() != address {
				continue
			}
			sessions := make([]session.Session, len(out[i].Sessions))
			copy(sessions, out[i].Sessions)
			sessions[j].Name = newName
			SortByRecency(sessions)
			out[i].Sessions = sessions
			return out
		}
	}
	return out
}

// FuzzyMatch reports whether pattern is a case-insensitive subsequence of s:
// every rune of pattern appears in s in order, not necessarily contiguously. An
// empty pattern always matches.
func FuzzyMatch(pattern, s string) bool {
	p := []rune(strings.ToLower(pattern))
	if len(p) == 0 {
		return true
	}
	i := 0
	for _, r := range strings.ToLower(s) {
		if r == p[i] {
			i++
			if i == len(p) {
				return true
			}
		}
	}
	return false
}
