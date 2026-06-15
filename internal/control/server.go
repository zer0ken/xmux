package control

import (
	"bufio"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
)

// opTimeout bounds how long an injected operation waits on the UI main
// goroutine. If the run loop has stopped, QueueUpdate would block forever; this
// timeout converts that into a failed (false) op instead.
const opTimeout = 30 * time.Second

// SocketPath returns the control socket path for a given pid in dir.
func SocketPath(dir string, pid int) string {
	return filepath.Join(dir, fmt.Sprintf("ctl-%d.sock", pid))
}

// Discover returns the path of the most-recently-modified ctl-*.sock in dir.
// Ties on modification time are broken by the higher pid. It errors if none
// exist.
func Discover(dir string) (string, error) {
	matches, err := filepath.Glob(filepath.Join(dir, "ctl-*.sock"))
	if err != nil {
		return "", err
	}
	type cand struct {
		path string
		mod  time.Time
		pid  int
	}
	var cands []cand
	for _, p := range matches {
		info, err := os.Stat(p)
		if err != nil {
			continue
		}
		cands = append(cands, cand{path: p, mod: info.ModTime(), pid: pidFromSock(p)})
	}
	if len(cands) == 0 {
		return "", fmt.Errorf("control: no ctl-*.sock found in %s", dir)
	}
	sort.Slice(cands, func(i, j int) bool {
		if !cands[i].mod.Equal(cands[j].mod) {
			return cands[i].mod.After(cands[j].mod)
		}
		return cands[i].pid > cands[j].pid
	})
	return cands[0].path, nil
}

// pidFromSock extracts the pid embedded in a ctl-<pid>.sock filename, or -1.
func pidFromSock(path string) int {
	base := filepath.Base(path)
	base = strings.TrimSuffix(strings.TrimPrefix(base, "ctl-"), ".sock")
	if pid, err := strconv.Atoi(base); err == nil {
		return pid
	}
	return -1
}

// Server is the AF_UNIX control endpoint for a running tview application.
type Server struct {
	app      *tview.Application
	render   func() string
	listener net.Listener
	sockPath string

	closeOnce sync.Once
	closeErr  error
}

// Serve binds an AF_UNIX listener at sockPath and serves control requests
// against app. render produces the dump payload. A stale socket file at
// sockPath is removed first. The returned Server must be Closed.
func Serve(app *tview.Application, render func() string, sockPath string) (*Server, error) {
	// Remove any stale socket so the bind succeeds.
	if err := os.Remove(sockPath); err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		return nil, err
	}
	// Best-effort: restrict to the owning user.
	_ = os.Chmod(sockPath, 0o600)

	s := &Server{
		app:      app,
		render:   render,
		listener: ln,
		sockPath: sockPath,
	}
	go s.acceptLoop()
	return s, nil
}

// acceptLoop accepts connections until the listener is closed.
func (s *Server) acceptLoop() {
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			return // listener closed
		}
		go s.handleConn(conn)
	}
}

// handleConn serves newline-delimited requests on a single connection, replying
// to each with a length-framed payload. It returns on read error or EOF.
func (s *Server) handleConn(conn net.Conn) {
	defer conn.Close()
	r := bufio.NewReader(conn)
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		req := parseRequest(line)
		payload := s.dispatch(req)
		if err := writeFrame(conn, payload); err != nil {
			return
		}
	}
}

// dispatch executes one request and returns its reply payload.
func (s *Server) dispatch(req request) string {
	switch req.verb {
	case "ping":
		return "pong"
	case "dump":
		var out string
		s.onMain(func() { out = s.render() })
		return out
	case "key":
		ev, ok := parseKey(req.arg)
		if !ok {
			return "err: unknown key"
		}
		s.onMain(func() {
			deliver(s.app, ev)
			s.app.ForceDraw()
		})
		return "ok"
	case "text":
		for _, r := range req.arg {
			ev := tcell.NewEventKey(tcell.KeyRune, r, tcell.ModNone)
			s.onMain(func() {
				deliver(s.app, ev)
				s.app.ForceDraw()
			})
		}
		return "ok"
	default:
		return "err: unknown command"
	}
}

// onMain runs f on the tview main goroutine via QueueUpdate, whose blocking-FIFO
// delivery is the synchronization barrier (f fully executes before onMain
// returns). It bounds the wait by opTimeout so a stopped run loop cannot block
// forever; it returns false on timeout.
func (s *Server) onMain(f func()) bool {
	done := make(chan struct{})
	go func() {
		s.app.QueueUpdate(f)
		close(done)
	}()
	select {
	case <-done:
		return true
	case <-time.After(opTimeout):
		return false
	}
}

// deliver replays tview's own event path for a key event: the application input
// capture (if any) runs first and may swallow the event; otherwise the focused
// primitive's input handler receives it.
func deliver(app *tview.Application, ev *tcell.EventKey) {
	if capture := app.GetInputCapture(); capture != nil {
		ev = capture(ev)
		if ev == nil {
			return
		}
	}
	if f := app.GetFocus(); f != nil {
		if h := f.InputHandler(); h != nil {
			h(ev, func(p tview.Primitive) { app.SetFocus(p) })
		}
	}
}

// Close stops accepting connections, closes the listener, and removes the
// socket file. It is safe to call more than once.
func (s *Server) Close() error {
	s.closeOnce.Do(func() {
		err := s.listener.Close()
		if rmErr := os.Remove(s.sockPath); rmErr != nil && !os.IsNotExist(rmErr) {
			err = errors.Join(err, rmErr)
		}
		s.closeErr = err
	})
	return s.closeErr
}

// Client is a minimal control-channel client for the `xmux ctl` command.
type Client struct {
	conn net.Conn
	r    *bufio.Reader
}

// Dial connects to a control socket.
func Dial(sockPath string) (*Client, error) {
	conn, err := net.Dial("unix", sockPath)
	if err != nil {
		return nil, err
	}
	return &Client{conn: conn, r: bufio.NewReader(conn)}, nil
}

// Do sends one request line and returns the framed response payload.
func (c *Client) Do(line string) (string, error) {
	if _, err := c.conn.Write([]byte(line + "\n")); err != nil {
		return "", err
	}
	return readFrame(c.r)
}

// Close closes the client connection.
func (c *Client) Close() error {
	return c.conn.Close()
}
