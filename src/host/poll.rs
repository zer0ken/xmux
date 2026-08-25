//! A POLL host's self-looping enumeration task, owned by `HostManager` for muxes
//! with no host-level control stream: it emits `HostEvent`s onto the same bus.

use super::HostEvent;

/// What a source's failures have said so far, so a sweep can be asked whether its outcome
/// is NEWS. A polled source that cannot answer fails every sweep with the same message for
/// as long as xmux runs, forty sweeps to the minute: the message is worth writing when it
/// arrives and when it changes, and worth counting the rest of the time.
#[derive(Default)]
struct Failures {
    standing: Option<String>,
    sweeps: u64,
}

impl Failures {
    /// Folds in a failed sweep. `None` means this message is new (or replaces a different
    /// one) and is worth writing whole; `Some(n)` means the failure already stood and this
    /// is the nth sweep to hit it.
    fn failed(&mut self, error: &str) -> Option<u64> {
        if self.standing.as_deref() == Some(error) {
            self.sweeps += 1;
            return Some(self.sweeps);
        }
        self.standing = Some(error.to_string());
        self.sweeps = 1;
        None
    }

    /// Folds in a sweep that answered. `Some((message, sweeps))` when that ends a run of
    /// failures, which is as much news as the failure starting; `None` when nothing was
    /// standing to recover from.
    fn recovered(&mut self) -> Option<(String, u64)> {
        let stood = self.standing.take()?;
        let sweeps = std::mem::take(&mut self.sweeps);
        Some((stood, sweeps))
    }
}

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
    // What this source's failures have said so far, so a standing failure is counted
    // instead of rewritten every tick.
    let mut failures = Failures::default();
    loop {
        ticker.tick().await;
        // `poll_once` (the mux-blind sweep) hands each event back here. The app's
        // receiver dropping (its exit) is the loop's other stop condition besides abort,
        // so a failed send latches `gone` and the loop returns after this sweep.
        let mut gone = false;
        mux.poll_once(&source, &transport, &crate::source::ExecRunner, &mut |ev| {
            // Log enumeration at the producer (where `err` is in hand). A sweep that says
            // what the one before it said is not news, whichever way it went: an unchanged
            // session set is TRACE, and so is a failure already standing. WARN is for a
            // failure arriving or changing, INFO for a set changing or a source answering
            // again. So the log carries the source's HISTORY rather than its cadence, and
            // an unreachable source cannot fill the file on its own.
            if let HostEvent::Sessions {
                source: ref host,
                ref sessions,
                ref err,
            } = ev
            {
                let n = sessions.len();
                if let Some(error) = err {
                    match failures.failed(error) {
                        Some(sweeps) => {
                            tracing::trace!(host, sweeps, "enumeration_failing_still")
                        }
                        None => tracing::warn!(host, error, "enumeration_failed"),
                    }
                } else {
                    // A failure that stopped is as much news as one that started: without
                    // this line the log would end on a failure the source has since
                    // recovered from.
                    if let Some((stood, sweeps)) = failures.recovered() {
                        tracing::info!(host, sweeps, was = %stood, "enumeration_recovered");
                    }
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

#[cfg(test)]
mod tests {
    use super::Failures;

    #[test]
    fn a_failure_is_news_once_and_counted_after() {
        let mut f = Failures::default();
        assert_eq!(f.failed("no server running"), None, "the first is news");
        assert_eq!(f.failed("no server running"), Some(2));
        assert_eq!(f.failed("no server running"), Some(3));
    }

    #[test]
    fn a_different_message_is_news_again() {
        let mut f = Failures::default();
        assert_eq!(f.failed("no server running"), None);
        assert_eq!(
            f.failed("connection refused"),
            None,
            "a new message is news"
        );
        assert_eq!(
            f.failed("connection refused"),
            Some(2),
            "and counts from one"
        );
    }

    #[test]
    fn recovery_reports_the_run_it_ended() {
        let mut f = Failures::default();
        f.failed("connection refused");
        f.failed("connection refused");
        assert_eq!(
            f.recovered(),
            Some(("connection refused".to_string(), 2)),
            "the message and how many sweeps hit it"
        );
        assert_eq!(f.recovered(), None, "and it is reported once");
    }

    #[test]
    fn a_source_that_never_failed_recovers_from_nothing() {
        assert_eq!(Failures::default().recovered(), None);
    }

    #[test]
    fn a_failure_after_a_recovery_is_news() {
        let mut f = Failures::default();
        f.failed("connection refused");
        f.recovered();
        assert_eq!(f.failed("connection refused"), None);
    }
}
