// Package source abstracts a mux server reachable from this machine: the local
// mux, or a remote one over ssh. It owns the machine boundary — argv assembly,
// ssh transport with connect-timeout and injection-safe quoting, and the
// reachable-but-empty vs unreachable distinction — so the layers above speak in
// sessions, not transports.
package source

import (
	"context"
	"path/filepath"

	"github.com/zer0ken/xmux/internal/config"
	"github.com/zer0ken/xmux/internal/mux"
	"github.com/zer0ken/xmux/internal/session"
)

// connectTimeout bounds the ssh TCP connect; the per-source scan timeout must
// exceed it so a slow-but-alive remote is not cancelled mid-connect.
const connectTimeout = "5"

// Source is one mux server. Remote sources run their mux over ssh.
type Source struct {
	Alias       string // "local" or an ssh-config alias
	Binary      string // mux binary name on that machine
	Remote      bool
	ControlPath string // ssh ControlMaster socket (non-windows remotes)
	GOOS        string // platform of the machine running xmux (gates ControlMaster)
	Runner      Runner // injectable; nil ⇒ the real exec runner
}

func (s Source) runner() Runner {
	if s.Runner != nil {
		return s.Runner
	}
	return execRunner{}
}

// sshArgs builds the ssh options preceding the remote command, ending with
// `-- <alias>` so an alias beginning with "-" is treated as the destination,
// never an option.
func (s Source) sshArgs(tty bool) []string {
	var a []string
	if tty {
		a = append(a, "-t") // request a pty; omit BatchMode so auth can prompt
	} else {
		a = append(a, "-o", "BatchMode=yes") // listing must never hang on a prompt
	}
	a = append(a, "-o", "ConnectTimeout="+connectTimeout)
	if s.GOOS != "windows" && s.ControlPath != "" {
		// Windows OpenSSH lacks ControlMaster; only multiplex elsewhere.
		a = append(a, "-o", "ControlMaster=auto", "-o", "ControlPath="+s.ControlPath, "-o", "ControlPersist=60s")
	}
	a = append(a, "--", s.Alias)
	return a
}

// execArgv turns a full mux argv (argv[0] = the mux binary) into the executable
// name and args to run: local runs the mux directly; remote wraps it in ssh.
func (s Source) execArgv(tty bool, muxArgv []string) (string, []string) {
	if !s.Remote {
		return muxArgv[0], muxArgv[1:]
	}
	return "ssh", append(s.sshArgs(tty), remoteCommand(muxArgv))
}

// AttachCommand is the argv that hands the terminal over to attach this source's
// named session (over ssh -t for a remote).
func (s Source) AttachCommand(name string) []string {
	n, a := s.execArgv(true, mux.Attach(s.Binary, name))
	return append([]string{n}, a...)
}

// Run executes a non-interactive mux command and returns its stdout.
func (s Source) Run(ctx context.Context, muxArgv []string) ([]byte, error) {
	name, args := s.execArgv(false, muxArgv)
	return s.runner().Run(ctx, name, args...)
}

// ListSessions returns the source's sessions. A reachable mux with no sessions
// returns (nil, nil); an unreachable source returns a non-nil error.
func (s Source) ListSessions(ctx context.Context) ([]session.Session, error) {
	out, err := s.Run(ctx, mux.ListSessions(s.Binary))
	if err != nil {
		if isNoSessions(err) {
			return nil, nil
		}
		return nil, err
	}
	return mux.ParseSessions(s.Alias, string(out)), nil
}

// Build assembles the source list for a config: local first, then each ssh host
// (ssh-config aliases merged with config overrides) in order.
func Build(cfg config.Config, sshAliases []string, goos, xmuxDir string) []Source {
	srcs := []Source{{
		Alias:  session.LocalSource,
		Binary: cfg.LocalBin(goos),
		Remote: false,
		GOOS:   goos,
	}}
	for _, spec := range cfg.HostSpecs(sshAliases) {
		srcs = append(srcs, Source{
			Alias:       spec.Alias,
			Binary:      spec.Bin,
			Remote:      true,
			GOOS:        goos,
			ControlPath: filepath.Join(xmuxDir, "cm-"+spec.Alias+".sock"),
		})
	}
	return srcs
}
