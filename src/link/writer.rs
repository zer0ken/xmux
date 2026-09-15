//! The control-mode writer: drains queued `HostCmd`s to the child, pushing one
//! in-flight correlation entry per command line to stay in lockstep with the reader.

use std::io::Write;

use crate::mux::ControlProtocol;

use super::{HostCmd, InFlight, PendingReply};

/// Drains the command channel, writing exact command bytes to `w` and pushing ONE
/// correlation entry per command LINE, so the FIFO stays in lockstep with the
/// `%begin` blocks the reader pops. The correlator is pushed BEFORE the bytes
/// reach the child, so the reader can never observe the reply's `%begin` before
/// its FIFO entry exists (the writer thread and the reader thread race otherwise).
/// A write error means the pipe is broken (the child died) — record which host's
/// channel went and return so no further stale entries are queued; the reader hits
/// EOF and the client is reaped. Returning drops `rx`, so every later send on this
/// host's channel reports the refusal to its caller rather than looking delivered.
/// Flushes after each command so a real child sees it promptly. Returns on `Shutdown`.
pub fn run_writer<W: Write>(
    host: &str,
    rx: std::sync::mpsc::Receiver<HostCmd>,
    proto: &dyn ControlProtocol,
    w: &mut W,
    in_flight: &InFlight,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            HostCmd::Send(line) => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if let Err(e) = w.write_all(line.as_bytes()) {
                    tracing::warn!(host, error = %e, "control_writer_broken");
                    return;
                }
            }
            HostCmd::Resize { cols, rows } => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if let Err(e) = w.write_all(proto.size_line(cols, rows).as_bytes()) {
                    tracing::warn!(host, error = %e, "control_writer_broken");
                    return;
                }
            }
            HostCmd::Query { line, reply } => {
                in_flight.lock().unwrap().push_back(reply);
                if let Err(e) = w.write_all(line.as_bytes()) {
                    tracing::warn!(host, error = %e, "control_writer_broken");
                    return;
                }
            }
            HostCmd::Shutdown => return,
        }
        let _ = w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::test_control_proto;

    #[test]
    fn writer_serializes_commands_and_correlates() {
        // The writer writes each command's exact bytes and pushes ONE Ignore
        // correlator per command line, keeping the FIFO in lockstep with the
        // `%begin` blocks the reader pops.
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Send("refresh-client -f no-output\n".to_string()))
            .unwrap();
        tx.send(HostCmd::Resize { cols: 80, rows: 24 }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer("test", rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("refresh-client -f no-output\n"));
        assert!(s.contains("refresh-client -C 80x24\n"));
        // One Ignore per command line written: send + resize = 2.
        assert_eq!(in_flight.lock().unwrap().len(), 2);
    }

    /// A writer whose child is gone must not let later commands look delivered. The
    /// writer returns on the broken write, which drops the receiver, so every later
    /// send on that channel reports the refusal to its caller. That refusal is the
    /// only signal this side has that a command never reached the host.
    #[test]
    fn writer_returns_on_broken_pipe_so_later_sends_are_refused() {
        struct BrokenPipe;
        impl Write for BrokenPipe {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Send(
            "switch-client -c /dev/pts/17 -t a\n".to_string(),
        ))
        .unwrap();
        let mut out = BrokenPipe;
        run_writer("test", rx, test_control_proto(), &mut out, &in_flight);
        assert!(
            tx.send(HostCmd::Send(
                "switch-client -c /dev/pts/17 -t b\n".to_string()
            ))
            .is_err(),
            "a send after the writer gave up reports the refusal instead of looking delivered"
        );
    }

    #[test]
    fn writer_query_list_clients_correlates() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Query {
            line: "list-clients -F '#{client_name}\t#{session_name}'".into(),
            reply: PendingReply::DisplayClientTty,
        })
        .unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer("test", rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("list-clients"),
            "writes the list-clients command: {s}"
        );
        assert!(
            matches!(
                in_flight.lock().unwrap().front(),
                Some(PendingReply::DisplayClientTty)
            ),
            "pushes the DisplayClientTty correlator"
        );
    }
}
