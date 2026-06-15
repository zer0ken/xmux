package mux

import (
	"strconv"
	"strings"

	"github.com/zer0ken/xmux/internal/session"
)

// splitLines splits raw mux output into non-blank lines, tolerating both \r\n
// and \n endings.
func splitLines(out string) []string {
	raw := strings.Split(strings.ReplaceAll(out, "\r\n", "\n"), "\n")
	lines := make([]string, 0, len(raw))
	for _, ln := range raw {
		if ln == "" {
			continue
		}
		lines = append(lines, ln)
	}
	return lines
}

// ParseSessions parses list-sessions output (SessionFormat) into sessions tagged
// with source. Malformed lines (short, non-numeric numeric columns, or empty
// name) are skipped so banners and garbage cannot poison the list. The name is
// rejoined from fields[3:] so a tab inside a name survives. Order is preserved.
func ParseSessions(source, out string) []session.Session {
	var sessions []session.Session
	for _, ln := range splitLines(out) {
		fields := strings.Split(ln, "\t")
		if len(fields) < 4 {
			continue
		}
		windows, err := strconv.Atoi(fields[0])
		if err != nil {
			continue
		}
		attachedN, err := strconv.Atoi(fields[1])
		if err != nil {
			continue
		}
		var lastAttached int64
		if fields[2] != "" {
			lastAttached, err = strconv.ParseInt(fields[2], 10, 64)
			if err != nil {
				continue
			}
		}
		name := strings.Join(fields[3:], "\t")
		if name == "" {
			continue
		}
		sessions = append(sessions, session.Session{
			Source:       source,
			Name:         name,
			Windows:      windows,
			Attached:     attachedN > 0,
			LastAttached: lastAttached,
		})
	}
	return sessions
}

// ParsePanes parses list-panes output (PaneFormat) into windows-and-panes,
// grouping panes by window_index in first-seen order. Each window takes its
// Index, Name, and Active from the first row seen for that window; the window
// name is rejoined from fields[5:] so a tab inside it survives. Malformed lines
// (short or non-numeric window_index) are skipped.
func ParsePanes(out string) []session.WindowPanes {
	var windows []session.WindowPanes
	pos := map[int]int{} // window_index -> position in windows
	for _, ln := range splitLines(out) {
		fields := strings.Split(ln, "\t")
		if len(fields) < 6 {
			continue
		}
		winIdx, err := strconv.Atoi(fields[0])
		if err != nil {
			continue
		}
		winActive, err := strconv.Atoi(fields[1])
		if err != nil {
			continue
		}
		paneIdx, err := strconv.Atoi(fields[2])
		if err != nil {
			continue
		}
		paneActive, err := strconv.Atoi(fields[3])
		if err != nil {
			continue
		}
		command := fields[4]
		winName := strings.Join(fields[5:], "\t")

		pane := session.Pane{Index: paneIdx, Active: paneActive > 0, Command: command}
		if i, ok := pos[winIdx]; ok {
			windows[i].Panes = append(windows[i].Panes, pane)
			continue
		}
		pos[winIdx] = len(windows)
		windows = append(windows, session.WindowPanes{
			Index:  winIdx,
			Name:   winName,
			Active: winActive > 0,
			Panes:  []session.Pane{pane},
		})
	}
	return windows
}
