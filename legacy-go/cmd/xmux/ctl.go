package main

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"github.com/zer0ken/xmux/internal/control"
)

// runCtl drives a running switcher over its control socket. With command args it
// sends one command; with none it streams commands from stdin. The target is the
// explicit --sock, else --pid's socket, else the newest socket.
func runCtl(e Env, pid int, sock string, args []string) int {
	path, err := resolveCtlSocket(e.xmuxDir, pid, sock)
	if err != nil {
		fmt.Fprintln(os.Stderr, "xmux ctl:", err)
		return 1
	}
	client, err := control.Dial(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, "xmux ctl:", err)
		return 1
	}
	defer client.Close()

	if len(args) > 0 {
		return ctlOne(client, strings.Join(args, " "))
	}
	sc := bufio.NewScanner(os.Stdin)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		if rc := ctlOne(client, line); rc != 0 {
			return rc
		}
	}
	return 0
}

func resolveCtlSocket(xmuxDir string, pid int, sock string) (string, error) {
	switch {
	case sock != "":
		return sock, nil
	case pid > 0:
		return control.SocketPath(xmuxDir, pid), nil
	default:
		return control.Discover(xmuxDir)
	}
}

func ctlOne(client *control.Client, line string) int {
	resp, err := client.Do(line)
	if err != nil {
		fmt.Fprintln(os.Stderr, "xmux ctl:", err)
		return 1
	}
	fmt.Print(resp)
	if !strings.HasSuffix(resp, "\n") {
		fmt.Println()
	}
	return 0
}
