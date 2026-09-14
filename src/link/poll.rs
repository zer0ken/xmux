//! A POLL host's one-shot enumeration task, owned by `HostManager` for muxes with no
//! host-level control stream: it enumerates once when something asks for it - the launch
//! scan, a detection, or an explicit re-scan - and emits the result onto the same event
//! bus the control clients use.

use super::HostEvent;

/// A POLL host's one-shot enumeration task. A poll host has no host-level control stream,
/// so the [`HostManager`](super::HostManager) owns this task to enumerate its sessions +
/// panes and emit them as [`HostEvent`]s onto the same bus the control clients use.
///
/// The enumeration runs ONCE, at spawn, and the task then returns. It does not repeat on
/// a cadence: a POLL host's list is fetched exactly when something asked for it - the
/// launch scan, a detection, or an explicit re-scan
/// ([`HostManager::rescan`](super::HostManager::rescan)) - and never on a timer of its
/// own, so a machine is never queried for no one.
///
/// The task is re-armed only by that explicit re-scan. Selecting the card does not re-arm
/// it, and a probe does not: the finished handle stays for the re-scan to remove and
/// re-spawn. The task is aborted by a reap or the app exiting.
pub(super) async fn run_poll(
    source: String,
    transport: Box<dyn crate::transport::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // The one enumeration this spawn was asked for. A failure is still an answer (the nav
    // shows the host unreachable), so the task returns either way.
    mux.poll_once(
        &source,
        &transport,
        &crate::model::source::ExecRunner,
        &mut |ev| {
            // Log at the producer, where `err` is in hand. A success lists the sessions;
            // a failure is WARN.
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
                        tracing::info!(
                            host,
                            n = sessions.len(),
                            names = ?names,
                            "sessions_enumerated"
                        );
                    }
                }
            }
            let _ = events.send(ev);
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A POLL host is enumerated exactly once per spawn and the task then returns rather
    /// than polling on its own: a machine is never queried for no one. Uses a LOCAL
    /// psmux, whose local-registry enumeration succeeds (possibly empty) without any
    /// binary or network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_enumerates_once_and_returns() {
        let transport = crate::transport::local(None);
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, tx));

        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { source, .. } if source == "src"),
            "the spawn's one enumeration lands"
        );

        // The task returns after its one enumeration - no second sweep on any cadence.
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("the task returns after its one enumeration")
            .expect("the task body returns cleanly");
    }

    /// A failing enumeration is still an answer - the nav shows the host unreachable -
    /// and the task returns after it rather than repeating the request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_returns_after_a_failed_enumeration() {
        let transport = crate::transport::ssh(
            "xmux-nonexistent-host.invalid".into(),
            String::new(),
            "linux".into(),
        );
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, tx));

        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { source, err: Some(_), .. } if source == "src"),
            "the failing sweep lands with its error"
        );
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("the task returns after the failed sweep")
            .expect("the task body returns cleanly");
    }
}
