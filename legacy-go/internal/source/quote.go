package source

import "strings"

// quote renders one argument safe for a POSIX shell. A string of only safe
// characters passes through; anything else is single-quoted with embedded
// single-quotes escaped as '\''. This is the SOLE point an untrusted value (a
// session name from a remote list-sessions) enters a remote shell command.
func quote(s string) string {
	if s == "" {
		return "''"
	}
	if isShellSafe(s) {
		return s
	}
	return "'" + strings.ReplaceAll(s, "'", `'\''`) + "'"
}

func isShellSafe(s string) bool {
	for _, r := range s {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		case strings.ContainsRune("-_./", r):
		default:
			return false
		}
	}
	return true
}

// remoteCommand joins a mux argv into a single shell command line, quoting each
// element, for execution by the remote shell ssh hands it to.
func remoteCommand(argv []string) string {
	parts := make([]string, len(argv))
	for i, a := range argv {
		parts[i] = quote(a)
	}
	return strings.Join(parts, " ")
}
