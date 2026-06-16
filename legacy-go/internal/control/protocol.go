// Package control is a programmatic control channel that drives a running
// tview application headlessly. It backs xmux's own tests and the `xmux ctl`
// command, injecting keystrokes and dumping the rendered screen over an
// AF_UNIX socket.
package control

import (
	"bufio"
	"fmt"
	"io"
	"strconv"
	"strings"

	"github.com/gdamore/tcell/v2"
)

// maxFrame bounds a single length-framed payload to guard against a corrupt or
// hostile length header.
const maxFrame = 1 << 24

// namedKeys maps lowercase key names to their tcell key code. Names producing a
// rune (space) are handled separately in parseKey.
var namedKeys = map[string]tcell.Key{
	"up":        tcell.KeyUp,
	"down":      tcell.KeyDown,
	"left":      tcell.KeyLeft,
	"right":     tcell.KeyRight,
	"enter":     tcell.KeyEnter,
	"esc":       tcell.KeyEscape,
	"escape":    tcell.KeyEscape,
	"tab":       tcell.KeyTab,
	"backtab":   tcell.KeyBacktab,
	"home":      tcell.KeyHome,
	"end":       tcell.KeyEnd,
	"pgup":      tcell.KeyPgUp,
	"pgdn":      tcell.KeyPgDn,
	"backspace": tcell.KeyBackspace2,
	"delete":    tcell.KeyDelete,
	"insert":    tcell.KeyInsert,
}

// parseKey maps a key name to a tcell event. Named keys and ctrl+<letter> are
// matched case-insensitively; a single rune is taken verbatim (case preserved,
// so "R" differs from "r"). Returns (nil, false) for anything unrecognized.
func parseKey(name string) (*tcell.EventKey, bool) {
	// A single rune is preserved exactly as given, including its case. This is
	// checked before lowercasing so "R" and "r" stay distinct.
	if r := []rune(name); len(r) == 1 {
		return tcell.NewEventKey(tcell.KeyRune, r[0], tcell.ModNone), true
	}

	lc := strings.ToLower(name)

	if lc == "space" {
		return tcell.NewEventKey(tcell.KeyRune, ' ', tcell.ModNone), true
	}

	if k, ok := namedKeys[lc]; ok {
		return tcell.NewEventKey(k, 0, tcell.ModNone), true
	}

	// ctrl+<letter> or ctrl-<letter>, letter a-z (case-insensitive).
	if strings.HasPrefix(lc, "ctrl+") || strings.HasPrefix(lc, "ctrl-") {
		rest := lc[len("ctrl+"):]
		if len(rest) == 1 && rest[0] >= 'a' && rest[0] <= 'z' {
			k := tcell.Key(int(tcell.KeyCtrlA) + int(rest[0]-'a'))
			return tcell.NewEventKey(k, 0, tcell.ModCtrl), true
		}
	}

	return nil, false
}

// writeFrame writes payload as a length-framed message: a decimal byte count
// followed by a newline, then the raw payload bytes.
func writeFrame(w io.Writer, payload string) error {
	if _, err := io.WriteString(w, strconv.Itoa(len(payload))+"\n"); err != nil {
		return err
	}
	_, err := io.WriteString(w, payload)
	return err
}

// readFrame reads a length-framed message written by writeFrame. The length
// header must not exceed maxFrame.
func readFrame(r *bufio.Reader) (string, error) {
	line, err := r.ReadString('\n')
	if err != nil {
		return "", err
	}
	n, err := strconv.Atoi(strings.TrimRight(line, "\r\n"))
	if err != nil {
		return "", fmt.Errorf("control: bad frame length %q: %w", line, err)
	}
	if n < 0 || n > maxFrame {
		return "", fmt.Errorf("control: frame length %d out of range", n)
	}
	buf := make([]byte, n)
	if _, err := io.ReadFull(r, buf); err != nil {
		return "", err
	}
	return string(buf), nil
}

// request is a parsed command line: a lowercased verb and the verbatim
// remainder as its argument.
type request struct {
	verb string
	arg  string
}

// parseRequest splits a request line on its first space. The verb is
// lowercased; the arg is the remainder verbatim. A trailing CR/LF is trimmed
// from the line before splitting.
func parseRequest(line string) request {
	line = strings.TrimRight(line, "\r\n")
	if i := strings.IndexByte(line, ' '); i >= 0 {
		return request{verb: strings.ToLower(line[:i]), arg: line[i+1:]}
	}
	return request{verb: strings.ToLower(line)}
}
