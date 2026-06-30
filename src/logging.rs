//! Structured logging for xmux: a non-blocking rolling file subscriber backed by
//! `tracing`. Writing goes exclusively to a file (`xmux_dir/xmux.log`) — never to
//! stdout or stderr — so ratatui's alt-screen is never corrupted by a stray log line.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Initialises the tracing subscriber and returns the `WorkerGuard` that keeps
/// the background log-writer alive. The caller MUST bind the guard to a variable
/// in `main` (or wherever the program lifetime lives) — dropping it early flushes
/// the writer and silences any subsequent log calls.
///
/// All output goes to `xmux_dir/xmux.log` via a daily rolling appender wrapped
/// in a non-blocking writer. The env-filter reads `XMUX_LOG`; when the variable
/// is absent or contains an invalid directive the subscriber falls back to
/// `xmux=info`, which logs all `info`-and-above events inside the `xmux` crate.
pub fn init(xmux_dir: &Path) -> WorkerGuard {
    let file_appender = tracing_appender::rolling::daily(xmux_dir, "xmux.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Parse XMUX_LOG; fall back to "xmux=info" when the variable is absent or
    // the directive string is syntactically invalid (EnvFilter::try_from_env can
    // return an error for malformed directives, not just for a missing variable).
    let env_filter =
        EnvFilter::try_from_env("XMUX_LOG").unwrap_or_else(|_| EnvFilter::new("xmux=info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_filter(env_filter);

    tracing_subscriber::registry().with(fmt_layer).init();

    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Returns a unique directory under `std::env::temp_dir()` for the test,
    /// creating it on demand and removing it on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&p).expect("create temp dir");
            TempDir(p)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `init` must return a guard without panicking and must create `xmux.log`
    /// (or a date-suffixed variant) inside the supplied directory. Nothing in the
    /// logging path references stdout or stderr: the writer is the file appender
    /// returned by `tracing_appender::rolling::daily`.
    #[test]
    fn init_creates_log_file_in_xmux_dir() {
        let dir = TempDir::new("xmux-logging-test-init");
        // init() may only be called ONCE per process (the global subscriber can
        // only be set once). Run this test in isolation; in a normal `cargo test`
        // run there is only one call to init(), so it is safe here.
        let guard = init(dir.path());

        // Emit one event so the non-blocking writer has something to flush.
        tracing::info!("logging init test");

        // Drop the guard to flush the background writer before checking the dir.
        drop(guard);

        // The daily rolling appender writes to `<dir>/xmux.log.<YYYY-MM-DD>`.
        // At least one file whose name starts with "xmux.log" must exist.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("xmux.log"))
            .collect();

        assert!(
            !entries.is_empty(),
            "expected xmux.log* to be created in {}, got: {:?}",
            dir.path().display(),
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>()
        );
    }
}
