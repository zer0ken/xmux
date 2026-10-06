//! Probes every host concurrently to gather the sessions reachable from this
//! machine, isolating each host so one unreachable mux never blocks or fails
//! the rest. It owns the fan-out: bounded concurrency, a per-host timeout, and
//! order-preserving results.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::model::host_def::{within_deadline, HostDef};
use crate::session::Session;

/// One host's scan outcome. A non-`None` `err` means the host was
/// unreachable, in which case `sessions` is empty.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// The host alias.
    pub host: String,
    /// Empty when unreachable.
    pub sessions: Vec<Session>,
    /// `Some` ⇒ unreachable (the message).
    pub err: Option<String>,
}

impl ScanResult {
    /// This host's reachability in the single [`Liveness`](crate::model::Liveness)
    /// enum. The message stays in `err`.
    pub fn liveness(&self) -> crate::model::Liveness {
        crate::model::Liveness::from_scan_err(&self.err)
    }
}

/// Enumerates one host within a single budget shared by first contact and listing.
async fn scan_one(s: HostDef, per_host_timeout: Duration) -> ScanResult {
    let alias = s.alias.clone();
    let deadline = tokio::time::Instant::now() + per_host_timeout;
    // The outer timeouts bound a runner that does not limit itself; the deadline
    // makes a real command time out first, so they never drop it mid-read.
    let mut host = match timeout(per_host_timeout, within_deadline(deadline, s.host_for_op())).await
    {
        Ok(Ok(host)) => host,
        Ok(Err(e)) => {
            return ScanResult {
                host: alias,
                sessions: Vec::new(),
                err: Some(e.to_string()),
            };
        }
        Err(_) => {
            return ScanResult {
                host: alias,
                sessions: Vec::new(),
                err: Some(format!(
                    "timed out after {}s",
                    per_host_timeout.as_secs_f64()
                )),
            };
        }
    };
    match timeout(
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        within_deadline(deadline, host.enumerate_with(s.run_with())),
    )
    .await
    {
        Ok(Ok(())) => ScanResult {
            host: alias,
            sessions: host.inventory.sessions,
            err: None,
        },
        Ok(Err(e)) => ScanResult {
            host: alias,
            sessions: Vec::new(),
            err: Some(e.to_string()),
        },
        Err(_) => ScanResult {
            host: alias,
            sessions: Vec::new(),
            err: Some(format!(
                "timed out after {}s",
                per_host_timeout.as_secs_f64()
            )),
        },
    }
}

/// Probes every host concurrently and returns one [`ScanResult`] per host,
/// in input order. At most `max_concurrent` probes run at once; each probe is
/// bounded by `timeout`. One unreachable host never blocks or fails the others.
pub async fn scan_all(
    defs: &[HostDef],
    per_host_timeout: Duration,
    max_concurrent: usize,
) -> Vec<ScanResult> {
    let max_concurrent = max_concurrent.max(1);
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let mut set: JoinSet<(usize, ScanResult)> = JoinSet::new();

    for (i, s) in defs.iter().enumerate() {
        let s = s.clone();
        let sem = sem.clone();
        set.spawn(async move {
            // Acquire a slot BEFORE starting the timeout so a queued host does
            // not burn its budget waiting for a free slot.
            let _permit = sem.acquire().await.expect("semaphore not closed");
            let result = scan_one(s, per_host_timeout).await;
            (i, result)
        });
    }

    let mut out: Vec<Option<ScanResult>> = (0..defs.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        let (i, result) = joined.expect("scan task panicked");
        out[i] = Some(result);
    }
    out.into_iter()
        .map(|o| o.expect("every index filled"))
        .collect()
}

/// Streams each host's scan outcome as it completes, in completion order. Like
/// [`scan_all`] it probes concurrently with bounded concurrency and a per-host
/// timeout, but it hands each result out the moment that host resolves instead
/// of withholding everything until the slowest host answers. A caller
/// (`xmux ls`) can print what it already knows while a dead host is still timing
/// out, so the command never appears frozen. The receiver closes once every probe
/// has produced its result.
pub async fn scan_stream(
    defs: &[HostDef],
    per_host_timeout: Duration,
    max_concurrent: usize,
) -> mpsc::Receiver<ScanResult> {
    let max_concurrent = max_concurrent.max(1);
    let (tx, rx) = mpsc::channel(defs.len().max(1));
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let mut set: JoinSet<()> = JoinSet::new();

    for s in defs.iter().cloned() {
        let sem = sem.clone();
        let tx = tx.clone();
        set.spawn(async move {
            // Acquire a slot BEFORE starting the timeout so a queued host does
            // not burn its budget waiting for a free slot.
            let _permit = sem.acquire().await.expect("semaphore not closed");
            let result = scan_one(s, per_host_timeout).await;
            let _ = tx.send(result).await;
        });
    }
    drop(tx);
    // Drain the JoinSet off the caller's runtime so the channel alone signals
    // completion; it closes after the last probe has sent its result.
    tokio::spawn(async move { while set.join_next().await.is_some() {} });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::host_def::{RunError, Runner};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Returns canned list-sessions output (or an error), ignoring the command.
    struct StaticRunner {
        out: Vec<u8>,
        err_msg: Option<String>,
    }

    #[async_trait]
    impl Runner for StaticRunner {
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            match &self.err_msg {
                Some(m) => Err(RunError::Other(m.clone())),
                None => Ok(self.out.clone()),
            }
        }
    }

    // A generic host for the scan-behavior tests (ordering, unreachable
    // propagation, concurrency, timeout) — all host-type-agnostic. Uses `tmux`
    // (the aggregate-server path) so `list_sessions` exercises the runner directly;
    // local psmux's one-server-per-session registry path is tested in `host_def`.
    // Modeled as a REMOTE host so each distinct `alias` is a distinct host id: the
    // session tag is the host id (`transport.host_id()`), which for a remote equals the
    // alias. (Only one LOCAL host can exist, always with the alias `"local"`, so
    // distinct test hosts are remotes.) The `StaticRunner` ignores the wrapped argv, so
    // remote-vs-local does not change the canned output.
    fn scan_host(alias: &str, r: Arc<dyn Runner>) -> HostDef {
        HostDef {
            alias: alias.into(),
            binary: "tmux".into(),
            kind: crate::transport::MachineKind::Ssh {
                id: String::new(),
                alias: alias.into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            runner: Some(r),
            remote_shells: Default::default(),
            credentials: Default::default(),
        }
    }

    fn static_ok(line: &str) -> Arc<dyn Runner> {
        Arc::new(StaticRunner {
            out: line.as_bytes().to_vec(),
            err_msg: None,
        })
    }

    #[test]
    fn scan_result_projects_liveness() {
        use crate::model::Liveness;
        let live = ScanResult {
            host: "a".into(),
            sessions: Vec::new(),
            err: None,
        };
        assert_eq!(live.liveness(), Liveness::Live);
        let dead = ScanResult {
            host: "a".into(),
            sessions: Vec::new(),
            err: Some("boom".into()),
        };
        assert_eq!(dead.liveness(), Liveness::Unreachable);
    }

    #[tokio::test]
    async fn scan_all_preserves_order_and_content() {
        let defs = vec![
            scan_host("a", static_ok("2:1:editor\n")),
            scan_host("b", static_ok("1:0:build\n")),
            scan_host("c", static_ok("3:1:shell\n")),
        ];
        let got = scan_all(&defs, Duration::from_secs(1), 4).await;
        assert_eq!(got.len(), 3);
        let want_alias = ["a", "b", "c"];
        let want_name = ["editor", "build", "shell"];
        for (i, r) in got.iter().enumerate() {
            assert_eq!(r.host, want_alias[i]);
            assert!(r.err.is_none());
            assert_eq!(r.sessions.len(), 1);
            assert_eq!(r.sessions[0].name, want_name[i]);
            assert_eq!(r.sessions[0].host, want_alias[i]);
        }
    }

    #[tokio::test]
    async fn scan_all_one_unreachable_does_not_stop_others() {
        let defs = vec![
            scan_host("a", static_ok("1:1:one\n")),
            scan_host(
                "b",
                Arc::new(StaticRunner {
                    out: Vec::new(),
                    err_msg: Some("ssh: connect to host b port 22: Connection timed out".into()),
                }),
            ),
            scan_host("c", static_ok("1:0:two\n")),
        ];
        let got = scan_all(&defs, Duration::from_secs(1), 4).await;
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
        let defs = vec![scan_host(
            "a",
            Arc::new(StaticRunner {
                out: Vec::new(),
                err_msg: None,
            }),
        )];
        let got = scan_all(&defs, Duration::from_secs(1), 4).await;
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
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(8)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(b"1:0:s\n".to_vec())
        }
    }

    #[tokio::test]
    async fn scan_all_respects_concurrency_cap() {
        let cr = Arc::new(ConcurrencyRunner {
            active: AtomicI32::new(0),
            max: AtomicI32::new(0),
        });
        let defs: Vec<HostDef> = (0..5).map(|_| scan_host("s", cr.clone())).collect();
        let got = scan_all(&defs, Duration::from_secs(1), 2).await;
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
        let defs: Vec<HostDef> = (0..4).map(|_| scan_host("s", cr.clone())).collect();
        let got = scan_all(&defs, Duration::from_secs(1), 0).await;
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
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(b"1:0:s\n".to_vec())
        }
    }

    struct SlowFirstUseRunner;

    #[async_trait]
    impl Runner for SlowFirstUseRunner {
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            tokio::time::sleep(Duration::from_millis(30)).await;
            if args.last().map(String::as_str) == Some(crate::transport::vocab::SHELL_PROBE) {
                Ok(b"\n".to_vec())
            } else {
                Ok(b"1:0:ready\n".to_vec())
            }
        }
    }

    /// Runs a real command that outlives every budget through [`ExecRunner`], and
    /// records whether the scan dropped that run before the runner returned. A drop
    /// skips the runner's kill, reap, and drain, which on Windows leaves a pipe read
    /// in flight when its handle closes (#116).
    struct HungExecRunner {
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    struct DropMark {
        dropped: Arc<std::sync::atomic::AtomicBool>,
        returned: bool,
    }

    impl Drop for DropMark {
        fn drop(&mut self) {
            if !self.returned {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl Runner for HungExecRunner {
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            // A single process, so the post-kill drain reaches EOF at once: a shell
            // wrapper would fork a grandchild holding the pipe write ends.
            #[cfg(windows)]
            let (name, args) = (
                "powershell",
                vec![
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    "Start-Sleep -Seconds 30".to_string(),
                ],
            );
            #[cfg(not(windows))]
            let (name, args) = ("sleep", vec!["30".to_string()]);
            let mut mark = DropMark {
                dropped: self.dropped.clone(),
                returned: false,
            };
            let out = crate::model::host_def::ExecRunner.run(name, &args).await;
            mark.returned = true;
            out
        }
    }

    #[tokio::test]
    async fn scan_budget_lets_the_command_tear_itself_down() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let defs = vec![scan_host(
            "prod",
            Arc::new(HungExecRunner {
                dropped: dropped.clone(),
            }),
        )];

        let start = std::time::Instant::now();
        let got = scan_all(&defs, Duration::from_secs(2), 1).await;

        assert!(got[0].err.is_some());
        assert!(
            !dropped.load(Ordering::SeqCst),
            "the scan budget dropped a command before its own teardown returned"
        );
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn first_shell_probe_and_listing_share_the_scan_timeout() {
        let defs = vec![scan_host("prod", Arc::new(SlowFirstUseRunner))];

        let got = scan_all(&defs, Duration::from_millis(40), 1).await;

        assert_eq!(got[0].err.as_deref(), Some("timed out after 0.04s"));
    }

    #[tokio::test]
    async fn scan_all_per_host_timeout() {
        let defs = vec![scan_host("slow", Arc::new(BlockingRunner))];
        let start = std::time::Instant::now();
        let got = scan_all(&defs, Duration::from_millis(20), 4).await;
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "did not honor per-host timeout"
        );
        assert_eq!(got.len(), 1);
        assert!(got[0].err.is_some());
    }

    #[tokio::test]
    async fn scan_stream_hands_each_result_out_as_it_completes() {
        // A slow host and a fast one; the fast one must reach the receiver
        // first, so a caller can print it without waiting on the slow one.
        let defs = vec![
            scan_host("slow", Arc::new(BlockingRunner)),
            scan_host("fast", static_ok("1:0:ready\n")),
        ];
        let mut rx = scan_stream(&defs, Duration::from_secs(1), 4).await;
        let first = rx.recv().await.expect("a result");
        let second = rx.recv().await.expect("a result");
        assert!(
            rx.recv().await.is_none(),
            "channel closes after every probe"
        );
        // The fast host completes before the slow one, regardless of input order.
        assert_eq!(first.host, "fast");
        assert!(first.err.is_none());
        assert_eq!(first.sessions[0].name, "ready");
        assert_eq!(second.host, "slow");
        assert!(second.err.is_some(), "the slow host times out");
    }
}
