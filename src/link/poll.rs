//! A POLL host's re-enumeration task, owned by `HostManager` for muxes with no
//! host-level control stream: it re-enumerates on the mux's cadence while the host
//! keeps answering, and emits the results onto the same event bus the control
//! clients use.

use super::HostEvent;

/// A POLL host's re-enumeration task. A poll host has no host-level control stream, so
/// the [`HostManager`](super::HostManager) owns this task to re-enumerate sessions +
/// panes on the mux's cadence and emit them as [`HostEvent`]s onto the same bus the
/// control clients use.
///
/// The enumeration runs at spawn and then again on the mux's cadence WHILE the host
/// keeps answering. A poll sweep of an already-connected host reuses the path the host
/// answers over (a ControlMaster socket on a remote, a local command on a local host)
/// rather than opening a fresh unauthenticated connection each time, so keeping the
/// session/window list current costs nothing a host is not already honouring.
///
/// A sweep that FAILS is the last one. A request that answers nothing is one a later
/// request cannot answer either - a host that refuses one sweep refuses the next
/// identically, and a request that repeats on a timer is a connection that repeats on
/// a timer whether or not anyone is waiting for the answer. So the task returns at the
/// first failure, and the host is asked again only when the user asks: a re-scan or
/// selecting the card re-arms it ([`HostManager::rescan`](super::HostManager::rescan)
/// and the `ensure` a selection raises).
///
/// Returning at a failure also drops the channel it held, which is what keeps the host
/// from being read as still connected: the reconnect-sweep-era semantics are gone, so
/// nothing re-probes it until the user acts. The task is aborted by a reap or the app
/// exiting.
pub(super) async fn run_poll(
    source: String,
    transport: Box<dyn crate::transport::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    interval_ms: u64,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // Fixed-cadence ticker: the first tick is immediate (enumerate on spawn), then a
    // sweep every `interval_ms` of wall-clock. Skip ticks missed while one enumeration
    // ran long, so a slow probe paces the loop instead of piling up overlapping sweeps.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Per-source last-known name set: suppress INFO when an answering sweep is identical
    // to the one before it, so an idle connected host does not fill the log on its cadence.
    // A failure is logged once because the sweep that fails is the last one.
    let mut last_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut first_poll = true;
    loop {
        ticker.tick().await;
        // Whether the app's receiver dropped (its exit): a failed send latches `gone`
        // and the task returns after this sweep.
        let mut gone = false;
        // Whether this sweep failed. A failing sweep is the last one - the host is asked
        // again only when the user asks - so a failed send and a failed sweep both end
        // the loop.
        let mut failed = false;
        mux.poll_once(
            &source,
            &transport,
            &crate::model::source::ExecRunner,
            &mut |ev| {
                // Log the enumeration at the producer, where `err` is in hand. A success
                // that changed the session set (or is the first) is INFO; an unchanged
                // one is TRACE. A failure is WARN, and ends the loop.
                if let HostEvent::Sessions {
                    source: ref host,
                    ref sessions,
                    ref err,
                } = ev
                {
                    match err {
                        Some(error) => {
                            failed = true;
                            tracing::warn!(host, error, "enumeration_failed");
                        }
                        None => {
                            let names: std::collections::BTreeSet<String> =
                                sessions.iter().map(|s| s.name.clone()).collect();
                            if first_poll || names != last_names {
                                let names_list: Vec<&str> =
                                    names.iter().map(|s| s.as_str()).collect();
                                tracing::info!(
                                    host,
                                    n = sessions.len(),
                                    names = ?names_list,
                                    "sessions_enumerated"
                                );
                                last_names = names;
                                first_poll = false;
                            } else {
                                tracing::trace!(
                                    host,
                                    n = sessions.len(),
                                    "sessions_enumerated_unchanged"
                                );
                            }
                        }
                    }
                }
                if events.send(ev).is_err() {
                    gone = true;
                }
            },
        )
        .await;
        if gone || failed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sweep that fails is the last one: the task returns after it rather than
    /// repeating a request the host cannot answer. The failure is the real ssh dial to a
    /// nonexistent host, which refuses within the connect budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_stops_at_the_first_failed_enumeration() {
        let transport = crate::transport::ssh(
            "xmux-nonexistent-host.invalid".into(),
            String::new(),
            "linux".into(),
        );
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, 100, tx));

        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { source, err: Some(_), .. } if source == "src"),
            "the spawn's failing sweep lands with its error"
        );

        // The task returns after the failure: nothing asks the host again.
        tokio::time::timeout(std::time::Duration::from_secs(30), task)
            .await
            .expect("the task returns after the failed sweep")
            .expect("the task body returns cleanly");
    }

    /// While the host keeps answering, the task re-enumerates on the cadence instead of
    /// asking once and going quiet: session/window changes (a session switch inside a
    /// session) show up in the nav. Uses a LOCAL psmux, whose local-registry enumeration
    /// succeeds (possibly empty) without any binary or network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_reenumerates_on_cadence_while_the_host_answers() {
        let transport = crate::transport::local(None);
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, 50, tx));

        // A few sweeps across a handful of cadence ticks: a successful poll host keeps
        // answering, so the enumerations keep coming rather than stopping after one.
        let mut sweeps = 0usize;
        for _ in 0..5 {
            match tokio::time::timeout(std::time::Duration::from_secs(15), rx.recv()).await {
                Ok(Some(HostEvent::Sessions { err: None, .. })) => sweeps += 1,
                Ok(Some(HostEvent::Sessions { err: Some(_), .. })) => {
                    panic!("a local psmux enumeration must not fail")
                }
                Ok(None) => panic!("the event channel closed"),
                Err(_) => panic!("no enumeration within the budget"),
                Ok(Some(_)) => {}
            }
        }
        assert!(
            sweeps >= 2,
            "a poll host that keeps answering is re-enumerated, not asked once: {sweeps}"
        );
        task.abort();
    }
}
