package config

import (
	"bufio"
	"os"
	"strings"
)

// SSHHostAliases parses an OpenSSH client config at path and returns the
// concrete host aliases declared by Host lines, in first-seen order and
// deduplicated. Glob patterns (containing * or ?) and negations (starting with
// !) are skipped, as are comments, blank lines, and non-Host directives.
// Include and Match directives are not expanded. A missing file yields nil.
func SSHHostAliases(path string) []string {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()

	var aliases []string
	seen := make(map[string]bool)

	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Fields(line)
		if !strings.EqualFold(fields[0], "Host") {
			continue
		}
		for _, pattern := range fields[1:] {
			if strings.HasPrefix(pattern, "!") ||
				strings.ContainsAny(pattern, "*?") {
				continue
			}
			if seen[pattern] {
				continue
			}
			aliases = append(aliases, pattern)
			seen[pattern] = true
		}
	}
	return aliases
}
