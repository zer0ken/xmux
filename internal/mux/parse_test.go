package mux

import (
	"reflect"
	"testing"

	"github.com/zer0ken/xmux/internal/session"
)

func TestParseSessionsBasic(t *testing.T) {
	out := "3\t1\t1700000000\tmain\n2\t0\t1699999999\tother\n"
	got := ParseSessions("local", out)
	want := []session.Session{
		{Source: "local", Name: "main", Windows: 3, Attached: true, LastAttached: 1700000000},
		{Source: "local", Name: "other", Windows: 2, Attached: false, LastAttached: 1699999999},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParseSessions basic = %#v, want %#v", got, want)
	}
}

func TestParseSessionsCRLF(t *testing.T) {
	out := "1\t1\t100\ta\r\n1\t0\t200\tb\r\n"
	got := ParseSessions("local", out)
	if len(got) != 2 {
		t.Fatalf("got %d sessions, want 2: %#v", len(got), got)
	}
	if got[0].Name != "a" || got[1].Name != "b" {
		t.Errorf("CRLF names = %q, %q; want a, b", got[0].Name, got[1].Name)
	}
}

func TestParseSessionsNameWithTabAndSlash(t *testing.T) {
	// A name containing a literal tab and a "/" must survive verbatim.
	out := "4\t1\t1700000000\tproj/a\tb\n"
	got := ParseSessions("ssh-host", out)
	want := []session.Session{
		{Source: "ssh-host", Name: "proj/a\tb", Windows: 4, Attached: true, LastAttached: 1700000000},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParseSessions tab+slash name = %#v, want %#v", got, want)
	}
}

func TestParseSessionsEmptyLastAttached(t *testing.T) {
	// Older muxes leave session_last_attached empty -> 0.
	out := "1\t0\t\tlegacy\n"
	got := ParseSessions("local", out)
	want := []session.Session{
		{Source: "local", Name: "legacy", Windows: 1, Attached: false, LastAttached: 0},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParseSessions empty last_attached = %#v, want %#v", got, want)
	}
}

func TestParseSessionsSkipsGarbage(t *testing.T) {
	// A banner line (<4 fields), a non-numeric windows line, a non-numeric
	// last_attached line, an empty name line, and blank lines are all skipped.
	out := "" +
		"some random banner text\n" +
		"\n" +
		"x\t1\t100\tbadwin\n" + // non-numeric windows -> skip
		"1\tnope\t100\tbadattach\n" + // non-numeric attached -> skip
		"1\t1\tabc\tbadtime\n" + // non-numeric non-empty last_attached -> skip
		"1\t1\t100\t\n" + // empty name -> skip
		"2\t1\t300\tgood\n"
	got := ParseSessions("local", out)
	want := []session.Session{
		{Source: "local", Name: "good", Windows: 2, Attached: true, LastAttached: 300},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParseSessions garbage = %#v, want %#v", got, want)
	}
}

func TestParseSessionsEmptyOutput(t *testing.T) {
	if got := ParseSessions("local", ""); len(got) != 0 {
		t.Errorf("ParseSessions empty = %#v, want empty", got)
	}
}

func TestParseSessionsOrderPreserved(t *testing.T) {
	out := "1\t0\t1\tz\n1\t0\t2\ta\n1\t0\t3\tm\n"
	got := ParseSessions("local", out)
	names := []string{got[0].Name, got[1].Name, got[2].Name}
	want := []string{"z", "a", "m"}
	if !reflect.DeepEqual(names, want) {
		t.Errorf("ParseSessions order = %v, want %v", names, want)
	}
}

func TestParsePanesBasic(t *testing.T) {
	// Two windows; window 0 has two panes, window 1 has one.
	out := "" +
		"0\t1\t0\t1\tbash\teditor\n" +
		"0\t1\t1\t0\tvim\teditor\n" +
		"1\t0\t0\t1\tssh\tserver\n"
	got := ParsePanes(out)
	want := []session.WindowPanes{
		{Index: 0, Name: "editor", Active: true, Panes: []session.Pane{
			{Index: 0, Active: true, Command: "bash"},
			{Index: 1, Active: false, Command: "vim"},
		}},
		{Index: 1, Name: "server", Active: false, Panes: []session.Pane{
			{Index: 0, Active: true, Command: "ssh"},
		}},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParsePanes basic = %#v, want %#v", got, want)
	}
}

func TestParsePanesWindowNameWithSpacesAndTab(t *testing.T) {
	out := "2\t1\t0\t1\tzsh\tmy window\tname\n"
	got := ParsePanes(out)
	want := []session.WindowPanes{
		{Index: 2, Name: "my window\tname", Active: true, Panes: []session.Pane{
			{Index: 0, Active: true, Command: "zsh"},
		}},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParsePanes window name = %#v, want %#v", got, want)
	}
}

func TestParsePanesGroupingOrderPreserved(t *testing.T) {
	// Windows appear in first-seen order even when interleaved.
	out := "" +
		"5\t1\t0\t1\ta\tfive\n" +
		"2\t0\t0\t1\tb\ttwo\n" +
		"5\t1\t1\t0\tc\tfive\n" +
		"2\t0\t1\t0\td\ttwo\n"
	got := ParsePanes(out)
	if len(got) != 2 {
		t.Fatalf("got %d windows, want 2: %#v", len(got), got)
	}
	if got[0].Index != 5 || got[1].Index != 2 {
		t.Errorf("window order = %d, %d; want 5, 2", got[0].Index, got[1].Index)
	}
	if len(got[0].Panes) != 2 || len(got[1].Panes) != 2 {
		t.Fatalf("pane counts = %d, %d; want 2, 2", len(got[0].Panes), len(got[1].Panes))
	}
	if got[0].Panes[0].Command != "a" || got[0].Panes[1].Command != "c" {
		t.Errorf("window 5 pane commands = %q, %q; want a, c", got[0].Panes[0].Command, got[0].Panes[1].Command)
	}
}

func TestParsePanesSkipsShortLines(t *testing.T) {
	out := "" +
		"short line\n" +
		"0\t1\t0\n" + // too few fields -> skip
		"\n" +
		"x\t1\t0\t1\tbash\twin\n" + // non-numeric window_index -> skip
		"0\t1\t0\t1\tbash\twin\n"
	got := ParsePanes(out)
	want := []session.WindowPanes{
		{Index: 0, Name: "win", Active: true, Panes: []session.Pane{
			{Index: 0, Active: true, Command: "bash"},
		}},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("ParsePanes short lines = %#v, want %#v", got, want)
	}
}

func TestParsePanesEmptyOutput(t *testing.T) {
	if got := ParsePanes(""); len(got) != 0 {
		t.Errorf("ParsePanes empty = %#v, want empty", got)
	}
}
