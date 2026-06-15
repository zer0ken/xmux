// Package mux builds the argv for mux (tmux/psmux) subcommands and parses their
// tab-delimited output. Builders are pure: they assemble []string argv slices
// with no shell involved (argv[0] is the mux binary name). Parsers are pure
// functions over the raw command output.
package mux

import "strconv"

// SessionFormat is the list-sessions -F template. The free-form session name is
// LAST so a tab inside a name cannot shift the fixed numeric columns.
const SessionFormat = "#{session_windows}\t#{session_attached}\t#{session_last_attached}\t#{session_name}"

// PaneFormat is the list-panes -F template. The free-form window name is LAST so
// a tab inside it cannot shift the fixed columns; pane_current_command sits in a
// fixed slot because a process name has no tabs.
const PaneFormat = "#{window_index}\t#{window_active}\t#{pane_index}\t#{pane_active}\t#{pane_current_command}\t#{window_name}"

// ListSessions lists all sessions on the server in SessionFormat.
func ListSessions(bin string) []string {
	return []string{bin, "list-sessions", "-F", SessionFormat}
}

// ListPanes lists every pane across ALL windows of session name. The -s flag
// widens scope to the whole session (never -a, which leaks across servers).
func ListPanes(bin, name string) []string {
	return []string{bin, "list-panes", "-s", "-t", name, "-F", PaneFormat}
}

// Attach attaches the current client to session name.
func Attach(bin, name string) []string {
	return []string{bin, "attach", "-t", name}
}

// SwitchClient switches the current client to session name.
func SwitchClient(bin, name string) []string {
	return []string{bin, "switch-client", "-t", name}
}

// DetachClient detaches the current client. -E is deliberately omitted because
// psmux treats it as a no-op, so the design must not depend on it.
func DetachClient(bin string) []string {
	return []string{bin, "detach-client"}
}

// NewSession creates-or-attaches a DETACHED session and prints its assigned
// name. -A makes it idempotent, -d keeps it detached, and -P -F prints the
// assigned name even when the mux auto-names (e.g. "0"). A non-empty name is
// requested with -s; an empty name lets the mux auto-name.
func NewSession(bin, name string) []string {
	argv := []string{bin, "new-session", "-A", "-d", "-P", "-F", "#{session_name}"}
	if name != "" {
		argv = append(argv, "-s", name)
	}
	return argv
}

// WindowTarget builds a "session:window" target.
func WindowTarget(session string, window int) string {
	return session + ":" + strconv.Itoa(window)
}

// PaneTarget builds a "session:window.pane" target.
func PaneTarget(session string, window, pane int) string {
	return session + ":" + strconv.Itoa(window) + "." + strconv.Itoa(pane)
}

// CapturePane prints the visible content of the target pane (a pane, or the
// active pane of a window/session target) to stdout — the preview source. -e
// includes the pane's ANSI colour escapes so the preview reproduces its colours.
func CapturePane(bin, target string) []string {
	return []string{bin, "capture-pane", "-p", "-e", "-t", target}
}

// SelectWindow makes the target window active in its session.
func SelectWindow(bin, target string) []string {
	return []string{bin, "select-window", "-t", target}
}

// SelectPane makes the target pane active in its window.
func SelectPane(bin, target string) []string {
	return []string{bin, "select-pane", "-t", target}
}

// KillSession kills session name.
func KillSession(bin, name string) []string {
	return []string{bin, "kill-session", "-t", name}
}

// RenameSession renames session oldName to newName.
func RenameSession(bin, oldName, newName string) []string {
	return []string{bin, "rename-session", "-t", oldName, newName}
}
