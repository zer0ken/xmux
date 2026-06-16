package discovery

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zer0ken/xmux/internal/source"
)

// listSessionsOut is the SessionFormat output for one session line:
// windows\tattached\tlast_attached\tname.
func listSessionsOut(line string) []byte { return []byte(line) }

// staticRunner returns canned list-sessions output (or an error), ignoring the
// command. It satisfies source.Runner.
type staticRunner struct {
	out []byte
	err error
}

func (r staticRunner) Run(_ context.Context, _ string, _ ...string) ([]byte, error) {
	return r.out, r.err
}

// localSource builds a non-remote Source with an injected runner.
func localSource(alias string, r source.Runner) source.Source {
	return source.Source{Alias: alias, Binary: "psmux", Remote: false, Runner: r}
}

func TestScanAllPreservesOrderAndContent(t *testing.T) {
	srcs := []source.Source{
		localSource("a", staticRunner{out: listSessionsOut("2\t1\t1781246739\teditor\n")}),
		localSource("b", staticRunner{out: listSessionsOut("1\t0\t0\tbuild\n")}),
		localSource("c", staticRunner{out: listSessionsOut("3\t1\t1781246800\tshell\n")}),
	}
	got := ScanAll(context.Background(), srcs, time.Second, 4)
	if len(got) != 3 {
		t.Fatalf("want 3 results, got %d", len(got))
	}
	wantAlias := []string{"a", "b", "c"}
	wantName := []string{"editor", "build", "shell"}
	for i, r := range got {
		if r.Source != wantAlias[i] {
			t.Errorf("results[%d].Source = %q, want %q", i, r.Source, wantAlias[i])
		}
		if r.Err != nil {
			t.Errorf("results[%d].Err = %v, want nil", i, r.Err)
		}
		if len(r.Sessions) != 1 || r.Sessions[0].Name != wantName[i] {
			t.Fatalf("results[%d].Sessions = %+v, want one named %q", i, r.Sessions, wantName[i])
		}
		if r.Sessions[0].Source != wantAlias[i] {
			t.Errorf("results[%d] session Source = %q, want %q", i, r.Sessions[0].Source, wantAlias[i])
		}
	}
}

func TestScanAllOneUnreachableDoesNotStopOthers(t *testing.T) {
	boom := errors.New("ssh: connect to host b port 22: Connection timed out")
	srcs := []source.Source{
		localSource("a", staticRunner{out: listSessionsOut("1\t1\t0\tone\n")}),
		localSource("b", staticRunner{err: boom}),
		localSource("c", staticRunner{out: listSessionsOut("1\t0\t0\ttwo\n")}),
	}
	got := ScanAll(context.Background(), srcs, time.Second, 4)
	if len(got) != 3 {
		t.Fatalf("want 3 results, got %d", len(got))
	}
	if got[1].Err == nil {
		t.Errorf("results[1] should carry the unreachable error")
	}
	if got[1].Sessions != nil {
		t.Errorf("results[1].Sessions must be nil when unreachable, got %+v", got[1].Sessions)
	}
	if got[0].Err != nil || len(got[0].Sessions) != 1 || got[0].Sessions[0].Name != "one" {
		t.Errorf("results[0] must succeed: %+v", got[0])
	}
	if got[2].Err != nil || len(got[2].Sessions) != 1 || got[2].Sessions[0].Name != "two" {
		t.Errorf("results[2] must succeed: %+v", got[2])
	}
}

func TestScanAllReachableEmpty(t *testing.T) {
	// A reachable mux with no sessions: empty output, no error.
	srcs := []source.Source{
		localSource("a", staticRunner{out: nil, err: nil}),
	}
	got := ScanAll(context.Background(), srcs, time.Second, 4)
	if len(got) != 1 {
		t.Fatalf("want 1 result, got %d", len(got))
	}
	if got[0].Err != nil {
		t.Errorf("reachable-empty must have nil Err, got %v", got[0].Err)
	}
	if len(got[0].Sessions) != 0 {
		t.Errorf("reachable-empty must have zero sessions, got %+v", got[0].Sessions)
	}
}

// concurrencyRunner tracks the live count of in-flight Run calls and records the
// peak observed concurrency.
type concurrencyRunner struct {
	active atomic.Int32
	max    atomic.Int32
}

func (r *concurrencyRunner) Run(_ context.Context, _ string, _ ...string) ([]byte, error) {
	n := r.active.Add(1)
	for {
		m := r.max.Load()
		if n <= m || r.max.CompareAndSwap(m, n) {
			break
		}
	}
	time.Sleep(8 * time.Millisecond)
	r.active.Add(-1)
	return []byte("1\t0\t0\ts\n"), nil
}

func TestScanAllRespectsConcurrencyCap(t *testing.T) {
	cr := &concurrencyRunner{}
	const n = 5
	srcs := make([]source.Source, n)
	for i := range srcs {
		srcs[i] = localSource("s", cr)
	}
	got := ScanAll(context.Background(), srcs, time.Second, 2)
	if len(got) != n {
		t.Fatalf("want %d results, got %d", n, len(got))
	}
	if peak := cr.max.Load(); peak > 2 {
		t.Errorf("observed max concurrency %d exceeded cap of 2", peak)
	}
}

func TestScanAllMaxConcurrentBelowOneTreatedAsOne(t *testing.T) {
	cr := &concurrencyRunner{}
	const n = 4
	srcs := make([]source.Source, n)
	for i := range srcs {
		srcs[i] = localSource("s", cr)
	}
	got := ScanAll(context.Background(), srcs, time.Second, 0)
	if len(got) != n {
		t.Fatalf("want %d results, got %d", n, len(got))
	}
	if peak := cr.max.Load(); peak > 1 {
		t.Errorf("maxConcurrent<1 must behave as 1; observed peak %d", peak)
	}
}

// blockingRunner honors the context: it returns the context error when the
// per-source timeout fires, otherwise sleeps a long time.
type blockingRunner struct{}

func (blockingRunner) Run(ctx context.Context, _ string, _ ...string) ([]byte, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-time.After(10 * time.Second):
		return []byte("1\t0\t0\ts\n"), nil
	}
}

func TestScanAllPerSourceTimeout(t *testing.T) {
	srcs := []source.Source{
		localSource("slow", blockingRunner{}),
	}
	start := time.Now()
	got := ScanAll(context.Background(), srcs, 20*time.Millisecond, 4)
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("ScanAll did not honor the per-source timeout; took %v", elapsed)
	}
	if len(got) != 1 {
		t.Fatalf("want 1 result, got %d", len(got))
	}
	if got[0].Err == nil {
		t.Errorf("timed-out source must carry an error, got %+v", got[0])
	}
}
