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
/// A write error means the pipe is broken (the child died) — return so no further
/// stale entries are queued; the reader hits EOF and the client is reaped. Flushes
/// after each command so a real child sees it promptly. Returns on `Shutdown`.
pub fn run_writer<W: Write>(
    rx: std::sync::mpsc::Receiver<HostCmd>,
    proto: &dyn ControlProtocol,
    w: &mut W,
    in_flight: &InFlight,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            HostCmd::Send(line) => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
            HostCmd::Resize { cols, rows } => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(proto.size_line(cols, rows).as_bytes()).is_err() {
                    return;
                }
            }
            HostCmd::Query { line, reply } => {
                in_flight.lock().unwrap().push_back(reply);
                if w.write_all(line.as_bytes()).is_err() {
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
    use crate::host::test_control_proto;

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
        run_writer(rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("refresh-client -f no-output\n"));
        assert!(s.contains("refresh-client -C 80x24\n"));
        // One Ignore per command line written: send + resize = 2.
        assert_eq!(in_flight.lock().unwrap().len(), 2);
    }

    #[test]
    fn writer_query_list_panes_correlates() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Query {
            line: format!("list-panes -s -t if -F '{}'\n", crate::mux::PANE_FORMAT),
            reply: PendingReply::ListPanes {
                address: "jupiter00/if".into(),
            },
        })
        .unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("list-panes -s -t if -F"),
            "writes the list-panes command: {s}"
        );
        assert!(
            matches!(in_flight.lock().unwrap().front(), Some(PendingReply::ListPanes { address }) if address == "jupiter00/if"),
            "pushes the ListPanes correlator keyed by the session address"
        );
    }
}
