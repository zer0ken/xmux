//! A POLL host's self-looping enumeration task, owned by `HostManager` for muxes
//! with no host-level control stream: it emits `HostEvent`s onto the same bus.

use super::HostEvent;

/// A POLL host's self-looping enumeration task. A poll host has no host-level control
/// stream, so the [`HostManager`](super::HostManager) owns this task to re-enumerate sessions + panes on
/// the mux's cadence and emit them as [`HostEvent`]s onto the same bus the control
/// clients use. Runs until aborted (reap / teardown) or the event receiver is dropped
/// (app exit). Mirrors a control client's connect-then-stream role for poll muxes.
pub(super) async fn run_poll(
    source: String,
    transport: Box<dyn crate::machine::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    interval_ms: u64,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // Fixed-cadence ticker: the first tick is immediate (enumerate on spawn), then a
    // sweep every `interval_ms` of wall-clock. Skip ticks missed while one enumeration
    // ran long, so a slow probe paces the loop instead of piling up overlapping sweeps.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Per-source last-known name set: suppress INFO when the enumeration is identical to
    // the previous sweep (reduces log noise for idle polls while keeping change visibility).
    let mut last_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut first_poll = true;
    loop {
        ticker.tick().await;
        // `poll_once` (the mux-blind sweep) hands each event back here. The app's
        // receiver dropping (its exit) is the loop's other stop condition besides abort,
        // so a failed send latches `gone` and the loop returns after this sweep.
        let mut gone = false;
        mux.poll_once(&source, &transport, &crate::source::ExecRunner, &mut |ev| {
            // Log enumeration at the producer (where `err` is in hand): on success emit
            // INFO when the set changed, TRACE when unchanged; on error emit WARN. This
            // keeps the log quiet for idle polls while making changes and failures visible.
            if let HostEvent::Sessions {
                source: ref host,
                ref sessions,
                ref err,
            } = ev
            {
                let n = sessions.len();
                if let Some(error) = err {
                    tracing::warn!(host, error, "enumeration_failed");
                } else {
                    let names: std::collections::BTreeSet<String> =
                        sessions.iter().map(|s| s.name.clone()).collect();
                    if first_poll || names != last_names {
                        let names_list: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                        tracing::info!(host, n, names = ?names_list, "sessions_enumerated");
                        last_names = names;
                        first_poll = false;
                    } else {
                        tracing::trace!(host, n, "sessions_enumerated_unchanged");
                    }
                }
            }
            if events.send(ev).is_err() {
                gone = true;
            }
        })
        .await;
        if gone {
            return;
        }
    }
}
