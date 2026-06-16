package control

import (
	"bufio"
	"bytes"
	"strings"
	"testing"

	"github.com/gdamore/tcell/v2"
)

func TestParseKeyNamed(t *testing.T) {
	cases := map[string]tcell.Key{
		"up":        tcell.KeyUp,
		"DOWN":      tcell.KeyDown,
		"left":      tcell.KeyLeft,
		"Right":     tcell.KeyRight,
		"enter":     tcell.KeyEnter,
		"esc":       tcell.KeyEscape,
		"escape":    tcell.KeyEscape,
		"tab":       tcell.KeyTab,
		"backtab": tcell.KeyBacktab,
		"home":    tcell.KeyHome,
		"end":     tcell.KeyEnd,
		"pgup":    tcell.KeyPgUp,
		"pgdn":    tcell.KeyPgDn,
		// tcell.NewEventKey canonicalizes KeyBackspace2 to KeyBackspace, so the
		// "backspace" name (spec'd as KeyBackspace2) surfaces as KeyBackspace.
		"backspace": tcell.KeyBackspace,
		"delete":    tcell.KeyDelete,
		"insert":    tcell.KeyInsert,
	}
	for name, want := range cases {
		ev, ok := parseKey(name)
		if !ok {
			t.Fatalf("parseKey(%q): ok=false, want true", name)
		}
		if ev.Key() != want {
			t.Errorf("parseKey(%q): key=%v, want %v", name, ev.Key(), want)
		}
	}
}

func TestParseKeySpace(t *testing.T) {
	ev, ok := parseKey("space")
	if !ok {
		t.Fatal("parseKey(space): ok=false")
	}
	if ev.Key() != tcell.KeyRune || ev.Rune() != ' ' {
		t.Errorf("parseKey(space): key=%v rune=%q, want KeyRune ' '", ev.Key(), ev.Rune())
	}
}

func TestParseKeyCtrl(t *testing.T) {
	for _, name := range []string{"ctrl+c", "ctrl-c", "CTRL+C", "Ctrl-C"} {
		ev, ok := parseKey(name)
		if !ok {
			t.Fatalf("parseKey(%q): ok=false", name)
		}
		if ev.Key() != tcell.KeyCtrlC {
			t.Errorf("parseKey(%q): key=%v, want KeyCtrlC", name, ev.Key())
		}
		if ev.Modifiers()&tcell.ModCtrl == 0 {
			t.Errorf("parseKey(%q): missing ModCtrl", name)
		}
	}
}

func TestParseKeySingleRuneCasePreserved(t *testing.T) {
	upper, ok := parseKey("R")
	if !ok || upper.Key() != tcell.KeyRune || upper.Rune() != 'R' {
		t.Fatalf("parseKey(R): ok=%v key=%v rune=%q, want KeyRune 'R'", ok, upper.Key(), upper.Rune())
	}
	lower, ok := parseKey("r")
	if !ok || lower.Key() != tcell.KeyRune || lower.Rune() != 'r' {
		t.Fatalf("parseKey(r): ok=%v key=%v rune=%q, want KeyRune 'r'", ok, lower.Key(), lower.Rune())
	}
	if upper.Rune() == lower.Rune() {
		t.Error("parseKey: 'R' and 'r' produced same rune, case not preserved")
	}
}

func TestParseKeyUnknown(t *testing.T) {
	for _, name := range []string{"nope", "ctrl+", "ctrl+1", "", "fnord"} {
		if ev, ok := parseKey(name); ok {
			t.Errorf("parseKey(%q): ok=true (ev=%v), want false", name, ev)
		}
	}
}

func TestFrameRoundTrip(t *testing.T) {
	for _, payload := range []string{
		"pong",
		"",
		"a single line",
		"line one\nline two\nline three",
	} {
		var buf bytes.Buffer
		if err := writeFrame(&buf, payload); err != nil {
			t.Fatalf("writeFrame(%q): %v", payload, err)
		}
		got, err := readFrame(bufio.NewReader(&buf))
		if err != nil {
			t.Fatalf("readFrame(%q): %v", payload, err)
		}
		if got != payload {
			t.Errorf("round trip: got %q, want %q", got, payload)
		}
	}
}

func TestReadFrameOversized(t *testing.T) {
	r := bufio.NewReader(strings.NewReader("99999999\nx"))
	if _, err := readFrame(r); err == nil {
		t.Error("readFrame: expected error for oversized frame, got nil")
	}
}

func TestParseRequest(t *testing.T) {
	cases := []struct {
		line     string
		wantVerb string
		wantArg  string
	}{
		{"ping", "ping", ""},
		{"PING\r\n", "ping", ""},
		{"key down", "key", "down"},
		{"text hello world", "text", "hello world"},
		{"text  leading", "text", " leading"},
		{"", "", ""},
	}
	for _, c := range cases {
		got := parseRequest(c.line)
		if got.verb != c.wantVerb || got.arg != c.wantArg {
			t.Errorf("parseRequest(%q) = {%q,%q}, want {%q,%q}",
				c.line, got.verb, got.arg, c.wantVerb, c.wantArg)
		}
	}
}
