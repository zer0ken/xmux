"""End-to-end scenarios for xmux against Docker hosts, across muxes and host systems.

Starts the host containers, runs every scenario for every mux and host system with a
fresh xmux in a pseudo terminal, prints a pass/fail table, and removes the containers.
Exits non-zero when any cell fails. `run.sh` starts it inside the Linux client
container; the Windows client run starts it on the Windows machine itself.

usage: python3 suite.py [--client linux|windows] [--os debian,alpine]
                        [--mux tmux,...] [--scenario first-launch,...] [--out DIR]
"""
import argparse
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor

import driver

MUXES = ["tmux", "screen", "zellij", "abduco", "tuios", "herdr"]
SYSTEMS = {"debian": "deb", "alpine": "alp"}
NETWORK = "xmux-e2e"
PREFIX = "xmux-e2e-"
PASSWORD = "e2e-pass"
COLS, ROWS = 140, 60

# Every cell of a known xmux defect or limitation, by (scenario or None for every
# scenario, mux, host system or None for every system). Such a cell still runs; it
# reports KNOWN when it fails and PASS when it passes.
KNOWN = {
    (None, "screen", "alpine"): "#588",
    ("in-client-switch", "tuios", None): "#333",
}
# Cells that do not apply: the mux cannot move a client between sessions (a screen,
# abduco, or herdr client belongs to one session's server), and abduco has no keys of its
# own beyond the detach key `detach-inside` covers.
NOT_APPLICABLE = {("in-client-switch", "screen"), ("in-client-switch", "abduco"),
                  ("in-client-switch", "herdr"), ("native-keys", "abduco")}

# The in-client keys that detach a mux's own client, and the input that moves the client
# from <mux>1 to <mux>2 from inside it: tmux's next-session key, zellij's action, which
# acts on the one client the pane has, and tuios's next-session key (Alt+Shift+N).
DETACH = {"tmux": "\x02d", "screen": "\x01d", "zellij": "\x0fd", "abduco": "\x1c",
          "tuios": "\x02d", "herdr": "\x02q"}
INSIDE_SWITCH = {"tmux": ["\x02", ")"], "zellij": ["zellij action switch-session zellij2", "Enter"],
                 "tuios": ["\x1bN"]}
# A client attached directly on the host, beside xmux's own.
DIRECT_ATTACH = {"tmux": "tmux attach -t {s}", "screen": "screen -x {s}",
                 "zellij": "zellij attach {s}", "abduco": "abduco -a {s}",
                 "tuios": "tuios attach {s}", "herdr": "herdr session attach {s}"}
LISTING = {"tmux": "tmux ls -F '#S'", "screen": "screen -ls", "zellij": "zellij ls -n",
           "abduco": "abduco", "tuios": "tuios ls --json", "herdr": "herdr session list --json"}

LOG_LOCK = threading.Lock()


def log(msg):
    with LOG_LOCK:
        print(time.strftime("%H:%M:%S"), msg, flush=True)


def docker(*args, check=True, input=None, timeout=120):
    r = subprocess.run(["docker", *args], input=input, capture_output=True, text=True,
                       timeout=timeout)
    if check and r.returncode != 0:
        raise RuntimeError(f"docker {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


class Failure(Exception):
    """A scenario check that did not hold."""


# ---------------------------------------------------------------------------- hosts

class Hosts:
    """The host containers: two key-login hosts and one password-only host per system."""

    def __init__(self, systems, publish, attach_self):
        self.systems, self.publish, self.attach_self = systems, publish, attach_self
        self.started, self.network_created = [], False

    @staticmethod
    def aliases(system):
        short = SYSTEMS[system]
        return f"{short}-1", f"{short}-2", f"{short}-pw"

    def up(self, pubkey):
        if docker("network", "ls", "-q", "-f", f"name=^{NETWORK}$").strip() == "":
            docker("network", "create", NETWORK)
            self.network_created = True
        if self.attach_self:
            docker("network", "connect", NETWORK, self.attach_self, check=False)
        jobs = [(s, a) for s in self.systems for a in self.aliases(s)]
        with ThreadPoolExecutor(len(jobs)) as pool:
            list(pool.map(lambda j: self._start(j[0], j[1], pubkey), jobs))

    def _start(self, system, alias, pubkey):
        name = PREFIX + alias
        docker("rm", "-f", name, check=False)
        args = ["run", "-d", "--name", name, "--label", "xmux-e2e", "--hostname", alias,
                "--network", NETWORK, "--network-alias", alias]
        if self.publish:
            args += ["-p", "127.0.0.1::22"]
        docker(*args, f"{PREFIX}host:{system}")
        self.started.append(name)
        if not alias.endswith("-pw"):
            docker("exec", "-i", name, "sh", "-c",
                   "cat > /home/dev/.ssh/authorized_keys && chown dev /home/dev/.ssh/authorized_keys"
                   " && chmod 600 /home/dev/.ssh/authorized_keys", input=pubkey)
        self.seed(alias)

    def seed(self, alias):
        """Waits for sshd and starts every mux's sessions on a fresh container."""
        name = PREFIX + alias
        end = time.monotonic() + 30
        while docker("exec", name, "sh", "-c", "ls /run/sshd.pid /var/run/sshd.pid 2>/dev/null",
                     check=False).strip() == "":
            if time.monotonic() > end:
                raise RuntimeError(f"sshd did not start on {alias}")
            time.sleep(0.2)
        self.sh(alias, "sh /opt/e2e/sessions.sh " + " ".join(MUXES))

    def sh(self, alias, command, check=True):
        """Runs a command as the remote user through a login shell on the host."""
        return docker("exec", "-u", "dev", "-w", "/home/dev", PREFIX + alias, "sh", "-lc",
                      command, check=check)

    def stop(self, alias):
        docker("stop", "-t", "1", PREFIX + alias)

    def start(self, alias):
        docker("start", PREFIX + alias)
        self.seed(alias)

    def port(self, alias):
        return docker("port", PREFIX + alias, "22/tcp").split("\n")[0].rsplit(":", 1)[1].strip()

    def has_session(self, alias, mux, session):
        out = self.sh(alias, LISTING[mux], check=False)
        if mux == "herdr":
            return re.search(r'"name":"%s","running":true' % re.escape(session), out) is not None
        return re.search(r"(^|[\s.\"])%s($|[\s\"])" % re.escape(session), out, re.M) is not None

    def down(self):
        for name in self.started:
            docker("rm", "-f", name, check=False)
        if self.attach_self:
            docker("network", "disconnect", "-f", NETWORK, self.attach_self, check=False)
        if self.network_created:
            docker("network", "rm", NETWORK, check=False)


# --------------------------------------------------------------------------- clients

XMUX_CONFIG = """[discovery]
neighbors = false
wsl = false

[update]
check = false
"""


def ssh_stanza(alias, hostname, port=None):
    lines = [f"Host {alias}", f"  HostName {hostname}", "  User dev",
             "  StrictHostKeyChecking no", "  UserKnownHostsFile /dev/null", "  LogLevel ERROR"]
    if port:
        lines += [f"  Port {port}", f"  HostKeyAlias {alias}"]
    return "\n".join(lines) + "\n"


class LinuxClient:
    """Runs xmux inside this container as a fresh user, so ssh and xmux share one home."""

    def __init__(self, xmux, workdir):
        self.xmux, self.workdir = xmux, workdir
        self.key = os.path.join(workdir, "id_ed25519")
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "e2e", "-f",
                        self.key], check=True)
        self.count, self.lock = 0, threading.Lock()

    def pubkey(self):
        return open(self.key + ".pub").read()

    def launch(self, hosts, aliases):
        with self.lock:
            self.count += 1
            user = f"e2e{self.count}"
        subprocess.run(["useradd", "-m", "-s", "/bin/bash", user], check=True)
        home = f"/home/{user}"
        os.makedirs(f"{home}/.ssh")
        os.makedirs(f"{home}/.config/xmux")
        shutil.copy(self.key, f"{home}/.ssh/id_ed25519")
        shutil.copy(self.key + ".pub", f"{home}/.ssh/id_ed25519.pub")
        with open(f"{home}/.ssh/config", "w") as f:
            f.write("".join(ssh_stanza(a, a) for a in aliases))
        with open(f"{home}/.config/xmux/config.toml", "w") as f:
            f.write(XMUX_CONFIG)
        subprocess.run(["chown", "-R", f"{user}:{user}", home], check=True)
        os.chmod(f"{home}/.ssh/id_ed25519", 0o600)
        env = {"HOME": home, "USER": user, "LOGNAME": user, "SHELL": "/bin/bash",
               "PATH": "/usr/local/bin:/usr/bin:/bin", "TERM": "xterm-256color",
               "LANG": "en_US.UTF-8"}
        argv = ["setpriv", f"--reuid={user}", f"--regid={user}", "--init-groups",
                self.xmux, "--name", user]
        return App(driver.Term(argv, env, COLS, ROWS, cwd=home), home)


class WindowsClient:
    """Runs the Windows xmux on this machine against the published host ports.

    Windows OpenSSH reads ~/.ssh from the profile folder whatever HOME says, so a
    wrapper named ssh.exe first on PATH hands the real ssh the temporary config with -F.
    """

    def __init__(self, xmux, workdir, hosts):
        self.xmux, self.workdir, self.hosts = xmux, workdir, hosts
        self.key = os.path.join(workdir, "id_ed25519")
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "e2e", "-f",
                        self.key], check=True)
        self.bin = os.path.join(workdir, "bin")
        os.makedirs(self.bin)
        src = os.path.join(os.path.dirname(os.path.abspath(__file__)), "windows", "ssh_wrapper.rs")
        subprocess.run([os.environ.get("RUSTC", "rustc"), "-O", "-o", os.path.join(self.bin, "ssh.exe"), src], check=True)
        self.count, self.lock = 0, threading.Lock()

    def pubkey(self):
        return open(self.key + ".pub").read()

    def launch(self, hosts, aliases):
        with self.lock:
            self.count += 1
            n = self.count
        home = os.path.join(self.workdir, f"home{n}")
        os.makedirs(os.path.join(home, ".ssh"))
        os.makedirs(os.path.join(home, ".config", "xmux"))
        key = os.path.join(home, ".ssh", "id_ed25519")
        shutil.copy(self.key, key)
        shutil.copy(self.key + ".pub", key + ".pub")
        config = os.path.join(home, ".ssh", "config")
        with open(config, "w", newline="\n") as f:
            for a in aliases:
                f.write(ssh_stanza(a, "127.0.0.1", hosts.port(a)))
                f.write(f"  IdentityFile {key}\n  IdentitiesOnly yes\n  IdentityAgent none\n")
        with open(os.path.join(home, ".config", "xmux", "config.toml"), "w") as f:
            f.write(XMUX_CONFIG)
        env = dict(os.environ, HOME=home, USERPROFILE=home, TERM="xterm-256color",
                   XMUX_E2E_SSH_CONFIG=config,
                   PATH=self.bin + os.pathsep + os.environ.get("PATH", ""))
        return App(driver.Term([self.xmux, "--name", f"e2e{n}"], env, COLS, ROWS, cwd=home), home)


# ------------------------------------------------------------------------------- app

class App:
    """One running xmux and the key sequences the scenarios are written in."""

    def __init__(self, term, home):
        self.t, self.home, self.tokens = term, home, 0

    def close(self):
        self.t.close()

    def card(self, section, name, timeout=20):
        """Waits for the card and returns its number, or None when it is selected."""
        return self.t.wait(lambda ls: driver.find_card(ls, section, name),
                           f"card {name} under {section}", timeout)[0]

    def selected(self, section, name, timeout=20):
        self.t.wait(lambda ls: (driver.find_card(ls, section, name) or (0, False))[1],
                    f"selection on {name} under {section}", timeout)

    def open(self, section, name):
        """Selects a card with a jump, which also executes it from the landing screen.

        A card that already holds the selection is executed with Enter while the landing
        screen shows, and is otherwise already open.
        """
        num = self.card(section, name)
        if num is None:
            if any("machines scanned" in l for l in self.t.lines()):
                self.t.send("Enter")
                self.t.wait_gone("machines scanned", 10)
            return
        # A card's number changes while sources are still arriving or a host returns, so
        # a jump that lands elsewhere is repeated when the card's number moved meanwhile.
        for _ in range(5):
            self.t.send("C-g", gap=0.3)
            self.t.send(*str(num), gap=0.2)
            got = self.t.wait(lambda ls: next((m.group(1) for l in ls for m in
                                               [re.search(rf"card {num}\s+(\S+)", l)] if m), None),
                              f"the jump popup on card {num}", 10)
            # The popup names a card by its path: a session under its section, a machine
            # card by the machine alone.
            if got != (name if section is None else f"{section}/{name}"):
                self.t.send("Esc", gap=1.0)
            else:
                self.t.send("Enter")
                self.t.wait_gone("Enter select", 10)
                try:
                    return self.selected(section, name, 5)
                except driver.Timeout:
                    pass
            moved = self.card(section, name)
            if moved is None:
                return
            if moved == num:
                lost = next(((s, n) for k, s, n in driver.nav_cards(self.t.lines()) if k is None), None)
                raise Failure(f"the jump to card {num} ({name} under {section}) selected {lost}")
            num = moved
        raise Failure(f"card numbers kept moving away from {name} under {section}")

    def tab_to(self, label, timeout=10):
        """Presses Tab until the cursor sits on the login pane row holding `label`."""
        end = time.monotonic() + timeout
        while True:
            lines, (y, _) = self.t.lines(), self.t.cursor()
            if label in lines[y]:
                return
            if time.monotonic() > end:
                raise driver.Timeout(f"Tab never reached {label!r}\n{self.t.text()}")
            self.t.send("Tab", gap=0.4)

    def focus_terminal(self):
        self.t.send("C-g", gap=0.3)
        self.t.send("Right", gap=0.3)

    def whereami(self, path, timeout=30):
        """Types into the shown session and waits for its answer naming `path`."""
        self.tokens += 1
        token = f"t{self.tokens}x{random.randrange(1000, 9999)}"
        self.focus_terminal()
        self.t.send(f"whereami {token}", "Enter")
        self.t.wait_text(f"at={path} {token}", timeout)
        self.token = token

    def rescan(self):
        self.t.send("C-g", gap=0.3)
        self.t.send("r", gap=0.3)

    def sections(self, prefix):
        return {sec for _, sec, _ in driver.nav_cards(self.t.lines())
                if sec and sec.startswith(prefix)}

    def host_card_line(self, alias):
        for ln in driver.nav_lines(self.t.lines()):
            m = driver.CARD.match(ln)
            if m and m.group(2) == alias:
                return ln
        return None


# ------------------------------------------------------------------------- scenarios

class Cell:
    """One mux on one host system, with the hosts and client the scenarios use."""

    def __init__(self, system, mux, hosts, client, out):
        self.system, self.mux, self.hosts, self.client, self.out = system, mux, hosts, client, out
        self.h1, self.h2, self.pw = Hosts.aliases(system)
        self.other = "zellij" if mux == "tmux" else "tmux"

    def launch(self, *aliases):
        app = self.client.launch(self.hosts, aliases)
        self.apps.append(app)
        return app

    def path(self, host, session, mux=None):
        return f"{host}/{mux or self.mux}/{session}"


def first_launch(c):
    m = c.mux
    app = c.launch(c.h1, c.h2)
    app.t.wait(lambda ls: any("machines scanned" in l for l in ls), "the landing screen", 30)
    app.t.wait_text(c.path(c.h1, f"{m}1"), 40)
    app.open(f"{c.h1}/{m}", f"{m}1")
    app.t.wait_gone("machines scanned", 10)
    app.whereami(c.path(c.h1, f"{m}1"))


def switch(c):
    # The second session is shown once first, so the return attaches at the size it
    # already has and a mux that repaints only on a resize leaves the view blank.
    m, n = c.mux, c.other
    app = c.launch(c.h1, c.h2)
    app.open(f"{c.h1}/{m}", f"{m}2")
    app.whereami(c.path(c.h1, f"{m}2"))
    app.open(f"{c.h1}/{m}", f"{m}1")
    app.whereami(c.path(c.h1, f"{m}1"))
    app.open(f"{c.h2}/{n}", f"{n}1")
    app.whereami(c.path(c.h2, f"{n}1", n))
    app.open(f"{c.h1}/{m}", f"{m}2")
    app.whereami(c.path(c.h1, f"{m}2"))


def new_session(c):
    m = c.mux
    name = f"new{random.randrange(1000, 9999)}"
    app = c.launch(c.h1)
    app.open(f"{c.h1}/{m}", f"{m}1")
    app.whereami(c.path(c.h1, f"{m}1"))
    app.t.send("C-g", gap=0.3)
    app.t.send("n", gap=0.3)
    app.t.wait(lambda ls: any(re.search(rf"host {{2,}}{re.escape(c.h1)}/{m}", l) for l in ls),
               f"the new-session popover on {c.h1}/{m}", 10)
    app.t.send(name, "Enter")
    app.selected(f"{c.h1}/{m}", name, 40)
    app.whereami(c.path(c.h1, name))
    if not c.hosts.has_session(c.h1, m, name):
        raise Failure(f"{c.h1} does not list {name} as a live {m} session")


def password_login(c):
    m = c.mux
    app = c.launch(c.pw)
    app.t.wait(lambda ls: any(re.search(rf"\d+\s+{c.pw}\s+login needed", l) for l in ls),
               f"{c.pw} as login needed", 40)
    app.open(None, c.pw)
    app.t.wait(lambda ls: "address*" in ls[app.t.cursor()[0]], "the login pane focus", 10)
    app.tab_to("password ")
    app.t.send(PASSWORD, gap=0.3)
    app.tab_to("[ Log in ]")
    app.t.send("Enter")
    app.card(f"{c.pw}/{m}", f"{m}1", 40)
    app.open(f"{c.pw}/{m}", f"{m}1")
    app.whereami(c.path(c.pw, f"{m}1"))
    app.t.send("C-g", gap=0.3)
    app.t.send("L", gap=0.3)
    app.t.wait_text("type logout", 10)
    app.t.send("logout", "Enter")
    # The suite wrote the host's ssh config entry, not xmux, so the logout asks first.
    app.t.wait_text(f"Host {c.pw} goes with its options", 30)
    app.t.send("remove", "Enter")
    app.t.wait(lambda ls: not app.sections(f"{c.pw}/") and app.host_card_line(c.pw),
               f"{c.pw} as one host card", 30)
    config = open(os.path.join(app.home, ".ssh", "config")).read().splitlines()
    if f"Host {c.pw}" in config:
        raise Failure(f"logout left the ssh config entry of {c.pw}")


def unreachable(c):
    m = c.mux
    app = c.launch(c.h1, c.h2)
    app.open(f"{c.h2}/{m}", f"{m}1")
    app.whereami(c.path(c.h2, f"{m}1"))
    c.hosts.stop(c.h2)
    try:
        app.rescan()
        app.t.wait(lambda ls: not app.sections(f"{c.h2}/") and "▲" in (app.host_card_line(c.h2) or ""),
                   f"{c.h2} as one unreachable host card", 60)
    finally:
        c.hosts.start(c.h2)
    app.rescan()
    app.card(f"{c.h2}/{m}", f"{m}1", 60)
    app.open(f"{c.h2}/{m}", f"{m}1")
    app.whereami(c.path(c.h2, f"{m}1"))


def detach_inside(c):
    m = c.mux
    app = c.launch(c.h1)
    app.open(f"{c.h1}/{m}", f"{m}1")
    app.whereami(c.path(c.h1, f"{m}1"))
    app.t.send(DETACH[m], gap=1.0)
    if not c.hosts.has_session(c.h1, m, f"{m}1"):
        raise Failure(f"{m}1 is gone from {c.h1} after detaching inside the client")
    app.open(f"{c.h1}/{m}", f"{m}2")
    app.open(f"{c.h1}/{m}", f"{m}1")
    app.whereami(c.path(c.h1, f"{m}1"))


def shared_client(c):
    m = c.mux
    session, logfile = f"{m}2", f"/tmp/direct-{m}.log"
    command = f"stty cols 100 rows 30; exec {DIRECT_ATTACH[m].format(s=session)}"
    c.hosts.sh(c.h1, f"rm -f {logfile}")
    docker("exec", "-d", "-u", "dev", "-w", "/home/dev", "-e", "TERM=xterm-256color",
           PREFIX + c.h1, "sh", "-lc",
           f"sleep 100000 | script -qfc '{command}' {logfile} >/dev/null 2>&1")
    try:
        time.sleep(2)
        app = c.launch(c.h1)
        app.open(f"{c.h1}/{m}", session)
        app.whereami(c.path(c.h1, session))
        time.sleep(2)
        alive = c.hosts.sh(c.h1, "pgrep -f '[s]cript -qfc stty' >/dev/null && echo alive",
                           check=False).strip()
        if alive != "alive":
            raise Failure(f"the direct client on {c.h1} ended when xmux attached {session}")
        seen = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", c.hosts.sh(c.h1, f"cat {logfile}", check=False))
        if app.token not in seen:
            raise Failure("the direct client does not show what was typed through xmux")
    finally:
        c.hosts.sh(c.h1, "pkill -f '[s]cript -qfc stty'; pkill -f '[s]leep 100000'", check=False)


def in_client_switch(c):
    # The second host: no other scenario adds sessions there, so <mux>1 is the first
    # session of its source xmux attaches.
    m = c.mux
    app = c.launch(c.h2)
    app.open(f"{c.h2}/{m}", f"{m}1")
    app.whereami(c.path(c.h2, f"{m}1"))
    app.t.send(*INSIDE_SWITCH[m])
    app.selected(f"{c.h2}/{m}", f"{m}2", 20)
    app.whereami(c.path(c.h2, f"{m}2"))


def wait_regex(app, pattern, what, timeout=15):
    return app.t.wait(lambda ls: next((m for l in ls for m in [re.search(pattern, l)] if m), None),
                      what, timeout)


def click(app, x, y):
    """A left click at screen cell (x, y), counted from 0, as the terminal reports it."""
    app.t.send(f"\x1b[<0;{x + 1};{y + 1}M", f"\x1b[<0;{x + 1};{y + 1}m", gap=0.5)


def native_keys(c):
    """The mux's own keys for its windows, panes, and copy mode, typed through xmux."""
    m = c.mux
    name = f"nk{random.randrange(1000, 9999)}"
    c.hosts.sh(c.h1, CREATE[m].format(s=name))
    app = c.launch(c.h1)
    app.open(f"{c.h1}/{m}", name)
    app.whereami(c.path(c.h1, name))
    t = app.t
    if m == "tmux":
        t.send("\x02", "%", gap=0.5)
        t.send("tmux display -p 'panes=#{window_panes}'", "Enter")
        t.wait_text("panes=2")
        t.send("seq 1 300", "Enter", gap=0.5)
        t.send("\x02", "[", "\x1b[5~", gap=0.5)
        wait_regex(app, r"\[\d+/\d+\]", "tmux's copy mode position")
        t.send("q", gap=0.5)
        t.wait(lambda ls: not any(re.search(r"\[\d+/\d+\]", l) for l in ls), "copy mode to end")
    elif m == "screen":
        t.send("\x01", "c", gap=0.5)
        t.send("echo win=$WINDOW", "Enter")
        t.wait_text("win=1")
        t.send("\x01", "n", gap=0.5)
        t.send("echo win=$WINDOW", "Enter")
        t.wait_text("win=0")
    elif m == "zellij":
        t.send("\x10", "n", gap=1.0)
        t.wait_text("Pane #2")
        t.send("echo pane=$ZELLIJ_PANE_ID", "Enter")
        t.wait_text("pane=1")
        y, x = next((y, l.find("Pane #1")) for y, l in enumerate(t.lines()) if "Pane #1" in l)
        click(app, x, y + 3)
        t.send("echo pane=$ZELLIJ_PANE_ID", "Enter")
        t.wait_text("pane=0")
        t.send("\x14", "n", gap=1.0)
        t.wait_text("Tab #2")
    elif m == "tuios":
        t.send("\x02", "c", gap=1.0)
        t.send("tuios list-windows", "Enter")
        t.wait_text("2 window(s)")
    elif m == "herdr":
        t.send("\x02", "v", gap=1.0)
        t.send("echo panes=$(herdr pane list | grep -o '\"pane_id\"' | wc -l)", "Enter")
        t.wait_text("panes=2")


# How each mux starts a detached session for a scenario that needs one of its own.
CREATE = {"tmux": "tmux new-session -d -s {s} -x 100 -y 30", "screen": "screen -dmS {s}",
          "zellij": "zellij attach -b {s}", "tuios": "tuios new {s} --detach",
          "herdr": "nohup herdr --session {s} server >/dev/null 2>&1 &"}

# Two groups: whether xmux's own behavior works, and whether each mux's native workflow
# survives inside xmux's terminal view.
GROUPS = {
    "xmux behavior": {
        "first-launch": first_launch,
        "switch": switch,
        "new-session": new_session,
        "password-login": password_login,
        "unreachable": unreachable,
    },
    "native workflow": {
        "native-keys": native_keys,
        "detach-inside": detach_inside,
        "shared-client": shared_client,
        "in-client-switch": in_client_switch,
    },
}
SCENARIOS = {name: run for group in GROUPS.values() for name, run in group.items()}


def known(scenario, mux, system):
    for (s, m, o), reason in KNOWN.items():
        if s in (None, scenario) and m == mux and o in (None, system):
            return reason
    return None


def run_cell(scenario, cell, out):
    if (scenario, cell.mux) in NOT_APPLICABLE:
        return "n/a"
    reason = known(scenario, cell.mux, cell.system)
    cell.apps = []
    t0 = time.monotonic()
    try:
        SCENARIOS[scenario](cell)
        result = "PASS"
    except Exception as e:
        result = f"KNOWN {reason}" if reason else "FAIL"
        name = f"{scenario}-{cell.mux}-{cell.system}.txt"
        with open(os.path.join(out, name), "w", encoding="utf-8") as f:
            f.write(f"{result}\n{traceback.format_exc()}\n")
            for app in cell.apps:
                f.write("\n" + "\n".join(l.rstrip() for l in app.t.lines()) + "\n")
        log(f"{result}: {scenario} {cell.mux}/{cell.system}: {str(e).splitlines()[0]}")
    finally:
        for app in cell.apps:
            app.close()
    log(f"{scenario} {cell.mux}/{cell.system}: {result} in {time.monotonic() - t0:.0f}s")
    return result


def table(results, scenarios, columns):
    head = ["scenario"] + [f"{m} {o}" for o, m in columns]
    rows = [[s] + [results.get((s, o, m), "-") for o, m in columns] for s in scenarios]
    widths = [max(len(r[i]) for r in [head] + rows) for i in range(len(head))]
    fmt = lambda r: "| " + " | ".join(v.ljust(w) for v, w in zip(r, widths)) + " |"
    return "\n".join([fmt(head), "|" + "|".join("-" * (w + 2) for w in widths) + "|"]
                     + [fmt(r) for r in rows])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--client", choices=["linux", "windows"], default="linux")
    ap.add_argument("--os", default=",".join(SYSTEMS))
    ap.add_argument("--mux", default=",".join(MUXES))
    ap.add_argument("--scenario", default=None)
    ap.add_argument("--xmux", default=os.environ.get("XMUX_E2E_BIN", "xmux"))
    ap.add_argument("--out", default=os.environ.get("XMUX_E2E_OUT", "out"))
    args = ap.parse_args()
    systems = args.os.split(",")
    muxes = args.mux.split(",")
    default = "first-launch,switch" if args.client == "windows" else ",".join(SCENARIOS)
    scenarios = (args.scenario or default).split(",")
    os.makedirs(args.out, exist_ok=True)

    if args.client == "windows" and os.name == "nt":
        sys.exit("the Windows client run is blocked by #581: xmux on Windows keeps its config "
                 "and state in the profile folder whatever HOME says, so a run would use the "
                 "real ~/.xmux")

    workdir = tempfile.mkdtemp(prefix="xmux-e2e-")
    hosts = Hosts(systems, publish=args.client == "windows",
                  attach_self=os.environ.get("XMUX_E2E_SELF"))
    t0 = time.monotonic()
    try:
        client = (LinuxClient(args.xmux, workdir) if args.client == "linux"
                  else WindowsClient(args.xmux, workdir, hosts))
        log(f"starting hosts for {', '.join(systems)}")
        hosts.up(client.pubkey())
        log(f"hosts up in {time.monotonic() - t0:.0f}s")
        results = {}

        def run_system(system):
            for mux in muxes:
                cell = Cell(system, mux, hosts, client, args.out)
                for s in scenarios:
                    results[(s, system, mux)] = run_cell(s, cell, args.out)

        with ThreadPoolExecutor(len(systems)) as pool:
            list(pool.map(run_system, systems))
    finally:
        hosts.down()
        shutil.rmtree(workdir, ignore_errors=True)

    columns = [(o, m) for o in systems for m in muxes]
    report = "\n\n".join(
        f"{group}\n\n" + table(results, [s for s in names if s in scenarios], columns)
        for group, names in GROUPS.items() if set(names) & set(scenarios))
    print("\n" + report)
    with open(os.path.join(args.out, "results.md"), "w", encoding="utf-8") as f:
        f.write(report + "\n")
    failed = [k for k, v in results.items() if v == "FAIL"]
    print(f"\n{len(failed)} failed, {time.monotonic() - t0:.0f}s")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
