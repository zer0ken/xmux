package manage

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"github.com/zer0ken/xmux/internal/mux"
	"github.com/zer0ken/xmux/internal/source"
)

// fakeRunner records the command it was asked to run and returns canned results.
// For a LOCAL source it receives name=Binary and args=the mux argv WITHOUT the
// leading binary.
type fakeRunner struct {
	name string
	args []string
	out  []byte
	err  error
}

func (f *fakeRunner) Run(_ context.Context, name string, args ...string) ([]byte, error) {
	f.name = name
	f.args = args
	return f.out, f.err
}

func localSource(fr *fakeRunner) source.Source {
	return source.Source{Alias: "local", Binary: "psmux", Runner: fr}
}

func TestCreateNamedTrimsAndTargets(t *testing.T) {
	fr := &fakeRunner{out: []byte("myname\n")}
	got, err := Create(context.Background(), localSource(fr), "x")
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if got != "myname" {
		t.Errorf("Create = %q, want %q (trimmed)", got, "myname")
	}
	if fr.name != "psmux" {
		t.Errorf("runner name = %q, want %q", fr.name, "psmux")
	}
	want := []string{"new-session", "-A", "-d", "-P", "-F", "#{session_name}", "-s", "x"}
	if !reflect.DeepEqual(fr.args, want) {
		t.Errorf("runner args = %#v, want %#v", fr.args, want)
	}
}

func TestCreateAutoNameOmitsTarget(t *testing.T) {
	fr := &fakeRunner{out: []byte("0\n")}
	got, err := Create(context.Background(), localSource(fr), "")
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if got != "0" {
		t.Errorf("Create = %q, want %q", got, "0")
	}
	for _, a := range fr.args {
		if a == "-s" {
			t.Errorf("auto-name must NOT pass -s: %#v", fr.args)
		}
	}
}

func TestCreateErrorReturnsEmpty(t *testing.T) {
	fr := &fakeRunner{out: []byte("ignored\n"), err: errors.New("boom")}
	got, err := Create(context.Background(), localSource(fr), "x")
	if err == nil {
		t.Fatal("want error, got nil")
	}
	if got != "" {
		t.Errorf("on error Create must return %q, got %q", "", got)
	}
}

func TestKillTargetsAndPropagatesError(t *testing.T) {
	fr := &fakeRunner{}
	if err := Kill(context.Background(), localSource(fr), "x"); err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	want := []string{"kill-session", "-t", "x"}
	if !reflect.DeepEqual(fr.args, want) {
		t.Errorf("runner args = %#v, want %#v", fr.args, want)
	}

	fe := &fakeRunner{err: errors.New("boom")}
	if err := Kill(context.Background(), localSource(fe), "x"); err == nil {
		t.Fatal("Kill must propagate the runner error")
	}
}

func TestRenameTargets(t *testing.T) {
	fr := &fakeRunner{}
	if err := Rename(context.Background(), localSource(fr), "old", "new"); err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	want := []string{"rename-session", "-t", "old", "new"}
	if !reflect.DeepEqual(fr.args, want) {
		t.Errorf("runner args = %#v, want %#v", fr.args, want)
	}
}

func TestPanesParsesAndTargets(t *testing.T) {
	fr := &fakeRunner{out: []byte("1\t1\t1\t1\tbash\tshell\n2\t0\t1\t1\ttail\tlogs\n")}
	got, err := Panes(context.Background(), localSource(fr), "x")
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("want 2 windows, got %d (%+v)", len(got), got)
	}
	if got[0].Index != 1 || got[0].Name != "shell" || !got[0].Active || len(got[0].Panes) != 1 || got[0].Panes[0].Command != "bash" {
		t.Errorf("window[0] = %+v", got[0])
	}
	if got[1].Index != 2 || got[1].Name != "logs" || got[1].Active || len(got[1].Panes) != 1 || got[1].Panes[0].Command != "tail" {
		t.Errorf("window[1] = %+v", got[1])
	}
	want := []string{"list-panes", "-s", "-t", "x", "-F", mux.PaneFormat}
	if !reflect.DeepEqual(fr.args, want) {
		t.Errorf("runner args = %#v, want %#v", fr.args, want)
	}
}

func TestPanesErrorReturnsNil(t *testing.T) {
	fr := &fakeRunner{err: errors.New("boom")}
	got, err := Panes(context.Background(), localSource(fr), "x")
	if err == nil {
		t.Fatal("want error, got nil")
	}
	if got != nil {
		t.Errorf("on error Panes must return nil, got %+v", got)
	}
}
