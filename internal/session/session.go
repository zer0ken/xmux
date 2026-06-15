// Package session defines the cross-environment data types: a Session living on
// a source (mux server), its windows-and-panes detail, and the <source>/<name>
// address that targets one session across the server boundary.
package session

import (
	"fmt"
	"strings"
)

// LocalSource is the reserved source name for the local mux server.
const LocalSource = "local"

// Session is one mux session as seen on a source.
type Session struct {
	Source       string // "local" or an ssh alias
	Name         string // session name (may contain "/")
	Windows      int
	Attached     bool
	LastAttached int64 // unix seconds; 0 when the mux does not report it
}

// Address is the cross-environment target string, "<source>/<name>".
func (s Session) Address() string {
	return s.Source + "/" + s.Name
}

// Pane is one pane within a window.
type Pane struct {
	Index   int
	Active  bool
	Command string // pane_current_command
}

// WindowPanes groups the panes of a single window, in window order.
type WindowPanes struct {
	Index  int
	Name   string
	Active bool
	Panes  []Pane
}

// ParseTarget splits a "<source>/<name>" address on the FIRST "/" so a session
// name containing "/" is preserved. Both halves must be non-empty.
func ParseTarget(addr string) (Session, error) {
	i := strings.IndexByte(addr, '/')
	if i < 0 {
		return Session{}, fmt.Errorf("invalid target %q: want <source>/<session>", addr)
	}
	source, name := addr[:i], addr[i+1:]
	if source == "" || name == "" {
		return Session{}, fmt.Errorf("invalid target %q: source and session must be non-empty", addr)
	}
	return Session{Source: source, Name: name}, nil
}
