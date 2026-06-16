package ui

import (
	"strings"
	"testing"
)

func TestANSIToTviewNoUnderlineBleed(t *testing.T) {
	// underline-off must close the underline so following text isn't underlined.
	out := ansiToTview("\x1b[4mUNDER\x1b[24mNORMAL")
	if !strings.Contains(out, "[-:-:u]UNDER") {
		t.Errorf("UNDER should be underlined: %q", out)
	}
	if !strings.Contains(out, "[-:-:-]NORMAL") {
		t.Errorf("NORMAL must clear the underline (no bleed): %q", out)
	}
}

func TestANSIToTviewReset(t *testing.T) {
	out := ansiToTview("\x1b[4mU\x1b[0m end")
	if !strings.Contains(out, "[-:-:-] end") {
		t.Errorf("reset must clear attributes: %q", out)
	}
}

func TestANSIToTviewTruecolorThenUnderline(t *testing.T) {
	// the trailing 4 (underline) after a 38;2 truecolor must NOT be dropped.
	out := ansiToTview("\x1b[38;2;255;0;0;4mX")
	if !strings.Contains(out, "[#ff0000:-:u]X") {
		t.Errorf("truecolor + trailing underline: %q", out)
	}
}

func TestANSIToTviewUnderlineColorIgnoredNotMisparsed(t *testing.T) {
	// 58;2;r;g;b (underline colour) must be consumed, not parsed as 2=dim.
	out := ansiToTview("\x1b[4;58;2;0;255;0mX")
	if !strings.Contains(out, "[-:-:u]X") {
		t.Errorf("underline colour must be ignored cleanly (no spurious dim): %q", out)
	}
}

func TestANSIToTview256AndBasic(t *testing.T) {
	// ANSI 31 is palette index 1, which tcell names "maroon" (the terminal's red).
	if out := ansiToTview("\x1b[31mR"); !strings.Contains(out, "[maroon:-:-]R") {
		t.Errorf("basic colour: %q", out)
	}
	if out := ansiToTview("\x1b[38;5;9mR"); !strings.Contains(out, "[red:-:-]R") {
		t.Errorf("256-colour 9 should map to red: %q", out)
	}
}

func TestANSIToTviewEscapesLiteralBrackets(t *testing.T) {
	// literal [tag]-looking text must not be eaten by tview's dynamic colours.
	out := ansiToTview("[INFO] hi")
	if !strings.Contains(out, "[INFO[]") { // tview.Escape form
		t.Errorf("literal brackets must be escaped: %q", out)
	}
}

func TestANSIToTviewDropsUnknownSequences(t *testing.T) {
	// an OSC hyperlink and a non-SGR CSI must be dropped, not leaked as garbage.
	out := ansiToTview("a\x1b]8;;http://x\x07b\x1b[2Kc")
	if strings.ContainsRune(out, 0x1b) {
		t.Errorf("no raw escape bytes may leak: %q", out)
	}
	for _, w := range []string{"a", "b", "c"} {
		if !strings.Contains(out, w) {
			t.Errorf("text %q lost: %q", w, out)
		}
	}
}
