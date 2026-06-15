// Package manage performs lifecycle operations (create, kill, rename, inspect)
// directly against the live mux on a source. Each function builds a mux argv and
// runs it through source.Source.Run; nothing is cached and no state is held.
package manage

import (
	"context"
	"strings"

	"github.com/zer0ken/xmux/internal/mux"
	"github.com/zer0ken/xmux/internal/session"
	"github.com/zer0ken/xmux/internal/source"
)

// Create creates-or-attaches a DETACHED session on the source and returns its
// assigned name. The mux prints the name (auto-named when name==""); the trailing
// newline/space is trimmed. On error the name is empty.
func Create(ctx context.Context, s source.Source, name string) (string, error) {
	out, err := s.Run(ctx, mux.NewSession(s.Binary, name))
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

// Kill kills a session by name.
func Kill(ctx context.Context, s source.Source, name string) error {
	_, err := s.Run(ctx, mux.KillSession(s.Binary, name))
	return err
}

// Rename renames a session.
func Rename(ctx context.Context, s source.Source, oldName, newName string) error {
	_, err := s.Run(ctx, mux.RenameSession(s.Binary, oldName, newName))
	return err
}

// Panes returns the source session's windows-with-panes (for the tree's lazy
// child loading and active-pane resolution).
func Panes(ctx context.Context, s source.Source, name string) ([]session.WindowPanes, error) {
	out, err := s.Run(ctx, mux.ListPanes(s.Binary, name))
	if err != nil {
		return nil, err
	}
	return mux.ParsePanes(string(out)), nil
}

// Capture returns the visible content of a target pane (a "session",
// "session:window", or "session:window.pane" target) — the live preview source.
func Capture(ctx context.Context, s source.Source, target string) (string, error) {
	out, err := s.Run(ctx, mux.CapturePane(s.Binary, target))
	if err != nil {
		return "", err
	}
	return string(out), nil
}

// SelectWindow makes a window active in its session (used before attach so the
// client lands on the chosen window).
func SelectWindow(ctx context.Context, s source.Source, sess string, window int) error {
	_, err := s.Run(ctx, mux.SelectWindow(s.Binary, mux.WindowTarget(sess, window)))
	return err
}

// SelectPane makes a pane active in its window (used before attach so the client
// lands on the chosen pane).
func SelectPane(ctx context.Context, s source.Source, sess string, window, pane int) error {
	_, err := s.Run(ctx, mux.SelectPane(s.Binary, mux.PaneTarget(sess, window, pane)))
	return err
}
