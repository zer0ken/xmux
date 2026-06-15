package attach

import (
	"errors"
	"reflect"
	"runtime"
	"strings"
	"testing"
)

func TestInMux(t *testing.T) {
	t.Setenv("TMUX", "/tmp/tmux-1000/default,1234,0")
	if !InMux() {
		t.Errorf("InMux() = false, want true when $TMUX is set")
	}

	t.Setenv("TMUX", "")
	if InMux() {
		t.Errorf("InMux() = true, want false when $TMUX is empty")
	}
}

func TestNestGuardOutside(t *testing.T) {
	if err := NestGuard(false); err != nil {
		t.Errorf("NestGuard(false) = %v, want nil", err)
	}
}

func TestNestGuardInside(t *testing.T) {
	err := NestGuard(true)
	if err == nil {
		t.Fatalf("NestGuard(true) = nil, want error")
	}
	if !strings.Contains(strings.ToLower(err.Error()), "detach") {
		t.Errorf("NestGuard(true) message = %q, want it to mention detaching", err.Error())
	}
}

// fakeExecer records the argv it was handed and returns a canned error.
type fakeExecer struct {
	got    []string
	called bool
	err    error
}

func (f *fakeExecer) Exec(argv []string) error {
	f.called = true
	f.got = argv
	return f.err
}

func TestRunAttachPassesArgvAndError(t *testing.T) {
	sentinel := errors.New("boom")
	f := &fakeExecer{err: sentinel}
	argv := []string{"tmux", "attach", "-t", "dev"}

	err := RunAttach(f, argv)
	if !errors.Is(err, sentinel) {
		t.Errorf("RunAttach error = %v, want %v", err, sentinel)
	}
	if !reflect.DeepEqual(f.got, argv) {
		t.Errorf("Execer got argv %#v, want %#v", f.got, argv)
	}
}

func TestRunAttachEmptyArgv(t *testing.T) {
	f := &fakeExecer{}
	if err := RunAttach(f, nil); err == nil {
		t.Errorf("RunAttach(nil) = nil, want error")
	}
	if f.called {
		t.Errorf("Execer was called for empty argv, want it NOT called")
	}
}

func TestOSExecerRunsHarmlessCommand(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("harmless-command check uses a Windows builtin")
	}
	if err := (OSExecer{}).Exec([]string{"cmd", "/c", "exit", "0"}); err != nil {
		t.Errorf("OSExecer.Exec(cmd /c exit 0) = %v, want nil", err)
	}
}
