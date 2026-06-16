package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"

	"github.com/zer0ken/xmux/internal/config"
	"github.com/zer0ken/xmux/internal/discovery"
	"github.com/zer0ken/xmux/internal/manage"
	"github.com/zer0ken/xmux/internal/session"
	"github.com/zer0ken/xmux/internal/source"
	"github.com/zer0ken/xmux/internal/ui"
)

const (
	scanConcurrency = 8
	scanTimeout     = 6 * time.Second // must exceed the ssh connect timeout (5s)
	detailTimeout   = 6 * time.Second
)

// Env is the resolved runtime: the source list and the lookups the commands
// share. It is built once per process from config + ssh-config.
type Env struct {
	cfg         config.Config
	cfgWarnings []string
	srcs        []source.Source
	byAlias     map[string]source.Source
	localBin    string
	xmuxDir     string
	ctx         context.Context
}

func homeDir() string {
	if h, err := os.UserHomeDir(); err == nil {
		return h
	}
	return "."
}

func configPath() string { return filepath.Join(homeDir(), ".config", "xmux", "config.toml") }
func sshConfigPath() string { return filepath.Join(homeDir(), ".ssh", "config") }
func xmuxDirPath() string   { return filepath.Join(homeDir(), ".xmux") }

// buildEnv loads config and assembles the sources. The returned error is the
// config-parse error (non-nil for a malformed config); the Env is still usable
// with defaults so `doctor` can report the problem instead of dying on it.
// Interactive commands treat the error as fatal.
func buildEnv(ctx context.Context) (Env, error) {
	cfg, warnings, cfgErr := config.LoadVerbose(configPath())
	goos := runtime.GOOS
	aliases := config.SSHHostAliases(sshConfigPath())
	xmuxDir := xmuxDirPath()
	srcs := source.Build(cfg, aliases, goos, xmuxDir)
	byAlias := make(map[string]source.Source, len(srcs))
	for _, s := range srcs {
		byAlias[s.Alias] = s
	}
	return Env{
		cfg:         cfg,
		cfgWarnings: warnings,
		srcs:        srcs,
		byAlias:     byAlias,
		localBin:    cfg.LocalBin(goos),
		xmuxDir:     xmuxDir,
		ctx:         ctx,
	}, cfgErr
}

// scan probes every source and returns the merged, recency-sorted host/session
// groups (used by `ls`, which needs no window/pane detail).
func (e Env) scan() []ui.Group {
	results := discovery.ScanAll(e.ctx, e.srcs, scanTimeout, scanConcurrency)
	return toGroups(results)
}

// deepScan is the interactive snapshot: the groups plus every reachable session's
// windows-and-panes, fetched concurrently (the always-expanded tree shows them
// all, so they are loaded up front rather than on expand).
func (e Env) deepScan() ui.Scan {
	groups := e.scan()
	panes := map[string][]session.WindowPanes{}
	var mu sync.Mutex
	var wg sync.WaitGroup
	sem := make(chan struct{}, scanConcurrency)
	for _, g := range groups {
		if g.Err != nil {
			continue
		}
		src := e.byAlias[g.Source]
		for _, sess := range g.Sessions {
			wg.Add(1)
			go func(src source.Source, sess session.Session) {
				defer wg.Done()
				sem <- struct{}{}
				defer func() { <-sem }()
				ctx, cancel := context.WithTimeout(e.ctx, detailTimeout)
				defer cancel()
				if w, err := manage.Panes(ctx, src, sess.Name); err == nil {
					mu.Lock()
					panes[sess.Address()] = w
					mu.Unlock()
				}
			}(src, sess)
		}
	}
	wg.Wait()
	return ui.Scan{Groups: groups, Panes: panes}
}

// toGroups converts scan results to display groups, sorting sessions by recency.
func toGroups(results []discovery.Result) []ui.Group {
	groups := make([]ui.Group, len(results))
	for i, r := range results {
		sess := append([]session.Session(nil), r.Sessions...)
		ui.SortByRecency(sess)
		groups[i] = ui.Group{Source: r.Source, Err: r.Err, Sessions: sess}
	}
	return groups
}

// lsLines renders scan groups for `xmux ls`: one "<source>/<name>" line per
// reachable session, an unreachable line per dead source, and whether EVERY
// source is unreachable (a reachable mux with zero sessions is empty, not failed).
func lsLines(groups []ui.Group) (lines, unreachable []string, allUnreachable bool) {
	reachable := 0
	for _, g := range groups {
		if g.Err != nil {
			unreachable = append(unreachable, fmt.Sprintf("%s\t(unreachable: %v)", g.Source, g.Err))
			continue
		}
		reachable++
		for _, s := range g.Sessions {
			lines = append(lines, fmt.Sprintf("%s\t%dw\tattached=%t", s.Address(), s.Windows, s.Attached))
		}
	}
	allUnreachable = reachable == 0 && len(groups) > 0
	return lines, unreachable, allUnreachable
}

// ops builds the switcher's side-effecting actions over the live mux.
func (e Env) ops() ui.SwitcherOps {
	return ui.SwitcherOps{
		New: func(alias, name string) (session.Session, error) {
			src, ok := e.byAlias[alias]
			if !ok {
				return session.Session{}, fmt.Errorf("unknown source %q", alias)
			}
			assigned, err := manage.Create(e.ctx, src, name)
			if err != nil {
				return session.Session{}, err
			}
			return session.Session{Source: alias, Name: assigned, Windows: 1}, nil
		},
		Kill: func(s session.Session) error {
			return manage.Kill(e.ctx, e.byAlias[s.Source], s.Name)
		},
		Rename: func(s session.Session, newName string) error {
			return manage.Rename(e.ctx, e.byAlias[s.Source], s.Name, newName)
		},
		Panes: func(s session.Session) ([]session.WindowPanes, error) {
			ctx, cancel := context.WithTimeout(e.ctx, detailTimeout)
			defer cancel()
			return manage.Panes(ctx, e.byAlias[s.Source], s.Name)
		},
		Capture: func(alias, target string) (string, error) {
			src, ok := e.byAlias[alias]
			if !ok {
				return "", fmt.Errorf("unknown source %q", alias)
			}
			ctx, cancel := context.WithTimeout(e.ctx, detailTimeout)
			defer cancel()
			return manage.Capture(ctx, src, target)
		},
		Refresh: func() (ui.Scan, error) {
			return e.deepScan(), nil
		},
	}
}
