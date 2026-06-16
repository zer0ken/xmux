package control

import (
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
)

func TestSocketPath(t *testing.T) {
	got := SocketPath("/some/dir", 1234)
	want := filepath.Join("/some/dir", "ctl-1234.sock")
	if got != want {
		t.Errorf("SocketPath = %q, want %q", got, want)
	}
}

func TestDiscover(t *testing.T) {
	dir := t.TempDir()
	if _, err := Discover(dir); err == nil {
		t.Error("Discover on empty dir: expected error, got nil")
	}

	older := SocketPath(dir, 100)
	newer := SocketPath(dir, 200)
	if err := os.WriteFile(older, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	// Ensure a distinct mtime ordering, then make newer the most recent.
	old := time.Now().Add(-time.Hour)
	if err := os.Chtimes(older, old, old); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(newer, nil, 0o600); err != nil {
		t.Fatal(err)
	}

	got, err := Discover(dir)
	if err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if got != newer {
		t.Errorf("Discover = %q, want %q (most recent)", got, newer)
	}
}

func TestDiscoverTieBreakHigherPid(t *testing.T) {
	dir := t.TempDir()
	a := SocketPath(dir, 100)
	b := SocketPath(dir, 200)
	for _, p := range []string{a, b} {
		if err := os.WriteFile(p, nil, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	// Same mtime for both: tie-break must pick the higher pid.
	ts := time.Now()
	for _, p := range []string{a, b} {
		if err := os.Chtimes(p, ts, ts); err != nil {
			t.Fatal(err)
		}
	}
	got, err := Discover(dir)
	if err != nil {
		t.Fatal(err)
	}
	if got != b {
		t.Errorf("Discover tie-break = %q, want %q (higher pid)", got, b)
	}
}

func TestServerEndToEnd(t *testing.T) {
	dir := t.TempDir()
	sock := SocketPath(dir, os.Getpid())

	app := tview.NewApplication()
	root := tview.NewTextView()
	app.SetRoot(root, true).SetFocus(root)

	screen := tcell.NewSimulationScreen("UTF-8")
	if err := screen.Init(); err != nil {
		t.Fatal(err)
	}
	app.SetScreen(screen)

	var mu sync.Mutex
	var got []rune
	app.SetInputCapture(func(ev *tcell.EventKey) *tcell.EventKey {
		if ev.Key() == tcell.KeyRune {
			mu.Lock()
			got = append(got, ev.Rune())
			mu.Unlock()
		}
		return ev
	})

	runDone := make(chan struct{})
	go func() {
		_ = app.Run()
		close(runDone)
	}()
	t.Cleanup(func() {
		app.Stop()
		<-runDone
	})

	srv, err := Serve(app, func() string { return "SCREEN-DUMP" }, sock)
	if err != nil {
		t.Fatalf("Serve: %v", err)
	}

	// Wait for the run loop and server to be live by retrying Dial+ping.
	var client *Client
	deadline := time.Now().Add(5 * time.Second)
	for {
		c, derr := Dial(sock)
		if derr == nil {
			if resp, perr := c.Do("ping"); perr == nil && resp == "pong" {
				client = c
				break
			}
			_ = c.Close()
		}
		if time.Now().After(deadline) {
			t.Fatalf("server never became ready (last dial err: %v)", derr)
		}
		time.Sleep(20 * time.Millisecond)
	}
	defer client.Close()

	if resp, err := client.Do("ping"); err != nil || resp != "pong" {
		t.Fatalf("ping = %q, %v; want pong", resp, err)
	}

	if resp, err := client.Do("dump"); err != nil || resp != "SCREEN-DUMP" {
		t.Fatalf("dump = %q, %v; want SCREEN-DUMP", resp, err)
	}

	if resp, err := client.Do("key a"); err != nil || resp != "ok" {
		t.Fatalf("key a = %q, %v; want ok", resp, err)
	}
	mu.Lock()
	gotA := string(got)
	mu.Unlock()
	if gotA != "a" {
		t.Fatalf("after 'key a', captured runes = %q, want %q", gotA, "a")
	}

	if resp, err := client.Do("text hi"); err != nil || resp != "ok" {
		t.Fatalf("text hi = %q, %v; want ok", resp, err)
	}
	mu.Lock()
	gotAll := string(got)
	mu.Unlock()
	if gotAll != "ahi" {
		t.Fatalf("after 'text hi', captured runes = %q, want %q (in order)", gotAll, "ahi")
	}

	if resp, err := client.Do("key fnord"); err != nil || resp != "err: unknown key" {
		t.Fatalf("key fnord = %q, %v; want 'err: unknown key'", resp, err)
	}
	if resp, err := client.Do("bogus"); err != nil || resp != "err: unknown command" {
		t.Fatalf("bogus = %q, %v; want 'err: unknown command'", resp, err)
	}

	// Stop the app and close the server; the socket file must disappear and a
	// second Close must be safe.
	app.Stop()
	<-runDone
	if err := srv.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, err := os.Stat(sock); !os.IsNotExist(err) {
		t.Errorf("socket still present after Close: stat err = %v", err)
	}
	if err := srv.Close(); err != nil {
		t.Errorf("second Close: %v", err)
	}
}
