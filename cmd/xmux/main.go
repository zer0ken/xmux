// Command xmux is a stateless cross-environment session switcher: one terminal
// that sees and moves between every reachable tmux/psmux session — local and
// over ssh — regardless of OS or mux kind.
package main

import (
	"context"
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/zer0ken/xmux/internal/attach"
	"github.com/zer0ken/xmux/internal/session"
)

var version = "dev"

func main() {
	os.Exit(run())
}

func run() int {
	ctx := context.Background()
	exit := 0

	root := &cobra.Command{
		Use:           "xmux",
		Short:         "cross-environment mux session switcher",
		Long:          "xmux shows every reachable tmux/psmux session (local + ssh) as one tree and switches between them.",
		SilenceUsage:  true,
		SilenceErrors: true,
		RunE: func(*cobra.Command, []string) error {
			e, err := buildEnv(ctx)
			if err != nil {
				return err
			}
			exit = runHome(e)
			return nil
		},
	}

	root.AddCommand(&cobra.Command{
		Use: "popup", Short: "in-mux switcher, bound via display-popup -E",
		SilenceUsage: true, SilenceErrors: true,
		RunE: func(*cobra.Command, []string) error {
			e, err := buildEnv(ctx)
			if err != nil {
				return err
			}
			exit = runPopup(e)
			return nil
		},
	})

	root.AddCommand(&cobra.Command{
		Use: "ls", Short: "list every reachable session (scriptable)",
		SilenceUsage: true, SilenceErrors: true,
		RunE: func(*cobra.Command, []string) error {
			e, err := buildEnv(ctx)
			if err != nil {
				return err
			}
			exit = runLs(e)
			return nil
		},
	})

	root.AddCommand(&cobra.Command{
		Use: "attach <source>/<session>", Short: "attach one session directly",
		Args:         cobra.ExactArgs(1),
		SilenceUsage: true, SilenceErrors: true,
		RunE: func(_ *cobra.Command, args []string) error {
			e, err := buildEnv(ctx)
			if err != nil {
				return err
			}
			exit = runDirectAttach(e, args[0])
			return nil
		},
	})

	root.AddCommand(&cobra.Command{
		Use: "doctor", Short: "diagnose configuration and source reachability",
		SilenceUsage: true, SilenceErrors: true,
		RunE: func(*cobra.Command, []string) error {
			e, cfgErr := buildEnv(ctx) // tolerate a malformed config — report, don't die
			exit = runDoctor(e, cfgErr)
			return nil
		},
	})

	var ctlPid int
	var ctlSock string
	ctlCmd := &cobra.Command{
		Use: "ctl [command...]", Short: "drive a running switcher over its control socket",
		SilenceUsage: true, SilenceErrors: true,
		RunE: func(_ *cobra.Command, args []string) error {
			e, err := buildEnv(ctx)
			if err != nil {
				return err
			}
			exit = runCtl(e, ctlPid, ctlSock, args)
			return nil
		},
	}
	ctlCmd.Flags().IntVar(&ctlPid, "pid", 0, "target the instance with this pid")
	ctlCmd.Flags().StringVar(&ctlSock, "sock", "", "target this socket path")
	root.AddCommand(ctlCmd)

	root.AddCommand(&cobra.Command{
		Use: "version", Short: "print version",
		Run: func(*cobra.Command, []string) { fmt.Println("xmux", version) },
	})

	if err := root.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, "xmux:", err)
		return 1
	}
	return exit
}

// runDirectAttach attaches one "<source>/<session>" without the tree.
func runDirectAttach(e Env, addr string) int {
	t, err := session.ParseTarget(addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "xmux:", err)
		return 1
	}
	src, ok := e.byAlias[t.Source]
	if !ok {
		fmt.Fprintf(os.Stderr, "xmux: unknown source %q (not local or an ssh-config host)\n", t.Source)
		return 1
	}
	if err := attach.NestGuard(attach.InMux()); err != nil {
		fmt.Fprintln(os.Stderr, "xmux:", err)
		return 1
	}
	if err := attach.RunAttach(attach.OSExecer{}, src.AttachCommand(t.Name)); err != nil {
		fmt.Fprintln(os.Stderr, "xmux: attach failed:", err)
		return 1
	}
	return 0
}
