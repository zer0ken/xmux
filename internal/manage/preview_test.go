package manage

import (
	"context"
	"reflect"
	"testing"

	"github.com/zer0ken/xmux/internal/source"
)

type captureRunner struct {
	name string
	args []string
	out  []byte
	err  error
}

func (r *captureRunner) Run(_ context.Context, name string, args ...string) ([]byte, error) {
	r.name = name
	r.args = args
	return r.out, r.err
}

func TestCapture(t *testing.T) {
	r := &captureRunner{out: []byte("$ npm run dev\nReady\n")}
	s := source.Source{Alias: "local", Binary: "psmux", Runner: r}
	got, err := Capture(context.Background(), s, "editor:1.0")
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	if got != "$ npm run dev\nReady\n" {
		t.Errorf("Capture text = %q", got)
	}
	if !reflect.DeepEqual(r.args, []string{"capture-pane", "-p", "-e", "-t", "editor:1.0"}) {
		t.Errorf("Capture argv = %v", r.args)
	}
}

func TestSelectWindow(t *testing.T) {
	r := &captureRunner{}
	s := source.Source{Alias: "local", Binary: "psmux", Runner: r}
	if err := SelectWindow(context.Background(), s, "editor", 3); err != nil {
		t.Fatalf("err: %v", err)
	}
	if !reflect.DeepEqual(r.args, []string{"select-window", "-t", "editor:3"}) {
		t.Errorf("SelectWindow argv = %v", r.args)
	}
}

func TestSelectPane(t *testing.T) {
	r := &captureRunner{}
	s := source.Source{Alias: "local", Binary: "psmux", Runner: r}
	if err := SelectPane(context.Background(), s, "editor", 3, 2); err != nil {
		t.Fatalf("err: %v", err)
	}
	if !reflect.DeepEqual(r.args, []string{"select-pane", "-t", "editor:3.2"}) {
		t.Errorf("SelectPane argv = %v", r.args)
	}
}
