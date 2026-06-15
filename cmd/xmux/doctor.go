package main

import (
	"context"
	"fmt"
	"os/exec"
)

// runDoctor reports configuration health and per-source reachability. It is a
// diagnostic: a malformed config or an unreachable host is reported, not fatal.
func runDoctor(e Env, cfgErr error) int {
	fmt.Println("xmux doctor")

	switch {
	case cfgErr != nil:
		fmt.Printf("config.toml: ERROR — %v (using defaults)\n", cfgErr)
	case len(e.cfgWarnings) > 0:
		for _, w := range e.cfgWarnings {
			fmt.Printf("config.toml: WARNING — %s\n", w)
		}
	default:
		fmt.Println("config.toml: ok")
	}

	fmt.Printf("local mux: %s\n", e.localBin)
	if _, err := exec.LookPath("ssh"); err != nil {
		fmt.Println("ssh: NOT FOUND on PATH — remote sources unavailable")
	} else {
		fmt.Println("ssh: ok")
	}

	fmt.Println("sources:")
	for _, s := range e.srcs {
		ctx, cancel := context.WithTimeout(e.ctx, scanTimeout)
		sess, err := s.ListSessions(ctx)
		cancel()
		if err != nil {
			fmt.Printf("  %s (%s): UNREACHABLE — %v\n", s.Alias, s.Binary, err)
			continue
		}
		fmt.Printf("  %s (%s): ok, %d session(s)\n", s.Alias, s.Binary, len(sess))
	}
	return 0
}
