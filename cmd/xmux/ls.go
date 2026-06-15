package main

import (
	"fmt"
	"os"
)

// runLs prints every reachable session as one "<source>/<name>" line; dead
// sources go to stderr. It fails only when every source is unreachable.
func runLs(e Env) int {
	lines, unreachable, allUnreachable := lsLines(e.scan())
	for _, l := range lines {
		fmt.Println(l)
	}
	for _, u := range unreachable {
		fmt.Fprintln(os.Stderr, u)
	}
	if allUnreachable {
		return 1
	}
	return 0
}
