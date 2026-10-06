//! A POLL host's enumeration task, owned by `HostManager` for muxes with no host-level
//! control stream: it enumerates when something asks for it - the launch scan, a
//! detection, or an explicit re-scan - and, over a path that is already open, keeps the
//! answering host's list current on a cadence. It emits onto the same event bus the
//! control clients use.

use std::time::Duration;

use super::HostEvent;

/// How often a connected POLL host is re-enumerated over a path it already holds open.
/// Short enough that a rename or a session made inside the mux reaches the nav while the
/// user is still looking, and well inside the shared ssh master's 60 s persistence, so
/// the master the first enumeration opened is the one every later one rides.
pub(super) const POLL_REFRESH: Duration = Duration::from_secs(3);

/// A POLL host's enumeration task. A poll host has no host-level control stream, so the
/// [`HostManager`](super::HostManager) owns this task to enumerate its sessions + panes
/// and emit them as [`HostEvent`]s onto the same bus the control clients use.
///
/// The enumeration runs at spawn. `refresh` is `Some` only for a host reached over a path
/// that is already open (the local box, a WSL distribution, or an ssh machine whose runs
/// share one master), and then the enumeration repeats on that cadence WHILE the host
/// keeps answering: a repeat there opens nothing on the machine, so it is the open path
/// carrying changes, not a new request. With `refresh` `None` every repeat would be a
/// fresh login, so the task returns after its one enumeration and the host is asked again
/// only by an explicit re-scan.
///
/// An enumeration that FAILS is the last one. A request that answers nothing is one a
/// later request cannot answer either, so the task returns and the host is asked again
/// only when the user asks ([`HostManager::rescan`](super::HostManager::rescan)). The
/// task is aborted by a reap or the app exiting.
pub(super) async fn run_poll(
    host: String,
    transport: Box<dyn crate::transport::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    refresh: Option<Duration>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // The last answered name list, so an unchanged refresh does not log on its cadence:
    // the file records what changed, not that a timer fired.
    let mut last_names: Option<Vec<String>> = None;
    loop {
        // A failed enumeration and a dropped receiver (the app exiting) both end the task.
        let mut stop = false;
        mux.poll_once(
            &host,
            &transport,
            &crate::model::host_def::ExecRunner,
            &mut |ev| {
                // Log at the producer, where `err` is in hand. A success that changed the
                // session list (or is the first) is INFO carrying that list; an unchanged
                // one is TRACE. A failure is WARN.
                if let HostEvent::Sessions {
                    ref host,
                    ref sessions,
                    ref err,
                } = ev
                {
                    match err {
                        Some(error) => {
                            stop = true;
                            tracing::warn!(host, error, "enumeration_failed");
                        }
                        None => {
                            let names: Vec<String> =
                                sessions.iter().map(|s| s.name.clone()).collect();
                            if last_names.as_ref() != Some(&names) {
                                tracing::info!(
                                    host,
                                    n = sessions.len(),
                                    names = ?names,
                                    "sessions_enumerated"
                                );
                                last_names = Some(names);
                            } else {
                                tracing::trace!(host, n = sessions.len(), "sessions_unchanged");
                            }
                        }
                    }
                }
                if events.send(ev).is_err() {
                    stop = true;
                }
            },
        )
        .await;
        match refresh {
            Some(every) if !stop => tokio::time::sleep(every).await,
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a refresh cadence a POLL host is enumerated exactly once per spawn and the
    /// task then returns: a path where every repeat is a fresh login is never repeated on
    /// its own. Uses a LOCAL psmux, whose local-registry enumeration succeeds (possibly
    /// empty) without any binary or network.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_a_refresh_the_task_enumerates_once_and_returns() {
        let transport = crate::transport::local(None);
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll("src".to_string(), transport, mux, None, tx));

        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { host, .. } if host == "src"),
            "the spawn's one enumeration lands"
        );

        // The task returns after its one enumeration - no second sweep on any cadence.
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("the task returns after its one enumeration")
            .expect("the task body returns cleanly");
    }

    /// With a refresh cadence an answering host is enumerated again and again, so a change
    /// made inside the mux reaches the nav without anyone asking, and the task stays live.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_a_refresh_an_answering_host_is_enumerated_again() {
        let transport = crate::transport::local(None);
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let task = tokio::spawn(run_poll(
            "src".to_string(),
            transport,
            mux,
            Some(Duration::from_millis(50)),
            tx,
        ));
        for sweep in 0..2 {
            let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
                .await
                .expect("each sweep answers within its own budget")
                .expect("each sweep emits its result");
            assert!(
                matches!(&ev, HostEvent::Sessions { host, err: None, .. } if host == "src"),
                "sweep {sweep} lands as an answered enumeration"
            );
        }
        assert!(!task.is_finished(), "an answering host keeps its task live");
        task.abort();
    }

    /// A failing enumeration is still an answer - the nav shows the host unreachable -
    /// and the task returns after it rather than repeating the request, even where a
    /// refresh cadence was given.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_returns_after_a_failed_enumeration() {
        let transport = crate::transport::ssh(
            "xmux-nonexistent-host.invalid".into(),
            String::new(),
            "linux".into(),
        );
        let mux = crate::mux::for_binary("psmux").expect("psmux is a known mux");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        // A cadence is given, so returning is the failure's doing, not a missing refresh.
        let task = tokio::spawn(run_poll(
            "src".to_string(),
            transport,
            mux,
            Some(Duration::from_millis(50)),
            tx,
        ));

        let first = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the enumeration answers within its own budget")
            .expect("it emits its result");
        assert!(
            matches!(&first, HostEvent::Sessions { host, err: Some(_), .. } if host == "src"),
            "the failing sweep lands with its error"
        );
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("the task returns after the failed sweep")
            .expect("the task body returns cleanly");
    }
}
