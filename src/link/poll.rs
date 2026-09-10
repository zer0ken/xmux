//! A POLL host's enumeration task, owned by `HostManager` for muxes with no
//! host-level control stream: it enumerates ONCE and emits the result onto the same
//! event bus the control clients use.

use super::HostEvent;

/// A POLL host's enumeration task. A poll host has no host-level control stream, so
/// the [`HostManager`](super::HostManager) owns this task to enumerate its sessions +
/// panes and emit them as [`HostEvent`]s onto the same bus the control clients use.
///
/// The enumeration runs ONCE, at spawn. It does not repeat on a cadence: every sweep of
/// a remote poll host is a fresh connection to that machine, and a sweep that repeats on
/// a timer is a connection that repeats on a timer whether or not anyone is waiting for
/// the answer - which is what a server's own defences read as an attack rather than as
/// a client. So the answer is fetched when something asked for it, and after that the
/// task PARKS.
///
/// Parking rather than returning is what keeps the channel LIVE
/// ([`HostManager::is_live`](super::HostManager::is_live)): a finished task reads as a
/// dropped channel, and every path that ensures a host's channel would then re-enumerate
/// it - a keystroke on a selected card would spawn a connection. So the task stays,
/// holding the one enumeration it was spawned to run, until it is aborted (a re-scan, a
/// reap, or the app exiting). Re-enumeration is that abort-and-respawn, raised by a user
/// asking for it ([`HostManager::rescan`](super::HostManager::rescan)).
///
/// Runs until aborted, or returns early when the event receiver is gone (app exit).
pub(super) async fn run_poll(
    source: String,
    transport: Box<dyn crate::transport::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // `poll_once` (the mux-blind enumeration) hands each event back here. The app's
    // receiver dropping (its exit) means there is nobody to park for, so the task returns.
    let mut gone = false;
    mux.poll_once(
        &source,
        &transport,
        &crate::model::source::ExecRunner,
        &mut |ev| {
            // Log the enumeration at the producer, where `err` is in hand. One line per
            // enumeration is one line per thing that asked for one, so the log carries
            // what the user did rather than a cadence nobody chose.
            if let HostEvent::Sessions {
                source: ref host,
                ref sessions,
                ref err,
            } = ev
            {
                match err {
                    Some(error) => tracing::warn!(host, error, "enumeration_failed"),
                    None => {
                        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                        tracing::info!(host, n = sessions.len(), names = ?names, "sessions_enumerated");
                    }
                }
            }
            if events.send(ev).is_err() {
                gone = true;
            }
        },
    )
    .await;
    if gone {
        return;
    }
    // Hold the channel open with no further work. Only an abort ends this.
    std::future::pending::<()>().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enumeration runs ONCE per spawn and the task then holds the channel open
    /// without asking the machine anything else. This is the whole point of the task's
    /// shape: a repeating sweep of a remote host is a repeating connection to it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_enumerates_once_and_then_asks_for_nothing_more() {
        let transport = crate::transport::ssh(
            "xmux-nonexistent-host.invalid".into(),
            String::new(),
            "linux".into(),
        );
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, tx));

        // The one enumeration this spawn was for.
        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { source, .. } if source == "src"),
            "the spawn's enumeration lands"
        );
        while let Ok(ev) = rx.try_recv() {
            assert!(
                !matches!(ev, HostEvent::Sessions { .. }),
                "one enumeration per spawn, not several"
            );
        }

        // Well past the fastest cadence the task could have kept, nothing else has been
        // emitted. The wait is real time because the enumeration is a real subprocess.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        assert!(
            rx.try_recv().is_err(),
            "five seconds on, the parked task has asked the machine nothing more"
        );
        assert!(
            !task.is_finished(),
            "the task parks rather than finishing, so its channel still reads as live"
        );
        task.abort();
    }
}
