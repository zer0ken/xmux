package attach

import (
	"reflect"
	"testing"

	"github.com/zer0ken/xmux/internal/mux"
	"github.com/zer0ken/xmux/internal/session"
)

func TestPlanSwitchTeleport(t *testing.T) {
	target := session.Session{Source: "local", Name: "dev"}
	got := PlanSwitch("local", "tmux", target)
	if !got.Teleport {
		t.Errorf("Teleport = false, want true for same-source switch")
	}
	want := mux.SwitchClient("tmux", "dev")
	if !reflect.DeepEqual(got.Argv, want) {
		t.Errorf("Argv = %#v, want %#v", got.Argv, want)
	}
}

func TestPlanSwitchCrossServer(t *testing.T) {
	target := session.Session{Source: "remote", Name: "dev"}
	got := PlanSwitch("local", "tmux", target)
	if got.Teleport {
		t.Errorf("Teleport = true, want false for cross-source switch")
	}
	want := mux.DetachClient("tmux")
	if !reflect.DeepEqual(got.Argv, want) {
		t.Errorf("Argv = %#v, want %#v", got.Argv, want)
	}
}
