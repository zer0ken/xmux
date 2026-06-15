package source

import (
	"errors"
	"strings"
)

// isNoSessions reports whether err means "the mux is reachable but has no
// sessions" rather than "the host is unreachable". tmux exits non-zero with a
// "no server running" message when idle, so this distinguishes an empty-but-alive
// mux from a dead one. NEVER infer this from the exit code alone — only a real
// command exit (ExitErr, carrying stderr) can be benign; a missing binary or a
// connect failure is a plain error and is always unreachable.
func isNoSessions(err error) bool {
	var ee *ExitErr
	if !errors.As(err, &ee) {
		return false
	}
	// command-not-found (127), not-executable (126), and ssh failure (255) are
	// never a healthy-but-empty mux — a broken host must not be hidden as empty.
	switch ee.Code {
	case 126, 127, 255:
		return false
	}
	// Match the marker as a line PREFIX, not anywhere — so a login banner or MOTD
	// line like "you have no sessions pending" cannot masquerade as the idle mux.
	for _, line := range strings.Split(strings.ToLower(ee.Stderr), "\n") {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, "no server running") || strings.HasPrefix(line, "no sessions") {
			return true
		}
	}
	return false
}
