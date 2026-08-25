//! Structured logging for xmux: a non-blocking rolling file subscriber backed by
//! `tracing`. Writing goes exclusively to a file (`xmux_dir/xmux.log`) — never to
//! stdout or stderr — so ratatui's alt-screen is never corrupted by a stray log line.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// The base name the daily appender writes under; the date it suffixes makes the rest.
const LOG_BASE: &str = "xmux.log";

/// Base name of the ERROR-only triage log. Real failures are a handful of lines a day but
/// sit buried in a file that, on a busy Windows box, is hundreds of thousands of lines of
/// routine INFO; mirroring ERROR (and above) into its own bounded daily file keeps them
/// greppable in one `grep` instead of a 20 MB scan. It is NOT gated by `XMUX_LOG` — a
/// triage log that can be silenced is no triage log.
const ERROR_BASE: &str = "xmux-errors.log";

/// The guards that keep both background log writers alive. Dropping this drops both,
/// flushing each writer; the caller MUST bind it to a variable for the program lifetime.
pub struct LogGuard {
    _main: WorkerGuard,
    _errors: WorkerGuard,
}

/// How many daily files are kept. A rolling log that only ever rolls is a log that grows
/// without end: the oldest file goes when a new day opens, so the directory holds a bounded
/// window rather than every day xmux has ever run. Two weeks, because the window has to be
/// long enough to answer "when did this host start failing" from what is on disk.
const KEEP_DAYS: usize = 14;

/// The files this log is written to, as the pattern the appender produces: the daily
/// suffix means no single path is the log for long, so the pattern is what a reader needs
/// to find them. Named to be SHOWN - the unreachable screen states it - and opened by
/// nothing.
pub fn log_files(xmux_dir: &Path) -> std::path::PathBuf {
    xmux_dir.join(format!("{LOG_BASE}.<date>"))
}

/// Initialises the tracing subscriber and returns the `WorkerGuard` that keeps
/// the background log-writer alive. The caller MUST bind the guard to a variable
/// in `main` (or wherever the program lifetime lives) — dropping it early flushes
/// the writer and silences any subsequent log calls.
///
/// All output goes to `xmux_dir/xmux.log` via a daily rolling appender wrapped
/// in a non-blocking writer, keeping [`KEEP_DAYS`] days and dropping what is older. The env-filter reads `XMUX_LOG`; when the variable
/// is absent or contains an invalid directive the subscriber falls back to
/// `xmux=info`, which logs all `info`-and-above events inside the `xmux` crate.
pub fn init(xmux_dir: &Path) -> LogGuard {
    // The bounded window is the whole reason this is built rather than taken from the
    // one-line `daily` helper, which keeps every file it ever opens. A builder that cannot
    // be built falls back to that helper: logging without retention beats no logging.
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_BASE)
        .max_log_files(KEEP_DAYS)
        .build(xmux_dir)
        .unwrap_or_else(|_| tracing_appender::rolling::daily(xmux_dir, LOG_BASE));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // The ERROR-only triage mirror, bounded the same way and on its own writer.
    let error_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(ERROR_BASE)
        .max_log_files(KEEP_DAYS)
        .build(xmux_dir)
        .unwrap_or_else(|_| tracing_appender::rolling::daily(xmux_dir, ERROR_BASE));
    let (error_non_blocking, error_guard) = tracing_appender::non_blocking(error_appender);

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
    let error_layer = tracing_subscriber::fmt::layer()
        .with_writer(error_non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        // ERROR-and-above only, independent of XMUX_LOG: the triage log must always
        // capture real failures even when the main log's level is raised.
        .with_filter(LevelFilter::ERROR);

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(error_layer)
        .init();

    LogGuard {
        _main: guard,
        _errors: error_guard,
    }
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

    /// `init` must return a guard without panicking and must create both the main
    /// `xmux.log` and the ERROR-only `xmux-errors.log` (or date-suffixed variants)
    /// inside the supplied directory. Nothing in the logging path references stdout
    /// or stderr: the writers are the file appenders.
    #[test]
    fn init_creates_log_files_in_xmux_dir() {
        let dir = TempDir::new("xmux-logging-test-init");
        // init() may only be called ONCE per process (the global subscriber can
        // only be set once). Run this test in isolation; in a normal `cargo test`
        // run there is only one call to init(), so it is safe here.
        let guard = init(dir.path());

        // Emit one INFO (main log only) and one ERROR (mirrored to the triage log)
        // so the non-blocking writers have something to flush.
        tracing::info!("logging init test");
        tracing::error!("logging init test");

        // Drop the guards to flush the background writers before checking the dir.
        drop(guard);

        // The daily rolling appender writes `<dir>/<base>.<YYYY-MM-DD>`; at least
        // one file under each base must exist.
        let bases = ["xmux.log", "xmux-errors.log"];
        for base in bases {
            let entries: Vec<_> = std::fs::read_dir(dir.path())
                .expect("read_dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(base))
                .collect();

            assert!(
                !entries.is_empty(),
                "expected {base}* to be created in {}, got: {:?}",
                dir.path().display(),
                std::fs::read_dir(dir.path())
                    .unwrap()
                    .filter_map(|e| e.ok().map(|e| e.file_name()))
                    .collect::<Vec<_>>()
            );
        }
    }
}
