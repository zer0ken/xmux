package attach

import (
	"errors"
	"os"
	"os/exec"
)

// InMux reports whether the process is running inside a mux, by checking $TMUX
// (psmux also sets TMUX for tmux-compat, so this one check covers both).
func InMux() bool {
	return os.Getenv("TMUX") != ""
}

// NestGuard returns a descriptive error when inMux is true, else nil. Attaching a
// mux from inside a mux is refused (psmux/tmux forbid nesting). The message tells
// the user to detach first (prefix d). It takes a bool param (the InMux() result)
// so it is testable without touching the environment.
func NestGuard(inMux bool) error {
	if inMux {
		return errors.New("already inside a mux session: detach first (prefix d), then run xmux")
	}
	return nil
}

// Execer hands the controlling terminal to a child process and waits.
type Execer interface {
	Exec(argv []string) error
}

// OSExecer runs argv[0] with argv[1:], wiring os.Stdin/Stdout/Stderr, via exec
// (same code on Windows and unix — it just hands over the terminal and waits).
type OSExecer struct{}

// Exec runs argv[0] with argv[1:], wiring the standard streams, and waits.
func (OSExecer) Exec(argv []string) error {
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

// RunAttach runs the given argv through the Execer. It returns an error for empty
// argv without calling the Execer.
func RunAttach(e Execer, argv []string) error {
	if len(argv) == 0 {
		return errors.New("attach: empty argv")
	}
	return e.Exec(argv)
}
