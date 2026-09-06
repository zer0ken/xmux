//! An interactive PTY console: a child that talks to a terminal, run on a PTY that xmux
//! answers for. This module is the mechanics only - it names no mux, no transport, and no
//! app state, so what the child actually is stays the caller's business.
//!
//! A console is not an [`Attachment`](super::attachment::Attachment). An attachment is
//! a live mux client the registry keys by display address, reaps on an id, and keeps a
//! stale grid for; a console is one bounded conversation whose spawner owns it from the
//! open to the exit code and which nothing else can reach. They share the PTY
//! mechanics - the control thread that owns the writer and the master so no blocking
//! write reaches the caller's thread, and the terminal-query answers a child needs when
//! no real terminal sits behind the PTY - and differ in who owns the result.
//!
//! The reader thread hands every chunk it read to the caller, because a caller that must
//! RECOGNISE something in the stream (a prompt, a marker) needs the bytes in the order
//! they arrived. The same chunks feed a vt100 grid the console keeps to itself: a child
//! that asks where the cursor is gets an answer from it, and nothing outside reads it.

use std::io::Read;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

use super::attachment::{
    pty_control_loop, query_responses, trailing_partial_query, MasterSink, PtyCmd,
};
use super::grid::Grid;

/// A live console: the channel input rides to the child, and the child itself, whose
/// exit code is the only verdict this layer knows.
pub struct Console {
    /// Queues input to the control thread, so writing never blocks the caller.
    control_tx: Sender<PtyCmd>,
    child: Box<dyn Child + Send + Sync>,
}

impl Console {
    /// Queues `bytes` to the child.
    pub fn input(&self, bytes: Vec<u8>) {
        let _ = self.control_tx.send(PtyCmd::Input(bytes));
    }

    /// Kills the child. The reader thread ends on the master EOF that follows.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Waits for the child and returns its exit code, dropping the control channel first
    /// so the control thread releases the master. Blocking, and bounded only by the
    /// child: call it from a thread that may wait.
    pub fn wait(mut self) -> Option<u32> {
        drop(self.control_tx);
        self.child.wait().ok().map(|s| s.exit_code())
    }
}

/// Opens a PTY at `cols`x`rows`, spawns `argv` in it with `env_clear`'s keys removed
/// from the child's environment, and starts the control thread and the reader thread.
///
/// Returns the console and the receiver carrying every chunk the reader read, in order.
/// The receiver disconnects when the child's master hits EOF, which is how a caller
/// learns the conversation is over without polling the child.
///
/// Blocking: it opens a PTY and spawns a process, both of which wait on the OS. Call it
/// off the runtime thread.
pub fn spawn_console(
    argv: &[String],
    cols: u16,
    rows: u16,
    env_clear: &[String],
) -> anyhow::Result<(Console, Receiver<Vec<u8>>)> {
    let grid = Arc::new(Mutex::new(Grid::new(rows, cols)));
    anyhow::ensure!(!argv.is_empty(), "spawn_console: argv must not be empty");
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    for k in env_clear {
        cmd.env_remove(k);
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let (control_tx, control_rx) = channel::<PtyCmd>();
    std::thread::spawn(move || pty_control_loop(control_rx, MasterSink::new(writer, pair.master)));

    let (tap_tx, tap_rx) = channel::<Vec<u8>>();
    let read_grid = grid.clone();
    let read_ctl = control_tx.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Carries an incomplete trailing query to the next read, so a query split across
        // reads is still answered, and a complete one is never answered twice.
        let mut qtail: Vec<u8> = Vec::new();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let cursor = {
                let mut g = match read_grid.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                g.feed(&buf[..n]);
                g.cursor()
            };
            qtail.extend_from_slice(&buf[..n]);
            let resp = query_responses(&qtail, cursor);
            if !resp.is_empty() {
                let _ = read_ctl.send(PtyCmd::Input(resp));
            }
            let keep = trailing_partial_query(&qtail).len();
            let cut = qtail.len() - keep;
            qtail.drain(0..cut);
            if tap_tx.send(buf[..n].to_vec()).is_err() {
                break; // nobody is listening any more
            }
        }
    });

    Ok((Console { control_tx, child }, tap_rx))
}

/// A console over a real PTY, driven by `/bin/sh`, so the mechanics are exercised
/// rather than mocked. Unix only: the child is a POSIX shell.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Everything the tap carried until the child was gone.
    fn drain(tap: &Receiver<Vec<u8>>) -> String {
        let mut seen = Vec::new();
        while let Ok(chunk) = tap.recv_timeout(Duration::from_secs(5)) {
            seen.extend_from_slice(&chunk);
        }
        String::from_utf8_lossy(&seen).into_owned()
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    /// The tap carries what the child wrote, in the order it wrote it. That is what lets
    /// a caller recognise a prompt: the bytes are the conversation.
    #[test]
    fn the_tap_carries_what_the_child_wrote() {
        let (console, tap) =
            spawn_console(&sh("printf 'hello console'"), 40, 4, &[]).expect("spawn");
        let seen = drain(&tap);
        assert!(
            seen.contains("hello console"),
            "the tap carried the child's bytes: {seen:?}"
        );
        assert_eq!(console.wait(), Some(0), "the exit code is the verdict");
    }

    /// The tap disconnects when the child is gone, which is how a caller learns the
    /// conversation ended without polling the child.
    #[test]
    fn the_tap_disconnects_when_the_child_exits() {
        let (console, tap) = spawn_console(&sh("exit 3"), 20, 3, &[]).expect("spawn");
        drain(&tap);
        assert_eq!(console.wait(), Some(3), "the child's own code comes back");
    }

    /// What the caller types reaches the child. This is how a recognised prompt gets its
    /// answer.
    #[test]
    fn input_reaches_the_child() {
        let (console, tap) =
            spawn_console(&sh("read line; printf 'got:%s' \"$line\""), 40, 4, &[]).expect("spawn");
        console.input(b"typed\n".to_vec());
        let seen = drain(&tap);
        assert!(
            seen.contains("got:typed"),
            "the child read what was typed: {seen:?}"
        );
    }

    /// A child that asks the terminal where the cursor is gets an answer, because no
    /// real terminal sits behind this PTY. Without it such a child waits forever and says
    /// nothing more. The answer is visible as the tty echoes what xmux wrote back.
    #[test]
    fn the_console_answers_a_terminal_query_itself() {
        // Ask for the cursor position on a fresh grid, whose cursor is at 1;1.
        let (console, tap) =
            spawn_console(&sh("printf '\\033[6n'; sleep 1"), 40, 4, &[]).expect("spawn");
        let seen = drain(&tap);
        assert!(
            seen.contains("[1;1R"),
            "the console reported the cursor position to the child: {seen:?}"
        );
        let _ = console.wait();
    }
}
