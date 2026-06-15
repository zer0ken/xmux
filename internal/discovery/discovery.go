// Package discovery probes every source concurrently to gather the sessions
// reachable from this machine, isolating each source so one unreachable mux
// never blocks or fails the rest. It owns the fan-out: bounded concurrency, a
// per-source timeout, and order-preserving results.
package discovery

import (
	"context"
	"sync"
	"time"

	"github.com/zer0ken/xmux/internal/session"
	"github.com/zer0ken/xmux/internal/source"
)

// Result is one source's scan outcome. A non-nil Err means the source was
// unreachable, in which case Sessions is nil.
type Result struct {
	Source   string            // the source Alias
	Sessions []session.Session // nil when unreachable
	Err      error             // non-nil ⇒ unreachable
}

// ScanAll probes every source concurrently and returns one Result per source,
// in input order. At most maxConcurrent probes run at once; each probe is
// bounded by its own timeout derived from ctx. One unreachable source never
// blocks or fails the others.
func ScanAll(ctx context.Context, srcs []source.Source, timeout time.Duration, maxConcurrent int) []Result {
	if maxConcurrent < 1 {
		maxConcurrent = 1
	}
	results := make([]Result, len(srcs))
	sem := make(chan struct{}, maxConcurrent)
	var wg sync.WaitGroup
	for i, s := range srcs {
		wg.Add(1)
		go func() {
			defer wg.Done()
			// Acquire a slot BEFORE starting the timeout so a queued source does
			// not burn its budget waiting for a free slot.
			sem <- struct{}{}
			defer func() { <-sem }()
			perCtx, cancel := context.WithTimeout(ctx, timeout)
			defer cancel()
			sessions, err := s.ListSessions(perCtx)
			results[i] = Result{Source: s.Alias, Sessions: sessions, Err: err}
		}()
	}
	wg.Wait()
	return results
}
