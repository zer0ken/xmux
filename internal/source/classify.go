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
	s := strings.ToLower(ee.Stderr)
	for _, marker := range []string{"no server running", "no sessions"} {
		if strings.Contains(s, marker) {
			return true
		}
	}
	return false
}
