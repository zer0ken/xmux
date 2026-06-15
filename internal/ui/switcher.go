package ui

import (
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
	"github.com/rivo/uniseg"
	"github.com/zer0ken/xmux/internal/session"
)

const (
	treeWidth       = 46
	previewInterval = time.Second // live preview poll cadence
)

// reverseStyle is the selection highlight: a true reverse-video attribute the
// terminal renders by swapping its own fg/bg — visible on any theme (swapping
// ColorDefault with ColorDefault is a no-op).
var reverseStyle = tcell.StyleDefault.Reverse(true)

// Node references. Hosts, sessions, and windows are selectable; panes are shown
// for context but never selectable, so the cursor skips them.
type swHostRef struct {
	Source      string
	Unreachable bool
}
type swSessionRef struct{ S session.Session }
type swWindowRef struct {
	S      session.Session
	Window int
}

// Scan is a fully-populated snapshot of the reachable environment: the host /
// session groups plus every session's windows-and-panes, keyed by address.
type Scan struct {
	Groups []Group
	Panes  map[string][]session.WindowPanes
}

// SwitcherOps are the side-effecting actions the switcher delegates to the host
// program.
type SwitcherOps struct {
	New     func(sourceAlias, name string) (session.Session, error)
	Kill    func(s session.Session) error
	Rename  func(s session.Session, newName string) error
	Panes   func(s session.Session) ([]session.WindowPanes, error)
	Capture func(sourceAlias, target string) (string, error)
	Refresh func() (Scan, error)
}

// Result is the outcome of one switcher run. Window >= 0 means attach with that
// window selected; -1 means the session's current window.
type Result struct {
	Chosen *session.Session
	Window int
}

// Control is the seam an external driver (the control channel) attaches through.
type Control func(app *tview.Application, root tview.Primitive, render func() string) (stop func())

type previewTarget struct{ Source, Target string }

const (
	inputFilter = iota
	inputNew
	inputRename
)

type switcher struct {
	app     *tview.Application
	flex    *tview.Flex
	middle  *tview.Flex
	header  *tview.TextView
	tree    *tview.TreeView
	preview *tview.TextView
	input   *tview.InputField
	footer  *tview.TextView

	ops    SwitcherOps
	groups []Group
	panes  map[string][]session.WindowPanes

	filter       string
	inputMode    int
	pendingKill  *session.Session
	result       Result
	flash        string
	nameColWidth int

	previewMu  sync.Mutex
	previewTgt previewTarget
	pollKick   chan struct{}
}

func newSwitcher(scan Scan, ops SwitcherOps) *switcher {
	applyTheme()
	s := &switcher{
		app:      tview.NewApplication(),
		ops:      ops,
		groups:   scan.Groups,
		panes:    scan.Panes,
		result:   Result{Window: -1},
		pollKick: make(chan struct{}, 1),
	}
	if s.panes == nil {
		s.panes = map[string][]session.WindowPanes{}
	}

	s.header = tview.NewTextView()
	s.header.SetDynamicColors(true)
	s.header.SetText("[::b]xmux[::-]\n[::d]cross-environment MUX manager[::-]")

	s.tree = tview.NewTreeView()
	s.tree.SetGraphics(false)
	s.tree.SetTopLevel(1)
	s.tree.SetBorder(true).SetTitle(" Hosts · Sessions · Windows · Panes ")
	s.tree.SetChangedFunc(s.onFocusChanged)
	s.tree.SetInputCapture(s.onTreeKey)

	s.preview = tview.NewTextView()
	s.preview.SetDynamicColors(false).SetWrap(false) // arbitrary pane content, shown verbatim
	s.preview.SetBorder(true).SetTitle(" Preview ")

	s.input = tview.NewInputField()
	s.input.SetDoneFunc(s.onInputDone)

	s.footer = tview.NewTextView()
	s.footer.SetDynamicColors(true)

	s.middle = tview.NewFlex().SetDirection(tview.FlexColumn)
	s.middle.AddItem(s.tree, treeWidth, 0, true)
	s.middle.AddItem(s.preview, 0, 1, false)

	s.flex = tview.NewFlex().SetDirection(tview.FlexRow)
	s.flex.AddItem(s.header, 2, 0, false)
	s.flex.AddItem(s.middle, 0, 1, true)
	s.flex.AddItem(s.input, 0, 0, false)
	s.flex.AddItem(s.footer, 1, 0, false)

	s.rebuildTree()
	s.app.SetRoot(s.flex, true)
	s.app.SetFocus(s.tree)
	s.updateFooter()
	return s
}

// RunSwitcher runs one interactive switcher session, polling the live preview
// for as long as it is open.
func RunSwitcher(scan Scan, ops SwitcherOps, controls ...Control) (Result, error) {
	s := newSwitcher(scan, ops)

	stop := make(chan struct{})
	go s.runPoller(stop)
	defer close(stop)

	var stops []func()
	for _, c := range controls {
		if c == nil {
			continue
		}
		if st := c(s.app, s.flex, s.renderText); st != nil {
			stops = append(stops, st)
		}
	}
	defer func() {
		for _, st := range stops {
			st()
		}
	}()

	if err := s.app.Run(); err != nil {
		return Result{}, err
	}
	return s.result, nil
}

// --- tree -------------------------------------------------------------------

func (s *switcher) visibleGroups() []Group {
	if s.filter == "" {
		return s.groups
	}
	filtered := FilterGroups(s.groups, s.filter)
	if len(filtered) == 0 {
		return sourcesOnly(s.groups) // XM-01: a non-matching filter must not be a dead end
	}
	return filtered
}

func sourcesOnly(groups []Group) []Group {
	out := make([]Group, len(groups))
	for i, g := range groups {
		out[i] = Group{Source: g.Source, Err: g.Err}
	}
	return out
}

func (s *switcher) rebuildTree() {
	groups := s.visibleGroups()

	s.nameColWidth = 0
	for _, g := range groups {
		if g.Err != nil {
			continue
		}
		for _, sess := range g.Sessions {
			if w := uniseg.StringWidth(sess.Name); w > s.nameColWidth {
				s.nameColWidth = w
			}
		}
	}

	root := tview.NewTreeNode("").SetSelectable(false)
	var current, firstNode *tview.TreeNode
	var bestRecency int64 = -1

	for _, g := range groups {
		unreachable := g.Err != nil
		host := tview.NewTreeNode(s.hostLabel(g)).
			SetReference(swHostRef{Source: g.Source, Unreachable: unreachable}).
			SetColor(tcell.ColorYellow).
			SetSelectedTextStyle(reverseStyle).
			SetSelectable(true)
		if firstNode == nil {
			firstNode = host
		}
		if !unreachable {
			for _, sess := range g.Sessions {
				sessNode := tview.NewTreeNode(s.sessionLabel(sess)).
					SetReference(swSessionRef{S: sess}).
					SetSelectedTextStyle(reverseStyle).
					SetSelectable(true)
				if sess.LastAttached > bestRecency {
					bestRecency = sess.LastAttached
					current = sessNode
				}
				for _, w := range s.panes[sess.Address()] {
					winNode := tview.NewTreeNode(windowLabel(w)).
						SetReference(swWindowRef{S: sess, Window: w.Index}).
						SetSelectedTextStyle(reverseStyle).
						SetSelectable(true)
					for _, p := range w.Panes {
						winNode.AddChild(tview.NewTreeNode(paneLabel(p)).SetSelectable(false))
					}
					sessNode.AddChild(winNode)
				}
				host.AddChild(sessNode)
			}
		}
		root.AddChild(host)
	}

	s.tree.SetRoot(root)
	if current == nil {
		current = firstNode
	}
	if current != nil {
		s.tree.SetCurrentNode(current)
	}
	s.setTreeTitle()
	s.onFocusChanged(current)
}

func (s *switcher) setTreeTitle() {
	if s.filter != "" {
		// keep it short — the full label + filter overflows the pane width.
		s.tree.SetTitle(fmt.Sprintf(" filter: %s ", s.filter))
		return
	}
	s.tree.SetTitle(" Hosts · Sessions · Windows · Panes ")
}

func (s *switcher) hostLabel(g Group) string {
	if g.Err != nil {
		return g.Source + "  ⚠ unreachable"
	}
	return g.Source
}

func plural(n int) string {
	if n == 1 {
		return "1 window"
	}
	return fmt.Sprintf("%d windows", n)
}

func (s *switcher) sessionLabel(sess session.Session) string {
	pad := s.nameColWidth - uniseg.StringWidth(sess.Name)
	if pad < 0 {
		pad = 0
	}
	star := ""
	if sess.Attached {
		star = "  ●"
	}
	return sess.Name + strings.Repeat(" ", pad) + "   " + plural(sess.Windows) + star
}

func windowLabel(w session.WindowPanes) string {
	s := fmt.Sprintf("window %d: %s", w.Index, w.Name)
	if w.Active {
		s += "  (active)"
	}
	return s
}

func paneLabel(p session.Pane) string {
	s := fmt.Sprintf("pane %d  %s", p.Index, p.Command)
	if p.Active {
		s += "  (active)"
	}
	return s
}

// --- live preview (follows the cursor) --------------------------------------

func (s *switcher) onFocusChanged(node *tview.TreeNode) {
	tgt := s.targetFor(node)
	s.previewMu.Lock()
	changed := tgt != s.previewTgt
	s.previewTgt = tgt
	s.previewMu.Unlock()
	if !changed {
		return
	}
	if tgt.Target == "" {
		s.preview.SetTitle(" Preview ")
	} else {
		s.preview.SetTitle(fmt.Sprintf(" Preview · %s ", tgt.Target))
	}
	select {
	case s.pollKick <- struct{}{}:
	default:
	}
}

// targetFor maps the focused node to the capture-pane target whose active pane
// is what attaching here would land on: a host previews its most-recent
// session's active window; a session its active window; a window its active pane.
func (s *switcher) targetFor(node *tview.TreeNode) previewTarget {
	if node == nil {
		return previewTarget{}
	}
	switch ref := node.GetReference().(type) {
	case swHostRef:
		if sess, ok := s.firstSessionOf(ref.Source); ok {
			return previewTarget{Source: sess.Source, Target: sess.Name}
		}
	case swSessionRef:
		return previewTarget{Source: ref.S.Source, Target: ref.S.Name}
	case swWindowRef:
		return previewTarget{Source: ref.S.Source, Target: fmt.Sprintf("%s:%d", ref.S.Name, ref.Window)}
	}
	return previewTarget{}
}

func (s *switcher) firstSessionOf(source string) (session.Session, bool) {
	for _, g := range s.groups {
		if g.Source == source && len(g.Sessions) > 0 {
			return g.Sessions[0], true // recency-sorted ⇒ what `attach` (no -t) picks
		}
	}
	return session.Session{}, false
}

func (s *switcher) runPoller(stop <-chan struct{}) {
	ticker := time.NewTicker(previewInterval)
	defer ticker.Stop()
	for {
		select {
		case <-stop:
			return
		case <-ticker.C:
			s.pollOnce()
		case <-s.pollKick:
			s.pollOnce()
		}
	}
}

func (s *switcher) pollOnce() {
	s.previewMu.Lock()
	tgt := s.previewTgt
	s.previewMu.Unlock()
	if tgt.Target == "" {
		s.app.QueueUpdateDraw(func() { s.preview.SetText("") })
		return
	}
	text, err := s.ops.Capture(tgt.Source, tgt.Target)
	s.app.QueueUpdateDraw(func() {
		s.previewMu.Lock()
		cur := s.previewTgt
		s.previewMu.Unlock()
		if cur != tgt {
			return // cursor moved while fetching — drop this stale frame
		}
		if err != nil {
			s.preview.SetText("(preview unavailable)")
			return
		}
		s.preview.SetText(strings.TrimRight(text, "\n"))
	})
}

// --- keys -------------------------------------------------------------------

func (s *switcher) onTreeKey(ev *tcell.EventKey) *tcell.EventKey {
	if s.pendingKill != nil {
		s.resolveKill(ev)
		return nil
	}
	switch ev.Key() {
	case tcell.KeyEscape:
		s.quit()
		return nil
	case tcell.KeyEnter:
		s.onEnter()
		return nil
	case tcell.KeyUp, tcell.KeyDown, tcell.KeyPgUp, tcell.KeyPgDn:
		return ev // navigation (tview skips non-selectable pane rows)
	case tcell.KeyHome:
		s.setCurrent(s.edgeNode(true))
		return nil
	case tcell.KeyEnd:
		s.setCurrent(s.edgeNode(false))
		return nil
	}
	switch ev.Rune() {
	case 'q':
		s.quit()
	case '/':
		s.openInput(inputFilter)
	case 'n':
		s.openInput(inputNew)
	case 'R':
		s.openInput(inputRename)
	case 'x':
		s.armKill()
	case 'r':
		s.doRefresh()
	}
	return nil
}

func (s *switcher) onEnter() {
	node := s.tree.GetCurrentNode()
	if node == nil {
		return
	}
	switch ref := node.GetReference().(type) {
	case swHostRef:
		if ref.Unreachable {
			return
		}
		if sess, ok := s.firstSessionOf(ref.Source); ok {
			s.choose(sess, -1) // attach with no session specified ⇒ the host's recent session
		}
	case swSessionRef:
		s.choose(ref.S, -1)
	case swWindowRef:
		s.choose(ref.S, ref.Window)
	}
}

func (s *switcher) choose(sess session.Session, window int) {
	chosen := sess
	s.result = Result{Chosen: &chosen, Window: window}
	s.app.Stop()
}

// currentSession resolves the session a session/window node belongs to (nil on a
// host node).
func (s *switcher) currentSession() *session.Session {
	node := s.tree.GetCurrentNode()
	if node == nil {
		return nil
	}
	switch ref := node.GetReference().(type) {
	case swSessionRef:
		sess := ref.S
		return &sess
	case swWindowRef:
		sess := ref.S
		return &sess
	}
	return nil
}

func (s *switcher) currentSource() string {
	node := s.tree.GetCurrentNode()
	if node == nil {
		return ""
	}
	switch ref := node.GetReference().(type) {
	case swHostRef:
		return ref.Source
	case swSessionRef:
		return ref.S.Source
	case swWindowRef:
		return ref.S.Source
	}
	return ""
}

func (s *switcher) currentHostUnreachable() bool {
	node := s.tree.GetCurrentNode()
	if node == nil {
		return false
	}
	if ref, ok := node.GetReference().(swHostRef); ok {
		return ref.Unreachable
	}
	return false
}

func (s *switcher) setCurrent(node *tview.TreeNode) {
	if node == nil {
		return
	}
	s.tree.SetCurrentNode(node)
	s.onFocusChanged(node)
}

// edgeNode returns the first (or last) selectable node in display order. A
// selectable node is one with a reference (panes have none).
func (s *switcher) edgeNode(first bool) *tview.TreeNode {
	var found *tview.TreeNode
	var walk func(n *tview.TreeNode)
	walk = func(n *tview.TreeNode) {
		for _, c := range n.GetChildren() {
			if c.GetReference() != nil && (found == nil || !first) {
				found = c
			}
			walk(c)
		}
	}
	walk(s.tree.GetRoot())
	return found
}

func (s *switcher) findSessionNode(address string) *tview.TreeNode {
	for _, host := range s.tree.GetRoot().GetChildren() {
		for _, sess := range host.GetChildren() {
			if ref, ok := sess.GetReference().(swSessionRef); ok && ref.S.Address() == address {
				return sess
			}
		}
	}
	return nil
}

// --- input row (filter / new / rename) --------------------------------------

func (s *switcher) openInput(mode int) {
	s.flash = ""
	s.inputMode = mode
	switch mode {
	case inputFilter:
		s.input.SetLabel(" filter: ").SetText(s.filter)
	case inputNew:
		if s.currentSource() == "" {
			return
		}
		if s.currentHostUnreachable() {
			s.flash = "host unreachable — cannot create here"
			s.updateFooter()
			return
		}
		s.input.SetLabel(" new session name (empty = auto): ").SetText("")
	case inputRename:
		sess := s.currentSession()
		if sess == nil {
			return
		}
		s.input.SetLabel(" rename to: ").SetText(sess.Name)
	}
	s.flex.ResizeItem(s.input, 1, 0)
	s.app.SetFocus(s.input)
	s.updateFooter()
}

func (s *switcher) closeInput() {
	s.flex.ResizeItem(s.input, 0, 0)
	s.app.SetFocus(s.tree)
	s.updateFooter()
}

func (s *switcher) onInputDone(key tcell.Key) {
	if key != tcell.KeyEnter {
		s.closeInput() // every non-Enter key closes the row or focus is stranded
		return
	}
	val := strings.TrimSpace(s.input.GetText())
	switch s.inputMode {
	case inputFilter:
		s.filter = val
		s.closeInput()
		s.rebuildTree()
	case inputNew:
		s.doCreate(val)
		s.closeInput()
	case inputRename:
		s.doRename(val)
		s.closeInput()
	}
}

func (s *switcher) doCreate(name string) {
	src := s.currentSource()
	if src == "" {
		return
	}
	created, err := s.ops.New(src, name)
	if err != nil {
		s.flash = "create failed: " + err.Error()
		return
	}
	if wins, perr := s.ops.Panes(created); perr == nil {
		s.panes[created.Address()] = wins
	}
	s.groups = AddSession(s.groups, created)
	s.rebuildTree()
	if node := s.findSessionNode(created.Address()); node != nil {
		s.tree.SetCurrentNode(node)
		s.onFocusChanged(node)
	}
}

func (s *switcher) doRename(newName string) {
	sess := s.currentSession()
	if sess == nil || newName == "" || newName == sess.Name {
		return
	}
	if strings.HasPrefix(newName, "-") {
		// the mux silently no-ops a '-'-leading name (getopt eats it) — refuse it
		s.flash = "rename: name cannot start with '-'"
		s.updateFooter()
		return
	}
	if err := s.ops.Rename(*sess, newName); err != nil {
		s.flash = "rename failed: " + err.Error()
		return
	}
	if wins, ok := s.panes[sess.Address()]; ok {
		delete(s.panes, sess.Address())
		s.panes[sess.Source+"/"+newName] = wins
	}
	s.groups = RenameSession(s.groups, sess.Address(), newName)
	s.rebuildTree()
}

// --- kill (inline confirm) --------------------------------------------------

func (s *switcher) armKill() {
	sess := s.currentSession()
	if sess == nil {
		return
	}
	s.pendingKill = sess
	s.updateFooter()
}

func (s *switcher) resolveKill(ev *tcell.EventKey) {
	sess := s.pendingKill
	s.pendingKill = nil
	if sess != nil && (ev.Rune() == 'y' || ev.Rune() == 'Y') {
		if err := s.ops.Kill(*sess); err != nil {
			s.flash = "kill failed: " + err.Error()
		} else {
			delete(s.panes, sess.Address())
			s.groups = RemoveSession(s.groups, sess.Address())
			s.rebuildTree()
		}
	}
	s.updateFooter()
}

// --- refresh / quit / footer ------------------------------------------------

func (s *switcher) doRefresh() {
	scan, err := s.ops.Refresh()
	if err != nil {
		s.flash = "refresh failed: " + err.Error()
		return
	}
	s.groups = scan.Groups
	s.panes = scan.Panes
	if s.panes == nil {
		s.panes = map[string][]session.WindowPanes{}
	}
	s.rebuildTree()
}

func (s *switcher) quit() {
	s.result = Result{Window: -1}
	s.app.Stop()
}

func (s *switcher) updateFooter() {
	switch {
	case s.pendingKill != nil:
		s.footer.SetText(fmt.Sprintf(" kill %s? [y]es / [n]o", s.pendingKill.Address()))
	case s.flash != "":
		s.footer.SetText(" " + s.flash)
	default:
		s.footer.SetText(" enter attach · n new · R rename · x kill · / filter · r refresh · q quit")
	}
}

// renderText draws the live layout to an off-screen simulation screen and
// flattens it — the payload the control channel's `dump` returns.
func (s *switcher) renderText() string {
	w, h := 100, 30
	if _, _, rw, rh := s.flex.GetRect(); rw > 0 && rh > 0 {
		w, h = rw, rh
	}
	sim := tcell.NewSimulationScreen("UTF-8")
	if err := sim.Init(); err != nil {
		return ""
	}
	defer sim.Fini()
	sim.SetSize(w, h)
	s.flex.SetRect(0, 0, w, h)
	s.flex.Draw(sim)
	sim.Show()
	return screenToString(sim)
}

func screenToString(sim tcell.SimulationScreen) string {
	cells, w, h := sim.GetContents()
	var b strings.Builder
	for y := 0; y < h; y++ {
		line := make([]rune, 0, w)
		for x := 0; x < w; x++ {
			rs := cells[y*w+x].Runes
			if len(rs) == 0 || rs[0] == 0 {
				line = append(line, ' ')
			} else {
				line = append(line, rs[0])
			}
		}
		b.WriteString(strings.TrimRight(string(line), " "))
		b.WriteByte('\n')
	}
	return b.String()
}

func applyTheme() {
	tview.Styles.PrimitiveBackgroundColor = tcell.ColorDefault
	tview.Styles.ContrastBackgroundColor = tcell.ColorDefault
	tview.Styles.MoreContrastBackgroundColor = tcell.ColorDefault
	tview.Styles.PrimaryTextColor = tcell.ColorDefault
	tview.Styles.BorderColor = tcell.ColorDefault
	tview.Styles.TitleColor = tcell.ColorDefault
}
