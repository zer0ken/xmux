package main

import (
	"fmt"
	"os"

	"github.com/rivo/tview"
	"github.com/zer0ken/xmux/internal/attach"
	"github.com/zer0ken/xmux/internal/control"
	"github.com/zer0ken/xmux/internal/manage"
	"github.com/zer0ken/xmux/internal/session"
	"github.com/zer0ken/xmux/internal/ui"
)

// controlHook exposes the running switcher on a per-process unix socket so it
// can be driven and observed headlessly. Off when XMUX_CONTROL=0; a failure to
// open it never breaks the UI.
func (e Env) controlHook() ui.Control {
	if os.Getenv("XMUX_CONTROL") == "0" {
		return nil
	}
	return func(app *tview.Application, root tview.Primitive, render func() string) func() {
		if err := os.MkdirAll(e.xmuxDir, 0o700); err != nil {
			return func() {}
		}
		srv, err := control.Serve(app, render, control.SocketPath(e.xmuxDir, os.Getpid()))
		if err != nil {
			return func() {}
		}
		return func() { _ = srv.Close() }
	}
}

// runHome is the full-screen switcher. Pick a session → attach; on detach,
// control returns here, the tree is re-scanned and re-rendered (detach-to-home).
// Loops until the user quits.
func runHome(e Env) int {
	if attach.InMux() {
		fmt.Fprintln(os.Stderr, "xmux: warning — inside a mux; attach is refused here. Detach first (prefix d), or bind `xmux popup`.")
	}
	for {
		fmt.Fprintln(os.Stderr, "xmux: scanning sessions… (probing local + ssh hosts)")
		res, err := ui.RunSwitcher(e.deepScan(), e.ops(), e.controlHook())
		if err != nil {
			fmt.Fprintln(os.Stderr, "xmux:", err)
			return 1
		}
		if res.Chosen == nil {
			return 0
		}
		s := *res.Chosen
		src, ok := e.byAlias[s.Source]
		if !ok {
			fmt.Fprintf(os.Stderr, "xmux: unknown source %q\n", s.Source)
			continue
		}
		if err := attach.NestGuard(attach.InMux()); err != nil {
			fmt.Fprintln(os.Stderr, "xmux:", err)
			continue
		}
		if res.Window >= 0 {
			// land on the chosen window (best-effort; an attach still proceeds)
			_ = manage.SelectWindow(e.ctx, src, s.Name, res.Window)
		}
		if err := attach.RunAttach(attach.OSExecer{}, src.AttachCommand(s.Name)); err != nil {
			fmt.Fprintln(os.Stderr, "xmux: attach failed:", err)
		}
	}
}

// runPopup is the in-mux switcher (bound via `display-popup -E "xmux popup"`).
// Same-server pick teleports (switch-client); cross-server detaches to the home
// loop. Exits after one action so the popup closes back onto the pane.
func runPopup(e Env) int {
	fmt.Fprintln(os.Stderr, "xmux: scanning sessions… (probing local + ssh hosts)")
	res, err := ui.RunSwitcher(e.deepScan(), e.ops(), e.controlHook())
	if err != nil {
		fmt.Fprintln(os.Stderr, "xmux:", err)
		return 1
	}
	if res.Chosen == nil {
		return 0
	}
	s := *res.Chosen
	plan := attach.PlanSwitch(session.LocalSource, e.localBin, s)
	if res.Window >= 0 && plan.Teleport {
		// same-server teleport: pre-select the window so switch-client lands on it
		if src, ok := e.byAlias[s.Source]; ok {
			_ = manage.SelectWindow(e.ctx, src, s.Name, res.Window)
		}
	}
	if err := attach.RunAttach(attach.OSExecer{}, plan.Argv); err != nil {
		fmt.Fprintln(os.Stderr, "xmux:", err)
		return 1
	}
	return 0
}
