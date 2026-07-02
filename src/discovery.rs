//! Probes every source concurrently to gather the sessions reachable from this
//! machine, isolating each source so one unreachable mux never blocks or fails
//! the rest. It owns the fan-out: bounded concurrency, a per-source timeout, and
//! order-preserving results.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::session::Session;
use crate::source::Source;

/// One source's scan outcome. A non-`None` `err` means the source was
/// unreachable, in which case `sessions` is empty.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// The source alias.
    pub source: String,
    /// Empty when unreachable.
    pub sessions: Vec<Session>,
    /// `Some` ⇒ unreachable (the message).
    pub err: Option<String>,
}

/// Probes every source concurrently and returns one [`ScanResult`] per source,
/// in input order. At most `max_concurrent` probes run at once; each probe is
/// bounded by `timeout`. One unreachable source never blocks or fails the others.
pub async fn scan_all(
    srcs: &[Source],
    per_source_timeout: Duration,
    max_concurrent: usize,
) -> Vec<ScanResult> {
    let max_concurrent = max_concurrent.max(1);
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let mut set: JoinSet<(usize, ScanResult)> = JoinSet::new();

    for (i, s) in srcs.iter().enumerate() {
        let s = s.clone();
        let sem = sem.clone();
        set.spawn(async move {
            // Acquire a slot BEFORE starting the timeout so a queued source does
            // not burn its budget waiting for a free slot.
            let _permit = sem.acquire().await.expect("semaphore not closed");
            let alias = s.alias.clone();
            let result = match timeout(per_source_timeout, s.list_sessions()).await {
                Ok(Ok(sessions)) => ScanResult {
                    source: alias,
                    sessions,
                    err: None,
                },
                Ok(Err(e)) => ScanResult {
                    source: alias,
                    sessions: Vec::new(),
                    err: Some(e.to_string()),
                },
                Err(_elapsed) => ScanResult {
                    source: alias,
                    sessions: Vec::new(),
                    err: Some(format!(
                        "timed out after {}s",
                        per_source_timeout.as_secs_f64()
                    )),
                },
            };
            (i, result)
        });
    }

    let mut out: Vec<Option<ScanResult>> = (0..srcs.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        let (i, result) = joined.expect("scan task panicked");
        out[i] = Some(result);
    }
    out.into_iter()
        .map(|o| o.expect("every index filled"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{RunError, Runner};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Returns canned list-sessions output (or an error), ignoring the command.
    struct StaticRunner {
        out: Vec<u8>,
        err_msg: Option<String>,
    }

    #[async_trait]
    impl Runner for StaticRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            match &self.err_msg {
                Some(m) => Err(RunError::Other(m.clone())),
                None => Ok(self.out.clone()),
            }
        }
    }

    // A generic source for the scan-behavior tests (ordering, unreachable
    // propagation, concurrency, timeout) — all source-type-agnostic. Uses `tmux`
    // (the aggregate-server path) so `list_sessions` exercises the runner directly;
    // local psmux's one-server-per-session registry path is tested in `source`.
    // Modeled as a REMOTE source so each distinct `alias` is a distinct host id: the
    // session tag is the host id (`transport.host_id()`), which for a remote equals the
    // alias. (Only one LOCAL source can exist, always with the alias `"local"`, so
    // distinct test hosts are remotes.) The `StaticRunner` ignores the wrapped argv, so
    // remote-vs-local does not change the canned output.
    fn scan_source(alias: &str, r: Arc<dyn Runner>) -> Source {
        Source {
            alias: alias.into(),
            binary: "tmux".into(),
            kind: crate::machine::MachineKind::Ssh {
                alias: alias.into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            runner: Some(r),
        }
    }

    fn static_ok(line: &str) -> Arc<dyn Runner> {
        Arc::new(StaticRunner {
            out: line.as_bytes().to_vec(),
            err_msg: None,
        })
    }

    #[tokio::test]
    async fn scan_all_preserves_order_and_content() {
        let srcs = vec![
            scan_source("a", static_ok("2\t1\t1781246739\teditor\n")),
            scan_source("b", static_ok("1\t0\t0\tbuild\n")),
            scan_source("c", static_ok("3\t1\t1781246800\tshell\n")),
        ];
        let got = scan_all(&srcs, Duration::from_secs(1), 4).await;
        assert_eq!(got.len(), 3);
        let want_alias = ["a", "b", "c"];
        let want_name = ["editor", "build", "shell"];
        for (i, r) in got.iter().enumerate() {
            assert_eq!(r.source, want_alias[i]);
            assert!(r.err.is_none());
            assert_eq!(r.sessions.len(), 1);
            assert_eq!(r.sessions[0].name, want_name[i]);
            assert_eq!(r.sessions[0].source, want_alias[i]);
        }
    }

    #[tokio::test]
    async fn scan_all_one_unreachable_does_not_stop_others() {
        let srcs = vec![
            scan_source("a", static_ok("1\t1\t0\tone\n")),
            scan_source(
                "b",
                Arc::new(StaticRunner {
                    out: Vec::new(),
                    err_msg: Some("ssh: connect to host b port 22: Connection timed out".into()),
                }),
            ),
            scan_source("c", static_ok("1\t0\t0\ttwo\n")),
        ];
        let got = scan_all(&srcs, Duration::from_secs(1), 4).await;
        assert_eq!(got.len(), 3);
        assert!(got[1].err.is_some());
        assert!(got[1].sessions.is_empty());
        assert!(got[0].err.is_none());
        assert_eq!(got[0].sessions[0].name, "one");
        assert!(got[2].err.is_none());
        assert_eq!(got[2].sessions[0].name, "two");
    }

    #[tokio::test]
    async fn scan_all_reachable_empty() {
        let srcs = vec![scan_source(
            "a",
            Arc::new(StaticRunner {
                out: Vec::new(),
                err_msg: None,
            }),
        )];
        let got = scan_all(&srcs, Duration::from_secs(1), 4).await;
        assert_eq!(got.len(), 1);
        assert!(got[0].err.is_none());
        assert!(got[0].sessions.is_empty());
    }

    /// Tracks live in-flight calls and records the peak observed concurrency.
    struct ConcurrencyRunner {
        active: AtomicI32,
        max: AtomicI32,
    }

    #[async_trait]
    impl Runner for ConcurrencyRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(8)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(b"1\t0\t0\ts\n".to_vec())
        }
    }

    #[tokio::test]
    async fn scan_all_respects_concurrency_cap() {
        let cr = Arc::new(ConcurrencyRunner {
            active: AtomicI32::new(0),
            max: AtomicI32::new(0),
        });
        let srcs: Vec<Source> = (0..5).map(|_| scan_source("s", cr.clone())).collect();
        let got = scan_all(&srcs, Duration::from_secs(1), 2).await;
        assert_eq!(got.len(), 5);
        assert!(
            cr.max.load(Ordering::SeqCst) <= 2,
            "peak concurrency {} exceeded cap 2",
            cr.max.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn scan_all_max_concurrent_below_one_treated_as_one() {
        let cr = Arc::new(ConcurrencyRunner {
            active: AtomicI32::new(0),
            max: AtomicI32::new(0),
        });
        let srcs: Vec<Source> = (0..4).map(|_| scan_source("s", cr.clone())).collect();
        let got = scan_all(&srcs, Duration::from_secs(1), 0).await;
        assert_eq!(got.len(), 4);
        assert!(
            cr.max.load(Ordering::SeqCst) <= 1,
            "max_concurrent<1 must behave as 1; peak {}",
            cr.max.load(Ordering::SeqCst)
        );
    }

    /// Sleeps a long time; a fired timeout drops (cancels) the future.
    struct BlockingRunner;

    #[async_trait]
    impl Runner for BlockingRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(b"1\t0\t0\ts\n".to_vec())
        }
    }

    #[tokio::test]
    async fn scan_all_per_source_timeout() {
        let srcs = vec![scan_source("slow", Arc::new(BlockingRunner))];
        let start = std::time::Instant::now();
        let got = scan_all(&srcs, Duration::from_millis(20), 4).await;
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "did not honor per-source timeout"
        );
        assert_eq!(got.len(), 1);
        assert!(got[0].err.is_some());
    }
}
