package ui

import (
	"fmt"
	"strings"

	"github.com/gdamore/tcell/v2"
	"github.com/rivo/tview"
	"github.com/rivo/uniseg"
	"github.com/zer0ken/xmux/internal/session"
)

const treeWidth = 42

// node references stored on tree nodes.
type swHostRef struct {
	Source      string
	Unreachable bool
}

type swSessionRef struct {
	S session.Session
}

// SwitcherOps are the side-effecting actions the switcher delegates to the host
// program. New/Kill/Rename act on the live mux; Panes fetches a session's
// windows-and-panes for the detail view; Refresh re-scans every source.
type SwitcherOps struct {
	New     func(sourceAlias, name string) (session.Session, error)
	Kill    func(s session.Session) error
	Rename  func(s session.Session, newName string) error
	Panes   func(s session.Session) ([]session.WindowPanes, error)
	Refresh func() ([]Group, error)
}

// Result is the outcome of one switcher run.
type Result struct {
	Chosen *session.Session // nil ⇒ the user quit
}

// Control is the seam an external driver (the control channel) attaches through.
// It receives the application, its root, and a function that renders the live
// screen to text; it returns a stop func run at teardown.
type Control func(app *tview.Application, root tview.Primitive, render func() string) (stop func())

type detailEntry struct {
	wins []session.WindowPanes
	err  error
}

const (
	inputFilter = iota
	inputNew
	inputRename
)

type switcher struct {
	app    *tview.Application
	flex   *tview.Flex
	middle *tview.Flex
	header *tview.TextView
	tree   *tview.TreeView
	detail *tview.TextView
	input  *tview.InputField
	footer *tview.TextView

	ops    SwitcherOps
	groups []Group

	filter        string
	expanded      map[string]bool // host source → expanded (absent ⇒ true)
	inputMode     int
	pendingKill   *session.Session
	chosen        *session.Session
	flash         string
	nameColWidth  int
	detailByAddr  map[string]detailEntry
	detailAddr    string
}

func newSwitcher(groups []Group, ops SwitcherOps) *switcher {
	applyTheme()
	s := &switcher{
		app:          tview.NewApplication(),
		ops:          ops,
		groups:       groups,
		expanded:     map[string]bool{},
		detailByAddr: map[string]detailEntry{},
	}

	s.header = tview.NewTextView()
	s.header.SetDynamicColors(true)
	s.header.SetText("[::b]xmux[::-]\n[::d]cross-environment MUX manager[::-]")

	s.tree = tview.NewTreeView()
	s.tree.SetGraphics(false)
	s.tree.SetTopLevel(1)
	s.tree.SetBorder(true).SetTitle(" Sessions ")
	s.tree.SetChangedFunc(s.onFocusChanged)
	s.tree.SetInputCapture(s.onTreeKey)

	s.detail = tview.NewTextView()
	s.detail.SetDynamicColors(true).SetWrap(false)
	s.detail.SetBorder(true)

	s.input = tview.NewInputField()
	s.input.SetDoneFunc(s.onInputDone)

	s.footer = tview.NewTextView()
	s.footer.SetDynamicColors(true)

	s.middle = tview.NewFlex().SetDirection(tview.FlexColumn)
	s.middle.AddItem(s.tree, treeWidth, 0, true)
	s.middle.AddItem(s.detail, 0, 1, false)

	s.flex = tview.NewFlex().SetDirection(tview.FlexRow)
	s.flex.AddItem(s.header, 2, 0, false)
	s.flex.AddItem(s.middle, 0, 1, true)
	s.flex.AddItem(s.input, 0, 0, false) // hidden until a prompt opens
	s.flex.AddItem(s.footer, 1, 0, false)

	s.rebuildTree()
	s.app.SetRoot(s.flex, true)
	s.app.SetFocus(s.tree)
	s.updateFooter()
	return s
}

// RunSwitcher runs one interactive switcher session and returns the chosen
// session (or nil if the user quit). Controls are attached before the run loop.
func RunSwitcher(groups []Group, ops SwitcherOps, controls ...Control) (Result, error) {
	s := newSwitcher(groups, ops)
	var stops []func()
	for _, c := range controls {
		if c == nil {
			continue
		}
		if stop := c(s.app, s.flex, s.renderText); stop != nil {
			stops = append(stops, stop)
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
	return Result{Chosen: s.chosen}, nil
}

func (s *switcher) isExpanded(source string) bool {
	if v, ok := s.expanded[source]; ok {
		return v
	}
	return true // hosts auto-expand so every environment is visible
}

// visibleGroups applies the active filter. When the filter matches nothing, it
// falls back to source-only nodes so every host stays selectable as a create
// target (the XM-01 fix — a non-matching filter must not be a dead end).
func (s *switcher) visibleGroups() []Group {
	if s.filter == "" {
		return s.groups
	}
	filtered := FilterGroups(s.groups, s.filter)
	if len(filtered) == 0 {
		return sourcesOnly(s.groups)
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
		if g.Err != nil || !s.isExpanded(g.Source) {
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
		expanded := s.isExpanded(g.Source)
		host.SetExpanded(expanded)
		if firstNode == nil {
			firstNode = host
		}
		if !unreachable {
			for _, sess := range g.Sessions {
				leaf := tview.NewTreeNode(s.sessionLabel(sess)).
					SetReference(swSessionRef{S: sess}).
					SetSelectedTextStyle(reverseStyle).
					SetSelectable(true)
				host.AddChild(leaf)
				if firstNode == nil {
					firstNode = leaf
				}
				if expanded && sess.LastAttached > bestRecency {
					bestRecency = sess.LastAttached
					current = leaf
				}
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
		s.tree.SetTitle(fmt.Sprintf(" Sessions (filter: %s) ", s.filter))
		return
	}
	s.tree.SetTitle(" Sessions ")
}

func (s *switcher) hostLabel(g Group) string {
	if g.Err != nil {
		return g.Source + "  ⚠ unreachable"
	}
	if s.isExpanded(g.Source) {
		return "▾ " + g.Source
	}
	return "▸ " + g.Source
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

// --- detail pane (cursor-following, cached per address) ---------------------

func (s *switcher) onFocusChanged(node *tview.TreeNode) {
	if node == nil {
		s.clearDetail()
		return
	}
	sref, ok := node.GetReference().(swSessionRef)
	if !ok {
		s.clearDetail()
		return
	}
	addr := sref.S.Address()
	if addr == s.detailAddr {
		return // same row — no refetch, no repaint
	}
	s.detailAddr = addr
	s.detail.SetTitle(fmt.Sprintf(" %s · Windows/Panes ", addr))
	entry, ok := s.detailByAddr[addr]
	if !ok {
		wins, err := s.ops.Panes(sref.S)
		entry = detailEntry{wins: wins, err: err}
		s.detailByAddr[addr] = entry // cache success AND error so revisiting never re-blocks
	}
	s.detail.SetText(renderDetail(entry))
}

func (s *switcher) clearDetail() {
	s.detailAddr = ""
	s.detail.SetTitle("")
	s.detail.SetText("")
}

func renderDetail(e detailEntry) string {
	if e.err != nil {
		return "error: " + e.err.Error()
	}
	if len(e.wins) == 0 {
		return "(no windows)"
	}
	var b strings.Builder
	for _, w := range e.wins {
		active := ""
		if w.Active {
			active = "  (active)"
		}
		fmt.Fprintf(&b, "%d: %s%s\n", w.Index, w.Name, active)
		for _, p := range w.Panes {
			pa := ""
			if p.Active {
				pa = "  (active)"
			}
			fmt.Fprintf(&b, "   pane %d  %s%s\n", p.Index, p.Command, pa)
		}
	}
	return b.String()
}

// --- key handling (scoped to the tree) --------------------------------------

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
	case tcell.KeyUp, tcell.KeyDown, tcell.KeyHome, tcell.KeyEnd, tcell.KeyPgUp, tcell.KeyPgDn:
		return ev // navigation: let the TreeView move
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
	return nil // nav-mode: a stray key is consumed, never typed
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
		s.toggleExpand(ref.Source)
	case swSessionRef:
		chosen := ref.S
		s.chosen = &chosen
		s.app.Stop()
	}
}

func (s *switcher) toggleExpand(source string) {
	s.expanded[source] = !s.isExpanded(source)
	s.rebuildTree()
	if node := s.findHostNode(source); node != nil {
		s.tree.SetCurrentNode(node)
	}
}

func (s *switcher) findHostNode(source string) *tview.TreeNode {
	for _, child := range s.tree.GetRoot().GetChildren() {
		if ref, ok := child.GetReference().(swHostRef); ok && ref.Source == source {
			return child
		}
	}
	return nil
}

func (s *switcher) currentSession() *session.Session {
	node := s.tree.GetCurrentNode()
	if node == nil {
		return nil
	}
	if ref, ok := node.GetReference().(swSessionRef); ok {
		sess := ref.S
		return &sess
	}
	return nil
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

func (s *switcher) findSessionNode(address string) *tview.TreeNode {
	for _, host := range s.tree.GetRoot().GetChildren() {
		for _, leaf := range host.GetChildren() {
			if ref, ok := leaf.GetReference().(swSessionRef); ok && ref.S.Address() == address {
				return leaf
			}
		}
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
	}
	return ""
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
	// tview fires DoneFunc for Enter/Esc/Tab/Backtab alike; every non-Enter key
	// must close the row or focus is stranded inside it.
	if key != tcell.KeyEnter {
		s.closeInput()
		return
	}
	val := s.input.GetText()
	switch s.inputMode {
	case inputFilter:
		s.filter = strings.TrimSpace(val)
		s.closeInput()
		s.rebuildTree()
	case inputNew:
		s.doCreate(strings.TrimSpace(val))
		s.closeInput()
	case inputRename:
		s.doRename(strings.TrimSpace(val))
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
	delete(s.detailByAddr, created.Address()) // an address may be reused after a kill
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
		// A leading-dash name is silently no-op'd by the mux (getopt eats it),
		// which would diverge the model from reality — refuse it up front.
		s.flash = "rename: name cannot start with '-'"
		s.updateFooter()
		return
	}
	if err := s.ops.Rename(*sess, newName); err != nil {
		s.flash = "rename failed: " + err.Error()
		return
	}
	delete(s.detailByAddr, sess.Address())
	delete(s.detailByAddr, sess.Source+"/"+newName)
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
			delete(s.detailByAddr, sess.Address()) // don't serve a dead session's cached detail
			s.groups = RemoveSession(s.groups, sess.Address())
			s.detailAddr = ""
			s.rebuildTree()
		}
	}
	s.updateFooter()
}

// --- refresh ----------------------------------------------------------------

func (s *switcher) doRefresh() {
	groups, err := s.ops.Refresh()
	if err != nil {
		s.flash = "refresh failed: " + err.Error()
		return
	}
	s.groups = groups
	s.detailByAddr = map[string]detailEntry{}
	s.detailAddr = ""
	s.rebuildTree()
}

// --- footer / quit ----------------------------------------------------------

func (s *switcher) quit() {
	s.chosen = nil
	s.app.Stop()
}

func (s *switcher) updateFooter() {
	if s.pendingKill != nil {
		s.footer.SetText(fmt.Sprintf(" kill %s? [y]es / [n]o", s.pendingKill.Address()))
		return
	}
	if s.flash != "" {
		s.footer.SetText(" " + s.flash)
		return
	}
	s.footer.SetText(" enter switch · n new · R rename · x kill · / filter · r refresh · q quit")
}

// renderText draws the live layout to an off-screen simulation screen and
// flattens it to text — the payload the control channel's `dump` returns.
func (s *switcher) renderText() string {
	w, h := 90, 24
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

// reverseStyle is the selection highlight: a true reverse-video attribute, which
// the terminal renders by swapping its own fg/bg — visible on any theme, unlike
// swapping ColorDefault with ColorDefault (which is a no-op).
var reverseStyle = tcell.StyleDefault.Reverse(true)

func applyTheme() {
	tview.Styles.PrimitiveBackgroundColor = tcell.ColorDefault
	tview.Styles.ContrastBackgroundColor = tcell.ColorDefault
	tview.Styles.MoreContrastBackgroundColor = tcell.ColorDefault
	tview.Styles.PrimaryTextColor = tcell.ColorDefault
	tview.Styles.BorderColor = tcell.ColorDefault
	tview.Styles.TitleColor = tcell.ColorDefault
}
