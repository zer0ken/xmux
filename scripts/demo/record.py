"""Records one demo scenario on the laptop machine as an asciicast plus a key log.

Runs inside the demo container as the `dev` user. A bash shell runs on a pseudo
terminal; keys are written at a fixed pace and the next command waits until the
screen shows the expected text, so the recording carries the real latency of ssh,
tmux, and xmux. The terminal emulator answers the queries a TUI sends at startup
(cursor position, device attributes, colors), because a TUI that never hears back
waits for a timeout before it draws.

usage: python3 record.py <scenario> <out-dir>
"""
import fcntl, json, os, pty, re, select, struct, sys, termios, threading, time

import pyte

TYPE = 0.11       # typing a command
KEY = 0.45        # pressing one key in the app
CHORD = 0.6       # after the prefix, before its command key
HOLD = 0.3        # a held Ctrl-arrow; inside the app's 400 ms resize repeat window
REACT = 0.6       # after the screen answers, before the next command

# The nav leaves room for card padding around "3 my-important-session".
# A band holds a source title, its two cards, and the status line (4 rows).
NAV_WIDTH = 26
NAV_HEIGHT = 4

KEYS = {
    "Enter": b"\r", "Tab": b"\t", "Down": b"\x1b[B", "C-g": b"\x07",
    "Ctrl →": b"\x1b[1;5C", "Ctrl ←": b"\x1b[1;5D", "Ctrl ↑": b"\x1b[1;5A", "Ctrl ↓": b"\x1b[1;5B",
    "Space": b" ", "Left": b"\x1b[D",
}
LABELS = {"Enter": "Enter ⏎", "Down": "↓", "Left": "←"}

# The password-only server of the login scenario; make-gifs.sh gives it this password.
PASSWORD_HOST = "db-01"
PASSWORD = "demo-pass"

QUERIES = re.compile(rb"\x1b\[6n|\x1b\[0?c|\x1b\[\?u|\x1b\](1[01]);\?(?:\x07|\x1b\\)")


class Term:
    """A shell on a pseudo terminal, with a screen model and an output log."""

    def __init__(self, cols, rows):
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        self.events, self.keys, self.captions = [], [], []
        self.t0 = time.perf_counter()
        pid, self.fd = pty.fork()
        if pid == 0:
            env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(cols), LINES=str(rows))
            os.execvpe("bash", ["bash", "-l"], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.pid = pid
        threading.Thread(target=self._pump, daemon=True).start()

    def now(self):
        return time.perf_counter() - self.t0

    def _pump(self):
        while True:
            r, _, _ = select.select([self.fd], [], [], 0.5)
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            with self.lock:
                self.events.append((self.now(), data))
                self.stream.feed(data)
                for m in QUERIES.finditer(data):
                    self._answer(m)

    def _answer(self, m):
        q = m.group(0)
        if q == b"\x1b[6n":
            reply = b"\x1b[%d;%dR" % (self.screen.cursor.y + 1, self.screen.cursor.x + 1)
        elif q.startswith(b"\x1b[") and q.endswith(b"c"):
            reply = b"\x1b[?62;22c"
        elif q == b"\x1b[?u":
            reply = b"\x1b[?0u"
        else:
            color = b"cdcd/d6d6/f4f4" if m.group(1) == b"10" else b"1e1e/1e1e/2e2e"
            reply = b"\x1b]" + m.group(1) + b";rgb:" + color + b"\x1b\\"
        os.write(self.fd, reply)

    def text(self):
        with self.lock:
            return "\n".join(self.screen.display)

    def wait(self, needle, timeout=30):
        """Blocks until `needle` is on screen; returns the time it appeared."""
        end = time.perf_counter() + timeout
        while needle not in self.text():
            if time.perf_counter() > end:
                raise SystemExit(f"timed out waiting for {needle!r}:\n{self.text()}")
            time.sleep(0.005)
        return self.now()

    def close(self):
        try:
            os.kill(self.pid, 1)
        except OSError:
            pass


class Script:
    """Paces keys on the terminal's clock and logs each one with its label."""

    def __init__(self, term):
        self.term, self.due = term, 0.0

    def _at_due(self):
        while self.term.now() < self.due:
            time.sleep(0.002)

    def key(self, name, gap=KEY, label=None):
        self._at_due()
        os.write(self.term.fd, KEYS.get(name, name.encode()))
        self.term.keys.append((self.term.now(), label or LABELS.get(name, name)))
        self.due = self.term.now() + gap

    def type(self, s, gap=TYPE):
        for c in s:
            self.key(c, gap, "␣" if c == " " else c)

    def prefix(self):
        self.key("C-g", CHORD)

    def caption(self, text):
        self.term.captions.append((max(self.due, self.term.now()), text))

    def hold(self, seconds):
        self.due = max(self.due, self.term.now() + seconds)

    def answered(self, needle, react=REACT):
        """Waits for the screen, then holds as a person reading it would."""
        t = self.term.wait(needle)
        self.hold(react)
        return t


def fresh_app_state():
    """Starts xmux with no remembered selection and the demo nav size.

    The demo user has seen the first-key introduction, so no scenario depends on
    which one runs first after the machines start.
    """
    state = os.path.expanduser("~/.xmux")
    os.makedirs(state, exist_ok=True)
    for name in ("last_session", "nav_position", "auto_hide_nav", "nav_collapsed"):
        try:
            os.remove(os.path.join(state, name))
        except FileNotFoundError:
            pass
    for name, value in (("nav_width", NAV_WIDTH), ("nav_height", NAV_HEIGHT),
                        ("first_key_help_seen", 1)):
        with open(os.path.join(state, name), "w") as f:
            f.write(str(value))


def compare_manual(s):
    s.type("ssh gpu-01"); s.key("Enter", TYPE)
    s.answered("dev@gpu-01:")
    s.type("tmux ls"); s.key("Enter", TYPE)
    s.answered("train-llm: 1 windows")
    s.type("tmux attach -t my-important-session"); s.key("Enter", TYPE)
    return s.term.wait("epoch 17/50")


def compare_xmux(s):
    fresh_app_state()
    s.type("xmux"); s.key("Enter", TYPE)
    s.answered("3  gpu-01/tmux/my-important-session")
    s.key("Down"); s.key("Down"); s.key("Enter", TYPE)
    enter = s.term.keys[-1][0]
    return max(s.term.wait("epoch 17/50"), enter)


def features(s):
    fresh_app_state()
    s.caption("Open a session from the landing screen")
    s.type("xmux"); s.key("Enter", TYPE)
    s.answered("3  gpu-01/tmux/my-important-session", 1.4)
    s.key("Down"); s.key("Down"); s.hold(0.6); s.key("Enter")
    s.answered("epoch 17/50", 1.6)
    s.prefix(); s.key("Tab")
    s.hold(1.0)

    s.caption("Switch sessions")
    s.key("Down"); s.hold(1.8)
    s.prefix(); s.type("5", KEY); s.key("Enter")
    s.answered("listening on :8080", 1.8)
    s.prefix(); s.type("3", KEY); s.key("Enter")
    s.answered("epoch 17/50", 2.0)

    s.caption("Walk up to the host and the machine")
    s.prefix(); s.key("Left"); s.hold(0.6)
    s.key("Ctrl ↑"); s.answered("live updates", 2.0)
    s.key("Ctrl ↑"); s.answered("last reached", 2.0)
    s.key("Ctrl ↓"); s.answered("live updates", 1.4)
    s.key("Ctrl ↓"); s.answered("epoch 17/50", 2.0)

    s.caption("Resize the nav")
    s.prefix()
    for _ in range(10):
        s.key("Ctrl →", HOLD)
    s.hold(1.8)
    s.prefix()
    for _ in range(10):
        s.key("Ctrl ←", HOLD)
    s.hold(2.0)

    s.caption("Place the nav")
    for _ in range(4):
        s.prefix(); s.type("p", KEY)
        s.hold(2.2)

    s.caption("Auto-hide the nav")
    s.prefix(); s.type("t", KEY); s.hold(1.6)
    s.key("Enter"); s.hold(2.4)
    s.prefix(); s.key("Tab"); s.hold(2.0)
    s.prefix(); s.type("t", KEY); s.hold(2.0)

    s.caption("")
    s._at_due()
    return s.term.now()


def login(s):
    """Logs in to the password-only server, registers the key, and opens a session
    through the machine screen's host link.

    The server joins the ssh config only here, so the other scenarios never show it.
    """
    fresh_app_state()
    with open(os.path.expanduser("~/.ssh/config"), "a") as f:
        f.write(f"\nHost {PASSWORD_HOST}\n  User dev\n  StrictHostKeyChecking no\n"
                "  UserKnownHostsFile /dev/null\n  LogLevel ERROR\n")
    s.caption("Log in to a password machine")
    s.type("xmux"); s.key("Enter", TYPE)
    s.answered(f"7  {PASSWORD_HOST}  login needed", 1.0)
    s.prefix(); s.type("7", KEY); s.key("Enter")
    s.answered("register my public key", 1.6)
    # The pane opens on the address; three stops down is the password.
    for _ in range(3):
        s.key("Tab", 0.25)
    for c in PASSWORD:
        s.key(c, TYPE, "•")
    s.hold(0.4)
    s.key("Tab"); s.key("Tab"); s.key("Space", KEY, "Space"); s.key("Tab"); s.key("Enter")
    s.answered("✓ public key registered", 2.4)
    # The machine screen stays; its host link opens the host screen, whose links are
    # the machine and then each session.
    s.key("Down"); s.key("Enter")
    s.answered("host db-01/tmux", 1.4)
    s.key("Down"); s.key("Down"); s.key("Down"); s.key("Enter")
    s.answered("accepting connections", 2.0)
    s._at_due()
    return s.term.now()


SCENARIOS = {
    "compare-manual": (80, 22, compare_manual),
    "compare-xmux": (80, 22, compare_xmux),
    "features": (100, 28, features),
    "login": (100, 28, login),
}


def main():
    name, out = sys.argv[1], sys.argv[2]
    cols, rows, run = SCENARIOS[name]
    term = Term(cols, rows)
    term.wait("$ ")
    time.sleep(0.5)
    done = run(Script(term))
    time.sleep(2.0)
    term.close()
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, name + ".cast"), "w", encoding="utf-8") as f:
        f.write(json.dumps({"version": 2, "width": cols, "height": rows}) + "\n")
        for t, data in term.events:
            f.write(json.dumps([round(t, 4), "o", data.decode("utf-8", "replace")]) + "\n")
    with open(os.path.join(out, name + ".json"), "w", encoding="utf-8") as f:
        json.dump({"keys": [[round(t, 4), k] for t, k in term.keys],
                   "captions": [[round(t, 4), c] for t, c in term.captions],
                   "done": round(done, 4)}, f, ensure_ascii=False, indent=1)
    print(f"{name}: done at {done - term.keys[0][0]:.2f}s after the first key")


if __name__ == "__main__":
    main()
