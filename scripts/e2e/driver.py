"""Runs one program on a pseudo terminal and reads its screen.

The program draws into a pyte screen model; keys are written to the terminal, and every
step that depends on the program waits until the screen shows the expected text, up to
a timeout. The terminal answers the queries a TUI sends at startup (cursor position,
device attributes, colours), because a TUI that never hears back waits for a timeout
before it draws.

On Linux the program runs on a pty from the standard library; on Windows it runs on a
ConPTY through pywinpty.
"""
import os
import re
import sys
import threading
import time

import pyte

KEYS = {
    "Enter": "\r", "Esc": "\x1b", "Tab": "\t", "Backspace": "\x7f",
    "Up": "\x1b[A", "Down": "\x1b[B", "Right": "\x1b[C", "Left": "\x1b[D",
    "Home": "\x1b[H", "End": "\x1b[F", "C-g": "\x07", "C-u": "\x15",
}

QUERIES = re.compile(r"\x1b\[6n|\x1b\[0?c|\x1b\[\?u|\x1b\](1[01]);\?(?:\x07|\x1b\\)")


class Timeout(Exception):
    """A wait ran out; the message carries what was awaited and the screen."""


class Term:
    """One program on a pseudo terminal, with a screen model."""

    def __init__(self, argv, env, cols=120, rows=36, cwd=None):
        self.cols, self.rows = cols, rows
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.lock = threading.Lock()
        self.alive = True
        if os.name == "nt":
            from winpty import PtyProcess
            self.proc = PtyProcess.spawn(argv, dimensions=(rows, cols), env=env, cwd=cwd)
            self._read = lambda: self.proc.read(65536)
            self._write = self.proc.write
        else:
            import fcntl, pty, struct, termios
            pid, fd = pty.fork()
            if pid == 0:
                if cwd:
                    os.chdir(cwd)
                os.execvpe(argv[0], argv, env)
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            self.pid, self.fd = pid, fd
            self.decoder = __import__("codecs").getincrementaldecoder("utf-8")("replace")
            self._read = self._posix_read
            self._write = lambda s: os.write(fd, s.encode())
        threading.Thread(target=self._pump, daemon=True).start()

    def _posix_read(self):
        import select
        r, _, _ = select.select([self.fd], [], [], 0.5)
        if not r:
            return ""
        data = os.read(self.fd, 65536)
        if not data:
            raise EOFError
        return self.decoder.decode(data)

    def _pump(self):
        while True:
            try:
                data = self._read()
            except (OSError, EOFError):
                self.alive = False
                return
            if not data:
                continue
            with self.lock:
                self.stream.feed(data)
                for m in QUERIES.finditer(data):
                    self._answer(m)

    def _answer(self, m):
        q = m.group(0)
        if q == "\x1b[6n":
            reply = "\x1b[%d;%dR" % (self.screen.cursor.y + 1, self.screen.cursor.x + 1)
        elif q.startswith("\x1b[") and q.endswith("c"):
            reply = "\x1b[?62;22c"
        elif q == "\x1b[?u":
            reply = "\x1b[?0u"
        else:
            colour = "cdcd/d6d6/f4f4" if m.group(1) == "10" else "1e1e/1e1e/2e2e"
            reply = "\x1b]" + m.group(1) + ";rgb:" + colour + "\x1b\\"
        self._write(reply)

    def lines(self):
        with self.lock:
            rows = []
            for y, text in enumerate(self.screen.display):
                row = Row(text)
                row.reversed = frozenset(x for x, ch in self.screen.buffer[y].items() if ch.reverse)
                rows.append(row)
            return rows

    def cursor(self):
        with self.lock:
            return self.screen.cursor.y, self.screen.cursor.x

    def text(self):
        return "\n".join(self.lines())

    def send(self, *keys, gap=0.15):
        """Writes each key (a name from KEYS or literal text), pausing between them."""
        for k in keys:
            self._write(KEYS.get(k, k))
            time.sleep(gap)

    def wait(self, test, what, timeout=30):
        """Blocks until `test(lines)` is truthy and returns its value."""
        end = time.monotonic() + timeout
        while True:
            got = test(self.lines())
            if got:
                return got
            if time.monotonic() > end:
                raise Timeout(f"timed out after {timeout}s waiting for {what}\n{self.text()}")
            if not self.alive:
                raise Timeout(f"the program exited while waiting for {what}\n{self.text()}")
            time.sleep(0.05)

    def wait_text(self, needle, timeout=30):
        return self.wait(lambda ls: needle in "\n".join(ls), repr(needle), timeout)

    def wait_gone(self, needle, timeout=30):
        return self.wait(lambda ls: needle not in "\n".join(ls), f"{needle!r} to go", timeout)

    def close(self):
        if os.name == "nt":
            try:
                self.proc.terminate(force=True)
            except Exception:
                pass
        else:
            try:
                os.kill(self.pid, 9)
                os.waitpid(self.pid, 0)
            except OSError:
                pass


class Row(str):
    """A screen row's text and the columns whose cell is reverse video, the look of the
    hard selection."""

    reversed = frozenset()


# The nav is the left column up to its view border. A section title is `host/mux` at
# column 0 and its session cards follow it; a card is a number and a name, and the
# selected card's cells are reversed. The host cards come after a blank row, outside any
# section.
BORDER = "│"
CARD = re.compile(r"^\s*(\d+)\s+(\S+)")


def nav_lines(lines):
    out = []
    for ln in lines[:-1]:
        cut = ln.find(BORDER)
        out.append((ln[:cut] if cut >= 0 else ln).rstrip())
    return out


def nav_cards(lines):
    """Every card on screen as (number or None when selected, section, name)."""
    cards, section = [], None
    rows = nav_lines(lines)
    for i, ln in enumerate(rows):
        if not ln.strip():
            section = None
            continue
        # A title sits at column 0 and carries no number; a host card at column 0 does.
        words = ln.split()
        if not ln[0].isspace() and "/" in words[0] and not words[0].isdigit():
            section = words[0]
            continue
        m = CARD.match(ln)
        if m:
            selected = m.start(1) in getattr(lines[i], "reversed", ())
            num = None if selected else int(m.group(1))
            cards.append((num, section, m.group(2)))
    return cards


def find_card(lines, section, name):
    """The (number, selected) of the card `name` under `section` (None for a host card)."""
    for num, sec, nm in nav_cards(lines):
        if sec == section and nm == name:
            return (num, num is None)
    return None


if __name__ == "__main__":
    sys.exit("driver.py is a module; run suite.py")
