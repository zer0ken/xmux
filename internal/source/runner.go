package source

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"strings"
)

// Runner runs an external command and returns its stdout. It is an interface so
// the source layer is testable without spawning processes.
type Runner interface {
	Run(ctx context.Context, name string, args ...string) ([]byte, error)
}

// ExitErr carries a failed command's stderr so the no-sessions classifier can
// read it. Only a real non-zero exit produces one; a missing binary or a
// connection failure surfaces as a plain error (never benign).
type ExitErr struct {
	Stderr string
	Code   int // process exit code; 126/127/255 are never a healthy-but-empty mux
	Err    error
}

func (e *ExitErr) Error() string {
	if e.Err != nil {
		return e.Err.Error()
	}
	return "command failed: " + e.Stderr
}

func (e *ExitErr) Unwrap() error { return e.Err }

type execRunner struct{}

func (execRunner) Run(ctx context.Context, name string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	// Strip mux env so a local command run from inside a mux (e.g. the popup)
	// is not refused as nesting. cmd.Output() leaves stderr in *exec.ExitError.
	cmd.Env = muxCleanEnv(os.Environ())
	out, err := cmd.Output()
	if err != nil {
		var xe *exec.ExitError
		if errors.As(err, &xe) {
			return out, &ExitErr{Stderr: string(xe.Stderr), Code: xe.ExitCode(), Err: err}
		}
		return out, err
	}
	return out, nil
}

// muxCleanEnv returns env with every TMUX*/PSMUX* variable removed.
func muxCleanEnv(env []string) []string {
	var out []string
	for _, e := range env {
		if strings.HasPrefix(e, "TMUX") || strings.HasPrefix(e, "PSMUX") {
			continue
		}
		out = append(out, e)
	}
	return out
}
