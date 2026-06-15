package source

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"

	"github.com/zer0ken/xmux/internal/config"
)

// fakeRunner records the last command and returns canned results.
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

func TestQuoteNeutralizesShellMetachars(t *testing.T) {
	cases := map[string]string{
		"plain":          "plain",
		"with space":     "'with space'",
		"":               "''",
		"a/b-c_d.e":      "a/b-c_d.e", // safe set passes through
		"$(rm -rf /)":    "'$(rm -rf /)'",
		"a';rm -rf /;'b": `'a'\'';rm -rf /;'\''b'`,
		"`whoami`":       "'`whoami`'",
	}
	for in, want := range cases {
		if got := quote(in); got != want {
			t.Errorf("quote(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestRemoteCommandJoinsQuoted(t *testing.T) {
	got := remoteCommand([]string{"tmux", "rename-session", "-t", "old", "evil; rm -rf /"})
	want := "tmux rename-session -t old 'evil; rm -rf /'"
	if got != want {
		t.Fatalf("remoteCommand = %q, want %q", got, want)
	}
}

func TestSSHArgsNonInteractive(t *testing.T) {
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, GOOS: "linux", ControlPath: "/tmp/cm.sock"}
	a := s.sshArgs(false)
	joined := strings.Join(a, " ")
	if !strings.Contains(joined, "BatchMode=yes") {
		t.Errorf("non-interactive must use BatchMode=yes: %v", a)
	}
	if !strings.Contains(joined, "ConnectTimeout=5") {
		t.Errorf("must set ConnectTimeout=5: %v", a)
	}
	if !strings.Contains(joined, "ControlMaster=auto") {
		t.Errorf("non-windows must use ControlMaster: %v", a)
	}
	// `--` must immediately precede the host so an alias starting with "-" is a destination, not an option.
	if a[len(a)-2] != "--" || a[len(a)-1] != "prod" {
		t.Errorf("args must end with `-- prod`: %v", a)
	}
}

func TestSSHArgsInteractiveRequestsTTY(t *testing.T) {
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, GOOS: "linux"}
	a := s.sshArgs(true)
	joined := strings.Join(a, " ")
	if !strings.Contains(joined, "-t") {
		t.Errorf("interactive must request a tty (-t): %v", a)
	}
	if strings.Contains(joined, "BatchMode") {
		t.Errorf("interactive must NOT set BatchMode (auth prompts must work): %v", a)
	}
}

func TestSSHArgsWindowsOmitsControlMaster(t *testing.T) {
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, GOOS: "windows", ControlPath: "/tmp/cm.sock"}
	a := s.sshArgs(false)
	if strings.Contains(strings.Join(a, " "), "ControlMaster") {
		t.Errorf("windows OpenSSH lacks ControlMaster; must be omitted: %v", a)
	}
}

func TestExecArgvLocal(t *testing.T) {
	s := Source{Alias: "local", Binary: "psmux", Remote: false}
	name, args := s.execArgv(false, []string{"psmux", "list-sessions", "-F", "x"})
	if name != "psmux" || !reflect.DeepEqual(args, []string{"list-sessions", "-F", "x"}) {
		t.Fatalf("local execArgv = %q %v", name, args)
	}
}

func TestExecArgvRemote(t *testing.T) {
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, GOOS: "linux"}
	name, args := s.execArgv(false, []string{"tmux", "kill-session", "-t", "x"})
	if name != "ssh" {
		t.Fatalf("remote name = %q, want ssh", name)
	}
	last := args[len(args)-1]
	if last != "tmux kill-session -t x" {
		t.Fatalf("remote command arg = %q", last)
	}
}

func TestAttachCommandLocalAndRemote(t *testing.T) {
	loc := Source{Alias: "local", Binary: "psmux", Remote: false}
	if got := loc.AttachCommand("dev"); !reflect.DeepEqual(got, []string{"psmux", "attach", "-t", "dev"}) {
		t.Errorf("local AttachCommand = %v", got)
	}
	rem := Source{Alias: "prod", Binary: "tmux", Remote: true, GOOS: "linux"}
	got := rem.AttachCommand("api")
	if got[0] != "ssh" || got[len(got)-1] != "tmux attach -t api" {
		t.Errorf("remote AttachCommand = %v", got)
	}
	if !contains(got, "-t") {
		t.Errorf("remote attach must request a tty: %v", got)
	}
}

func TestListSessionsParsesOutput(t *testing.T) {
	fr := &fakeRunner{out: []byte("3\t1\t1781246739\teditor\n1\t0\t\tbuild\n")}
	s := Source{Alias: "local", Binary: "psmux", Runner: fr}
	got, err := s.ListSessions(context.Background())
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if len(got) != 2 || got[0].Name != "editor" || got[0].Windows != 3 || !got[0].Attached || got[0].Source != "local" {
		t.Fatalf("parsed sessions = %+v", got)
	}
	if got[1].LastAttached != 0 {
		t.Errorf("empty last_attached should parse to 0, got %d", got[1].LastAttached)
	}
}

func TestListSessionsBenignNoServerIsEmptyNotError(t *testing.T) {
	fr := &fakeRunner{err: &ExitErr{Stderr: "no server running on /tmp/tmux-1000/default"}}
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, Runner: fr}
	got, err := s.ListSessions(context.Background())
	if err != nil {
		t.Fatalf("a reachable mux with no sessions must be empty, not an error: %v", err)
	}
	if got != nil {
		t.Fatalf("want nil sessions, got %+v", got)
	}
}

func TestListSessionsUnreachableIsError(t *testing.T) {
	fr := &fakeRunner{err: errors.New("ssh: connect to host prod port 22: Connection timed out")}
	s := Source{Alias: "prod", Binary: "tmux", Remote: true, Runner: fr}
	if _, err := s.ListSessions(context.Background()); err == nil {
		t.Fatal("an unreachable host must surface an error")
	}
}

func TestIsNoSessions(t *testing.T) {
	if !isNoSessions(&ExitErr{Stderr: "no server running"}) {
		t.Error("'no server running' is benign")
	}
	if !isNoSessions(&ExitErr{Stderr: "error: no sessions"}) {
		t.Error("'no sessions' is benign")
	}
	if isNoSessions(&ExitErr{Stderr: "permission denied"}) {
		t.Error("'permission denied' is NOT benign")
	}
	if isNoSessions(errors.New("exec: \"tmux\": executable file not found")) {
		t.Error("a non-ExitErr (missing binary / connect failure) is NOT benign")
	}
}

func TestMuxCleanEnvStripsMuxVars(t *testing.T) {
	in := []string{"PATH=/bin", "TMUX=/x,1,0", "TMUX_PANE=%1", "PSMUX_SESSION=dev", "HOME=/h"}
	out := muxCleanEnv(in)
	for _, e := range out {
		if strings.HasPrefix(e, "TMUX") || strings.HasPrefix(e, "PSMUX") {
			t.Errorf("mux var leaked: %q", e)
		}
	}
	if !contains(out, "PATH=/bin") || !contains(out, "HOME=/h") {
		t.Errorf("non-mux vars must survive: %v", out)
	}
}

func TestBuildPutsLocalFirst(t *testing.T) {
	cfg := config.Config{}
	srcs := Build(cfg, []string{"prod", "db"}, "linux", "/home/u/.xmux")
	if len(srcs) != 3 {
		t.Fatalf("want local + 2 remotes, got %d", len(srcs))
	}
	if srcs[0].Alias != "local" || srcs[0].Remote {
		t.Errorf("source[0] must be local: %+v", srcs[0])
	}
	if srcs[1].Alias != "prod" || !srcs[1].Remote || srcs[1].Binary != "tmux" {
		t.Errorf("source[1] must be remote prod/tmux: %+v", srcs[1])
	}
}

func contains(ss []string, want string) bool {
	for _, s := range ss {
		if s == want {
			return true
		}
	}
	return false
}
