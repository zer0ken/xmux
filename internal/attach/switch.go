// Package attach resolves and performs the terminal handover into a mux session:
// the in-mux switch plan (same-server teleport vs cross-server detach-to-home)
// and the out-of-mux attach that hands the controlling terminal to a child.
package attach

import (
	"github.com/zer0ken/xmux/internal/mux"
	"github.com/zer0ken/xmux/internal/session"
)

// SwitchPlan is the resolved in-mux switch action.
type SwitchPlan struct {
	Teleport bool     // true ⇒ same-server instant switch; false ⇒ cross-server detach-to-home
	Argv     []string // the mux argv to run
}

// PlanSwitch resolves same-server teleport vs cross-server detach.
// A mux's switch-client cannot cross servers, so cross-source means detach
// back to the home loop (which re-renders and the user re-picks).
func PlanSwitch(fromSource, fromBin string, target session.Session) SwitchPlan {
	if target.Source == fromSource {
		return SwitchPlan{Teleport: true, Argv: mux.SwitchClient(fromBin, target.Name)}
	}
	return SwitchPlan{Teleport: false, Argv: mux.DetachClient(fromBin)}
}
